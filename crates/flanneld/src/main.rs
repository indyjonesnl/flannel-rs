mod annotation;
mod config;
mod ipmasq;
mod kube_mgr;
mod netlink;
mod peer;
mod subnet;

use crate::config::NetConf;
use crate::kube_mgr::KubeMgr;
use crate::netlink::Netlink;
use crate::peer::{reconcile, Action};
use crate::subnet::{vxlan_mtu, SubnetEnv};
use anyhow::{Context, Result};
use futures::StreamExt;
use ipnetwork::Ipv4Network;
use k8s_openapi::api::core::v1::Node;
use kube::runtime::{reflector, watcher, WatchStreamExt};
use kube::Api;
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::time::Duration;
use tracing::{info, warn};

const DEV: &str = "flannel.1";
const VNI: u32 = 1;
const DSTPORT: u16 = 8472;
const NET_CONF_PATH: &str = "/etc/kube-flannel/net-conf.json";

/// Misconfiguration that no amount of retrying will fix. Surfacing these as a
/// non-zero exit lets the DaemonSet crash-loop visibly instead of silently
/// spinning forever.
#[derive(Debug, thiserror::Error)]
enum Fatal {
    #[error("malformed net-conf.json: {0}")]
    NetConf(String),
    #[error("unsupported backend {0:?}; only vxlan is supported")]
    Backend(String),
}

/// State assembled by a successful bootstrap that the reconcile loop needs.
struct BootstrapState {
    dev_idx: u32,
    /// FLANNEL_NETWORK: the cluster-wide pod CIDR.
    network: String,
    /// FLANNEL_SUBNET: this node's pod CIDR.
    subnet: String,
}

/// Parse + validate net-conf.json. A parse failure or a non-vxlan backend are
/// fatal; everything else (fetching the raw string) is handled by the caller as
/// transient. Pure so it can be unit-tested without a cluster.
fn classify_net_conf(raw: &str) -> Result<NetConf, Fatal> {
    let nc = NetConf::parse(raw).map_err(|e| Fatal::NetConf(e.to_string()))?;
    if nc.backend.kind != "vxlan" {
        return Err(Fatal::Backend(nc.backend.kind));
    }
    Ok(nc)
}

/// One full bootstrap attempt. Transient failures are returned as plain anyhow
/// errors (the caller retries); fatal misconfig is returned as a `Fatal` that
/// downcasts so the caller can stop.
async fn try_bootstrap(mgr: &KubeMgr, nl: &Netlink) -> Result<BootstrapState> {
    // Read raw from the mounted ConfigMap file (transient on error), then
    // classify (fatal on bad config).
    let raw = tokio::fs::read_to_string(NET_CONF_PATH)
        .await
        .context("read /etc/kube-flannel/net-conf.json")?;
    let nc = classify_net_conf(&raw)?; // Fatal -> bails, downcasts in bootstrap()

    let own = mgr.own_node().await?; // node-not-found / no-PodCIDR -> transient
    let local: Ipv4Addr = own.public_ip.parse().context("parse node IP")?;
    let cidr: Ipv4Network = own.pod_cidr.parse().context("parse own PodCIDR")?;
    // Sanity check (parity: flannel verifies the lease sits within the network).
    // Non-fatal: kube assigns the PodCIDR, so just surface a misconfiguration.
    if let Ok(net) = nc.network.parse::<Ipv4Network>() {
        if !crate::subnet::subnet_in_network(cidr, net) {
            warn!(subnet = %own.pod_cidr, network = %nc.network,
                  "node PodCIDR is not within the flannel Network");
        }
    }
    let gateway = cidr.network();

    let link_mtu = nl.link_mtu_by_ip(local).await.unwrap_or_else(|e| {
        warn!(?e, "could not read underlay MTU; defaulting to 1500");
        1500
    });
    let overlay_mtu = vxlan_mtu(link_mtu);
    info!(link_mtu, overlay_mtu, "underlay/overlay MTU selected");

    let (mac, dev_idx) = nl
        .ensure_vxlan(DEV, VNI, DSTPORT, local, gateway, overlay_mtu)
        .await?;
    info!(%mac, dev_idx, "vxlan device ready");
    mgr.publish(&own.public_ip, &mac).await?;

    let env = SubnetEnv {
        network: nc.network.clone(),
        subnet: own.pod_cidr.clone(),
        mtu: overlay_mtu,
        ipmasq: true,
    };
    tokio::fs::create_dir_all("/run/flannel").await.ok();
    tokio::fs::write("/run/flannel/subnet.env", env.render())
        .await
        .context("write subnet.env")?; // transient FS error -> retry
    info!("wrote /run/flannel/subnet.env");

    Ok(BootstrapState {
        dev_idx,
        network: nc.network.clone(),
        subnet: own.pod_cidr.clone(),
    })
}

