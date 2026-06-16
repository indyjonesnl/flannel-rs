use crate::plan::IpPlan;
use anyhow::{Context, Result};
use futures::TryStreamExt;
use nix::sched::{setns, CloneFlags};
use std::net::Ipv4Addr;

/// Inside the container netns (identified by `netns_path`): find the moved interface
/// (currently named `temp_name`), rename it to `ifname`, bring it up, assign the
/// pod IP, and install routes (each via the gateway) plus a default route if asked.
/// Runs on a dedicated thread that setns()'s in, then restores the host ns.
pub fn configure_container_iface(
    netns_path: String,
    temp_name: String,
    ifname: String,
    plan: IpPlan,
    add_default_route: bool,
) -> Result<()> {
    let handle = std::thread::spawn(move || -> Result<()> {
        let host_ns = std::fs::File::open("/proc/self/ns/net").context("open host netns")?;
        let cont_ns =
            std::fs::File::open(&netns_path).with_context(|| format!("open netns {netns_path}"))?;
        // nix 0.29 setns takes `impl AsFd`; a `&File` satisfies it.
        setns(&cont_ns, CloneFlags::CLONE_NEWNET).context("setns into container")?;

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("build container-ns runtime")?;
        let res = rt.block_on(configure(&temp_name, &ifname, &plan, add_default_route));

        // Restore host ns regardless of result (thread also terminates).
        let _ = setns(&host_ns, CloneFlags::CLONE_NEWNET);
        res
    });
    handle
        .join()
        .map_err(|_| anyhow::anyhow!("container-ns thread panicked"))?
}

async fn configure(
    temp_name: &str,
    ifname: &str,
    plan: &IpPlan,
    add_default_route: bool,
) -> Result<()> {
    let (conn, h, _) = rtnetlink::new_connection().context("netlink conn in container ns")?;
    tokio::spawn(conn);

    let idx = idx_by_name(&h, temp_name)
        .await?
        .context("moved iface not found in container ns")?;
    h.link()
        .set(idx)
        .name(ifname.to_string())
        .execute()
        .await
        .context("rename iface")?;
    h.link()
        .set(idx)
        .up()
        .execute()
        .await
        .context("set iface up")?;
    h.address()
        .add(idx, plan.addr.into(), plan.prefix)
        .execute()
        .await
        .context("assign pod ip")?;

    let gw = plan.gateway;
    for net in &plan.routes {
        let mut req = h
            .route()
            .add()
            .v4()
            .destination_prefix(net.network(), net.prefix())
            .output_interface(idx);
        if let Some(g) = gw {
            req = req.gateway(g);
        }
        let _ = req.execute().await; // ignore EEXIST-style duplicates
    }
    if add_default_route {
        if let Some(g) = gw {
            let _ = h
                .route()
                .add()
                .v4()
                .destination_prefix(Ipv4Addr::UNSPECIFIED, 0)
                .gateway(g)
                .output_interface(idx)
                .execute()
                .await;
        }
    }
    Ok(())
}

async fn idx_by_name(h: &rtnetlink::Handle, name: &str) -> Result<Option<u32>> {
    let mut links = h.link().get().match_name(name.to_string()).execute();
    match links.try_next().await {
        Ok(Some(l)) => Ok(Some(l.header.index)),
        Ok(None) => Ok(None),
        Err(rtnetlink::Error::NetlinkError(e)) if e.raw_code() == -19 => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Best-effort teardown: enter the netns and delete `ifname` (removes the veth pair).
pub fn delete_container_iface(netns_path: String, ifname: String) -> Result<()> {
    let handle = std::thread::spawn(move || -> Result<()> {
        let host_ns = std::fs::File::open("/proc/self/ns/net").context("open host netns")?;
        let cont_ns = match std::fs::File::open(&netns_path) {
            Ok(f) => f,
            Err(_) => return Ok(()), // netns already gone -> nothing to delete
        };
        setns(&cont_ns, CloneFlags::CLONE_NEWNET).context("setns into container")?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _ = rt.block_on(async {
            let (conn, h, _) = rtnetlink::new_connection()?;
            tokio::spawn(conn);
            if let Some(idx) = idx_by_name(&h, &ifname).await? {
                let _ = h.link().del(idx).execute().await;
            }
            Ok::<(), anyhow::Error>(())
        });
        let _ = setns(&host_ns, CloneFlags::CLONE_NEWNET);
        Ok(())
    });
    handle
        .join()
        .map_err(|_| anyhow::anyhow!("container-ns del thread panicked"))?
}
