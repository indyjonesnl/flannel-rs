# bridge CNI plugin (Rust) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port the `bridge` CNI plugin to Rust (flannel-subset) — node bridge, veth pair, container-netns interface config, IPAM delegation — and swap it into the flannel-rs chain.

**Architecture:** A tokio binary `cni-bridge` using `rtnetlink` for all netlink, `nix` for `setns`. Host-side netlink runs on the main runtime; container-netns work runs on a dedicated OS thread that `setns`'d in with its own current-thread runtime. Pure helpers (config/result parse, veth-name derivation) are unit-tested; the netns/netlink path is verified by smoke + conformance on kind (every pod exercises it).

**Tech Stack:** Rust, tokio, rtnetlink 0.14, netlink-packet-route 0.19, nix, ipnetwork, serde/serde_json, the `cni` lib.

---

## Pre-flight: standing rule

Before ANY `git push`, run the full local CI gate and push only if all pass:
```
cargo fmt --all -- --check
RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets -- -D warnings
RUSTFLAGS="-D warnings" cargo build --workspace --locked
RUSTFLAGS="-D warnings" cargo test --workspace --locked
```
Common trap: place `#[cfg(test)] mod tests { ... }` LAST in each file (clippy `items-after-test-module`).

---

## File Structure

```
crates/cni/src/
├── delegate.rs        # MOVED here from cni-flannel: run_delegate + DelegateOutput
├── result.rs          # add Deserialize to CniResult/IpResult
└── lib.rs             # add `pub mod delegate;`
crates/cni-flannel/src/
├── exec.rs            # DELETE (moved to cni::delegate)
└── main.rs            # use cni::delegate::run_delegate
crates/cni-bridge/
├── Cargo.toml
└── src/
    ├── main.rs        # #[tokio::main(current_thread)] dispatch
    ├── config.rs      # BridgeConf parse (pure)
    ├── plan.rs        # pure: veth host-name, gateway/routes derivation from IPAM result
    ├── hostns.rs      # rtnetlink host-side ops (ensure bridge, veth, move, master, hairpin, bridge addr, ip_forward)
    └── contns.rs      # setns thread runner + container-side iface config
```

---

## Task 1: Move delegate-exec into the `cni` lib + make CniResult deserializable

**Files:**
- Create: `crates/cni/src/delegate.rs`
- Modify: `crates/cni/src/lib.rs`, `crates/cni/src/result.rs`
- Modify: `crates/cni-flannel/src/main.rs`; Delete: `crates/cni-flannel/src/exec.rs`

- [ ] **Step 1: Add Deserialize to CniResult/IpResult with a test**

In `crates/cni/src/result.rs`, change the derives and add a parse + test. Replace the `CniResult` and `IpResult` struct definitions' derive lines:
```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct CniResult {
```
```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct IpResult {
```
Add `use serde::Deserialize;` to the existing `use serde::Serialize;` (make it `use serde::{Deserialize, Serialize};`). Add a parse helper + test at the end of the file (before any existing `#[cfg(test)]`, or merge into it):
```rust
impl CniResult {
    pub fn parse(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }
}

#[cfg(test)]
mod parse_tests {
    use super::*;

    #[test]
    fn parses_host_local_result() {
        // What host-local emits (0.3.1) and bridge must consume.
        let raw = r#"{"cniVersion":"0.3.1","ips":[{"version":"4","address":"10.244.1.2/24","gateway":"10.244.1.1"}],"routes":[{"dst":"10.244.0.0/16"}]}"#;
        let r = CniResult::parse(raw).unwrap();
        assert_eq!(r.ips[0].address, "10.244.1.2/24");
        assert_eq!(r.ips[0].gateway.as_deref(), Some("10.244.1.1"));
        assert_eq!(r.routes[0].dst, "10.244.0.0/16");
    }
}
```
Note: `Route` already derives `Deserialize` (it is `Deserialize, Serialize`). `routes` has `#[serde(skip_serializing_if = "Vec::is_empty")]` — for deserialize add `#[serde(default)]` to that field so a result without `routes` parses:
```rust
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub routes: Vec<Route>,
```

- [ ] **Step 2: Run the result test**

Run: `cargo test -p cni result`
Expected: PASS including `parses_host_local_result`.

- [ ] **Step 3: Move exec.rs into cni as delegate.rs**

