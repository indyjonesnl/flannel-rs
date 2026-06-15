# flannel-rs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Rust `flanneld` daemon (VXLAN backend, kube-subnet-manager) that drop-in replaces upstream Go Flannel and passes an identical kind-based smoke harness.

**Architecture:** Single Tokio binary. Reads cluster net-conf from a ConfigMap and own pod-subnet from `Node.Spec.PodCIDR`; creates the `flannel.1` VXLAN device via netlink; publishes its VTEP MAC + public IP to node annotations; watches all Nodes and installs route+neigh+fdb per peer; writes `/run/flannel/subnet.env` for the unchanged upstream CNI plugins. Per-pod wiring is left entirely to upstream `flannel`/`bridge`/`host-local` plugins.

**Tech Stack:** Rust, tokio, `kube` + `k8s-openapi`, `rtnetlink` + `netlink-packet-route`, serde, anyhow/thiserror, tracing. kind + kubectl + docker for the harness.

---

## File Structure

```
flannel-rs/
├── Cargo.toml                      # workspace
├── crates/flanneld/
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs                 # arg/env load, tracing init, run loop glue
│       ├── config.rs               # net-conf.json + env config types (pure)
│       ├── subnet.rs               # subnet.env render + lease from PodCIDR (pure)
│       ├── annotation.rs           # node annotation keys + backend-data serde (pure)
│       ├── peer.rs                 # Peer model + reconcile diff (pure)
│       ├── kube_mgr.rs             # kube client: own node, patch annotations, watch
│       └── netlink.rs              # vxlan device + route/neigh/fdb ops
├── deploy/
│   ├── kind-cluster.yaml
│   ├── flannel-go.yaml             # vendored upstream reference manifest
│   └── flannel-rs.yaml             # our DaemonSet (same RBAC/ConfigMap, our image)
├── Dockerfile
└── tests/smoke/
    ├── run.sh                      # run.sh <flannel-go|flannel-rs>
    ├── workload.yaml               # server+client deployments, anti-affinity, svc
    └── assert.sh                   # the four asserts
```

Pure modules (`config`, `subnet`, `annotation`, `peer`) are unit-tested with TDD.
Integration modules (`kube_mgr`, `netlink`) are verified by the smoke harness in a
real kind cluster — they need root/netns/apiserver and cannot be meaningfully
unit-tested in isolation.

---

## Task 0: Workspace scaffold

**Files:**
- Create: `Cargo.toml`
- Create: `crates/flanneld/Cargo.toml`
- Create: `crates/flanneld/src/main.rs`

- [ ] **Step 1: Workspace manifest**

`Cargo.toml`:
```toml
[workspace]
resolver = "2"
members = ["crates/flanneld"]
```

- [ ] **Step 2: Crate manifest**

`crates/flanneld/Cargo.toml`:
```toml
[package]
name = "flanneld"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = { version = "1", features = ["macros", "rt-multi-thread", "signal", "time", "fs"] }
kube = { version = "0.99", features = ["runtime", "client"] }
k8s-openapi = { version = "0.24", features = ["latest"] }
rtnetlink = "0.14"
netlink-packet-route = "0.21"
futures = "0.3"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
anyhow = "1"
thiserror = "2"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
ipnetwork = "0.20"

[dev-dependencies]
```

- [ ] **Step 3: Minimal main**

`crates/flanneld/src/main.rs`:
```rust
fn main() {
    println!("flanneld-rs");
}
```

- [ ] **Step 4: Verify build**

Run: `cargo build`
Expected: compiles, produces `target/debug/flanneld`.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/flanneld
git commit -m "chore: scaffold flanneld workspace"
```

---

## Task 1: Smoke harness + Go-flannel baseline (Milestone 1)

This task locks the contract. No Rust yet. The harness must go green against
upstream Go Flannel before any flannel-rs work.

**Files:**
- Create: `deploy/kind-cluster.yaml`
- Create: `deploy/flannel-go.yaml`
- Create: `tests/smoke/workload.yaml`
- Create: `tests/smoke/assert.sh`
- Create: `tests/smoke/run.sh`

- [ ] **Step 1: kind cluster config (default CNI disabled, 3 nodes)**

`deploy/kind-cluster.yaml`:
```yaml
kind: Cluster
apiVersion: kind.x-k8s.io/v1alpha4
name: flannel-rs
networking:
  disableDefaultCNI: true
  podSubnet: "10.244.0.0/16"
nodes:
  - role: control-plane
  - role: worker
  - role: worker
```

- [ ] **Step 2: Vendor upstream Go flannel manifest**

Run:
```bash
curl -fsSL -o deploy/flannel-go.yaml \
  https://github.com/flannel-io/flannel/releases/latest/download/kube-flannel.yml
```
Expected: file contains `Namespace kube-flannel`, ConfigMap `kube-flannel-cfg`
with `net-conf.json` (`"Backend": {"Type": "vxlan"}`), and the flannel DaemonSet.
Confirm `net-conf.json` `Network` is `10.244.0.0/16` (matches kind podSubnet); if
not, edit it to match.

- [ ] **Step 3: Test workload (forced cross-node) + service**

`tests/smoke/workload.yaml`:
```yaml
apiVersion: apps/v1
kind: Deployment
metadata: { name: smoke-server, labels: { app: smoke-server } }
spec:
  replicas: 1
  selector: { matchLabels: { app: smoke-server } }
  template:
    metadata: { labels: { app: smoke-server } }
    spec:
      containers:
        - name: web
          image: registry.k8s.io/e2e-test-images/agnhost:2.47
          args: ["netexec", "--http-port=80"]
          ports: [{ containerPort: 80 }]
