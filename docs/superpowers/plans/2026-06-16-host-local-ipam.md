# host-local IPAM (Rust) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port Flannel's `host-local` IPAM CNI plugin to Rust (flannel-subset), swap it into the flannel-rs CNI chain, and keep the smoke + conformance suites green.

**Architecture:** A reusable `cni` library crate (env/config/result/error/version types) plus a `cni-host-local` binary crate (pure allocation core + disk-backed lease store + ADD/DEL/CHECK/VERSION dispatch). host-local never enters a netns — it is pure IP bookkeeping — so every command is unit-testable against a tempdir with no root. Verified by swapping only `/opt/cni/bin/host-local` to the Rust build (upstream Go `bridge` execs it) and re-running the existing harness.

**Tech Stack:** Rust, serde/serde_json, ipnetwork 0.20, fs2 (flock), thiserror. kind + the existing smoke/conformance harness for integration.

---

## File Structure

```
crates/
├── cni/                       # reusable CNI spec library
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs             # re-exports
│       ├── env.rs             # CniArgs::from_env
│       ├── config.rs          # NetConf, IpamConfig, RangeConfig, Route
│       ├── result.rs          # CniResult, IpResult, Dns
│       ├── error.rs           # CniError (code/msg/details, JSON)
│       └── version.rs         # VersionResult
└── cni-host-local/            # the `host-local` binary
    ├── Cargo.toml
    └── src/
        ├── main.rs            # dispatch: VERSION/ADD/DEL/CHECK
        ├── alloc.rs           # pure Allocator (next_ip, exclusions, wraparound)
        ├── store.rs           # disk lease store (leased/last_reserved/reserve/release/has + flock)
        └── command.rs         # cmd_add/cmd_del/cmd_check (tempdir-testable)
```

Modified: root `Cargo.toml` (workspace members), `Dockerfile`, `deploy/flannel-rs.yaml`.

---

## Task 1: Scaffold the two crates

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/cni/Cargo.toml`, `crates/cni/src/lib.rs`
- Create: `crates/cni-host-local/Cargo.toml`, `crates/cni-host-local/src/main.rs`

- [ ] **Step 1: Add workspace members**

Edit root `Cargo.toml` so members lists all three crates:
```toml
[workspace]
resolver = "2"
members = ["crates/flanneld", "crates/cni", "crates/cni-host-local"]
```

- [ ] **Step 2: cni lib manifest**

`crates/cni/Cargo.toml`:
```toml
[package]
name = "cni"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
```

- [ ] **Step 3: cni lib root**

`crates/cni/src/lib.rs`:
```rust
pub mod config;
pub mod env;
pub mod error;
pub mod result;
pub mod version;

pub use error::CniError;
```
(The module files are created in later tasks; this will not compile until Task 2–4 add them. That is expected — Task 1 ends after Step 6 with the binary crate compiling; the lib is completed in 2–4. To keep Step 1 green, create empty module files now: `config.rs`, `env.rs`, `error.rs`, `result.rs`, `version.rs` each containing only `// filled in later task`.)

Create the five files `crates/cni/src/{config,env,error,result,version}.rs` each with the single line:
```rust
// filled in a later task
```

- [ ] **Step 4: cni-host-local manifest**

`crates/cni-host-local/Cargo.toml`:
```toml
[package]
name = "cni-host-local"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "host-local"
path = "src/main.rs"

[dependencies]
cni = { path = "../cni" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
ipnetwork = "0.20"
fs2 = "0.4"

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 5: minimal binary**

`crates/cni-host-local/src/main.rs`:
```rust
fn main() {
    eprintln!("host-local (rust) — not yet implemented");
    std::process::exit(1);
}
```

- [ ] **Step 6: Verify build**

Run: `cargo build --workspace`
Expected: all three crates compile (cni has empty modules, cni-host-local builds the `host-local` binary). Warnings are fine.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock crates/cni crates/cni-host-local
git commit -m "chore: scaffold cni lib + cni-host-local crates"
```

---

## Task 2: CNI config types

**Files:**
- Modify: `crates/cni/src/config.rs`

- [ ] **Step 1: Write the failing test**