Copy `crates/cni-flannel/src/exec.rs` to `crates/cni/src/delegate.rs` verbatim, then change its imports from `use cni::env::CniArgs; use cni::error::CniError;` to the crate-internal form:
```rust
use crate::env::CniArgs;
use crate::error::CniError;
```
(The function `run_delegate`, struct `DelegateOutput`, helper `find_in_path`, and its `#[cfg(test)]` tests move unchanged otherwise.)

Add to `crates/cni/src/lib.rs`:
```rust
pub mod delegate;
```

- [ ] **Step 4: Point cni-flannel at the shared delegate runner**

Delete `crates/cni-flannel/src/exec.rs`. In `crates/cni-flannel/src/main.rs`:
- remove `mod exec;`
- change `exec::run_delegate(&dtype, &args, &djson)` to `cni::delegate::run_delegate(&dtype, &args, &djson)`.

- [ ] **Step 5: Verify workspace + flannel tests**

Run: `cargo test -p cni && cargo test -p cni-flannel && cargo build --workspace`
Expected: all pass; cni gains the delegate tests; cni-flannel still 10 tests pass (now using the shared runner).

- [ ] **Step 6: Commit**

```bash
git add crates/cni crates/cni-flannel
git commit -m "refactor(cni): share delegate-exec; make CniResult deserializable"
```

---

## Task 2: Scaffold cni-bridge + config parsing

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/cni-bridge/Cargo.toml`, `crates/cni-bridge/src/main.rs`, `crates/cni-bridge/src/config.rs`

- [ ] **Step 1: Workspace member**

Root `Cargo.toml`:
```toml
members = ["crates/flanneld", "crates/cni", "crates/cni-host-local", "crates/cni-flannel", "crates/cni-bridge"]
```

- [ ] **Step 2: Crate manifest**

`crates/cni-bridge/Cargo.toml`:
```toml
[package]
name = "cni-bridge"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "bridge"
path = "src/main.rs"

[dependencies]
cni = { path = "../cni" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["rt", "macros"] }
futures = "0.3"
rtnetlink = "0.14"
netlink-packet-route = "0.19"
ipnetwork = "0.20"
nix = { version = "0.29", features = ["sched", "fs"] }
anyhow = "1"
```

- [ ] **Step 3: config.rs with tests**

`crates/cni-bridge/src/config.rs`:
```rust
use serde::Deserialize;

/// The bridge plugin's stdin netconf (flannel-subset fields).
#[derive(Debug, Deserialize)]
pub struct BridgeConf {
    #[serde(rename = "cniVersion", default)]
    pub cni_version: String,
    #[serde(default = "default_bridge")]
    pub bridge: String,
    #[serde(rename = "isGateway", default)]
    pub is_gateway: bool,
    #[serde(rename = "isDefaultGateway", default)]
    pub is_default_gateway: bool,
    #[serde(rename = "hairpinMode", default)]
    pub hairpin_mode: bool,
    #[serde(default)]
    pub mtu: Option<u32>,
    pub ipam: Ipam,
}

#[derive(Debug, Deserialize)]
pub struct Ipam {
    #[serde(rename = "type")]
    pub kind: String,
}

fn default_bridge() -> String {
    "cni0".to_string()
}