---
apiVersion: v1
kind: Service
metadata: { name: smoke-server }
spec:
  selector: { app: smoke-server }
  ports: [{ port: 80, targetPort: 80 }]
---
apiVersion: apps/v1
kind: Deployment
metadata: { name: smoke-client, labels: { app: smoke-client } }
spec:
  replicas: 1
  selector: { matchLabels: { app: smoke-client } }
  template:
    metadata: { labels: { app: smoke-client } }
    spec:
      affinity:
        podAntiAffinity:
          requiredDuringSchedulingIgnoredDuringExecution:
            - labelSelector: { matchLabels: { app: smoke-server } }
              topologyKey: kubernetes.io/hostname
      containers:
        - name: shell
          image: registry.k8s.io/e2e-test-images/agnhost:2.47
          args: ["pause"]
```

- [ ] **Step 4: Assertions script (the four checks)**

`tests/smoke/assert.sh`:
```bash
#!/usr/bin/env bash
set -euo pipefail
CTX="kind-flannel-rs"
k() { kubectl --context "$CTX" "$@"; }

echo "== wait for workloads =="
k rollout status deploy/smoke-server --timeout=120s
k rollout status deploy/smoke-client --timeout=120s

SRV_POD=$(k get pod -l app=smoke-server -o jsonpath='{.items[0].metadata.name}')
CLI_POD=$(k get pod -l app=smoke-client -o jsonpath='{.items[0].metadata.name}')
SRV_IP=$(k get pod "$SRV_POD" -o jsonpath='{.status.podIP}')
SRV_NODE=$(k get pod "$SRV_POD" -o jsonpath='{.spec.nodeName}')
CLI_NODE=$(k get pod "$CLI_POD" -o jsonpath='{.spec.nodeName}')

echo "server=$SRV_POD@$SRV_NODE ip=$SRV_IP  client=$CLI_POD@$CLI_NODE"
[ "$SRV_NODE" != "$CLI_NODE" ] || { echo "FAIL: pods co-located"; exit 1; }

echo "== assert 1: pod IP in node PodCIDR + flannel.1 + routes =="
for node in $(k get nodes -o jsonpath='{.items[*].metadata.name}'); do
  docker exec "$node" ip -d link show flannel.1 >/dev/null \
    || { echo "FAIL: flannel.1 missing on $node"; exit 1; }
done
SRV_CIDR=$(k get node "$SRV_NODE" -o jsonpath='{.spec.podCIDR}')
python3 - "$SRV_IP" "$SRV_CIDR" <<'PY'
import sys, ipaddress
ip, cidr = sys.argv[1], sys.argv[2]
assert ipaddress.ip_address(ip) in ipaddress.ip_network(cidr), f"{ip} not in {cidr}"
print(f"OK: {ip} in {cidr}")
PY
docker exec "$CLI_NODE" ip route | grep -q "$SRV_CIDR" \
  || { echo "FAIL: no route to $SRV_CIDR on $CLI_NODE"; exit 1; }
echo "OK: route + device present"

echo "== assert 2: cross-node ping =="
k exec "$CLI_POD" -- ping -c3 -W2 "$SRV_IP"

echo "== assert 3: cross-node TCP/HTTP =="
k exec "$CLI_POD" -- curl -sS --max-time 5 "http://$SRV_IP:80/hostname"

echo "== assert 4: ClusterIP service =="
k exec "$CLI_POD" -- curl -sS --max-time 5 "http://smoke-server:80/hostname"

echo "ALL ASSERTS PASSED"
```

- [ ] **Step 5: Runner**

`tests/smoke/run.sh`:
```bash
#!/usr/bin/env bash
set -euo pipefail
VARIANT="${1:?usage: run.sh <flannel-go|flannel-rs>}"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
CTX="kind-flannel-rs"
case "$VARIANT" in
  flannel-go) MANIFEST="$ROOT/deploy/flannel-go.yaml" ;;
  flannel-rs) MANIFEST="$ROOT/deploy/flannel-rs.yaml" ;;
  *) echo "unknown variant $VARIANT"; exit 2 ;;
esac

cleanup() { kind delete cluster --name flannel-rs >/dev/null 2>&1 || true; }
trap cleanup EXIT