Replace `crates/cni/src/config.rs` with:
```rust
use serde::{Deserialize, Serialize};

/// Top-level network config passed to the IPAM plugin on stdin.
#[derive(Debug, Deserialize)]
pub struct NetConf {
    #[serde(rename = "cniVersion", default)]
    pub cni_version: String,
    #[serde(default)]
    pub name: String,
    pub ipam: IpamConfig,
}

#[derive(Debug, Deserialize)]
pub struct IpamConfig {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub ranges: Vec<Vec<RangeConfig>>,
    #[serde(default)]
    pub routes: Vec<Route>,
    #[serde(rename = "dataDir", default)]
    pub data_dir: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct RangeConfig {
    pub subnet: String,
    #[serde(default)]
    pub gateway: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Route {
    pub dst: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub gw: Option<String>,
}

impl NetConf {
    pub fn parse(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flannel_delegate_ipam() {
        // The shape flannel's meta-plugin/bridge hands to host-local.
        let raw = r#"{
          "cniVersion": "0.3.1",
          "name": "cbr0",
          "ipam": {
            "type": "host-local",
            "ranges": [[{"subnet": "10.244.1.0/24"}]],
            "routes": [{"dst": "0.0.0.0/0"}],
            "dataDir": "/var/lib/cni/networks"
          }
        }"#;
        let nc = NetConf::parse(raw).unwrap();
        assert_eq!(nc.cni_version, "0.3.1");
        assert_eq!(nc.name, "cbr0");
        assert_eq!(nc.ipam.kind, "host-local");
        assert_eq!(nc.ipam.ranges[0][0].subnet, "10.244.1.0/24");
        assert_eq!(nc.ipam.routes[0].dst, "0.0.0.0/0");
        assert_eq!(nc.ipam.data_dir.as_deref(), Some("/var/lib/cni/networks"));
    }
}
```

- [ ] **Step 2: Run test to verify it fails, then passes**

Run: `cargo test -p cni config::`
Expected: compiles and PASS (`parses_flannel_delegate_ipam`). (The test and impl are added together; the point is the assertions encode the contract.)

- [ ] **Step 3: Commit**

```bash
git add crates/cni/src/config.rs
git commit -m "feat(cni): config types for IPAM NetConf"
```

---

## Task 3: CNI result, error, version types

**Files:**
- Modify: `crates/cni/src/result.rs`, `crates/cni/src/error.rs`, `crates/cni/src/version.rs`

- [ ] **Step 1: Write result.rs with test**

`crates/cni/src/result.rs`:
```rust
use crate::config::Route;
use serde::Serialize;

/// CNI ADD result, spec 0.3.1 encoding.
#[derive(Debug, Serialize)]
pub struct CniResult {
    #[serde(rename = "cniVersion")]
    pub cni_version: String,
    pub ips: Vec<IpResult>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<Route>,
}

#[derive(Debug, Serialize)]
pub struct IpResult {
    pub version: String, // "4"
    pub address: String, // "10.244.1.2/24"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gateway: Option<String>,
}

impl CniResult {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("CniResult serializes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_0_3_1_result() {
        let r = CniResult {
            cni_version: "0.3.1".into(),
            ips: vec![IpResult {
                version: "4".into(),
                address: "10.244.1.2/24".into(),
                gateway: Some("10.244.1.1".into()),
            }],
            routes: vec![Route { dst: "0.0.0.0/0".into(), gw: None }],
        };
        let v: serde_json::Value = serde_json::from_str(&r.to_json()).unwrap();
        assert_eq!(v["cniVersion"], "0.3.1");
        assert_eq!(v["ips"][0]["version"], "4");
        assert_eq!(v["ips"][0]["address"], "10.244.1.2/24");
        assert_eq!(v["ips"][0]["gateway"], "10.244.1.1");
        assert_eq!(v["routes"][0]["dst"], "0.0.0.0/0");
    }
}
```

- [ ] **Step 2: Write error.rs with test**

`crates/cni/src/error.rs`:
```rust
use serde::Serialize;

/// CNI error result (printed to stdout, non-zero exit). Codes follow the spec:
/// 4 = invalid environment variables, 6 = failed to decode content,
/// 7 = invalid network config, 11 = try again later.
#[derive(Debug, Serialize, thiserror::Error)]
#[error("CNI error {code}: {msg}")]
pub struct CniError {
    #[serde(rename = "cniVersion")]
    pub cni_version: String,
    pub code: u32,
    pub msg: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

impl CniError {
    pub fn new(code: u32, msg: impl Into<String>) -> Self {
        Self { cni_version: "0.3.1".into(), code, msg: msg.into(), details: None }
    }
    pub fn with_details(mut self, details: impl Into<String>) -> Self {
        self.details = Some(details.into());
        self
    }
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("CniError serializes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_error() {
        let e = CniError::new(7, "no IP addresses available in range").with_details("range full");
        let v: serde_json::Value = serde_json::from_str(&e.to_json()).unwrap();
        assert_eq!(v["code"], 7);
        assert_eq!(v["msg"], "no IP addresses available in range");
        assert_eq!(v["details"], "range full");
        assert_eq!(v["cniVersion"], "0.3.1");
    }
}
```

- [ ] **Step 3: Write version.rs with test**

