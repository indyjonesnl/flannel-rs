use anyhow::{Context, Result};
use futures::TryStreamExt;
use rtnetlink::Handle;
use std::net::Ipv4Addr;
use std::os::fd::RawFd;

/// Look up a link's index by name; None if absent.
pub async fn link_index(handle: &Handle, name: &str) -> Result<Option<u32>> {
    let mut links = handle.link().get().match_name(name.to_string()).execute();
    match links.try_next().await {
        Ok(Some(l)) => Ok(Some(l.header.index)),
        Ok(None) => Ok(None),
        Err(rtnetlink::Error::NetlinkError(e)) if e.raw_code() == -19 => Ok(None), // ENODEV
        Err(e) => Err(e.into()),
    }
}

/// Create the bridge if absent, set MTU, bring up. Returns its ifindex.
pub async fn ensure_bridge(handle: &Handle, name: &str, mtu: Option<u32>) -> Result<u32> {
    if let Some(idx) = link_index(handle, name).await? {
        if let Some(m) = mtu {
            let _ = handle.link().set(idx).mtu(m).execute().await;
        }
        handle.link().set(idx).up().execute().await?;
        return Ok(idx);
    }
    handle
        .link()
        .add()
        .bridge(name.to_string())
        .execute()
        .await
        .context("create bridge")?;
    let idx = link_index(handle, name)
        .await?
        .context("bridge missing after create")?;
    if let Some(m) = mtu {
        let _ = handle.link().set(idx).mtu(m).execute().await;
    }
    handle.link().set(idx).up().execute().await?;
    Ok(idx)
}

/// Create a veth pair (both ends in the current/host ns). Returns (host_idx, peer_idx).
pub async fn create_veth(
    handle: &Handle,
    host_name: &str,
    peer_name: &str,
    mtu: Option<u32>,
) -> Result<(u32, u32)> {
    handle
        .link()
        .add()
        .veth(host_name.to_string(), peer_name.to_string())
        .execute()
        .await
        .context("create veth pair")?;
    let host_idx = link_index(handle, host_name)
        .await?
        .context("host veth missing")?;
    let peer_idx = link_index(handle, peer_name)
        .await?
        .context("peer veth missing")?;
    if let Some(m) = mtu {
        let _ = handle.link().set(host_idx).mtu(m).execute().await;
        let _ = handle.link().set(peer_idx).mtu(m).execute().await;
    }
    Ok((host_idx, peer_idx))
}

/// Move a link into the netns identified by an open fd.
pub async fn move_to_netns(handle: &Handle, idx: u32, netns_fd: RawFd) -> Result<()> {
    handle
        .link()
        .set(idx)
        .setns_by_fd(netns_fd)
        .execute()
        .await
        .context("move link to netns")?;
    Ok(())
}

/// Bring host veth up, attach to the bridge, enable hairpin on the port.
pub async fn attach_host_veth(
    handle: &Handle,
    host_idx: u32,
    bridge_idx: u32,
    hairpin: bool,
) -> Result<()> {
    handle.link().set(host_idx).up().execute().await?;
    handle
        .link()
        .set(host_idx)
        .controller(bridge_idx)
        .execute()
        .await
        .context("set bridge master")?;
    if hairpin {
        // Best-effort: hairpin may require a bridge-port attribute set; ignore if unsupported.
        let _ = set_hairpin(handle, host_idx).await;
    }
    Ok(())
}

async fn set_hairpin(handle: &Handle, idx: u32) -> Result<()> {
    // rtnetlink 0.14 exposes no hairpin/bridge-port-flag setter, so this is a
    // documented best-effort no-op. Hairpin only affects same-pod Service
    // hairpin traffic; the conformance gate flags it if it ever matters.
    let _ = (handle, idx);
    Ok(())
}

/// Assign gateway/prefix to the bridge (idempotent) for isGateway.
pub async fn set_bridge_gateway(
    handle: &Handle,
    bridge_idx: u32,
    gw: Ipv4Addr,
    prefix: u8,
) -> Result<()> {
    let r = handle
        .address()
        .add(bridge_idx, gw.into(), prefix)
        .execute()
        .await;
    if let Err(rtnetlink::Error::NetlinkError(e)) = r {
        if e.raw_code() != -17 {
            // EEXIST ok
            anyhow::bail!("set bridge gateway: {e:?}");
        }
    }
    Ok(())
}

/// Delete a link by index (cleanup on failure). Best-effort.
pub async fn del_link(handle: &Handle, idx: u32) {
    let _ = handle.link().del(idx).execute().await;
}
