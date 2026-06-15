mod config;
mod subnet;
mod annotation;
mod peer;
mod netlink;
mod kube_mgr;

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::time::Duration;
use anyhow::{Context, Result};
use ipnetwork::Ipv4Network;
use tracing::{info, warn};
use crate::netlink::Netlink;
use crate::kube_mgr::KubeMgr;
use crate::peer::{reconcile, Action};
use crate::subnet::{SubnetEnv, vxlan_mtu};

const DEV: &str = "flannel.1";
const VNI: u32 = 1;
const DSTPORT: u16 = 8472;
const LINK_MTU: u32 = 1500;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter(
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "info".into())).init();

    let node_name = std::env::var("NODE_NAME").context("NODE_NAME env required")?;
    let mgr = KubeMgr::new(node_name.clone()).await?;
    let nl = Netlink::new()?;

    let nc = mgr.net_conf().await?;
    anyhow::ensure!(nc.backend.kind == "vxlan", "only vxlan backend supported");
    let own = mgr.own_node().await?;
    let local: Ipv4Addr = own.public_ip.parse().context("parse node IP")?;
    let cidr: Ipv4Network = own.pod_cidr.parse().context("parse own PodCIDR")?;
    let gateway = cidr.network();

    let (mac, dev_idx) = nl.ensure_vxlan(DEV, VNI, DSTPORT, local, gateway).await?;
    info!(%mac, dev_idx, "vxlan device ready");
    mgr.publish(&own.public_ip, &mac).await?;

    let env = SubnetEnv {
        network: nc.network.clone(),
        subnet: own.pod_cidr.clone(),
        mtu: vxlan_mtu(LINK_MTU),
        ipmasq: true,
    };
    tokio::fs::create_dir_all("/run/flannel").await.ok();
    tokio::fs::write("/run/flannel/subnet.env", env.render()).await
        .context("write subnet.env")?;
    info!("wrote /run/flannel/subnet.env");

    let mut installed: HashMap<String, crate::peer::Peer> = HashMap::new();
    loop {
        match mgr.desired_peers().await {
            Ok(desired) => {
                for action in reconcile(&installed, &desired) {
                    match action {
                        Action::Add(p) => {
                            if let Err(e) = nl.add_route(dev_idx, &p).await { warn!(?e, "add_route"); }
                            if let Err(e) = nl.add_peer_l2(dev_idx, &p).await { warn!(?e, "add_peer_l2"); }
                            info!(node = %p.node, cidr = %p.pod_cidr, "peer added");
                        }
                        Action::Remove(p) => {
                            if let Err(e) = nl.del_peer(dev_idx, &p).await { warn!(?e, "del_peer"); }
                            info!(node = %p.node, "peer removed");
                        }
                    }
                }
                installed = desired;
            }
            Err(e) => warn!(?e, "list peers failed; will retry"),
        }
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
}