`crates/cni/src/version.rs`:
```rust
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct VersionResult {
    #[serde(rename = "cniVersion")]
    pub cni_version: String,
    #[serde(rename = "supportedVersions")]
    pub supported_versions: Vec<String>,
}

impl VersionResult {
    /// Versions whose result encoding this plugin emits.
    pub fn supported() -> Self {
        Self {
            cni_version: "0.3.1".into(),
            supported_versions: vec!["0.3.0".into(), "0.3.1".into()],
        }
    }
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("VersionResult serializes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertises_supported_versions() {
        let v: serde_json::Value = serde_json::from_str(&VersionResult::supported().to_json()).unwrap();
        assert_eq!(v["supportedVersions"][0], "0.3.0");
        assert_eq!(v["supportedVersions"][1], "0.3.1");
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p cni`
Expected: PASS — `serializes_0_3_1_result`, `serializes_error`, `advertises_supported_versions`, plus Task 2's `parses_flannel_delegate_ipam`.

- [ ] **Step 5: Commit**

```bash
git add crates/cni/src/result.rs crates/cni/src/error.rs crates/cni/src/version.rs
git commit -m "feat(cni): result, error, version encodings"
```

---

## Task 4: CNI env parsing

**Files:**
- Modify: `crates/cni/src/env.rs`

- [ ] **Step 1: Write env.rs with test**

`crates/cni/src/env.rs`:
```rust
use crate::error::CniError;
use std::collections::HashMap;

/// The CNI invocation, parsed from environment variables.
#[derive(Debug, Clone)]
pub struct CniArgs {
    pub command: String,
    pub container_id: String,
    pub netns: String,
    pub ifname: String,
    pub args: String,
    pub path: String,
}

impl CniArgs {
    /// Parse from a key->value map (real callers pass std::env::vars()).
    pub fn from_map(env: &HashMap<String, String>) -> Result<Self, CniError> {
        let get = |k: &str| env.get(k).cloned().unwrap_or_default();
        let command = env
            .get("CNI_COMMAND")
            .cloned()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| CniError::new(4, "CNI_COMMAND missing"))?;
        Ok(Self {
            command,
            container_id: get("CNI_CONTAINERID"),
            netns: get("CNI_NETNS"),
            ifname: get("CNI_IFNAME"),
            args: get("CNI_ARGS"),
            path: get("CNI_PATH"),
        })
    }

    pub fn from_env() -> Result<Self, CniError> {
        let map: HashMap<String, String> = std::env::vars().collect();
        Self::from_map(&map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn parses_add_invocation() {
        let a = CniArgs::from_map(&map(&[
            ("CNI_COMMAND", "ADD"),
            ("CNI_CONTAINERID", "abc123"),
            ("CNI_IFNAME", "eth0"),
            ("CNI_NETNS", "/var/run/netns/x"),
        ]))
        .unwrap();
        assert_eq!(a.command, "ADD");
        assert_eq!(a.container_id, "abc123");
        assert_eq!(a.ifname, "eth0");
    }

    #[test]
    fn missing_command_is_error_code_4() {
        let err = CniArgs::from_map(&map(&[("CNI_CONTAINERID", "abc")])).unwrap_err();
        assert_eq!(err.code, 4);
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p cni env::`
Expected: PASS (`parses_add_invocation`, `missing_command_is_error_code_4`).

- [ ] **Step 3: Commit**

```bash
git add crates/cni/src/env.rs
git commit -m "feat(cni): parse CNI invocation from environment"
```

---

## Task 5: Allocation core (pure)

**Files:**
- Create: `crates/cni-host-local/src/alloc.rs`
- Modify: `crates/cni-host-local/src/main.rs` (declare `mod alloc;`)

- [ ] **Step 1: Write alloc.rs with tests**