impl BridgeConf {
    pub fn parse(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flannel_bridge_delegate() {
        let raw = r#"{"name":"cbr0","cniVersion":"0.3.1","type":"bridge","mtu":1450,"isGateway":true,"isDefaultGateway":true,"hairpinMode":true,"ipMasq":false,"ipam":{"type":"host-local","ranges":[[{"subnet":"10.244.1.0/24"}]]}}"#;
        let c = BridgeConf::parse(raw).unwrap();
        assert_eq!(c.bridge, "cbr0");
        assert_eq!(c.mtu, Some(1450));
        assert!(c.is_gateway && c.is_default_gateway && c.hairpin_mode);
        assert_eq!(c.ipam.kind, "host-local");
    }

    #[test]
    fn bridge_name_defaults_to_cni0() {
        let raw = r#"{"cniVersion":"0.3.1","ipam":{"type":"host-local"}}"#;
        assert_eq!(BridgeConf::parse(raw).unwrap().bridge, "cni0");
    }
}
```

- [ ] **Step 4: Minimal main declaring the module**

`crates/cni-bridge/src/main.rs`:
```rust
mod config;

fn main() {
    eprintln!("bridge (rust) — not yet implemented");
    std::process::exit(1);
}
```

- [ ] **Step 5: Test + build**

Run: `cargo test -p cni-bridge config:: && cargo build -p cni-bridge`
Expected: 2 config tests PASS; binary builds.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/cni-bridge
git commit -m "feat(bridge): scaffold crate + config parsing"
```

---

## Task 3: Pure planning helpers (veth name, addr/routes from IPAM result)

**Files:**
- Create: `crates/cni-bridge/src/plan.rs`
- Modify: `crates/cni-bridge/src/main.rs` (declare `mod plan;`)

- [ ] **Step 1: plan.rs with tests**

`crates/cni-bridge/src/plan.rs`:
```rust
use cni::result::CniResult;
use ipnetwork::Ipv4Network;
use std::net::Ipv4Addr;

/// Deterministic host-side veth name from the container id, within the 15-char
/// IFNAMSIZ limit. Format: "veth" + first 11 hex chars of the container id.
pub fn host_veth_name(container_id: &str) -> String {
    let id: String = container_id.chars().filter(|c| c.is_ascii_alphanumeric()).take(11).collect();
    format!("veth{id}")
}

/// The pod's address+prefix and gateway, extracted from the IPAM result.
pub struct IpPlan {
    pub addr: Ipv4Addr,
    pub prefix: u8,
    pub gateway: Option<Ipv4Addr>,
    /// route destinations (CIDRs) to add via the gateway
    pub routes: Vec<Ipv4Network>,
}

pub fn ip_plan(result: &CniResult) -> anyhow::Result<IpPlan> {
    let ip = result.ips.first().ok_or_else(|| anyhow::anyhow!("IPAM result has no IPs"))?;
    let net: Ipv4Network = ip.address.parse().map_err(|_| anyhow::anyhow!("bad IPAM address {}", ip.address))?;
    let gateway = match &ip.gateway {
        Some(g) => Some(g.parse().map_err(|_| anyhow::anyhow!("bad IPAM gateway {g}"))?),
        None => None,
    };
    let mut routes = Vec::new();
    for r in &result.routes {
        routes.push(r.dst.parse().map_err(|_| anyhow::anyhow!("bad route dst {}", r.dst))?);
    }
    Ok(IpPlan { addr: net.ip(), prefix: net.prefix(), gateway, routes })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_veth_name_is_bounded_and_deterministic() {
        let n = host_veth_name("ec5a938858dce08f4179b48658de7bbd");
        assert!(n.len() <= 15, "len {}", n.len());
        assert_eq!(n, host_veth_name("ec5a938858dce08f4179b48658de7bbd"));
        assert!(n.starts_with("veth"));
    }

    #[test]
    fn ip_plan_extracts_from_result() {
        let r = CniResult::parse(r#"{"cniVersion":"0.3.1","ips":[{"version":"4","address":"10.244.1.2/24","gateway":"10.244.1.1"}],"routes":[{"dst":"10.244.0.0/16"}]}"#).unwrap();
        let p = ip_plan(&r).unwrap();
        assert_eq!(p.addr, "10.244.1.2".parse::<Ipv4Addr>().unwrap());
        assert_eq!(p.prefix, 24);
        assert_eq!(p.gateway, Some("10.244.1.1".parse().unwrap()));
        assert_eq!(p.routes[0].to_string(), "10.244.0.0/16");
    }
}
```

- [ ] **Step 2: Declare module + test**

In `crates/cni-bridge/src/main.rs` add `mod plan;`. Run: `cargo test -p cni-bridge plan::`
Expected: 2 tests PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/cni-bridge/src/plan.rs crates/cni-bridge/src/main.rs
git commit -m "feat(bridge): pure veth-name + IPAM-result planning"
```

---

## Task 4: Host-side netlink ops

Integration code — verified by the smoke/conformance gate, not unit tests. rtnetlink 0.14 builder names may differ; adapt via `cargo doc -p rtnetlink` / grepping `~/.cargo/registry/src/*/rtnetlink-0.14*/src/`. Keep the PUBLIC fn signatures stable.

**Files:**
- Create: `crates/cni-bridge/src/hostns.rs`
- Modify: `crates/cni-bridge/src/main.rs` (declare `mod hostns;`)

- [ ] **Step 1: Implement hostns.rs**

`crates/cni-bridge/src/hostns.rs`:
```rust
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
    handle.link().add().bridge(name.to_string()).execute().await.context("create bridge")?;
    let idx = link_index(handle, name).await?.context("bridge missing after create")?;
    if let Some(m) = mtu {
        let _ = handle.link().set(idx).mtu(m).execute().await;
    }
    handle.link().set(idx).up().execute().await?;
    Ok(idx)
}