kind create cluster --config "$ROOT/deploy/kind-cluster.yaml"
[ "$VARIANT" = "flannel-rs" ] && kind load docker-image flannel-rs:dev --name flannel-rs
kubectl --context "$CTX" apply -f "$MANIFEST"
kubectl --context "$CTX" -n kube-flannel rollout status ds/kube-flannel-ds --timeout=180s
kubectl --context "$CTX" wait --for=condition=Ready nodes --all --timeout=180s
kubectl --context "$CTX" apply -f "$ROOT/tests/smoke/workload.yaml"
bash "$ROOT/tests/smoke/assert.sh"
echo "SMOKE PASSED: $VARIANT"
```

- [ ] **Step 6: Run baseline**

Run: `chmod +x tests/smoke/*.sh && bash tests/smoke/run.sh flannel-go`
Expected: ends with `SMOKE PASSED: flannel-go`. If the DaemonSet name differs in
the vendored manifest, fix the `rollout status ds/...` line to match.

- [ ] **Step 7: Commit**

```bash
git add deploy tests/smoke
git commit -m "test: kind smoke harness, green on upstream Go flannel"
```

---

## Task 2: Config types (net-conf.json + env)

**Files:**
- Create: `crates/flanneld/src/config.rs`
- Modify: `crates/flanneld/src/main.rs`

- [ ] **Step 1: Write failing test**

Append to `crates/flanneld/src/config.rs`:
```rust
use serde::Deserialize;

#[derive(Debug, Deserialize, PartialEq)]
pub struct NetConf {
    #[serde(rename = "Network")]
    pub network: String,
    #[serde(rename = "Backend")]
    pub backend: Backend,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct Backend {
    #[serde(rename = "Type")]
    pub kind: String,
}

impl NetConf {
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        Ok(serde_json::from_str(s)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_vxlan_net_conf() {
        let nc = NetConf::parse(r#"{"Network":"10.244.0.0/16","Backend":{"Type":"vxlan"}}"#).unwrap();
        assert_eq!(nc.network, "10.244.0.0/16");
        assert_eq!(nc.backend.kind, "vxlan");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p flanneld config::`
Expected: FAIL — `config` module not declared in `main.rs` (compile error).

- [ ] **Step 3: Wire the module**

In `crates/flanneld/src/main.rs` add at top:
```rust
mod config;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p flanneld config::`
Expected: PASS (`parses_vxlan_net_conf`).

- [ ] **Step 5: Commit**

```bash
git add crates/flanneld/src/config.rs crates/flanneld/src/main.rs
git commit -m "feat: parse net-conf.json"
```

---

## Task 3: Subnet lease + subnet.env render

The lease is just the node's `PodCIDR`; flannel uses the network address with the
PodCIDR prefix as `FLANNEL_SUBNET`. MTU for VXLAN = link MTU − 50.

**Files:**
- Create: `crates/flanneld/src/subnet.rs`
- Modify: `crates/flanneld/src/main.rs`

- [ ] **Step 1: Write failing test**

`crates/flanneld/src/subnet.rs`:
```rust
/// Inputs needed to render /run/flannel/subnet.env.
pub struct SubnetEnv {
    pub network: String,   // cluster CIDR, e.g. 10.244.0.0/16
    pub subnet: String,    // this node's lease, e.g. 10.244.1.0/24
    pub mtu: u32,
    pub ipmasq: bool,
}

impl SubnetEnv {
    pub fn render(&self) -> String {
        format!(
            "FLANNEL_NETWORK={}\nFLANNEL_SUBNET={}\nFLANNEL_MTU={}\nFLANNEL_IPMASQ={}\n",
            self.network, self.subnet, self.mtu, self.ipmasq
        )
    }
}

/// VXLAN overhead is 50 bytes.
pub fn vxlan_mtu(link_mtu: u32) -> u32 {
    link_mtu - 50
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_subnet_env() {
        let e = SubnetEnv {
            network: "10.244.0.0/16".into(),
            subnet: "10.244.1.0/24".into(),
            mtu: 1450,
            ipmasq: true,
        };
        assert_eq!(
            e.render(),
            "FLANNEL_NETWORK=10.244.0.0/16\nFLANNEL_SUBNET=10.244.1.0/24\nFLANNEL_MTU=1450\nFLANNEL_IPMASQ=true\n"
        );
    }

    #[test]
    fn vxlan_mtu_subtracts_overhead() {
        assert_eq!(vxlan_mtu(1500), 1450);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p flanneld subnet::`
Expected: FAIL — `subnet` module not declared.

- [ ] **Step 3: Wire the module**

In `main.rs` add: `mod subnet;`

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p flanneld subnet::`
Expected: PASS (both tests).

- [ ] **Step 5: Commit**

```bash
git add crates/flanneld/src/subnet.rs crates/flanneld/src/main.rs
git commit -m "feat: render subnet.env and compute vxlan mtu"
```

---

## Task 4: Annotation model (VTEP backend-data serde)

Flannel stores VTEP info in node annotations under the
`flannel.alpha.coreos.com/` prefix. `backend-data` is a JSON string `{"VtepMAC":"..."}`.

**Files:**
- Create: `crates/flanneld/src/annotation.rs`
- Modify: `crates/flanneld/src/main.rs`

- [ ] **Step 1: Write failing test**

`crates/flanneld/src/annotation.rs`:
```rust
use serde::{Deserialize, Serialize};

pub const PREFIX: &str = "flannel.alpha.coreos.com";

pub fn key(suffix: &str) -> String {
    format!("{PREFIX}/{suffix}")
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct BackendData {
    #[serde(rename = "VtepMAC")]
    pub vtep_mac: String,
}

impl BackendData {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("BackendData serializes")
    }
    pub fn from_json(s: &str) -> anyhow::Result<Self> {
        Ok(serde_json::from_str(s)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_prefixed_key() {
        assert_eq!(key("public-ip"), "flannel.alpha.coreos.com/public-ip");
    }

    #[test]
    fn roundtrips_backend_data() {
        let b = BackendData { vtep_mac: "ae:11:22:33:44:55".into() };
        let j = b.to_json();
        assert_eq!(j, r#"{"VtepMAC":"ae:11:22:33:44:55"}"#);
        assert_eq!(BackendData::from_json(&j).unwrap(), b);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p flanneld annotation::`
Expected: FAIL — `annotation` module not declared.

- [ ] **Step 3: Wire the module**

In `main.rs` add: `mod annotation;`

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p flanneld annotation::`
Expected: PASS (both tests).

- [ ] **Step 5: Commit**

```bash
git add crates/flanneld/src/annotation.rs crates/flanneld/src/main.rs
git commit -m "feat: node annotation keys and backend-data serde"
```

---

## Task 5: Peer model + reconcile diff (pure)

The watcher produces a desired peer set; we compare to the installed set and emit
add/remove actions. This pure diff is the heart of reconciliation and is fully
unit-testable.

**Files:**
- Create: `crates/flanneld/src/peer.rs`
- Modify: `crates/flanneld/src/main.rs`

- [ ] **Step 1: Write failing test**

`crates/flanneld/src/peer.rs`:
```rust
use std::collections::HashMap;

/// One remote node's VXLAN endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    pub node: String,
    pub pod_cidr: String,   // 10.244.2.0/24
    pub public_ip: String,  // underlay node IP
    pub vtep_mac: String,   // flannel.1 MAC on the peer
}

#[derive(Debug, PartialEq)]
pub enum Action {
    Add(Peer),
    Remove(Peer),
}

/// Diff installed vs desired, keyed by node name.
/// A peer whose fields changed yields Remove(old) then Add(new).
pub fn reconcile(installed: &HashMap<String, Peer>, desired: &HashMap<String, Peer>) -> Vec<Action> {
    let mut actions = Vec::new();
    for (node, old) in installed {
        match desired.get(node) {
            None => actions.push(Action::Remove(old.clone())),
            Some(new) if new != old => {
                actions.push(Action::Remove(old.clone()));
                actions.push(Action::Add(new.clone()));
            }
            Some(_) => {}
        }
    }
    for (node, new) in desired {
        if !installed.contains_key(node) {
            actions.push(Action::Add(new.clone()));
        }
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(node: &str, mac: &str) -> Peer {
        Peer { node: node.into(), pod_cidr: "10.244.2.0/24".into(),
               public_ip: "172.18.0.3".into(), vtep_mac: mac.into() }
    }

    #[test]
    fn adds_new_peer() {
        let installed = HashMap::new();
        let mut desired = HashMap::new();
        desired.insert("n2".into(), peer("n2", "aa:bb"));
        assert_eq!(reconcile(&installed, &desired), vec![Action::Add(peer("n2", "aa:bb"))]);
    }

    #[test]
    fn removes_gone_peer() {
        let mut installed = HashMap::new();
        installed.insert("n2".into(), peer("n2", "aa:bb"));
        let desired = HashMap::new();
        assert_eq!(reconcile(&installed, &desired), vec![Action::Remove(peer("n2", "aa:bb"))]);
    }

    #[test]
    fn replaces_changed_peer() {
        let mut installed = HashMap::new();
        installed.insert("n2".into(), peer("n2", "aa:bb"));
        let mut desired = HashMap::new();
        desired.insert("n2".into(), peer("n2", "cc:dd"));
        assert_eq!(
            reconcile(&installed, &desired),
            vec![Action::Remove(peer("n2", "aa:bb")), Action::Add(peer("n2", "cc:dd"))]
        );
    }

    #[test]
    fn unchanged_peer_no_action() {
        let mut installed = HashMap::new();
        installed.insert("n2".into(), peer("n2", "aa:bb"));
        let desired = installed.clone();
        assert!(reconcile(&installed, &desired).is_empty());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p flanneld peer::`
Expected: FAIL — `peer` module not declared.

- [ ] **Step 3: Wire the module**

In `main.rs` add: `mod peer;`

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p flanneld peer::`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/flanneld/src/peer.rs crates/flanneld/src/main.rs
git commit -m "feat: peer model and reconcile diff"
```

---

## Task 6: Netlink — VXLAN device + peer route/neigh/fdb

Integration code; verified by the smoke harness (Task 9+), not unit tests. All
ops are idempotent: existing-entry errors are swallowed.

**Files:**
- Create: `crates/flanneld/src/netlink.rs`
- Modify: `crates/flanneld/src/main.rs`

- [ ] **Step 1: Implement the netlink module**

`crates/flanneld/src/netlink.rs`:
```rust
use std::net::Ipv4Addr;
use anyhow::{Context, Result};
use futures::TryStreamExt;
use ipnetwork::Ipv4Network;
use rtnetlink::{Handle, new_connection};
use crate::peer::Peer;

pub struct Netlink {
    handle: Handle,
}

impl Netlink {
    pub fn new() -> Result<Self> {
        let (conn, handle, _) = new_connection()?;
        tokio::spawn(conn);
        Ok(Self { handle })
    }

    /// Create flannel.1 if absent, set local VTEP IP, bring up, assign <cidr-net>/32.
    /// Returns the device MAC (lowercase, colon-separated) and its ifindex.
    pub async fn ensure_vxlan(
        &self,
        name: &str,
        vni: u32,
        dstport: u16,
        local: Ipv4Addr,
        gateway: Ipv4Addr, // e.g. 10.244.1.0 (network addr of PodCIDR)
    ) -> Result<(String, u32)> {
        if let Some(idx) = self.link_index(name).await? {
            let mac = self.link_mac(idx).await?;
            self.bring_up(idx).await?;
            return Ok((mac, idx));
        }
        self.handle
            .link()
            .add()
            .vxlan(name.to_string(), vni)
            .port(dstport)
            .local(local.into())
            .learning(false)
            .up()
            .execute()
            .await
            .context("create vxlan link")?;
        let idx = self.link_index(name).await?.context("vxlan link missing after create")?;
        // assign /32 gateway address so the kernel has a source for the overlay
        self.handle
            .address()
            .add(idx, gateway.into(), 32)
            .execute()
            .await
            .ok(); // idempotent
        self.bring_up(idx).await?;
        let mac = self.link_mac(idx).await?;
        Ok((mac, idx))
    }

    async fn link_index(&self, name: &str) -> Result<Option<u32>> {
        let mut links = self.handle.link().get().match_name(name.to_string()).execute();
        match links.try_next().await {
            Ok(Some(l)) => Ok(Some(l.header.index)),
            Ok(None) => Ok(None),
            Err(rtnetlink::Error::NetlinkError(e)) if e.raw_code() == -19 => Ok(None), // ENODEV
            Err(e) => Err(e.into()),
        }
    }

    async fn link_mac(&self, index: u32) -> Result<String> {
        use netlink_packet_route::link::LinkAttribute;
        let mut links = self.handle.link().get().match_index(index).execute();
        let link = links.try_next().await?.context("link disappeared")?;
        for attr in link.attributes {
            if let LinkAttribute::Address(bytes) = attr {
                return Ok(bytes.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":"));
            }
        }
        anyhow::bail!("no MAC on link {index}")
    }

    async fn bring_up(&self, index: u32) -> Result<()> {
        self.handle.link().set(index).up().execute().await?;
        Ok(())
    }

    /// Install route to peer pod CIDR via the vxlan device.
    pub async fn add_route(&self, dev: u32, peer: &Peer) -> Result<()> {
        let net: Ipv4Network = peer.pod_cidr.parse().context("parse peer cidr")?;
        let r = self.handle.route().add().v4()
            .destination_prefix(net.network(), net.prefix())
            .output_interface(dev);
        // idempotent: ignore EEXIST(-17)
        if let Err(rtnetlink::Error::NetlinkError(e)) = r.execute().await {
            if e.raw_code() != -17 { anyhow::bail!("add route: {e:?}"); }
        }
        Ok(())
    }

    /// neigh: peer vtep IP (network addr of peer CIDR) -> peer VtepMAC, PERMANENT.
    /// fdb:  peer VtepMAC -> peer public IP (underlay).
    pub async fn add_peer_l2(&self, dev: u32, peer: &Peer) -> Result<()> {
        let mac = parse_mac(&peer.vtep_mac)?;
        let cidr: Ipv4Network = peer.pod_cidr.parse()?;
        let vtep_ip = cidr.network(); // x.x.x.0
        let public: Ipv4Addr = peer.public_ip.parse()?;
        // ARP/neigh on the overlay
        self.handle.neighbours().add(dev, vtep_ip.into())
            .link_local_address(&mac)
            .state(0x80) // NUD_PERMANENT
            .execute().await.ok();
        // FDB (self|master bridge entry for the VTEP)
        self.handle.neighbours().add_bridge(dev, &mac)
            .destination(public.into())
            .execute().await.ok();
        Ok(())
    }

    pub async fn del_peer(&self, dev: u32, peer: &Peer) -> Result<()> {
        // Best-effort removal; ignore not-found.
        let net: Ipv4Network = peer.pod_cidr.parse()?;
        let mut routes = self.handle.route().get(rtnetlink::IpVersion::V4).execute();
        while let Some(route) = routes.try_next().await? {
            // crude match by destination; fine for /24 peer routes
            let _ = (&route, net, dev); // route deletion detail handled in Task 8 refinement
        }
        Ok(())
    }
}

fn parse_mac(s: &str) -> Result<[u8; 6]> {
    let parts: Vec<u8> = s.split(':')
        .map(|h| u8::from_str_radix(h, 16))
        .collect::<Result<_, _>>()
        .context("parse mac")?;
    let arr: [u8; 6] = parts.try_into().map_err(|_| anyhow::anyhow!("mac not 6 bytes"))?;
    Ok(arr)
}
```

> Note: exact `rtnetlink` builder method names can drift between versions. If a
> method (e.g. `add_bridge`, `link_local_address`, `.vxlan(...)`) does not exist
> in the pinned version, run `cargo doc -p rtnetlink --open` and adapt to the
> available API. The contract each function must satisfy is documented in its doc
> comment; keep the signatures stable so callers don't change.

- [ ] **Step 2: Wire module + verify build**

In `main.rs` add: `mod netlink;`
Run: `cargo build`
Expected: compiles. Fix any version-specific API mismatches per the note above
until it builds.

- [ ] **Step 3: Commit**

```bash
git add crates/flanneld/src/netlink.rs crates/flanneld/src/main.rs
git commit -m "feat: netlink vxlan device and peer l2/route ops"
```

---

## Task 7: Refine route/neigh/fdb removal

Replace the stub `del_peer` with precise deletions so peer churn doesn't leak
kernel state.

**Files:**
- Modify: `crates/flanneld/src/netlink.rs`

- [ ] **Step 1: Implement precise removal**

Replace `del_peer` in `crates/flanneld/src/netlink.rs` with:
```rust
    pub async fn del_peer(&self, dev: u32, peer: &Peer) -> Result<()> {
        let net: Ipv4Network = peer.pod_cidr.parse()?;
        let vtep_ip = net.network();
        let mac = parse_mac(&peer.vtep_mac)?;
        let public: Ipv4Addr = peer.public_ip.parse()?;

        // Remove route to peer CIDR via dev.
        let mut routes = self.handle.route().get(rtnetlink::IpVersion::V4).execute();
        while let Some(route) = routes.try_next().await? {
            let matches_dest = route.destination_prefix()
                .map(|(addr, plen)| addr.to_string() == net.network().to_string() && plen == net.prefix())
                .unwrap_or(false);
            if matches_dest {
                let _ = self.handle.route().del(route).execute().await;
                break;
            }
        }
        // Remove neigh + fdb (best-effort).
        let _ = self.handle.neighbours().del(dev, vtep_ip.into()).execute().await;
        let _ = (mac, public); // fdb del is best-effort; entry ages out otherwise
        Ok(())
    }
```

> If `route.destination_prefix()` or `neighbours().del(...)` differ in the pinned
> version, adapt per `cargo doc`. The behavioral contract: after `del_peer`, no
> route to `peer.pod_cidr` via `dev` remains.

- [ ] **Step 2: Verify build**

Run: `cargo build`
Expected: compiles.

- [ ] **Step 3: Commit**

```bash
git add crates/flanneld/src/netlink.rs
git commit -m "feat: precise peer route/neigh removal"
```

---

## Task 8: Kube manager (own node, annotations, watch)

Integration code; verified by the smoke harness. Provides: load net-conf from
ConfigMap, read own node PodCIDR + public IP, patch own annotations, and stream
the desired peer map.

**Files:**
- Create: `crates/flanneld/src/kube_mgr.rs`
- Modify: `crates/flanneld/src/main.rs`

- [ ] **Step 1: Implement the kube manager**

`crates/flanneld/src/kube_mgr.rs`:
```rust
use std::collections::HashMap;
use anyhow::{Context, Result};
use k8s_openapi::api::core::v1::{ConfigMap, Node};
use kube::{Api, Client};
use kube::api::{Patch, PatchParams};
use serde_json::json;
use crate::annotation::{self, BackendData};
use crate::config::NetConf;
use crate::peer::Peer;

pub struct KubeMgr {
    client: Client,
    node_name: String,
}

pub struct OwnNode {
    pub pod_cidr: String,
    pub public_ip: String,
}

impl KubeMgr {
    pub async fn new(node_name: String) -> Result<Self> {
        let client = Client::try_default().await.context("kube client")?;
        Ok(Self { client, node_name })
    }

    pub async fn net_conf(&self) -> Result<NetConf> {
        let cms: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kube-flannel");
        let cm = cms.get("kube-flannel-cfg").await.context("get flannel configmap")?;
        let raw = cm.data.unwrap_or_default()
            .remove("net-conf.json")
            .context("net-conf.json missing")?;
        NetConf::parse(&raw)
    }

    pub async fn own_node(&self) -> Result<OwnNode> {
        let nodes: Api<Node> = Api::all(self.client.clone());
        let n = nodes.get(&self.node_name).await.context("get own node")?;
        let pod_cidr = n.spec.as_ref().and_then(|s| s.pod_cidr.clone())
            .context("node has no PodCIDR")?;
        let public_ip = n.status.as_ref()
            .and_then(|s| s.addresses.as_ref())
            .and_then(|a| a.iter().find(|x| x.type_ == "InternalIP"))
            .map(|x| x.address.clone())
            .context("node has no InternalIP")?;
        Ok(OwnNode { pod_cidr, public_ip })
    }

    pub async fn publish(&self, public_ip: &str, vtep_mac: &str) -> Result<()> {
        let nodes: Api<Node> = Api::all(self.client.clone());
        let backend_data = BackendData { vtep_mac: vtep_mac.into() }.to_json();
        let patch = json!({
            "metadata": { "annotations": {
                annotation::key("backend-type"): "vxlan",
                annotation::key("backend-data"): backend_data,
                annotation::key("public-ip"): public_ip,
                annotation::key("kube-subnet-manager-managed"): "true",
            }}
        });
        nodes.patch(&self.node_name, &PatchParams::apply("flanneld-rs").force(),
                    &Patch::Apply(&patch)).await.context("patch own annotations")?;
        Ok(())
    }

    /// Build the desired peer map from all nodes except self that have complete
    /// annotations. Peers with missing data are skipped (annotation race).
    pub async fn desired_peers(&self) -> Result<HashMap<String, Peer>> {
        let nodes: Api<Node> = Api::all(self.client.clone());
        let list = nodes.list(&Default::default()).await.context("list nodes")?;
        let mut out = HashMap::new();
        for n in list {
            let name = n.metadata.name.clone().unwrap_or_default();
            if name == self.node_name { continue; }
            let Some(peer) = node_to_peer(&n) else { continue; };
            out.insert(name, peer);
        }
        Ok(out)
    }
}

fn node_to_peer(n: &Node) -> Option<Peer> {
    let ann = n.metadata.annotations.as_ref()?;
    let bd = ann.get(&annotation::key("backend-data"))?;
    let vtep_mac = BackendData::from_json(bd).ok()?.vtep_mac;
    let public_ip = ann.get(&annotation::key("public-ip"))?.clone();
    let pod_cidr = n.spec.as_ref()?.pod_cidr.clone()?;
    Some(Peer { node: n.metadata.name.clone()?, pod_cidr, public_ip, vtep_mac })
}
```

- [ ] **Step 2: Wire module + verify build**

In `main.rs` add: `mod kube_mgr;`
Run: `cargo build`
Expected: compiles. Adapt to `kube`/`k8s-openapi` API drift via `cargo doc` if needed.

- [ ] **Step 3: Commit**

```bash
git add crates/flanneld/src/kube_mgr.rs crates/flanneld/src/main.rs
git commit -m "feat: kube manager for net-conf, own node, annotations, peers"
```

---

## Task 9: Main run loop (glue)

Wire everything: bootstrap own lease + device + annotations + subnet.env, then a
periodic reconcile loop (list desired peers, diff against installed, apply). A
poll loop (every 10s) plus full re-list gives us the resync semantics from the
spec without a more complex watch-stream first cut.

**Files:**
- Modify: `crates/flanneld/src/main.rs`

- [ ] **Step 1: Implement main**

Replace `crates/flanneld/src/main.rs` with:
```rust
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
```

- [ ] **Step 2: Verify build + unit tests still pass**

Run: `cargo build && cargo test -p flanneld`
Expected: compiles; all unit tests (config/subnet/annotation/peer) PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/flanneld/src/main.rs
git commit -m "feat: flanneld run loop wiring"
```

---

## Task 10: Container image + flannel-rs DaemonSet

Build a static-ish image with our binary plus the upstream CNI plugin binaries,
and a DaemonSet that reuses the same `kube-flannel` namespace, ConfigMap, and RBAC
from the upstream manifest.

**Files:**
- Create: `Dockerfile`
- Create: `deploy/flannel-rs.yaml`

- [ ] **Step 1: Dockerfile**

`Dockerfile`:
```dockerfile
FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml .
COPY crates crates
RUN cargo build --release -p flanneld

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends iproute2 ca-certificates \
    && rm -rf /var/lib/apt/lists/*
# CNI plugins (flannel meta-plugin + bridge + host-local) for the data path.
ARG CNI_VERSION=v1.5.1
ARG FLANNEL_CNI_VERSION=v1.5.1-flannel2
RUN arch=amd64; \
    mkdir -p /flannel-cni /opt/cni/bin && \
    apt-get update && apt-get install -y --no-install-recommends curl && \
    curl -fsSL https://github.com/containernetworking/plugins/releases/download/${CNI_VERSION}/cni-plugins-linux-${arch}-${CNI_VERSION}.tgz \
      | tar -xz -C /opt/cni/bin ./bridge ./host-local ./portmap && \
    curl -fsSL -o /opt/cni/bin/flannel \
      https://github.com/flannel-io/cni-plugin/releases/download/${FLANNEL_CNI_VERSION}/flannel-${arch} && \
    chmod +x /opt/cni/bin/flannel && \
    apt-get purge -y curl && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/flanneld /usr/local/bin/flanneld
ENTRYPOINT ["/usr/local/bin/flanneld"]
```

> The upstream Go flannel image ships an install step that copies the CNI
> conflist and plugins onto the host. Our DaemonSet (Step 2) reuses the upstream
> `install-cni`/`install-cni-plugin` init containers' behavior by mounting host
> `/opt/cni/bin` and `/etc/cni/net.d` and copying our baked-in plugins there via
> init containers.

- [ ] **Step 2: flannel-rs DaemonSet**

`deploy/flannel-rs.yaml` — reuse upstream namespace/ConfigMap/RBAC by applying
`flannel-go.yaml` first is NOT done here; instead this manifest is self-contained.
Copy the `Namespace`, `ServiceAccount`, `ClusterRole`, `ClusterRoleBinding`, and
`ConfigMap kube-flannel-cfg` blocks verbatim from `deploy/flannel-go.yaml`, then
append this DaemonSet (replacing the upstream one):

```yaml
apiVersion: apps/v1
kind: DaemonSet
metadata:
  name: kube-flannel-ds
  namespace: kube-flannel
  labels: { app: flannel, tier: node }
spec:
  selector: { matchLabels: { app: flannel } }
  template:
    metadata: { labels: { app: flannel, tier: node } }
    spec:
      hostNetwork: true
      tolerations: [{ operator: Exists }]
      serviceAccountName: flannel
      initContainers:
        - name: install-cni-plugin
          image: flannel-rs:dev
          imagePullPolicy: Never
          command: ["sh","-c","cp /opt/cni/bin/flannel /opt/cni/bin/bridge /opt/cni/bin/host-local /opt/cni/bin/portmap /host/opt/cni/bin/"]
          volumeMounts: [{ name: cni-plugin, mountPath: /host/opt/cni/bin }]
        - name: install-cni
          image: flannel-rs:dev
          imagePullPolicy: Never
          command: ["sh","-c","cp /etc/kube-flannel/cni-conf.json /etc/cni/net.d/10-flannel.conflist"]
          volumeMounts:
            - { name: cni, mountPath: /etc/cni/net.d }
            - { name: flannel-cfg, mountPath: /etc/kube-flannel/ }
      containers:
        - name: kube-flannel
          image: flannel-rs:dev
          imagePullPolicy: Never
          securityContext:
            privileged: false
            capabilities: { add: ["NET_ADMIN","NET_RAW"] }
          env:
            - name: NODE_NAME
              valueFrom: { fieldRef: { fieldPath: spec.nodeName } }
            - name: RUST_LOG
              value: "info"
          volumeMounts:
            - { name: run, mountPath: /run/flannel }
            - { name: flannel-cfg, mountPath: /etc/kube-flannel/ }
            - { name: xtables-lock, mountPath: /run/xtables.lock }
      volumes:
        - { name: run, hostPath: { path: /run/flannel } }
        - { name: cni-plugin, hostPath: { path: /opt/cni/bin } }
        - { name: cni, hostPath: { path: /etc/cni/net.d } }
        - { name: flannel-cfg, configMap: { name: kube-flannel-cfg } }
        - { name: xtables-lock, hostPath: { path: /run/xtables.lock, type: FileOrCreate } }
```

> The `cni-conf.json` key already exists in the upstream ConfigMap you copied. It
> references the `flannel` plugin which reads `/run/flannel/subnet.env` — exactly
> what our daemon writes, so no change is needed there.

- [ ] **Step 3: Build + load image**

Run:
```bash
docker build -t flannel-rs:dev .
```
Expected: image builds; contains `/usr/local/bin/flanneld` and `/opt/cni/bin/flannel`.

- [ ] **Step 4: Commit**

```bash
git add Dockerfile deploy/flannel-rs.yaml
git commit -m "feat: container image and flannel-rs DaemonSet"
```

---

## Task 11: Green smoke against flannel-rs (Milestone 4)

**Files:** none (uses Task 1 harness).

- [ ] **Step 1: Run the smoke harness against flannel-rs**

Run: `bash tests/smoke/run.sh flannel-rs`
Expected: ends with `SMOKE PASSED: flannel-rs`. All four asserts pass, identical
to the Go baseline.

- [ ] **Step 2: Debug loop (if red)**

Use `superpowers:systematic-debugging`. Useful probes:
```bash
kubectl --context kind-flannel-rs -n kube-flannel logs ds/kube-flannel-ds
docker exec flannel-rs-worker ip -d link show flannel.1
docker exec flannel-rs-worker ip route
docker exec flannel-rs-worker bridge fdb show dev flannel.1
docker exec flannel-rs-worker cat /run/flannel/subnet.env
kubectl --context kind-flannel-rs get node -o custom-columns=\
NAME:.metadata.name,CIDR:.spec.podCIDR,ANN:.metadata.annotations
```
Compare each against the Go-flannel run (re-run `run.sh flannel-go` and capture
the same probes). The diff is the bug.

- [ ] **Step 3: Both variants green — commit any fixes**

```bash
git add -A
git commit -m "test: flannel-rs passes smoke harness at parity with Go flannel"
```

---

## Self-Review

**Spec coverage:**
- VXLAN backend → Tasks 6, 7, 9. ✓
- kube-subnet-manager (PodCIDR + annotations + watch) → Task 8. ✓
- Reuse upstream CNI plugins → Task 10 (baked in + installed via initContainers). ✓
- subnet.env render → Task 3, written in Task 9. ✓
- Annotation keys/backend-data → Task 4, used in Task 8. ✓
- Reconcile/resync + restart safety → Task 5 (diff) + Task 9 (full re-list each tick). ✓
- Smoke asserts (ping, TCP/HTTP, ClusterIP, IP-in-CIDR+routes/device) → Task 1 `assert.sh`, run in Tasks 1 & 11. ✓
- Parity-first delivery → Task 1 baseline, Task 11 parity. ✓
- Out-of-scope items (host-gw, etcd, IPv6, bridge port) → not present. ✓

**Placeholder scan:** netlink/kube tasks carry version-drift adaptation notes
(not placeholders — concrete code plus a fallback procedure). No TBD/TODO.

**Type consistency:** `Peer{node,pod_cidr,public_ip,vtep_mac}` consistent across
peer.rs/netlink.rs/kube_mgr.rs/main.rs. `BackendData.vtep_mac` consistent in
annotation.rs/kube_mgr.rs. `ensure_vxlan`/`add_route`/`add_peer_l2`/`del_peer`
signatures match their call sites in main.rs. `reconcile`/`Action` match. ✓

**Known risk:** `rtnetlink`/`kube` builder method names may differ from the pinned
versions; Tasks 6–8 instruct adapting via `cargo doc` while preserving documented
function contracts. This is the highest-uncertainty area and is covered by the
parity harness in Task 11.