`crates/cni-host-local/src/alloc.rs`:
```rust
use ipnetwork::Ipv4Network;
use std::collections::HashSet;
use std::net::Ipv4Addr;

/// Pure IP allocator over a single subnet. Excludes the network address,
/// the broadcast address, and the gateway. No I/O.
pub struct Allocator {
    net: Ipv4Network,
    gateway: Ipv4Addr,
}

impl Allocator {
    /// `gateway` defaults to the first usable address (network + 1).
    pub fn new(net: Ipv4Network, gateway: Option<Ipv4Addr>) -> Self {
        let gateway = gateway.unwrap_or_else(|| first_usable(net));
        Self { net, gateway }
    }

    pub fn gateway(&self) -> Ipv4Addr {
        self.gateway
    }

    pub fn prefix(&self) -> u8 {
        self.net.prefix()
    }

    /// Candidate host addresses in allocation order: network/broadcast/gateway
    /// excluded.
    fn usable_hosts(&self) -> Vec<Ipv4Addr> {
        let network = self.net.network();
        let broadcast = self.net.broadcast();
        self.net
            .iter()
            .filter(|ip| *ip != network && *ip != broadcast && *ip != self.gateway)
            .collect()
    }

    /// First address not in `leased`, scanning sequentially starting just after
    /// `last` (wrapping). Returns None when the range is exhausted.
    pub fn next_ip(&self, leased: &HashSet<Ipv4Addr>, last: Option<Ipv4Addr>) -> Option<Ipv4Addr> {
        let hosts = self.usable_hosts();
        if hosts.is_empty() {
            return None;
        }
        let start = match last {
            Some(l) => hosts.iter().position(|h| *h == l).map(|i| i + 1).unwrap_or(0),
            None => 0,
        };
        for k in 0..hosts.len() {
            let ip = hosts[(start + k) % hosts.len()];
            if !leased.contains(&ip) {
                return Some(ip);
            }
        }
        None
    }
}

fn first_usable(net: Ipv4Network) -> Ipv4Addr {
    Ipv4Addr::from(u32::from(net.network()) + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(s: &str) -> Ipv4Network {
        s.parse().unwrap()
    }
    fn ip(s: &str) -> Ipv4Addr {
        s.parse().unwrap()
    }

    #[test]
    fn default_gateway_is_first_usable() {
        let a = Allocator::new(net("10.244.1.0/24"), None);
        assert_eq!(a.gateway(), ip("10.244.1.1"));
    }

    #[test]
    fn first_allocation_skips_network_and_gateway() {
        let a = Allocator::new(net("10.244.1.0/24"), None);
        // .0 = network (excluded), .1 = gateway (excluded) => first is .2
        assert_eq!(a.next_ip(&HashSet::new(), None), Some(ip("10.244.1.2")));
    }

    #[test]
    fn sequential_after_last_reserved() {
        let a = Allocator::new(net("10.244.1.0/24"), None);
        let leased: HashSet<_> = [ip("10.244.1.2")].into_iter().collect();
        assert_eq!(a.next_ip(&leased, Some(ip("10.244.1.2"))), Some(ip("10.244.1.3")));
    }

    #[test]
    fn wraps_around_to_find_free() {
        let a = Allocator::new(net("10.244.1.0/24"), None);
        // last reserved is the broadcast-adjacent .254; only .2 is free.
        let mut leased: HashSet<Ipv4Addr> = HashSet::new();
        for o in 3..=254 {
            leased.insert(ip(&format!("10.244.1.{o}")));
        }
        assert_eq!(a.next_ip(&leased, Some(ip("10.244.1.254"))), Some(ip("10.244.1.2")));
    }

    #[test]
    fn exhausted_range_returns_none() {
        let a = Allocator::new(net("10.244.1.0/24"), None);
        let mut leased: HashSet<Ipv4Addr> = HashSet::new();
        for o in 2..=254 {
            leased.insert(ip(&format!("10.244.1.{o}")));
        }
        assert_eq!(a.next_ip(&leased, None), None);
    }

    #[test]
    fn explicit_gateway_is_excluded() {
        let a = Allocator::new(net("10.244.1.0/24"), Some(ip("10.244.1.5")));
        let chosen = a.next_ip(&HashSet::new(), None).unwrap();
        assert_ne!(chosen, ip("10.244.1.5"));
        assert_eq!(chosen, ip("10.244.1.2"));
    }
}
```

- [ ] **Step 2: Declare the module**

In `crates/cni-host-local/src/main.rs`, add at the top (above `fn main`):
```rust
mod alloc;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p cni-host-local alloc::`
Expected: PASS (6 tests).

- [ ] **Step 4: Commit**

```bash
git add crates/cni-host-local/src/alloc.rs crates/cni-host-local/src/main.rs
git commit -m "feat(host-local): pure IP allocation core"
```

---

## Task 6: Disk lease store

**Files:**
- Create: `crates/cni-host-local/src/store.rs`
- Modify: `crates/cni-host-local/src/main.rs` (declare `mod store;`)

- [ ] **Step 1: Write store.rs with tests**

