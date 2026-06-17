# portmap CNI plugin (Rust) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port the `portmap` CNI plugin to Rust (flannel-subset: DNAT + hairpin for hostPort), add a hostPort smoke test, and swap it in — making the entire Flannel data path Rust.

**Architecture:** A sync binary `cni-portmap` that reads `prevResult` + `runtimeConfig.portMappings` and installs nat-table DNAT/hairpin chains via a new shared `cni::iptables` helper (nft/legacy backend detection mirroring flanneld's ipmasq). Pure parts (config parse, rule-arg construction, chain naming) are unit-tested; the iptables path is verified by a new hostPort smoke assert + conformance.

**Tech Stack:** Rust, serde/serde_json, the `cni` lib, iptables (nft/legacy) via shelling. kind harness for integration.

---

## Pre-flight: standing rule

Before ANY `git push`, run the full local CI gate and push only if all pass:
```
cargo fmt --all -- --check
RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets -- -D warnings
RUSTFLAGS="-D warnings" cargo build --workspace --locked
RUSTFLAGS="-D warnings" cargo test --workspace --locked
```
Put any `#[cfg(test)] mod tests { ... }` LAST in each file (clippy `items-after-test-module`).

---

## File Structure

```
crates/cni/src/
├── iptables.rs   # NEW: backend detect + ensure_chain/ensure_rule/delete_rule/flush_delete_chain
└── lib.rs        # add `pub mod iptables;`
crates/cni-portmap/
├── Cargo.toml
└── src/
    ├── main.rs    # ADD/DEL/CHECK/VERSION dispatch
    ├── config.rs  # PortmapConf parse + pod_ipv4(prevResult)
    └── rules.rs   # chain names, DNAT rule-arg build, apply/remove
tests/smoke/
├── workload.yaml # add hostport-server pod (hostPort 31180)
└── assert.sh     # add assert 5 (curl node:31180)
```

---

## Task 1: shared `cni::iptables` helper

**Files:**
- Create: `crates/cni/src/iptables.rs`
- Modify: `crates/cni/src/lib.rs`, `crates/cni/Cargo.toml` (tempfile dev-dep already present from M2c)

- [ ] **Step 1: Implement iptables.rs with a fake-binary test**

`crates/cni/src/iptables.rs`:
```rust
use crate::error::CniError;
use std::process::Command;

/// Thin wrapper over the iptables CLI, pinned to the backend (nft/legacy) that
/// matches kube-proxy's active rules. All operations target the `nat` table.
pub struct Iptables {
    backend: String,
}

impl Iptables {
    /// Pick the backend whose `nat` table holds kube-proxy's `KUBE-` chains.
    /// Falls back to plain `iptables`.
    pub fn detect() -> Self {
        for b in ["iptables-nft", "iptables-legacy"] {
            if let Ok(out) = Command::new(b).args(["-t", "nat", "-S"]).output() {
                if out.status.success() && String::from_utf8_lossy(&out.stdout).contains("KUBE-") {
                    return Self { backend: b.to_string() };
                }
            }
        }
        Self { backend: "iptables".to_string() }
    }

    /// Explicit backend (used by tests and when detection is not needed).
    pub fn with_backend(backend: impl Into<String>) -> Self {
        Self { backend: backend.into() }
    }

    fn run(&self, args: &[&str]) -> Result<std::process::Output, CniError> {
        Command::new(&self.backend)
            .arg("--wait")
            .args(args)
            .output()
            .map_err(|e| CniError::new(7, format!("exec {}", self.backend)).with_details(e.to_string()))
    }

    /// Create a nat chain if absent.
    pub fn ensure_chain(&self, chain: &str) -> Result<(), CniError> {
        let out = self.run(&["-t", "nat", "-N", chain])?;
        if out.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("exists") {
            return Ok(()); // already created
        }
        Err(CniError::new(7, format!("create chain {chain}")).with_details(stderr.to_string()))
    }

    /// Append a rule to a nat chain if not already present (`-C` then `-A`).
    pub fn ensure_rule(&self, chain: &str, rule: &[&str]) -> Result<(), CniError> {
        let mut check = vec!["-t", "nat", "-C", chain];
        check.extend_from_slice(rule);
        if self.run(&check)?.status.success() {
            return Ok(());
        }
        let mut add = vec!["-t", "nat", "-A", chain];
        add.extend_from_slice(rule);
        let out = self.run(&add)?;
        if out.status.success() {
            Ok(())
        } else {
            Err(CniError::new(7, format!("append rule to {chain}"))
                .with_details(String::from_utf8_lossy(&out.stderr).to_string()))
        }
    }

    /// Delete a rule (best-effort; missing is fine).
    pub fn delete_rule(&self, chain: &str, rule: &[&str]) {
        let mut del = vec!["-t", "nat", "-D", chain];
        del.extend_from_slice(rule);
        let _ = self.run(&del);
    }

    /// Flush and delete a chain (best-effort).
    pub fn flush_delete_chain(&self, chain: &str) {
        let _ = self.run(&["-t", "nat", "-F", chain]);
        let _ = self.run(&["-t", "nat", "-X", chain]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    // A fake iptables that records each invocation's args to $FAKE_LOG and
    // exits 0, except `-C` (check) exits 1 so ensure_rule proceeds to `-A`.
    fn fake_iptables(dir: &Path) -> String {
        let p = dir.join("fake-iptables");
        std::fs::write(
            &p,
            "#!/bin/sh\necho \"$@\" >> \"$FAKE_LOG\"\nfor a in \"$@\"; do [ \"$a\" = \"-C\" ] && exit 1; done\nexit 0\n",
        )
        .unwrap();
        let mut perm = std::fs::metadata(&p).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&p, perm).unwrap();
        p.to_str().unwrap().to_string()
    }

    #[test]
    fn ensure_rule_checks_then_appends() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("log");
        std::env::set_var("FAKE_LOG", &log);
        let ipt = Iptables::with_backend(fake_iptables(tmp.path()));
        ipt.ensure_rule("CNI-HOSTPORT-DNAT", &["-j", "CNI-DN-abc"]).unwrap();
        let recorded = std::fs::read_to_string(&log).unwrap();
        assert!(recorded.contains("-C CNI-HOSTPORT-DNAT -j CNI-DN-abc"), "{recorded}");
        assert!(recorded.contains("-A CNI-HOSTPORT-DNAT -j CNI-DN-abc"), "{recorded}");
        std::env::remove_var("FAKE_LOG");
    }
}
```

Add `pub mod iptables;` to `crates/cni/src/lib.rs`.

- [ ] **Step 2: Test**

Run: `cargo test -p cni iptables::`
Expected: PASS (`ensure_rule_checks_then_appends`).

- [ ] **Step 3: Commit**

```bash
git add crates/cni/src/iptables.rs crates/cni/src/lib.rs
git commit -m "feat(cni): shared iptables helper (backend detect + rule ops)"
```

---

## Task 2: scaffold cni-portmap + config parse

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/cni-portmap/Cargo.toml`, `crates/cni-portmap/src/main.rs`, `crates/cni-portmap/src/config.rs`

- [ ] **Step 1: Workspace member**

Root `Cargo.toml` members add `"crates/cni-portmap"`:
```toml
members = ["crates/flanneld", "crates/cni", "crates/cni-host-local", "crates/cni-flannel", "crates/cni-bridge", "crates/cni-portmap"]
```

- [ ] **Step 2: Crate manifest**

`crates/cni-portmap/Cargo.toml`:
```toml
[package]
name = "cni-portmap"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "portmap"
path = "src/main.rs"

[dependencies]
cni = { path = "../cni" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
ipnetwork = "0.20"
```

- [ ] **Step 3: config.rs with tests**

`crates/cni-portmap/src/config.rs`:
```rust
use cni::result::CniResult;
use ipnetwork::Ipv4Network;
use serde::Deserialize;
use std::net::Ipv4Addr;

#[derive(Debug, Deserialize)]
pub struct PortmapConf {
    #[serde(rename = "cniVersion", default)]
    pub cni_version: String,
    #[serde(rename = "prevResult", default)]
    pub prev_result: Option<CniResult>,
    #[serde(rename = "runtimeConfig", default)]
    pub runtime_config: RuntimeConfig,
}

#[derive(Debug, Deserialize, Default)]
pub struct RuntimeConfig {
    #[serde(rename = "portMappings", default)]
    pub port_mappings: Vec<PortMapping>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct PortMapping {
    #[serde(rename = "hostPort")]
    pub host_port: u16,
    #[serde(rename = "containerPort")]
    pub container_port: u16,
    #[serde(default = "default_proto")]
    pub protocol: String,
}

fn default_proto() -> String {
    "tcp".to_string()
}

impl PortmapConf {
    pub fn parse(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }
}

/// The first IPv4 address from the prevResult.
pub fn pod_ipv4(result: &CniResult) -> Option<Ipv4Addr> {
    for ip in &result.ips {
        if let Ok(net) = ip.address.parse::<Ipv4Network>() {
            return Some(net.ip());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_portmap_conf_with_mappings() {
        let raw = r#"{
          "cniVersion":"0.3.1","name":"cbr0","type":"portmap",
          "runtimeConfig":{"portMappings":[{"hostPort":31180,"containerPort":80,"protocol":"tcp"}]},
          "prevResult":{"cniVersion":"0.3.1","ips":[{"version":"4","address":"10.244.1.5/24","gateway":"10.244.1.1"}]}
        }"#;
        let c = PortmapConf::parse(raw).unwrap();
        assert_eq!(c.runtime_config.port_mappings.len(), 1);
        let m = &c.runtime_config.port_mappings[0];
        assert_eq!(m.host_port, 31180);
        assert_eq!(m.container_port, 80);
        assert_eq!(m.protocol, "tcp");
        let pip = pod_ipv4(c.prev_result.as_ref().unwrap()).unwrap();
        assert_eq!(pip, "10.244.1.5".parse::<Ipv4Addr>().unwrap());
    }

    #[test]
    fn empty_runtime_config_yields_no_mappings() {
        let raw = r#"{"cniVersion":"0.3.1","prevResult":{"cniVersion":"0.3.1","ips":[]}}"#;
        let c = PortmapConf::parse(raw).unwrap();
        assert!(c.runtime_config.port_mappings.is_empty());
    }
}
```

- [ ] **Step 4: Minimal main**

`crates/cni-portmap/src/main.rs`:
```rust
mod config;

fn main() {
    eprintln!("portmap (rust) — not yet implemented");
    std::process::exit(1);
}
```

- [ ] **Step 5: Test + build**

Run: `cargo test -p cni-portmap config:: && cargo build -p cni-portmap`
Expected: 2 config tests PASS; binary builds.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/cni-portmap
git commit -m "feat(portmap): scaffold crate + config parsing"
```

---

## Task 3: rules — chain names, DNAT arg build, apply/remove

**Files:**
- Create: `crates/cni-portmap/src/rules.rs`
- Modify: `crates/cni-portmap/src/main.rs` (declare `mod rules;`)

- [ ] **Step 1: rules.rs with tests for the pure parts**

`crates/cni-portmap/src/rules.rs`:
```rust
use crate::config::PortMapping;
use cni::error::CniError;
use cni::iptables::Iptables;
use std::net::Ipv4Addr;

pub const TOP_CHAIN: &str = "CNI-HOSTPORT-DNAT";

/// Per-container DNAT chain name, within iptables' 28-char chain limit.
pub fn dn_chain(container_id: &str) -> String {
    let id: String = container_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(16)
        .collect();
    format!("CNI-DN-{id}")
}

/// Per-container hairpin-masquerade chain name.
pub fn hm_chain(container_id: &str) -> String {
    let id: String = container_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(16)
        .collect();
    format!("CNI-HM-{id}")
}

/// The DNAT rule args (after `-A <chain>`) for one mapping.
pub fn dnat_args(m: &PortMapping, pod_ip: Ipv4Addr) -> Vec<String> {
    vec![
        "-p".into(),
        m.protocol.clone(),
        "--dport".into(),
        m.host_port.to_string(),
        "-j".into(),
        "DNAT".into(),
        "--to-destination".into(),
        format!("{}:{}", pod_ip, m.container_port),
    ]
}

/// The hairpin masquerade rule args for one mapping (pod reaching its own hostPort).
pub fn hairpin_args(m: &PortMapping, pod_ip: Ipv4Addr) -> Vec<String> {
    vec![
        "-p".into(),
        m.protocol.clone(),
        "-s".into(),
        format!("{pod_ip}/32"),
        "-d".into(),
        format!("{pod_ip}/32"),
        "--dport".into(),
        m.container_port.to_string(),
        "-j".into(),
        "MASQUERADE".into(),
    ]
}

fn as_refs(v: &[String]) -> Vec<&str> {
    v.iter().map(|s| s.as_str()).collect()
}

/// Install DNAT + hairpin chains/rules for all mappings.
pub fn apply(ipt: &Iptables, container_id: &str, pod_ip: Ipv4Addr, mappings: &[PortMapping]) -> Result<(), CniError> {
    let dn = dn_chain(container_id);
    let hm = hm_chain(container_id);

    // Top DNAT chain + jumps from PREROUTING/OUTPUT (only for locally-addressed traffic).
    ipt.ensure_chain(TOP_CHAIN)?;
    ipt.ensure_rule("PREROUTING", &["-m", "addrtype", "--dst-type", "LOCAL", "-j", TOP_CHAIN])?;
    ipt.ensure_rule("OUTPUT", &["-m", "addrtype", "--dst-type", "LOCAL", "-j", TOP_CHAIN])?;

    // Per-container DNAT chain, jumped from the top chain.
    ipt.ensure_chain(&dn)?;
    ipt.ensure_rule(TOP_CHAIN, &["-j", &dn])?;

    // Hairpin masq chain, jumped from POSTROUTING.
    ipt.ensure_chain(&hm)?;
    ipt.ensure_rule("POSTROUTING", &["-j", &hm])?;

    for m in mappings {
        let d = dnat_args(m, pod_ip);
        ipt.ensure_rule(&dn, &as_refs(&d))?;
        let h = hairpin_args(m, pod_ip);
        ipt.ensure_rule(&hm, &as_refs(&h))?;
    }
    Ok(())
}

/// Remove this container's chains and their jumps (idempotent / best-effort).
pub fn remove(ipt: &Iptables, container_id: &str) {
    let dn = dn_chain(container_id);
    let hm = hm_chain(container_id);
    ipt.delete_rule(TOP_CHAIN, &["-j", &dn]);
    ipt.flush_delete_chain(&dn);
    ipt.delete_rule("POSTROUTING", &["-j", &hm]);
    ipt.flush_delete_chain(&hm);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapping() -> PortMapping {
        PortMapping { host_port: 31180, container_port: 80, protocol: "tcp".into() }
    }

    #[test]
    fn dn_chain_within_iptables_limit() {
        let c = dn_chain("ec5a938858dce08f4179b48658de7bbd");
        assert!(c.len() <= 28, "len {}", c.len());
        assert!(c.starts_with("CNI-DN-"));
    }

    #[test]
    fn dnat_args_target_pod_ip_and_port() {
        let a = dnat_args(&mapping(), "10.244.1.5".parse().unwrap());
        assert_eq!(
            a,
            vec!["-p", "tcp", "--dport", "31180", "-j", "DNAT", "--to-destination", "10.244.1.5:80"]
        );
    }

    #[test]
    fn hairpin_args_masquerade_self_traffic() {
        let a = hairpin_args(&mapping(), "10.244.1.5".parse().unwrap());
        assert_eq!(
            a,
            vec!["-p", "tcp", "-s", "10.244.1.5/32", "-d", "10.244.1.5/32", "--dport", "80", "-j", "MASQUERADE"]
        );
    }
}
```

- [ ] **Step 2: Declare module + test**

In `crates/cni-portmap/src/main.rs` add `mod rules;` (add `#![allow(dead_code)]` at top of main.rs for now, since the stub main doesn't use rules/config yet — removed in Task 4). Run: `cargo test -p cni-portmap rules::`
Expected: 3 tests PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/cni-portmap/src/rules.rs crates/cni-portmap/src/main.rs
git commit -m "feat(portmap): DNAT/hairpin chain build + apply/remove"
```

---

## Task 4: main dispatch

**Files:**
- Modify: `crates/cni-portmap/src/main.rs`

- [ ] **Step 1: Replace main.rs**

`crates/cni-portmap/src/main.rs`:
```rust
mod config;
mod rules;

use cni::env::CniArgs;
use cni::error::CniError;
use cni::iptables::Iptables;
use cni::version::VersionResult;
use config::PortmapConf;
use std::io::Read;
use std::process::ExitCode;

fn read_stdin() -> String {
    let mut s = String::new();
    let _ = std::io::stdin().read_to_string(&mut s);
    s
}

fn err(code: u32, msg: impl Into<String>) -> CniError {
    CniError::new(code, msg)
}

fn cmd_add(args: &CniArgs, stdin: &str) -> Result<String, CniError> {
    let conf = PortmapConf::parse(stdin).map_err(|e| err(6, "decode config").with_details(e.to_string()))?;
    let mappings = &conf.runtime_config.port_mappings;
    if mappings.is_empty() {
        return Ok(stdin.to_string()); // nothing to do; relay prevResult (the whole config carries it)
    }
    let prev = conf.prev_result.as_ref().ok_or_else(|| err(7, "portmap requires prevResult"))?;
    let pod_ip = config::pod_ipv4(prev).ok_or_else(|| err(7, "no IPv4 in prevResult"))?;

    let ipt = Iptables::detect();
    rules::apply(&ipt, &args.container_id, pod_ip, mappings)?;

    // Relay prevResult unchanged for any later chained plugin.
    Ok(serde_json::to_string(prev).map_err(|e| err(6, "encode result").with_details(e.to_string()))?)
}

fn cmd_del(args: &CniArgs, stdin: &str) -> Result<String, CniError> {
    // Parse best-effort; even with no mappings, removing chains is harmless.
    if let Ok(conf) = PortmapConf::parse(stdin) {
        if conf.runtime_config.port_mappings.is_empty() {
            return Ok(String::new());
        }
    }
    let ipt = Iptables::detect();
    rules::remove(&ipt, &args.container_id);
    Ok(String::new())
}

fn run() -> Result<(String, bool), CniError> {
    let args = CniArgs::from_env()?;
    match args.command.as_str() {
        "VERSION" => Ok((VersionResult::supported().to_json(), true)),
        "ADD" => cmd_add(&args, &read_stdin()).map(|s| (s, true)),
        "DEL" => cmd_del(&args, &read_stdin()).map(|s| (s, true)),
        "CHECK" => Ok((String::new(), true)),
        other => Err(err(4, format!("unknown CNI_COMMAND {other}"))),
    }
}

fn main() -> ExitCode {
    match run() {
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
(Remove the `#![allow(dead_code)]` added in Task 3 — main now uses config + rules. If `PortmapConf.cni_version` is flagged unused under clippy `-D warnings`, add a targeted `#[allow(dead_code)]` on just that field with a comment, as done in cni-bridge.)

- [ ] **Step 2: Build + VERSION + full local gate**

Run:
```bash
cargo build -p cni-portmap
BIN=$(find /home/jones/.cache/rusternetes-target ./target -path '*debug*' -name portmap -type f 2>/dev/null | grep -v deps | head -1)
CNI_COMMAND=VERSION "$BIN"; echo
cargo fmt --all
cargo fmt --all -- --check
RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
Expected: VERSION prints `{"cniVersion":"0.3.1","supportedVersions":["0.3.0","0.3.1"]}`; fmt clean; clippy clean; all unit tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/cni-portmap/src/main.rs
git commit -m "feat(portmap): ADD/DEL/CHECK/VERSION dispatch"
```

---

## Task 5: hostPort smoke test (workload + assert 5)

**Files:**
- Modify: `tests/smoke/workload.yaml`, `tests/smoke/assert.sh`

- [ ] **Step 1: Add a hostPort pod to the workload**

Append to `tests/smoke/workload.yaml` (new document):
```yaml
---
apiVersion: apps/v1
kind: Deployment
metadata: { name: hostport-server, labels: { app: hostport-server } }
spec:
  replicas: 1
  selector: { matchLabels: { app: hostport-server } }
  template:
    metadata: { labels: { app: hostport-server } }
    spec:
      containers:
        - name: web
          image: registry.k8s.io/e2e-test-images/agnhost:2.47
          args: ["netexec", "--http-port=80"]
          ports:
            - containerPort: 80
              hostPort: 31180
```

- [ ] **Step 2: Add assert 5 to assert.sh**

In `tests/smoke/assert.sh`, after the existing assert 4 block and before the final `echo "ALL ASSERTS PASSED"`, insert:
```bash
echo "== assert 5: hostPort (portmap DNAT) =="
k rollout status deploy/hostport-server --timeout=120s
HP_POD=$(k get pod -l app=hostport-server -o jsonpath='{.items[0].metadata.name}')
HP_NODE=$(k get pod "$HP_POD" -o jsonpath='{.spec.nodeName}')
echo "hostport pod $HP_POD on node $HP_NODE"
# hostPort 31180 is published on the node the pod runs on; curl it from inside that node.
retry 60 docker exec "$HP_NODE" curl -sS --max-time 5 "http://127.0.0.1:31180/hostname"
echo "OK: hostPort reachable on $HP_NODE"
```
(Uses the existing `retry` helper and `k()` from assert.sh. The workload's `rollout status` waits are in run.sh; this `rollout status` is a fast re-check.)

- [ ] **Step 3: Commit (test scaffolding; verified live in Task 7)**

```bash
git add tests/smoke/workload.yaml tests/smoke/assert.sh
git commit -m "test: hostPort smoke assert (exercises portmap)"
```

---

## Task 6: bake into image + install over Go portmap

**Files:**
- Modify: `Dockerfile`, `deploy/flannel-rs.yaml`

- [ ] **Step 1: Build + copy portmap**

In `Dockerfile`, extend the build line and add a COPY.

Find:
```dockerfile
RUN cargo build --release -p flanneld -p cni-host-local -p cni-flannel -p cni-bridge
```
Replace with:
```dockerfile
RUN cargo build --release -p flanneld -p cni-host-local -p cni-flannel -p cni-bridge -p cni-portmap
```
After `COPY --from=build /src/target/release/bridge /opt/cni/bin/bridge`, add:
```dockerfile
COPY --from=build /src/target/release/portmap /opt/cni/bin/portmap
```

- [ ] **Step 2: Install portmap onto nodes**

In `deploy/flannel-rs.yaml`, extend the `install-cni-plugins-rs` command:
```yaml
        command: ["sh", "-c", "cp -f /opt/cni/bin/flannel /opt/cni/bin/host-local /opt/cni/bin/bridge /opt/cni/bin/portmap /host/opt/cni/bin/"]
```

- [ ] **Step 3: Build image + verify**

Run:
```bash
docker build -t flannel-rs:dev .
docker run --rm --entrypoint ls flannel-rs:dev /opt/cni/bin/flannel /opt/cni/bin/host-local /opt/cni/bin/bridge /opt/cni/bin/portmap
python3 -c "import yaml; list(yaml.safe_load_all(open('deploy/flannel-rs.yaml')))" && echo "YAML OK"
```
Expected: image builds; all four Rust plugins present; YAML valid.

- [ ] **Step 4: Commit**

```bash
git add Dockerfile deploy/flannel-rs.yaml
git commit -m "build: bake Rust portmap; install over Go portmap"
```

---

## Task 7: Verify by smoke (+assert 5) + conformance

**Files:** none.

- [ ] **Step 1: Smoke (flannel-rs) incl. hostPort**

Run: `bash tests/smoke/run.sh flannel-rs`
Expected: ends `SMOKE PASSED: flannel-rs`, with assert 5 (`OK: hostPort reachable`). This drives the Rust portmap's DNAT path.

- [ ] **Step 2: Baseline (flannel-go) incl. hostPort**

Run: `bash tests/smoke/run.sh flannel-go`
Expected: `SMOKE PASSED: flannel-go` — Go portmap satisfies assert 5 too (parity).

- [ ] **Step 3: Conformance (flannel-rs)**

Run: `bash tests/conformance/run.sh flannel-rs`
Expected: 47 `[sig-network] [Conformance]` specs pass with all-Rust plugins.

- [ ] **Step 4: Confirm Rust portmap in use + debug if red**

Use `superpowers:systematic-debugging`. Probes:
```bash
docker exec flannel-rs-worker /opt/cni/bin/portmap </dev/null; echo "exit=$?"   # our code-4 CNI error JSON
HP_NODE=...; docker exec "$HP_NODE" iptables-nft -t nat -S | grep -E "CNI-HOSTPORT-DNAT|CNI-DN-|31180"
kubectl --context kind-flannel-rs describe pod <hostport-pod> | grep -A8 Events
```
Likely issues: backend mismatch (DNAT rules in nft but kube-proxy elsewhere — our detect() handles it); wrong chain jump (PREROUTING addrtype LOCAL); DNAT target IP/port; hairpin not needed for the node→pod assert but required if a test hits self. Fix Rust, rebuild image, re-run.

- [ ] **Step 5: Commit any fixes**

```bash
git add -A
git commit -m "fix: <root cause> so Rust portmap passes hostPort smoke + conformance"
```

---

## Self-Review

**Spec coverage:**
- Shared `cni::iptables` (backend detect + chain/rule ops) → Task 1. ✓
- PortmapConf + portMappings + pod_ipv4 from prevResult → Task 2. ✓
- DNAT + hairpin chain build/apply/remove (tcp/udp via protocol field, IPv4) → Task 3. ✓
- ADD (empty→relay; else apply; relay prevResult), DEL (remove chains, idempotent), CHECK stub, VERSION → Task 4. ✓
- hostPort test (workload pod + assert 5, both variants) → Task 5. ✓
- Bake + install over Go portmap (last Go binary gone) → Task 6. ✓
- Verify smoke+assert5 + conformance; flannel-go untouched → Task 7. ✓
- Error handling (empty→noop, missing IP→7, backend/rule fail→7, DEL idempotent, no panics) → Tasks 3,4. ✓

**Placeholder scan:** `cni::iptables` is integration-verified for `detect()` (shells) but its rule logic is unit-tested via a fake binary; CHECK stub is intentional (0.3.1). No TBD/TODO.

**Type consistency:** `PortmapConf{cni_version,prev_result,runtime_config}`, `RuntimeConfig{port_mappings}`, `PortMapping{host_port,container_port,protocol}`, `pod_ipv4`, `Iptables{detect,with_backend,ensure_chain,ensure_rule,delete_rule,flush_delete_chain}`, `rules::{TOP_CHAIN,dn_chain,hm_chain,dnat_args,hairpin_args,apply,remove}` — consistent across Tasks 1–4 and call sites. Reuses `cni::result::CniResult` (Deserialize from M2c), `env::CniArgs`, `error::CniError`, `version::VersionResult`.

**Known risk:** iptables backend interaction with kube-proxy (nft) and the exact rule forms — covered by the new hostPort assert (deterministic) + conformance in Task 7; debug probes provided.