/// Retry bootstrap with exponential backoff (1s..30s) for transient failures.
/// Fatal misconfig stops immediately so `main` exits non-zero.
async fn bootstrap(mgr: &KubeMgr, nl: &Netlink) -> Result<BootstrapState> {
    let mut delay = Duration::from_secs(1);
    loop {
        match try_bootstrap(mgr, nl).await {
            Ok(s) => return Ok(s),
            Err(e) if e.downcast_ref::<Fatal>().is_some() => return Err(e),
            Err(e) => {
                warn!(?e, ?delay, "bootstrap attempt failed; retrying");
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(30));
            }
        }
    }
}

/// True if any arg requests the version (`--version`/`-V`). Pure so the parsing
/// is unit-tested without spawning the process.
fn wants_version<I: IntoIterator<Item = String>>(args: I) -> bool {
    args.into_iter().any(|a| a == "--version" || a == "-V")
}

/// Reconcile installed VXLAN peers against the current node Store: build the
/// desired peer set, diff it against `installed`, and apply add/remove via
/// netlink. A failed add is left out of `installed` so it is retried on the next
/// watch event or resync tick.
async fn reconcile_from_store(
    store: &reflector::Store<Node>,
    self_name: &str,
    nl: &Netlink,
    dev_idx: u32,
    installed: &mut HashMap<String, crate::peer::Peer>,
) {
    let nodes: Vec<Node> = store.state().iter().map(|a| (**a).clone()).collect();
    let desired = crate::kube_mgr::desired_from_nodes(&nodes, self_name);
    let mut next = installed.clone();
    for action in reconcile(installed, &desired) {
        match action {
            Action::Add(p) => {
                let r1 = nl.add_route(dev_idx, &p).await;
                let r2 = nl.add_peer_l2(dev_idx, &p).await;
                match (r1, r2) {
                    (Ok(()), Ok(())) => {
                        info!(node = %p.node, cidr = %p.pod_cidr, "peer added");
                        next.insert(p.node.clone(), p);
                    }
                    (a, b) => {
                        if let Err(e) = a {
                            warn!(?e, node = %p.node, "add_route failed; will retry");
                        }
                        if let Err(e) = b {
                            warn!(?e, node = %p.node, "add_peer_l2 failed; will retry");
                        }
                        next.remove(&p.node);
                    }
                }
            }
            Action::Remove(p) => {
                if let Err(e) = nl.del_peer(dev_idx, &p).await {
                    warn!(?e, node = %p.node, "del_peer");
                }
                next.remove(&p.node);
                info!(node = %p.node, "peer removed");
            }
        }
    }
    *installed = next;
}