`crates/cni-host-local/src/store.rs`:
```rust
use fs2::FileExt;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

/// Disk-backed lease store, format-compatible with Go host-local:
/// `<data_dir>/<network>/<ip>` (contents `containerID\nifname`),
/// `last_reserved_ip.0`, and a `lock` file for `flock`.
pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(data_dir: &str, network: &str) -> io::Result<Self> {
        let dir = Path::new(data_dir).join(network);
        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    /// Acquire an exclusive lock for the duration of the returned guard.
    pub fn lock(&self) -> io::Result<File> {
        let f = File::create(self.dir.join("lock"))?;
        f.lock_exclusive()?;
        Ok(f)
    }

    pub fn leased(&self) -> io::Result<HashSet<Ipv4Addr>> {
        let mut set = HashSet::new();
        for entry in fs::read_dir(&self.dir)? {
            let name = entry?.file_name().to_string_lossy().to_string();
            if let Ok(ip) = name.parse::<Ipv4Addr>() {
                set.insert(ip);
            }
        }
        Ok(set)
    }

    pub fn last_reserved(&self) -> Option<Ipv4Addr> {
        fs::read_to_string(self.dir.join("last_reserved_ip.0"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
    }

    pub fn reserve(&self, ip: Ipv4Addr, container_id: &str, ifname: &str) -> io::Result<()> {
        fs::write(self.dir.join(ip.to_string()), format!("{container_id}\n{ifname}"))?;
        fs::write(self.dir.join("last_reserved_ip.0"), ip.to_string())?;
        Ok(())
    }

    /// Remove every lease file whose contents match this container_id+ifname.
    /// Idempotent: removing nothing is success.
    pub fn release(&self, container_id: &str, ifname: &str) -> io::Result<()> {
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.parse::<Ipv4Addr>().is_err() {
                continue;
            }
            let content = fs::read_to_string(entry.path()).unwrap_or_default();
            let mut lines = content.lines();
            let cid = lines.next().unwrap_or("");
            let ifn = lines.next().unwrap_or("");
            if cid == container_id && ifn == ifname {
                fs::remove_file(entry.path())?;
            }
        }
        Ok(())
    }

    pub fn has(&self, container_id: &str, ifname: &str) -> io::Result<bool> {
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.parse::<Ipv4Addr>().is_err() {
                continue;
            }
            let content = fs::read_to_string(entry.path()).unwrap_or_default();
            let mut lines = content.lines();
            if lines.next() == Some(container_id) && lines.next() == Some(ifname) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> Ipv4Addr {
        s.parse().unwrap()
    }

    #[test]
    fn reserve_then_leased_and_last() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Store::new(tmp.path().to_str().unwrap(), "cbr0").unwrap();
        s.reserve(ip("10.244.1.2"), "cid1", "eth0").unwrap();
        assert!(s.leased().unwrap().contains(&ip("10.244.1.2")));
        assert_eq!(s.last_reserved(), Some(ip("10.244.1.2")));
        assert!(s.has("cid1", "eth0").unwrap());
    }

    #[test]
    fn release_is_idempotent_and_targeted() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Store::new(tmp.path().to_str().unwrap(), "cbr0").unwrap();
        s.reserve(ip("10.244.1.2"), "cid1", "eth0").unwrap();
        s.reserve(ip("10.244.1.3"), "cid2", "eth0").unwrap();
        s.release("cid1", "eth0").unwrap();
        assert!(!s.leased().unwrap().contains(&ip("10.244.1.2")));
        assert!(s.leased().unwrap().contains(&ip("10.244.1.3")));
        // releasing again is a no-op (no error)
        s.release("cid1", "eth0").unwrap();
        assert!(!s.has("cid1", "eth0").unwrap());
    }
}
```

- [ ] **Step 2: Declare the module**

In `crates/cni-host-local/src/main.rs` add: `mod store;`

- [ ] **Step 3: Run tests**

Run: `cargo test -p cni-host-local store::`
Expected: PASS (`reserve_then_leased_and_last`, `release_is_idempotent_and_targeted`).

- [ ] **Step 4: Commit**

```bash
git add crates/cni-host-local/src/store.rs crates/cni-host-local/src/main.rs
git commit -m "feat(host-local): disk-backed lease store"
```

---

## Task 7: Command handlers (ADD/DEL/CHECK)

host-local never touches a netns, so the handlers are fully testable against a tempdir.

**Files:**
- Create: `crates/cni-host-local/src/command.rs`
- Modify: `crates/cni-host-local/src/main.rs` (declare `mod command;`)

- [ ] **Step 1: Write command.rs with tests**