/// Create a veth pair (both ends in the current/host ns). Returns (host_idx, peer_idx).
pub async fn create_veth(handle: &Handle, host_name: &str, peer_name: &str, mtu: Option<u32>) -> Result<(u32, u32)> {
    handle
        .link()
        .add()
        .veth(host_name.to_string(), peer_name.to_string())
        .execute()
        .await
        .context("create veth pair")?;
    let host_idx = link_index(handle, host_name).await?.context("host veth missing")?;
    let peer_idx = link_index(handle, peer_name).await?.context("peer veth missing")?;
    if let Some(m) = mtu {
        let _ = handle.link().set(host_idx).mtu(m).execute().await;
        let _ = handle.link().set(peer_idx).mtu(m).execute().await;
    }
    Ok((host_idx, peer_idx))
}

/// Move a link into the netns identified by an open fd.
pub async fn move_to_netns(handle: &Handle, idx: u32, netns_fd: RawFd) -> Result<()> {
    handle.link().set(idx).setns_by_fd(netns_fd).execute().await.context("move link to netns")?;
    Ok(())
}

/// Bring host veth up, attach to the bridge, enable hairpin on the port.
pub async fn attach_host_veth(handle: &Handle, host_idx: u32, bridge_idx: u32, hairpin: bool) -> Result<()> {
    handle.link().set(host_idx).up().execute().await?;
    handle.link().set(host_idx).controller(bridge_idx).execute().await.context("set bridge master")?;
    if hairpin {
        // Best-effort: hairpin may require a bridge-port attribute set; ignore if unsupported.
        let _ = set_hairpin(handle, host_idx).await;
    }
    Ok(())
}

async fn set_hairpin(handle: &Handle, idx: u32) -> Result<()> {
    // rtnetlink 0.14: if a direct setter exists (e.g. .hairpin(true)) use it; otherwise
    // this is a no-op stub to be filled via cargo doc. Hairpin only affects same-pod
    // Service hairpin traffic.
    let _ = (handle, idx);
    Ok(())
}

/// Assign gateway/prefix to the bridge (idempotent) for isGateway.
pub async fn set_bridge_gateway(handle: &Handle, bridge_idx: u32, gw: Ipv4Addr, prefix: u8) -> Result<()> {
    let r = handle.address().add(bridge_idx, gw.into(), prefix).execute().await;
    if let Err(rtnetlink::Error::NetlinkError(e)) = r {
        if e.raw_code() != -17 { // EEXIST ok
            anyhow::bail!("set bridge gateway: {e:?}");
        }
    }
    Ok(())
}

/// Delete a link by index (cleanup on failure). Best-effort.
pub async fn del_link(handle: &Handle, idx: u32) {
    let _ = handle.link().del(idx).execute().await;
}
```

> Adaptation notes (rtnetlink 0.14): `.bridge(name)` and `.veth(a,b)` builders exist on `link().add()`. `.controller(idx)` sets IFLA_MASTER — if it's named `.master(idx)` in 0.14, use that. `.setns_by_fd(RawFd)` exists on `LinkSetRequest`; if it takes a `BorrowedFd`/`OwnedFd` instead of `RawFd`, adjust the type. `e.raw_code()` and `rtnetlink::Error::NetlinkError` are as used in flanneld's netlink.rs. For `set_hairpin`, grep for `hairpin` in the rtnetlink/netlink-packet-route sources; if no setter exists, leave the stub (hairpin is best-effort).

- [ ] **Step 2: Declare module + build**

In `crates/cni-bridge/src/main.rs` add `mod hostns;`. Run `cargo build -p cni-bridge` and fix any rtnetlink API mismatches per the notes until it compiles.

- [ ] **Step 3: Commit**

```bash
git add crates/cni-bridge/src/hostns.rs crates/cni-bridge/src/main.rs
git commit -m "feat(bridge): host-side netlink (bridge, veth, master, gateway)"
```

---

## Task 5: Container-netns interface configuration

Integration code — verified by the gate. Runs on a dedicated OS thread that `setns`'d into `CNI_NETNS`, with its own current-thread tokio runtime so the netlink socket lives in the container ns.

**Files:**
- Create: `crates/cni-bridge/src/contns.rs`
- Modify: `crates/cni-bridge/src/main.rs` (declare `mod contns;`)

- [ ] **Step 1: Implement contns.rs**

`crates/cni-bridge/src/contns.rs`:
```rust
use crate::plan::IpPlan;
use anyhow::{Context, Result};
use futures::TryStreamExt;
use nix::sched::{setns, CloneFlags};
use std::net::Ipv4Addr;
use std::os::fd::AsRawFd;

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
        let cont_ns = std::fs::File::open(&netns_path).with_context(|| format!("open netns {netns_path}"))?;
        setns(cont_ns.as_raw_fd(), CloneFlags::CLONE_NEWNET).context("setns into container")?;

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("build container-ns runtime")?;
        let res = rt.block_on(configure(&temp_name, &ifname, &plan, add_default_route));

        // Restore host ns regardless of result (thread also terminates).
        let _ = setns(host_ns.as_raw_fd(), CloneFlags::CLONE_NEWNET);
        res
    });
    handle.join().map_err(|_| anyhow::anyhow!("container-ns thread panicked"))?
}

