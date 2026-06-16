# flannel CNI meta-plugin (Rust) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port Flannel's `flannel` CNI meta-plugin to Rust — read `/run/flannel/subnet.env`, build the delegate config, exec the delegate (Go `bridge`) — and swap it into the flannel-rs chain, eliminating the upstream Go flannel-cni-plugin image.

**Architecture:** New `cni-flannel` binary crate reusing the `cni` lib. A pure `subnetenv` parser and a pure `build_delegate` config transformer (fully unit-testable), plus an `exec` module that locates the delegate in `CNI_PATH` and runs it with the same CNI env (testable with a fake delegate script). Verified by swapping only `/opt/cni/bin/flannel` to the Rust build and keeping smoke + conformance green.

**Tech Stack:** Rust, serde/serde_json, the existing `cni` crate. kind + the existing harness for integration.

---

## File Structure

```
crates/cni-flannel/
├── Cargo.toml
└── src/
    ├── main.rs        # VERSION/ADD/DEL/CHECK dispatch + stdin/exit handling
    ├── subnetenv.rs   # parse /run/flannel/subnet.env -> SubnetEnv
    ├── delegate.rs    # FlannelConf::parse + pure build_delegate(conf, env) -> Value
    └── exec.rs        # find delegate in CNI_PATH, exec with CNI env, relay I/O
```

Modified: root `Cargo.toml`, `Dockerfile`, `deploy/flannel-rs.yaml`.

Reuses `crates/cni`: `env::CniArgs`, `error::CniError`, `version::VersionResult`.

---

## Task 1: Scaffold the cni-flannel crate

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/cni-flannel/Cargo.toml`, `crates/cni-flannel/src/main.rs`

- [ ] **Step 1: Add workspace member**

Edit root `Cargo.toml`:
```toml
[workspace]
resolver = "2"
members = ["crates/flanneld", "crates/cni", "crates/cni-host-local", "crates/cni-flannel"]
```

- [ ] **Step 2: Crate manifest**

`crates/cni-flannel/Cargo.toml`:
```toml
[package]
name = "cni-flannel"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "flannel"
path = "src/main.rs"

