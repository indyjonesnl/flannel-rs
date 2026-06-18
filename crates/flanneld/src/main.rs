mod annotation;
mod config;
mod ipmasq;
mod kube_mgr;
mod netlink;
mod peer;
mod subnet;

use crate::config::{BackendType, NetConf};
use crate::kube_mgr::KubeMgr;
use crate::netlink::Netlink;
use crate::peer::{reconcile, Action};
use crate::subnet::{host_gw_mtu, vxlan_mtu, SubnetEnv};
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
    #[error("unsupported backend {0:?}; only vxlan and host-gw are supported")]
    Backend(String),
}

/// State assembled by a successful bootstrap that the reconcile loop needs.
struct BootstrapState {
    /// Output interface for peer routes: the `flannel.1` index (vxlan) or the
    /// host NIC index (host-gw).
    dev_idx: u32,
    /// The selected backend (determines the per-peer netlink action).
    backend: BackendType,
    /// FLANNEL_NETWORK: the cluster-wide pod CIDR.
    network: String,
    /// FLANNEL_SUBNET: this node's pod CIDR.
    subnet: String,
}

/// Parse + validate net-conf.json. A parse failure or an unsupported backend are
/// fatal; everything else (fetching the raw string) is handled by the caller as
/// transient. Pure so it can be unit-tested without a cluster.
fn classify_net_conf(raw: &str) -> Result<(NetConf, BackendType), Fatal> {
    let nc = NetConf::parse(raw).map_err(|e| Fatal::NetConf(e.to_string()))?;
    let backend = BackendType::parse(&nc.backend.kind)
        .ok_or_else(|| Fatal::Backend(nc.backend.kind.clone()))?;
    Ok((nc, backend))
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
    let (nc, backend) = classify_net_conf(&raw)?; // Fatal -> bails, downcasts in bootstrap()

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
    // MTU is backend-specific: vxlan subtracts overlay overhead; host-gw routes
    // directly, so pods get the full link MTU.
    let mtu = match backend {
        BackendType::Vxlan => vxlan_mtu(link_mtu),
        BackendType::HostGw => host_gw_mtu(link_mtu),
    };
    info!(link_mtu, mtu, ?backend, "MTU selected");

    // Per-backend setup: vxlan creates the overlay device and publishes its
    // VtepMAC; host-gw creates no device, routes via the host NIC, and publishes
    // no backend-data. Both yield the output-interface index for peer routes.
    let dev_idx = match backend {
        BackendType::Vxlan => {
            let (mac, idx) = nl
                .ensure_vxlan(DEV, VNI, DSTPORT, local, gateway, mtu)
                .await?;
            info!(%mac, idx, "vxlan device ready");
            mgr.publish(backend, &own.public_ip, Some(&mac)).await?;
            idx
        }
        BackendType::HostGw => {
            let oif = nl.link_index_by_ip(local).await?;
            info!(oif, "host-gw: peer routes via host NIC");
            mgr.publish(backend, &own.public_ip, None).await?;
            oif
        }
    };

    let env = SubnetEnv {
        network: nc.network.clone(),
        subnet: own.pod_cidr.clone(),
        mtu,
        ipmasq: true,
    };
    tokio::fs::create_dir_all("/run/flannel").await.ok();
    tokio::fs::write("/run/flannel/subnet.env", env.render())
        .await
        .context("write subnet.env")?; // transient FS error -> retry
    info!("wrote /run/flannel/subnet.env");

    Ok(BootstrapState {
        dev_idx,
        backend,
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
    backend: BackendType,
    installed: &mut HashMap<String, crate::peer::Peer>,
) {
    let nodes: Vec<Node> = store.state().iter().map(|a| (**a).clone()).collect();
    let desired = crate::kube_mgr::desired_from_nodes(&nodes, self_name, backend);
    let mut next = installed.clone();
    for action in reconcile(installed, &desired) {
        match action {
            Action::Add(p) => {
                let added = match backend {
                    // vxlan: pod-CIDR route via VTEP on flannel.1 + fdb/neigh.
                    BackendType::Vxlan => {
                        let r1 = nl.add_route(dev_idx, &p).await;
                        let r2 = nl.add_peer_l2(dev_idx, &p).await;
                        if let Err(e) = &r1 {
                            warn!(?e, node = %p.node, "add_route failed; will retry");
                        }
                        if let Err(e) = &r2 {
                            warn!(?e, node = %p.node, "add_peer_l2 failed; will retry");
                        }
                        r1.is_ok() && r2.is_ok()
                    }
                    // host-gw: direct route to pod CIDR via the peer node IP.
                    BackendType::HostGw => match nl.add_host_gw_route(dev_idx, &p).await {
                        Ok(()) => true,
                        Err(e) => {
                            warn!(?e, node = %p.node, "add_host_gw_route failed; will retry");
                            false
                        }
                    },
                };
                if added {
                    info!(node = %p.node, cidr = %p.pod_cidr, ?backend, "peer added");
                    next.insert(p.node.clone(), p);
                } else {
                    next.remove(&p.node); // re-attempt next event/resync
                }
            }
            Action::Remove(p) => {
                let r = match backend {
                    BackendType::Vxlan => nl.del_peer(dev_idx, &p).await,
                    BackendType::HostGw => nl.del_host_gw_route(dev_idx, &p).await,
                };
                if let Err(e) = r {
                    warn!(?e, node = %p.node, "peer remove failed");
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
        backend,
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
                    reconcile_from_store(&store, &node_name, &nl, dev_idx, backend, &mut installed).await;
                }
                Some(Err(e)) => warn!(?e, "node watch error; watcher will resync"),
                None => return Err(anyhow::anyhow!("node watch stream ended unexpectedly")),
            },
            _ = peer_resync.tick() => {
                reconcile_from_store(&store, &node_name, &nl, dev_idx, backend, &mut installed).await;
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
    fn classify_rejects_unknown_backend() {
        let raw = r#"{"Network":"10.244.0.0/16","Backend":{"Type":"udp"}}"#;
        let err = classify_net_conf(raw).unwrap_err();
        match err {
            Fatal::Backend(kind) => assert_eq!(kind, "udp"),
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
        let (nc, backend) = classify_net_conf(raw).unwrap();
        assert_eq!(nc.network, "10.244.0.0/16");
        assert_eq!(backend, BackendType::Vxlan);
    }

    #[test]
    fn classify_accepts_host_gw() {
        let raw = r#"{"Network":"10.244.0.0/16","Backend":{"Type":"host-gw"}}"#;
        let (nc, backend) = classify_net_conf(raw).unwrap();
        assert_eq!(nc.network, "10.244.0.0/16");
        assert_eq!(backend, BackendType::HostGw);
    }
}