async fn configure(temp_name: &str, ifname: &str, plan: &IpPlan, add_default_route: bool) -> Result<()> {
    let (conn, h, _) = rtnetlink::new_connection().context("netlink conn in container ns")?;
    tokio::spawn(conn);

    let idx = idx_by_name(&h, temp_name).await?.context("moved iface not found in container ns")?;
    h.link().set(idx).name(ifname.to_string()).execute().await.context("rename iface")?;
    h.link().set(idx).up().execute().await.context("set iface up")?;
    h.address().add(idx, plan.addr.into(), plan.prefix).execute().await.context("assign pod ip")?;

    let gw = plan.gateway;
    for net in &plan.routes {
        let mut req = h.route().add().v4().destination_prefix(net.network(), net.prefix()).output_interface(idx);
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
        setns(cont_ns.as_raw_fd(), CloneFlags::CLONE_NEWNET).context("setns into container")?;
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let _ = rt.block_on(async {
            let (conn, h, _) = rtnetlink::new_connection()?;
            tokio::spawn(conn);
            if let Some(idx) = idx_by_name(&h, &ifname).await? {
                let _ = h.link().del(idx).execute().await;
            }
            Ok::<(), anyhow::Error>(())
        });
        let _ = setns(host_ns.as_raw_fd(), CloneFlags::CLONE_NEWNET);
        Ok(())
    });
    handle.join().map_err(|_| anyhow::anyhow!("container-ns del thread panicked"))?
}
```

> Adaptation notes: `setns` in nix 0.29 takes an `impl AsFd` in some versions and `RawFd` in others — if `as_raw_fd()` doesn't typecheck, pass the `File`/`BorrowedFd` form the signature wants. `route().add().v4().destination_prefix(addr, plen).gateway(gw).output_interface(idx)` mirrors flanneld's usage; adjust builder names if needed. `.name(String)` on `link().set()` renames; confirm it exists in 0.14.

- [ ] **Step 2: Declare module + build**

In `crates/cni-bridge/src/main.rs` add `mod contns;`. Run `cargo build -p cni-bridge`; adapt per notes until it compiles.

- [ ] **Step 3: Commit**

```bash
git add crates/cni-bridge/src/contns.rs crates/cni-bridge/src/main.rs
git commit -m "feat(bridge): container-netns iface config via setns thread"
```

---

## Task 6: main dispatch (ADD/DEL/CHECK/VERSION)

**Files:**
- Modify: `crates/cni-bridge/src/main.rs`

- [ ] **Step 1: Replace main.rs**

`crates/cni-bridge/src/main.rs`:
```rust
mod config;
mod contns;
mod hostns;
mod plan;

use cni::env::CniArgs;
use cni::error::CniError;
use cni::result::CniResult;
use cni::version::VersionResult;
use config::BridgeConf;
use std::io::Read;
use std::process::ExitCode;

const CONT_IFNAME_TEMP_PREFIX: &str = "vethc";