#[tokio::main]
async fn main() -> Result<()> {
    // Handle --version before anything else: lets `docker run <image> --version`
    // exec and exit 0 without NODE_NAME or a cluster — a cheap release smoke test
    // that the image's binaries are actually runnable.
    if wants_version(std::env::args().skip(1)) {
        println!("flanneld {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let node_name = std::env::var("NODE_NAME").context("NODE_NAME env required")?;
    let mgr = KubeMgr::new(node_name.clone()).await?;
    let nl = Netlink::new()?;

    let BootstrapState {
        dev_idx,
        network,
        subnet,
    } = bootstrap(&mgr, &nl).await?;

    // subnet.env advertises FLANNEL_IPMASQ=true, so install the matching
    // source-NAT rules. Detect the iptables backend kube-proxy uses once, then
    // re-assert the rules on every reconcile tick so a flush (e.g. kube-proxy
    // restart) self-heals.
    let masq = match ipmasq::IpMasq::detect() {
        Ok(m) => {
            info!(backend = m.backend(), "ip-masq backend selected");
            Some(m)
        }
        Err(e) => {
            warn!(
                ?e,
                "could not detect iptables backend; ip-masq rules not installed"
            );
            None
        }
    };
    if let Some(m) = &masq {
        match m.ensure(&network, &subnet) {
            Ok(()) => info!(%network, %subnet, "ip-masq rules ensured"),
            Err(e) => warn!(?e, "failed to install ip-masq rules; will retry"),
        }
    }

    let mut installed: HashMap<String, crate::peer::Peer> = HashMap::new();

    // Watch nodes via a reflector-backed store so peer changes reconcile
    // near-instantly; the watcher relists on desync with its own backoff.
    let api: Api<Node> = Api::all(mgr.client());
    let (store, writer) = reflector::store::<Node>();
    // default_backoff: on a watch error (e.g. transient apiserver hiccup) back off
    // per client-go conventions instead of hot-looping the select! arm.
    let mut stream = reflector(
        writer,
        watcher(api, watcher::Config::default()).default_backoff(),
    )
    .boxed();

    // Safety-net peer resync (also retries failed netlink ops) and ip-masq
    // re-assertion run on independent timers. interval() fires immediately on the
    // first tick, giving an initial reconcile + masq ensure right away.
    let mut peer_resync = tokio::time::interval(Duration::from_secs(60));
    let mut masq_tick = tokio::time::interval(Duration::from_secs(10));

    // Exactly one branch runs to completion per iteration, so reconciles never
    // overlap (no shared-state hazard across the netlink awaits).
    loop {
        tokio::select! {
            ev = stream.next() => match ev {
                Some(Ok(_)) => {
                    reconcile_from_store(&store, &node_name, &nl, dev_idx, &mut installed).await;
                }
                Some(Err(e)) => warn!(?e, "node watch error; watcher will resync"),
                None => return Err(anyhow::anyhow!("node watch stream ended unexpectedly")),
            },
            _ = peer_resync.tick() => {
                reconcile_from_store(&store, &node_name, &nl, dev_idx, &mut installed).await;
            }
            _ = masq_tick.tick() => {
                if let Some(m) = &masq {
                    if let Err(e) = m.ensure(&network, &subnet) {
                        warn!(?e, "failed to re-assert ip-masq rules; will retry");
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_rejects_malformed_json() {
        let err = classify_net_conf("{not json").unwrap_err();
        assert!(matches!(err, Fatal::NetConf(_)), "got {err:?}");
    }

    #[test]
    fn classify_rejects_non_vxlan_backend() {
        let raw = r#"{"Network":"10.244.0.0/16","Backend":{"Type":"host-gw"}}"#;
        let err = classify_net_conf(raw).unwrap_err();
        match err {
            Fatal::Backend(kind) => assert_eq!(kind, "host-gw"),
            other => panic!("expected Backend, got {other:?}"),
        }
    }

    #[test]
    fn version_flag_detected() {
        assert!(wants_version(vec![
            "flanneld".to_string(),
            "--version".to_string()
        ]));
        assert!(wants_version(vec!["-V".to_string()]));
        assert!(!wants_version(vec!["flanneld".to_string()]));
        assert!(!wants_version(Vec::<String>::new()));
    }

    #[test]
    fn classify_accepts_vxlan() {
        let raw = r#"{"Network":"10.244.0.0/16","Backend":{"Type":"vxlan"}}"#;
        let nc = classify_net_conf(raw).unwrap();
        assert_eq!(nc.network, "10.244.0.0/16");
        assert_eq!(nc.backend.kind, "vxlan");
    }
}