`crates/cni-host-local/src/command.rs`:
```rust
use crate::alloc::Allocator;
use crate::store::Store;
use cni::config::NetConf;
use cni::env::CniArgs;
use cni::error::CniError;
use cni::result::{CniResult, IpResult};
use ipnetwork::Ipv4Network;
use std::net::Ipv4Addr;

const DEFAULT_DATA_DIR: &str = "/var/lib/cni/networks";

fn load(stdin: &str) -> Result<NetConf, CniError> {
    NetConf::parse(stdin).map_err(|e| CniError::new(6, "failed to decode network config").with_details(e.to_string()))
}

fn allocator_for(nc: &NetConf) -> Result<Allocator, CniError> {
    let range = nc
        .ipam
        .ranges
        .first()
        .and_then(|r| r.first())
        .ok_or_else(|| CniError::new(7, "ipam.ranges is empty"))?;
    let net: Ipv4Network = range
        .subnet
        .parse()
        .map_err(|_| CniError::new(7, format!("invalid subnet {}", range.subnet)))?;
    let gw: Option<Ipv4Addr> = match &range.gateway {
        Some(g) => Some(g.parse().map_err(|_| CniError::new(7, format!("invalid gateway {g}")))?),
        None => None,
    };
    Ok(Allocator::new(net, gw))
}

fn store_for(nc: &NetConf) -> Result<Store, CniError> {
    let data_dir = nc.ipam.data_dir.as_deref().unwrap_or(DEFAULT_DATA_DIR);
    Store::new(data_dir, &nc.name).map_err(|e| CniError::new(5, "failed to open data dir").with_details(e.to_string()))
}

pub fn cmd_add(args: &CniArgs, stdin: &str) -> Result<String, CniError> {
    let nc = load(stdin)?;
    let alloc = allocator_for(&nc)?;
    let store = store_for(&nc)?;
    let _lock = store.lock().map_err(|e| CniError::new(11, "failed to lock data dir").with_details(e.to_string()))?;

    let leased = store.leased().map_err(|e| CniError::new(5, "read leases").with_details(e.to_string()))?;
    let ip = alloc
        .next_ip(&leased, store.last_reserved())
        .ok_or_else(|| CniError::new(7, "no IP addresses available in range"))?;
    store
        .reserve(ip, &args.container_id, &args.ifname)
        .map_err(|e| CniError::new(5, "write lease").with_details(e.to_string()))?;

    let result = CniResult {
        cni_version: if nc.cni_version.is_empty() { "0.3.1".into() } else { nc.cni_version.clone() },
        ips: vec![IpResult {
            version: "4".into(),
            address: format!("{}/{}", ip, alloc.prefix()),
            gateway: Some(alloc.gateway().to_string()),
        }],
        routes: nc.ipam.routes.clone(),
    };
    Ok(result.to_json())
}

pub fn cmd_del(args: &CniArgs, stdin: &str) -> Result<String, CniError> {
    let nc = load(stdin)?;
    let store = store_for(&nc)?;
    let _lock = store.lock().map_err(|e| CniError::new(11, "failed to lock data dir").with_details(e.to_string()))?;
    store
        .release(&args.container_id, &args.ifname)
        .map_err(|e| CniError::new(5, "release lease").with_details(e.to_string()))?;
    Ok(String::new()) // DEL has no result body
}

pub fn cmd_check(args: &CniArgs, stdin: &str) -> Result<String, CniError> {
    let nc = load(stdin)?;
    let store = store_for(&nc)?;
    if store.has(&args.container_id, &args.ifname).map_err(|e| CniError::new(5, "check lease").with_details(e.to_string()))? {
        Ok(String::new())
    } else {
        Err(CniError::new(7, "no allocation found for container"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn args(cmd: &str, cid: &str) -> CniArgs {
        let m: HashMap<String, String> = [
            ("CNI_COMMAND", cmd),
            ("CNI_CONTAINERID", cid),
            ("CNI_IFNAME", "eth0"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        CniArgs::from_map(&m).unwrap()
    }

    fn conf(data_dir: &str) -> String {
        format!(
            r#"{{"cniVersion":"0.3.1","name":"cbr0","ipam":{{"type":"host-local","ranges":[[{{"subnet":"10.244.1.0/24"}}]],"routes":[{{"dst":"0.0.0.0/0"}}],"dataDir":"{data_dir}"}}}}"#
        )
    }

    #[test]
    fn add_allocates_first_host_and_persists() {
        let tmp = tempfile::tempdir().unwrap();
        let c = conf(tmp.path().to_str().unwrap());
        let out = cmd_add(&args("ADD", "cid1"), &c).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["ips"][0]["address"], "10.244.1.2/24");
        assert_eq!(v["ips"][0]["gateway"], "10.244.1.1");
        assert_eq!(v["routes"][0]["dst"], "0.0.0.0/0");
    }

    #[test]
    fn second_add_gets_next_ip() {
        let tmp = tempfile::tempdir().unwrap();
        let c = conf(tmp.path().to_str().unwrap());
        cmd_add(&args("ADD", "cid1"), &c).unwrap();
        let out = cmd_add(&args("ADD", "cid2"), &c).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["ips"][0]["address"], "10.244.1.3/24");
    }

    #[test]
    fn del_frees_ip_for_reuse() {
        let tmp = tempfile::tempdir().unwrap();
        let c = conf(tmp.path().to_str().unwrap());
        cmd_add(&args("ADD", "cid1"), &c).unwrap(); // .2
        cmd_del(&args("DEL", "cid1"), &c).unwrap();
        // next ADD reuses .2 (it's free; last_reserved was .2, wraps to find free .2)
        let out = cmd_add(&args("ADD", "cid3"), &c).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["ips"][0]["address"], "10.244.1.2/24");
    }

    #[test]
    fn check_reflects_allocation() {
        let tmp = tempfile::tempdir().unwrap();
        let c = conf(tmp.path().to_str().unwrap());
        assert!(cmd_check(&args("CHECK", "cid1"), &c).is_err());
        cmd_add(&args("ADD", "cid1"), &c).unwrap();
        assert!(cmd_check(&args("CHECK", "cid1"), &c).is_ok());
    }
}
```