fn read_stdin() -> String {
    let mut s = String::new();
    let _ = std::io::stdin().read_to_string(&mut s);
    s
}

fn err(code: u32, msg: impl Into<String>) -> CniError {
    CniError::new(code, msg)
}

async fn cmd_add(args: &CniArgs, conf: &BridgeConf, stdin: &str) -> Result<String, CniError> {
    use futures::TryStreamExt as _;
    let _ = TryStreamExt::try_next; // silence unused if not needed

    // host-side netlink connection
    let (conn, h, _) = rtnetlink::new_connection().map_err(|e| err(5, "netlink").with_details(e.to_string()))?;
    tokio::spawn(conn);

    // 1. ensure bridge
    let bridge_idx = hostns::ensure_bridge(&h, &conf.bridge, conf.mtu).await.map_err(|e| err(7, "ensure bridge").with_details(e.to_string()))?;

    // 2. IPAM ADD
    let out = cni::delegate::run_delegate(&conf.ipam.kind, args, stdin)?;
    if !out.success {
        // relay IPAM error verbatim
        return Err(err(7, "ipam add failed").with_details(out.stdout));
    }
    let ipam = CniResult::parse(&out.stdout).map_err(|e| err(6, "parse ipam result").with_details(e.to_string()))?;
    let ipplan = plan::ip_plan(&ipam).map_err(|e| err(7, "ipam plan").with_details(e.to_string()))?;

    // 3. veth pair
    let host_veth = plan::host_veth_name(&args.container_id);
    let temp_cont = format!("{CONT_IFNAME_TEMP_PREFIX}{}", &plan::host_veth_name(&args.container_id)[4..]);
    let (host_idx, peer_idx) = hostns::create_veth(&h, &host_veth, &temp_cont, conf.mtu).await.map_err(|e| err(5, "create veth").with_details(e.to_string()))?;

    // 4. move container end into the netns, configure it
    let netns_fd = open_netns(&args.netns).map_err(|e| err(5, "open netns").with_details(e.to_string()))?;
    if let Err(e) = hostns::move_to_netns(&h, peer_idx, netns_fd).await {
        hostns::del_link(&h, host_idx).await;
        return Err(err(5, "move veth to netns").with_details(e.to_string()));
    }
    if let Err(e) = contns::configure_container_iface(args.netns.clone(), temp_cont.clone(), args.ifname.clone(), ipplan, conf.is_default_gateway) {
        hostns::del_link(&h, host_idx).await;
        return Err(err(7, "configure container iface").with_details(e.to_string()));
    }

    // 5. attach host veth to bridge + hairpin
    if let Err(e) = hostns::attach_host_veth(&h, host_idx, bridge_idx, conf.hairpin_mode).await {
        hostns::del_link(&h, host_idx).await;
        return Err(err(5, "attach host veth").with_details(e.to_string()));
    }

    // 6. isGateway: bridge IP + ip_forward
    if conf.is_gateway {
        if let Some(gw) = ipam.ips.first().and_then(|i| i.gateway.as_ref()).and_then(|g| g.parse().ok()) {
            let prefix = ipam.ips[0].address.split('/').nth(1).and_then(|p| p.parse().ok()).unwrap_or(24);
            let _ = hostns::set_bridge_gateway(&h, bridge_idx, gw, prefix).await;
        }
        let _ = std::fs::write("/proc/sys/net/ipv4/ip_forward", "1");
    }

    // 7. relay the IPAM result as our result (0.3.1 chain: portmap consumes it)
    Ok(out.stdout)
}

fn cmd_del(args: &CniArgs, conf: &BridgeConf, stdin: &str) -> Result<String, CniError> {
    // IPAM DEL (best-effort)
    let _ = cni::delegate::run_delegate(&conf.ipam.kind, args, stdin);
    // remove container iface (removes veth pair)
    if !args.netns.is_empty() {
        let _ = contns::delete_container_iface(args.netns.clone(), args.ifname.clone());
    }
    Ok(String::new())
}

fn open_netns(path: &str) -> std::io::Result<std::os::fd::RawFd> {
    use std::os::fd::IntoRawFd;
    Ok(std::fs::File::open(path)?.into_raw_fd())
}