[dependencies]
cni = { path = "../cni" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 3: Minimal binary**

`crates/cni-flannel/src/main.rs`:
```rust
fn main() {
    eprintln!("flannel meta-plugin (rust) — not yet implemented");
    std::process::exit(1);
}
```

- [ ] **Step 4: Verify build**

Run: `cargo build -p cni-flannel`
Expected: builds the `flannel` binary.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/cni-flannel
git commit -m "chore: scaffold cni-flannel crate"
```

---

## Task 2: subnet.env parser

**Files:**
- Create: `crates/cni-flannel/src/subnetenv.rs`
- Modify: `crates/cni-flannel/src/main.rs` (declare `mod subnetenv;`)

- [ ] **Step 1: Write subnetenv.rs with tests**

`crates/cni-flannel/src/subnetenv.rs`:
```rust
/// Parsed /run/flannel/subnet.env (the file flanneld writes).
#[derive(Debug, PartialEq)]
pub struct SubnetEnv {
    pub network: String, // FLANNEL_NETWORK, e.g. 10.244.0.0/16
    pub subnet: String,  // FLANNEL_SUBNET,  e.g. 10.244.1.0/24
    pub mtu: u32,        // FLANNEL_MTU
    pub ipmasq: bool,    // FLANNEL_IPMASQ
}

impl SubnetEnv {
    pub fn parse(s: &str) -> Result<Self, String> {
        let (mut network, mut subnet, mut mtu, mut ipmasq) = (None, None, None, None);
        for line in s.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let (k, v) = line.split_once('=').ok_or_else(|| format!("malformed line: {line}"))?;
            match k.trim() {
                "FLANNEL_NETWORK" => network = Some(v.trim().to_string()),
                "FLANNEL_SUBNET" => subnet = Some(v.trim().to_string()),
                "FLANNEL_MTU" => mtu = Some(v.trim().parse::<u32>().map_err(|e| e.to_string())?),
                "FLANNEL_IPMASQ" => ipmasq = Some(v.trim() == "true"),
                _ => {}
            }
        }
        Ok(Self {
            network: network.ok_or("missing FLANNEL_NETWORK")?,
            subnet: subnet.ok_or("missing FLANNEL_SUBNET")?,
            mtu: mtu.ok_or("missing FLANNEL_MTU")?,
            ipmasq: ipmasq.unwrap_or(false),
        })
    }

    pub fn load(path: &str) -> Result<Self, String> {
        let s = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        Self::parse(&s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_well_formed_subnet_env() {
        let raw = "FLANNEL_NETWORK=10.244.0.0/16\nFLANNEL_SUBNET=10.244.1.0/24\nFLANNEL_MTU=1450\nFLANNEL_IPMASQ=true\n";
        let e = SubnetEnv::parse(raw).unwrap();
        assert_eq!(
            e,
            SubnetEnv {
                network: "10.244.0.0/16".into(),
                subnet: "10.244.1.0/24".into(),
                mtu: 1450,
                ipmasq: true,
            }
        );
    }

    #[test]
    fn missing_required_key_errors() {
        let raw = "FLANNEL_NETWORK=10.244.0.0/16\nFLANNEL_MTU=1450\n";
        assert!(SubnetEnv::parse(raw).is_err()); // no FLANNEL_SUBNET
    }
}
```

- [ ] **Step 2: Declare module**

In `crates/cni-flannel/src/main.rs` add at top: `mod subnetenv;`

- [ ] **Step 3: Run tests**

Run: `cargo test -p cni-flannel subnetenv::`
Expected: PASS (`parses_well_formed_subnet_env`, `missing_required_key_errors`).

- [ ] **Step 4: Commit**

```bash
git add crates/cni-flannel/src/subnetenv.rs crates/cni-flannel/src/main.rs
git commit -m "feat(flannel): parse /run/flannel/subnet.env"
```

---

## Task 3: Delegate config builder (pure)

**Files:**
- Create: `crates/cni-flannel/src/delegate.rs`
- Modify: `crates/cni-flannel/src/main.rs` (declare `mod delegate;`)

- [ ] **Step 1: Write delegate.rs with tests**

`crates/cni-flannel/src/delegate.rs`:
```rust
use crate::subnetenv::SubnetEnv;
use serde_json::{json, Map, Value};

/// The flannel plugin's own stdin netconf.
pub struct FlannelConf {
    pub name: String,
    pub cni_version: String,
    pub delegate: Map<String, Value>,
}

impl FlannelConf {
    pub fn parse(s: &str) -> Result<Self, serde_json::Error> {
        let v: Value = serde_json::from_str(s)?;
        let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let cni_version = v
            .get("cniVersion")
            .and_then(|x| x.as_str())
            .unwrap_or("0.3.1")
            .to_string();
        let delegate = v
            .get("delegate")
            .and_then(|d| d.as_object())
            .cloned()
            .unwrap_or_default();
        Ok(Self { name, cni_version, delegate })
    }
}

/// Build the delegate netconf by filling flannel-derived fields the user did not
/// set, then always injecting the host-local ipam derived from subnet.env.
pub fn build_delegate(conf: &FlannelConf, env: &SubnetEnv) -> Value {
    let mut d = conf.delegate.clone();
    d.insert("name".into(), json!(conf.name));
    d.insert("cniVersion".into(), json!(conf.cni_version));
    if !d.contains_key("type") {
        d.insert("type".into(), json!("bridge"));
    }
    if !d.contains_key("ipMasq") {
        // flanneld installs the masquerade rules, so the delegate must not.
        d.insert("ipMasq".into(), json!(!env.ipmasq));
    }
    if !d.contains_key("mtu") {
        d.insert("mtu".into(), json!(env.mtu));
    }
    if d.get("type").and_then(|t| t.as_str()) == Some("bridge") && !d.contains_key("isGateway") {
        d.insert("isGateway".into(), json!(true));
    }
    d.insert(
        "ipam".into(),
        json!({
            "type": "host-local",
            "ranges": [[{"subnet": env.subnet}]],
            "routes": [{"dst": env.network}],
        }),
    );
    Value::Object(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> SubnetEnv {
        SubnetEnv {
            network: "10.244.0.0/16".into(),
            subnet: "10.244.1.0/24".into(),
            mtu: 1450,
            ipmasq: true,
        }
    }

    #[test]
    fn builds_bridge_delegate_from_flannel_conf() {
        let conf = FlannelConf::parse(
            r#"{"name":"cbr0","cniVersion":"0.3.1","type":"flannel","delegate":{"hairpinMode":true,"isDefaultGateway":true}}"#,
        )
        .unwrap();
        let d = build_delegate(&conf, &env());
        assert_eq!(d["name"], "cbr0");
        assert_eq!(d["cniVersion"], "0.3.1");
        assert_eq!(d["type"], "bridge");
        assert_eq!(d["mtu"], 1450);
        assert_eq!(d["ipMasq"], false); // FLANNEL_IPMASQ=true => delegate must not masq
        assert_eq!(d["isGateway"], true);
        assert_eq!(d["hairpinMode"], true); // user field preserved
        assert_eq!(d["isDefaultGateway"], true); // user field preserved
        assert_eq!(d["ipam"]["type"], "host-local");
        assert_eq!(d["ipam"]["ranges"][0][0]["subnet"], "10.244.1.0/24");
        assert_eq!(d["ipam"]["routes"][0]["dst"], "10.244.0.0/16");
    }

    #[test]
    fn does_not_overwrite_user_fields() {
        let conf = FlannelConf::parse(
            r#"{"name":"cbr0","cniVersion":"0.3.1","delegate":{"type":"ptp","mtu":9000,"ipMasq":true}}"#,
        )
        .unwrap();
        let d = build_delegate(&conf, &env());
        assert_eq!(d["type"], "ptp"); // not defaulted to bridge
        assert_eq!(d["mtu"], 9000); // user mtu kept
        assert_eq!(d["ipMasq"], true); // user value kept
        assert!(d.get("isGateway").is_none()); // only added for bridge
    }
}
```

- [ ] **Step 2: Declare module**

In `crates/cni-flannel/src/main.rs` add: `mod delegate;`

- [ ] **Step 3: Run tests**

Run: `cargo test -p cni-flannel delegate::`
Expected: PASS (`builds_bridge_delegate_from_flannel_conf`, `does_not_overwrite_user_fields`).

- [ ] **Step 4: Commit**

```bash
git add crates/cni-flannel/src/delegate.rs crates/cni-flannel/src/main.rs
git commit -m "feat(flannel): build delegate config from subnet.env"
```

---

## Task 4: Delegate exec

**Files:**
- Create: `crates/cni-flannel/src/exec.rs`
- Modify: `crates/cni-flannel/src/main.rs` (declare `mod exec;`)

- [ ] **Step 1: Write exec.rs with tests**

`crates/cni-flannel/src/exec.rs`:
```rust
use cni::env::CniArgs;
use cni::error::CniError;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// stdout of the delegate, plus whether it exited 0.
pub struct DelegateOutput {
    pub stdout: String,
    pub success: bool,
}

/// Find `name` in CNI_PATH (colon-separated dirs), exec it with the same CNI_*
/// environment and `stdin_json` piped to its stdin. Returns the child's stdout and
/// success flag. `Err` is only for our own failures (binary not found / spawn).
pub fn run_delegate(name: &str, args: &CniArgs, stdin_json: &str) -> Result<DelegateOutput, CniError> {
    let bin = find_in_path(name, &args.path)
        .ok_or_else(|| CniError::new(5, format!("delegate plugin {name:?} not found in CNI_PATH")))?;
    let mut child = Command::new(&bin)
        .env("CNI_COMMAND", &args.command)
        .env("CNI_CONTAINERID", &args.container_id)
        .env("CNI_NETNS", &args.netns)
        .env("CNI_IFNAME", &args.ifname)
        .env("CNI_ARGS", &args.args)
        .env("CNI_PATH", &args.path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| CniError::new(5, format!("exec delegate {name} failed")).with_details(e.to_string()))?;
    child
        .stdin
        .take()
        .expect("stdin piped")
        .write_all(stdin_json.as_bytes())
        .map_err(|e| CniError::new(5, "write delegate stdin").with_details(e.to_string()))?;
    let out = child
        .wait_with_output()
        .map_err(|e| CniError::new(5, "wait for delegate").with_details(e.to_string()))?;
    Ok(DelegateOutput {
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        success: out.status.success(),
    })
}

fn find_in_path(name: &str, cni_path: &str) -> Option<PathBuf> {
    for dir in cni_path.split(':').filter(|d| !d.is_empty()) {
        let p = Path::new(dir).join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::os::unix::fs::PermissionsExt;

    fn args(path: &str) -> CniArgs {
        let m: HashMap<String, String> = [
            ("CNI_COMMAND", "ADD"),
            ("CNI_CONTAINERID", "cid1"),
            ("CNI_IFNAME", "eth0"),
            ("CNI_PATH", path),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        CniArgs::from_map(&m).unwrap()
    }

    fn write_script(dir: &Path, name: &str, body: &str) {
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        let mut perm = std::fs::metadata(&p).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&p, perm).unwrap();
    }

    #[test]
    fn execs_delegate_and_relays_stdout() {
        let tmp = tempfile::tempdir().unwrap();
        // Fake delegate: echo a fixed result, and record the stdin it received.
        write_script(
            tmp.path(),
            "bridge",
            "#!/bin/sh\ncat > \"$CNI_PATH/received_stdin\"\necho '{\"cniVersion\":\"0.3.1\",\"ips\":[]}'\n",
        );
        let path = tmp.path().to_str().unwrap();
        let out = run_delegate("bridge", &args(path), r#"{"type":"bridge","x":1}"#).unwrap();
        assert!(out.success);
        let v: serde_json::Value = serde_json::from_str(out.stdout.trim()).unwrap();
        assert_eq!(v["cniVersion"], "0.3.1");
        let received = std::fs::read_to_string(tmp.path().join("received_stdin")).unwrap();
        assert!(received.contains("\"type\":\"bridge\""));
    }

    #[test]
    fn failing_delegate_reports_unsuccessful() {
        let tmp = tempfile::tempdir().unwrap();
        write_script(
            tmp.path(),
            "bridge",
            "#!/bin/sh\necho '{\"code\":7,\"msg\":\"boom\"}'\nexit 1\n",
        );
        let path = tmp.path().to_str().unwrap();
        let out = run_delegate("bridge", &args(path), "{}").unwrap();
        assert!(!out.success);
        assert!(out.stdout.contains("boom"));
    }

    #[test]
    fn missing_delegate_is_error() {
        let tmp = tempfile::tempdir().unwrap();
        let err = run_delegate("nonexistent", &args(tmp.path().to_str().unwrap()), "{}").unwrap_err();
        assert_eq!(err.code, 5);
    }
}
```

- [ ] **Step 2: Declare module**

In `crates/cni-flannel/src/main.rs` add: `mod exec;`

- [ ] **Step 3: Run tests**

Run: `cargo test -p cni-flannel exec::`
Expected: PASS (`execs_delegate_and_relays_stdout`, `failing_delegate_reports_unsuccessful`, `missing_delegate_is_error`).

- [ ] **Step 4: Commit**

```bash
git add crates/cni-flannel/src/exec.rs crates/cni-flannel/src/main.rs
git commit -m "feat(flannel): exec delegate plugin via CNI_PATH"
```

---

## Task 5: main dispatch

**Files:**
- Modify: `crates/cni-flannel/src/main.rs`

- [ ] **Step 1: Replace main.rs**

`crates/cni-flannel/src/main.rs`:
```rust
mod delegate;
mod exec;
mod subnetenv;

use cni::env::CniArgs;
use cni::error::CniError;
use cni::version::VersionResult;
use std::io::Read;
use std::process::ExitCode;

const SUBNET_ENV_PATH: &str = "/run/flannel/subnet.env";

fn read_stdin() -> String {
    let mut s = String::new();
    let _ = std::io::stdin().read_to_string(&mut s);
    s
}

/// Returns (delegate_type, delegate_json).
fn delegate_json(stdin: &str) -> Result<(String, String), CniError> {
    let conf = delegate::FlannelConf::parse(stdin)
        .map_err(|e| CniError::new(6, "failed to decode network config").with_details(e.to_string()))?;
    let env = subnetenv::SubnetEnv::load(SUBNET_ENV_PATH).map_err(|e| {
        CniError::new(11, "failed to read /run/flannel/subnet.env (flanneld not ready?)").with_details(e)
    })?;
    let d = delegate::build_delegate(&conf, &env);
    let dtype = d.get("type").and_then(|t| t.as_str()).unwrap_or("bridge").to_string();
    let json = serde_json::to_string(&d).map_err(|e| CniError::new(6, "encode delegate").with_details(e.to_string()))?;
    Ok((dtype, json))
}

/// Returns (stdout_to_relay, success).
fn run() -> Result<(String, bool), CniError> {
    let args = CniArgs::from_env()?;
    match args.command.as_str() {
        "VERSION" => Ok((VersionResult::supported().to_json(), true)),
        "ADD" | "DEL" | "CHECK" => {
            let stdin = read_stdin();
            let (dtype, djson) = delegate_json(&stdin)?;
            let out = exec::run_delegate(&dtype, &args, &djson)?;
            Ok((out.stdout, out.success))
        }
        other => Err(CniError::new(4, format!("unknown CNI_COMMAND {other}"))),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok((out, success)) => {
            if !out.is_empty() {
                print!("{out}");
            }
            if success {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(e) => {
            print!("{}", e.to_json());
            ExitCode::FAILURE
        }
    }
}
```

- [ ] **Step 2: Build + full crate test**

Run: `cargo build -p cni-flannel && cargo test -p cni-flannel`
Expected: builds; all subnetenv/delegate/exec tests PASS (7 total).

- [ ] **Step 3: Manual smoke (VERSION + fake delegate ADD)**

Run:
```bash
BIN=$(cargo build -p cni-flannel --release 2>/dev/null; find /home/jones/.cache/rusternetes-target ./target -name flannel -type f -path '*release*' 2>/dev/null | head -1)
echo "binary: $BIN"
CNI_COMMAND=VERSION "$BIN"; echo
# Fake delegate + subnet.env to exercise ADD end to end without a cluster:
D=$(mktemp -d); printf '#!/bin/sh\necho "{\\"cniVersion\\":\\"0.3.1\\",\\"ips\\":[]}"\n' > "$D/bridge"; chmod +x "$D/bridge"
SE=$(mktemp); printf 'FLANNEL_NETWORK=10.244.0.0/16\nFLANNEL_SUBNET=10.244.1.0/24\nFLANNEL_MTU=1450\nFLANNEL_IPMASQ=true\n' > "$SE"
# Point the plugin at our fake subnet.env by temporarily symlinking is not possible (const path);
# instead just confirm VERSION works and rely on unit tests for ADD wiring.
```
Expected: VERSION prints `{"cniVersion":"0.3.1","supportedVersions":["0.3.0","0.3.1"]}`. (ADD against the real `/run/flannel/subnet.env` path is covered by the cluster conformance run in Task 7; unit tests cover `delegate_json`'s pieces.)

- [ ] **Step 4: Commit**

```bash
git add crates/cni-flannel/src/main.rs
git commit -m "feat(flannel): VERSION/ADD/DEL/CHECK dispatch"
```

---

## Task 6: Bake into image + replace the Go meta-plugin initContainer

**Files:**
- Modify: `Dockerfile`
- Modify: `deploy/flannel-rs.yaml`

- [ ] **Step 1: Build the flannel binary in the image**

In `Dockerfile`, extend the build to include `cni-flannel` and copy the binary.

Find:
```dockerfile
RUN cargo build --release -p flanneld -p cni-host-local
```
Replace with:
```dockerfile
RUN cargo build --release -p flanneld -p cni-host-local -p cni-flannel
```
After the existing `COPY --from=build /src/target/release/host-local /opt/cni/bin/host-local`, add:
```dockerfile
COPY --from=build /src/target/release/flannel /opt/cni/bin/flannel
```

- [ ] **Step 2: Replace the ghcr meta-plugin initContainer**

In `deploy/flannel-rs.yaml`, replace the `install-cni-plugin` (ghcr) initContainer AND the separate `install-host-local-rs` initContainer with a single one that installs both Rust plugins from our image. Find these two blocks:
```yaml
      - name: install-cni-plugin
        image: ghcr.io/flannel-io/flannel-cni-plugin:v1.9.1-flannel1
        command: ["cp", "-f", "/flannel", "/opt/cni/bin/flannel"]
        volumeMounts:
        - name: cni-plugin
          mountPath: /opt/cni/bin
      # Overwrite the upstream Go host-local with our Rust build (milestone 2a).
      # Mounted at /host/... so it doesn't shadow the image's /opt/cni/bin/host-local.
      - name: install-host-local-rs
        image: flannel-rs:dev
        imagePullPolicy: Never
        command: ["cp", "-f", "/opt/cni/bin/host-local", "/host/opt/cni/bin/host-local"]
        volumeMounts:
        - name: cni-plugin
          mountPath: /host/opt/cni/bin
```
Replace both with:
```yaml
      # Install the Rust CNI plugins (flannel meta-plugin + host-local IPAM) onto
      # each node, overwriting the Go binaries. Mounted at /host/... so it does not
      # shadow the image's own /opt/cni/bin. No upstream Go flannel-cni-plugin image.
      - name: install-cni-plugins-rs
        image: flannel-rs:dev
        imagePullPolicy: Never
        command: ["sh", "-c", "cp -f /opt/cni/bin/flannel /opt/cni/bin/host-local /host/opt/cni/bin/"]
        volumeMounts:
        - name: cni-plugin
          mountPath: /host/opt/cni/bin
```
(Leave the later `install-cni` initContainer and all volumes unchanged. The `cni-plugin` volume is the hostPath `/opt/cni/bin`.)

- [ ] **Step 3: Build the image + verify both plugins present**

Run:
```bash
docker build -t flannel-rs:dev .
docker run --rm --entrypoint ls flannel-rs:dev -l /opt/cni/bin/flannel /opt/cni/bin/host-local
```
Expected: image builds; both `/opt/cni/bin/flannel` and `/opt/cni/bin/host-local` exist.

- [ ] **Step 4: Validate manifest YAML**

Run: `python3 -c "import yaml; list(yaml.safe_load_all(open('deploy/flannel-rs.yaml')))" && echo "YAML OK"`
Expected: `YAML OK`. Confirm `ghcr.io/flannel-io/flannel-cni-plugin` no longer appears: `! grep -q flannel-cni-plugin deploy/flannel-rs.yaml && echo "ghcr removed"`.

- [ ] **Step 5: Commit**

```bash
git add Dockerfile deploy/flannel-rs.yaml
git commit -m "build: bake Rust flannel meta-plugin; drop ghcr flannel-cni-plugin"
```

---

## Task 7: Verify by smoke + conformance (the gate)

**Files:** none.

- [ ] **Step 1: Smoke (flannel-rs)**

Run: `bash tests/smoke/run.sh flannel-rs`
Expected: `SMOKE PASSED: flannel-rs`. Chain is now flannel(Rust) → bridge(Go) → host-local(Rust).

- [ ] **Step 2: Conformance (flannel-rs)**

Run: `bash tests/conformance/run.sh flannel-rs`
Expected: the 47 `[sig-network] [Conformance]` specs pass with the Rust meta-plugin in the chain.

- [ ] **Step 3: Baseline still green**

Run: `bash tests/smoke/run.sh flannel-go`
Expected: `SMOKE PASSED: flannel-go` (still upstream Go meta-plugin).

- [ ] **Step 4: Confirm the Rust meta-plugin is the one in use**

On a scratch cluster (or by inspecting during a run): `docker exec flannel-rs-worker /opt/cni/bin/flannel </dev/null; echo "exit=$?"` with no `CNI_COMMAND` → prints our CNI error JSON (`"code":4,"msg":"CNI_COMMAND missing"`) and exits non-zero, confirming it's the Rust binary (the Go meta-plugin emits a different message).

- [ ] **Step 5: If red, debug**

Use `superpowers:systematic-debugging`. Probes:
```bash
kubectl --context kind-flannel-rs describe pod <pending-pod> | grep -A6 Events
docker exec flannel-rs-worker cat /run/flannel/subnet.env
docker exec flannel-rs-worker ls -l /opt/cni/bin/flannel /opt/cni/bin/host-local /opt/cni/bin/bridge
```
Likely issues: delegate `ipMasq` must be `false` (flanneld masqs) — double-masq would break egress; `routes` dst must be FLANNEL_NETWORK so pods can reach the whole pod CIDR; the result from bridge must be relayed unchanged so `portmap` (next in chain) gets a valid prevResult. Fix Rust code, rebuild image, re-run.

- [ ] **Step 6: Commit any fixes**

```bash
git add -A
git commit -m "fix: <what> so Rust flannel meta-plugin passes smoke + conformance"
```

---

## Self-Review

**Spec coverage:**
- `subnetenv` parse → Task 2. ✓
- `build_delegate` (type/mtu/ipMasq/isGateway/ipam, no-overwrite) → Task 3. ✓
- `exec` delegate via CNI_PATH, relay stdout/exit, not-found error → Task 4. ✓
- VERSION/ADD/DEL/CHECK dispatch; DEL reconstructs from stdin+subnet.env (same code path as ADD); missing subnet.env → code 11 → Task 5. ✓
- Bake + swap one binary; drop ghcr image → Task 6. ✓
- Verify smoke + conformance; flannel-go untouched → Task 7. ✓
- Error handling (code 11 subnet.env, code 5 not-found, relay delegate failure, no panics) → Tasks 4,5. ✓

**Placeholder scan:** Task 5 Step 3 notes ADD wiring is covered by conformance (not a placeholder — the const path makes a no-cluster ADD test impractical; the pieces are unit-tested). No TBD/TODO.

**Type consistency:** `SubnetEnv{network,subnet,mtu,ipmasq}`, `FlannelConf{name,cni_version,delegate}`, `build_delegate(&FlannelConf,&SubnetEnv)->Value`, `run_delegate(name,&CniArgs,&str)->Result<DelegateOutput,CniError>`, `DelegateOutput{stdout,success}`, `delegate_json(&str)->Result<(String,String),CniError>` — consistent across tasks and call sites. Reuses `cni::env::CniArgs`, `cni::error::CniError`, `cni::version::VersionResult` (verified to exist from M2a).

**Known risk:** exact delegate field semantics the Go `bridge` expects (ipMasq false, routes=network, isGateway) — covered by Task 7 conformance; debug notes call out the likely mismatches.