- [ ] **Step 2: Declare the module**

In `crates/cni-host-local/src/main.rs` add: `mod command;`

- [ ] **Step 3: Run tests**

Run: `cargo test -p cni-host-local command::`
Expected: PASS (4 tests). Note `del_frees_ip_for_reuse` relies on the allocator wrapping from `last_reserved=.2` and finding `.2` free again.

- [ ] **Step 4: Commit**

```bash
git add crates/cni-host-local/src/command.rs crates/cni-host-local/src/main.rs
git commit -m "feat(host-local): ADD/DEL/CHECK command handlers"
```

---

## Task 8: main dispatch

**Files:**
- Modify: `crates/cni-host-local/src/main.rs`

- [ ] **Step 1: Replace main.rs**

`crates/cni-host-local/src/main.rs`:
```rust
mod alloc;
mod command;
mod store;

use cni::env::CniArgs;
use cni::error::CniError;
use cni::version::VersionResult;
use std::io::Read;
use std::process::ExitCode;

fn read_stdin() -> String {
    let mut s = String::new();
    let _ = std::io::stdin().read_to_string(&mut s);
    s
}

fn run() -> Result<String, CniError> {
    let args = CniArgs::from_env()?;
    match args.command.as_str() {
        "VERSION" => Ok(VersionResult::supported().to_json()),
        "ADD" => command::cmd_add(&args, &read_stdin()),
        "DEL" => command::cmd_del(&args, &read_stdin()),
        "CHECK" => command::cmd_check(&args, &read_stdin()),
        other => Err(CniError::new(4, format!("unknown CNI_COMMAND {other}"))),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(out) => {
            if !out.is_empty() {
                print!("{out}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            print!("{}", e.to_json());
            ExitCode::FAILURE
        }
    }
}
```

- [ ] **Step 2: Build + full crate test**

Run: `cargo build -p cni-host-local && cargo test -p cni-host-local`
Expected: builds; all alloc/store/command tests PASS.

- [ ] **Step 3: Manual smoke of the binary**

Run:
```bash
BIN=$(cargo build -p cni-host-local --message-format=json 2>/dev/null | \
  python3 -c "import sys,json; [print(json.loads(l)['executable']) for l in sys.stdin if l.strip().startswith('{') and json.loads(l).get('executable') and 'host-local' in (json.loads(l).get('executable') or '')]" | head -1)
echo VERSION test:
CNI_COMMAND=VERSION "$BIN"
echo
echo ADD test:
D=$(mktemp -d)
echo "{\"cniVersion\":\"0.3.1\",\"name\":\"cbr0\",\"ipam\":{\"type\":\"host-local\",\"ranges\":[[{\"subnet\":\"10.244.1.0/24\"}]],\"routes\":[{\"dst\":\"0.0.0.0/0\"}],\"dataDir\":\"$D\"}}" | \
  CNI_COMMAND=ADD CNI_CONTAINERID=c1 CNI_IFNAME=eth0 "$BIN"
echo
```
Expected: VERSION prints `{"cniVersion":"0.3.1","supportedVersions":["0.3.0","0.3.1"]}`; ADD prints a result with `"address":"10.244.1.2/24"`, `"gateway":"10.244.1.1"`.

- [ ] **Step 4: Commit**

```bash
git add crates/cni-host-local/src/main.rs
git commit -m "feat(host-local): CNI command dispatch + stdin/stdout"
```

---

## Task 9: Bake host-local into the image + install it

**Files:**
- Modify: `Dockerfile`
- Modify: `deploy/flannel-rs.yaml`

- [ ] **Step 1: Build host-local in the image**

In `Dockerfile`, change the build stage to build both binaries, and copy `host-local` into the runtime image. Edit the build line and add a copy:

Find:
```dockerfile
RUN cargo build --release -p flanneld
```
Replace with:
```dockerfile
RUN cargo build --release -p flanneld -p cni-host-local
```
And in the runtime stage, after the existing `COPY --from=build /src/target/release/flanneld /usr/local/bin/flanneld`, add:
```dockerfile
COPY --from=build /src/target/release/host-local /opt/cni/bin/host-local
```

- [ ] **Step 2: Add an initContainer that installs the Rust host-local**

In `deploy/flannel-rs.yaml`, add a new initContainer (after `install-cni-plugin`, before `install-cni`) that overwrites the node's host-local with our Rust build. It uses the `flannel-rs:dev` image (which now bakes `/opt/cni/bin/host-local`) and the existing `cni-plugin` hostPath volume mounted at `/host/opt/cni/bin`:
```yaml
      - name: install-host-local-rs
        image: flannel-rs:dev
        imagePullPolicy: Never
        command: ["cp", "-f", "/opt/cni/bin/host-local", "/host/opt/cni/bin/host-local"]
        volumeMounts:
        - name: cni-plugin
          mountPath: /host/opt/cni/bin
```
Confirm a `cni-plugin` volume already exists in the manifest mapped to hostPath `/opt/cni/bin` (it backs the `install-cni-plugin` step). If the existing `install-cni-plugin` mounts `cni-plugin` at a different path, match the hostPath; the goal is that the file lands at the node's `/opt/cni/bin/host-local`.