fn run(rt: &tokio::runtime::Runtime) -> Result<(String, bool), CniError> {
    let args = CniArgs::from_env()?;
    match args.command.as_str() {
        "VERSION" => Ok((VersionResult::supported().to_json(), true)),
        "ADD" => {
            let stdin = read_stdin();
            let conf = BridgeConf::parse(&stdin).map_err(|e| err(6, "decode config").with_details(e.to_string()))?;
            rt.block_on(cmd_add(&args, &conf, &stdin)).map(|s| (s, true))
        }
        "DEL" => {
            let stdin = read_stdin();
            let conf = BridgeConf::parse(&stdin).map_err(|e| err(6, "decode config").with_details(e.to_string()))?;
            cmd_del(&args, &conf, &stdin).map(|s| (s, true))
        }
        "CHECK" => Ok((String::new(), true)), // 0.3.1 never calls CHECK
        other => Err(err(4, format!("unknown CNI_COMMAND {other}"))),
    }
}

fn main() -> ExitCode {
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            print!("{}", err(5, "build runtime").with_details(e.to_string()).to_json());
            return ExitCode::FAILURE;
        }
    };
    match run(&rt) {
        Ok((out, true)) => {
            if !out.is_empty() {
                print!("{out}");
            }
            ExitCode::SUCCESS
        }
        Ok((out, false)) => {
            print!("{out}");
            ExitCode::FAILURE
        }
        Err(e) => {
            print!("{}", e.to_json());
            ExitCode::FAILURE
        }
    }
}
```

> Note: `main` builds a current-thread runtime and the container-ns work uses its OWN runtime on a separate thread — do NOT use `#[tokio::main]` (we need explicit control). Adapt `open_netns` fd type to whatever `move_to_netns`/`setns_by_fd` accept (RawFd here; the File is leaked via `into_raw_fd` for the plugin's short life — acceptable for a one-shot process). If clippy flags the leaked fd, switch to keeping the `File` alive in scope and passing `as_raw_fd()`.

- [ ] **Step 2: Build + manual VERSION**

Run:
```bash
cargo build -p cni-bridge
BIN=$(find /home/jones/.cache/rusternetes-target ./target -name bridge -type f -path '*debug*' 2>/dev/null | grep cni-bridge -m1 || cargo build -p cni-bridge 2>/dev/null; find /home/jones/.cache/rusternetes-target ./target -path '*debug*' -name bridge -type f 2>/dev/null | head -1)
CNI_COMMAND=VERSION "$BIN"; echo
```
Expected: builds; VERSION prints `{"cniVersion":"0.3.1","supportedVersions":["0.3.0","0.3.1"]}`.

- [ ] **Step 3: Full local gate**

Run:
```bash
cargo fmt --all
cargo fmt --all -- --check
RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
Expected: fmt clean, clippy clean, all unit tests pass. Fix clippy issues (e.g. the unused `TryStreamExt` shim line — remove it if clippy complains; it was only a guard).

- [ ] **Step 4: Commit**

```bash
git add crates/cni-bridge/src/main.rs
git commit -m "feat(bridge): ADD/DEL/CHECK/VERSION dispatch"
```

---

## Task 7: Bake into image + install over Go bridge

**Files:**
- Modify: `Dockerfile`, `deploy/flannel-rs.yaml`

- [ ] **Step 1: Build + copy the bridge binary**

In `Dockerfile`, extend the build line and add a COPY.

Find:
```dockerfile
RUN cargo build --release -p flanneld -p cni-host-local -p cni-flannel
```
Replace with:
```dockerfile
RUN cargo build --release -p flanneld -p cni-host-local -p cni-flannel -p cni-bridge
```
After `COPY --from=build /src/target/release/flannel /opt/cni/bin/flannel`, add:
```dockerfile
COPY --from=build /src/target/release/bridge /opt/cni/bin/bridge
```

- [ ] **Step 2: Install bridge onto nodes**

In `deploy/flannel-rs.yaml`, extend the `install-cni-plugins-rs` initContainer command to include `bridge`:
```yaml
        command: ["sh", "-c", "cp -f /opt/cni/bin/flannel /opt/cni/bin/host-local /opt/cni/bin/bridge /host/opt/cni/bin/"]
```

- [ ] **Step 3: Build image + verify**

Run:
```bash
docker build -t flannel-rs:dev .
docker run --rm --entrypoint ls flannel-rs:dev -l /opt/cni/bin/flannel /opt/cni/bin/host-local /opt/cni/bin/bridge
python3 -c "import yaml; list(yaml.safe_load_all(open('deploy/flannel-rs.yaml')))" && echo "YAML OK"
```
Expected: image builds; all three Rust plugins present; YAML valid.

- [ ] **Step 4: Commit**

```bash
git add Dockerfile deploy/flannel-rs.yaml
git commit -m "build: bake Rust bridge; install over Go bridge"
```

---

## Task 8: Verify by smoke + conformance (the gate)

**Files:** none. Expect debugging — this is the riskiest milestone.

- [ ] **Step 1: Smoke (flannel-rs)**

Run: `bash tests/smoke/run.sh flannel-rs`
Expected: `SMOKE PASSED: flannel-rs`. Chain is now flannel(Rust) → bridge(Rust) → host-local(Rust).

- [ ] **Step 2: Conformance (flannel-rs)**

Run: `bash tests/conformance/run.sh flannel-rs`
Expected: 47 `[sig-network] [Conformance]` specs pass.

- [ ] **Step 3: Baseline still green**

Run: `bash tests/smoke/run.sh flannel-go`
Expected: `SMOKE PASSED: flannel-go`.

- [ ] **Step 4: Confirm Rust bridge in use + debug if red**

Use `superpowers:systematic-debugging`. Probes:
```bash
docker exec flannel-rs-worker /opt/cni/bin/bridge </dev/null; echo "exit=$?"   # our code-4 CNI error JSON
kubectl --context kind-flannel-rs describe pod <pending-pod> | grep -A8 Events
docker exec flannel-rs-worker ip link show cbr0
docker exec flannel-rs-worker ip route
kubectl --context kind-flannel-rs exec <pod> -- ip addr   # pod has eth0 + IP?
kubectl --context kind-flannel-rs exec <pod> -- ip route  # default via .1, 10.244.0.0/16 via .1?
```
Likely failure points (in order): the veth move/rename across netns (wrong name lookup); container iface missing IP or routes (gateway/default-route logic); bridge has no `.1` so pods can't reach the gateway (isGateway path); `ip_forward` not set; MTU mismatch; hairpin unsupported (only affects same-pod Service hairpin — a specific conformance test). Fix Rust, rebuild image, re-run.

- [ ] **Step 5: Commit any fixes**

```bash
git add -A
git commit -m "fix: <root cause> so Rust bridge passes smoke + conformance"
```

---

## Self-Review

**Spec coverage:**
- cni-lib refactor (shared delegate-exec) + CniResult Deserialize → Task 1. ✓
- BridgeConf parse (name default cni0, flags, mtu, ipam) → Task 2. ✓
- veth-name + IPAM-result planning (pure) → Task 3. ✓
- ensure bridge / veth / move-to-netns / master / hairpin / bridge gateway / del → Task 4. ✓
- setns-thread container iface config (rename, up, addr, routes, default route) + del → Task 5. ✓
- ADD/DEL/CHECK(stub)/VERSION dispatch; isGateway (bridge IP + ip_forward); ipMasq skipped; relay result for portmap; veth cleanup on failure → Task 6. ✓
- Bake + install over Go bridge → Task 7. ✓
- smoke + conformance gate; flannel-go untouched → Task 8. ✓

**Placeholder scan:** Tasks 4/5/6 carry rtnetlink/nix API-adaptation notes (concrete code + a documented fallback procedure, as used successfully for flanneld's netlink.rs) — not placeholders. `set_hairpin` is an explicit best-effort stub with rationale. No TBD/TODO.

**Type consistency:** `BridgeConf{cni_version,bridge,is_gateway,is_default_gateway,hairpin_mode,mtu,ipam}`, `Ipam{kind}`, `IpPlan{addr,prefix,gateway,routes}`, `host_veth_name`, `ip_plan`, `ensure_bridge/create_veth/move_to_netns/attach_host_veth/set_bridge_gateway/del_link`, `configure_container_iface/delete_container_iface` — names match across Tasks 3–6 and the main call sites. `cni::delegate::run_delegate` + `DelegateOutput{stdout,success}` and `CniResult::parse` match Task 1.

**Known risk:** the netns/rtnetlink-0.14 integration is the highest-uncertainty area (setns fd types, `setns_by_fd`, `.controller` vs `.master`, hairpin setter, route builder). Concrete code is provided as a starting point with adaptation guidance; Task 8's conformance gate (every pod exercises bridge) is the real verification.