- [ ] **Step 3: Build the image**

Run: `docker build -t flannel-rs:dev .`
Expected: image builds; verify both binaries present:
```bash
docker run --rm --entrypoint ls flannel-rs:dev -l /usr/local/bin/flanneld /opt/cni/bin/host-local
```

- [ ] **Step 4: Commit**

```bash
git add Dockerfile deploy/flannel-rs.yaml
git commit -m "build: bake Rust host-local into image and install over Go"
```

---

## Task 10: Verify by smoke + conformance (the gate)

**Files:** none (uses existing harness).

- [ ] **Step 1: Confirm the Rust host-local is the one in use**

Run `bash tests/smoke/run.sh flannel-rs` but before teardown is automatic, the key check is end-to-end success. To explicitly confirm our binary is installed, you may temporarily add a probe, or trust the smoke result. Primary:

Run: `bash tests/smoke/run.sh flannel-rs`
Expected: ends `SMOKE PASSED: flannel-rs`. Pods get IPs from the Rust IPAM (cross-node ping/HTTP/ClusterIP all pass).

- [ ] **Step 2: Conformance with Rust IPAM**

Run: `bash tests/conformance/run.sh flannel-rs`
Expected: the 47 `[sig-network] [Conformance]` specs pass with the Rust host-local allocating every pod IP.

- [ ] **Step 3: Baseline still green (untouched)**

Run: `bash tests/smoke/run.sh flannel-go`
Expected: `SMOKE PASSED: flannel-go` (still uses Go host-local).

- [ ] **Step 4: If red, debug**

Use `superpowers:systematic-debugging`. Probes:
```bash
# Is our binary actually installed?
docker exec flannel-rs-worker sh -c 'head -c4 /opt/cni/bin/host-local | xxd; /opt/cni/bin/host-local </dev/null; echo "exit=$?"'  # CNI_COMMAND unset => code 4 error JSON
# kubelet CNI errors:
kubectl --context kind-flannel-rs describe pod <pending-pod> | grep -A5 Events
# lease files our plugin wrote:
docker exec flannel-rs-worker ls -la /var/lib/cni/networks/cbr0/
kubectl --context kind-flannel-rs -n kube-flannel logs ds/kube-flannel-ds --tail=40
```
Likely issues: result `cniVersion` must equal the conflist's `0.3.1`; `dataDir`/network-name mismatch (bridge passes `name=cbr0`); gateway must be `.1` so the bridge's `isDefaultGateway` lines up. Fix the Rust code, rebuild image, re-run.

- [ ] **Step 5: Commit any fixes**

```bash
git add -A
git commit -m "fix: <what> so Rust host-local passes smoke + conformance"
```

---

## Self-Review

**Spec coverage:**
- `cni` lib (env/config/result/error/version) → Tasks 2,3,4. ✓
- `cni-host-local` allocation core (sequential, exclusions, wraparound, exhaustion) → Task 5. ✓
- Disk store (Go-compatible layout, last_reserved, flock, idempotent release) → Task 6. ✓
- ADD/DEL/CHECK/VERSION + 0.3.1 result, default gateway `.1` → Tasks 7,8. ✓
- Bake + swap one binary; Go bridge execs Rust IPAM → Task 9. ✓
- Verify by smoke + conformance; flannel-go untouched → Task 10. ✓
- Error handling (idempotent DEL, flock, create dirs, CNI error JSON, no panics) → Tasks 6,7,8. ✓

**Placeholder scan:** Task 1 Step 3 intentionally creates stub module files filled by 2–4 — concrete, not a placeholder. No TBD/TODO elsewhere.

**Type consistency:** `CniArgs{command,container_id,netns,ifname,args,path}`, `NetConf{cni_version,name,ipam}`, `IpamConfig{kind,ranges,routes,data_dir}`, `RangeConfig{subnet,gateway}`, `Allocator::new/next_ip/gateway/prefix`, `Store::new/lock/leased/last_reserved/reserve/release/has`, `CniResult{cni_version,ips,routes}`, `IpResult{version,address,gateway}`, `CniError::new/with_details/to_json` — consistent across all tasks and call sites.

**Known risk:** the exact CNI result fields the upstream Go `bridge` expects from its IPAM delegate at spec 0.3.1 — covered by Task 10's conformance gate; debug notes call out the most likely mismatches (cniVersion echo, network name, gateway).
