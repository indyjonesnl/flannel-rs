# flannel-rs lease/backend behaviour-parity tests (spec 2) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Unit-test flannel-rs's kube subnet-manager lease/annotation encode-decode contract by extracting the pure Node↔domain functions from `kube_mgr.rs` and asserting them against Go Flannel's `pkg/subnet/kube` behaviour.

**Architecture:** Extract three pure functions from `kube_mgr.rs` (`build_publish_patch`, `extract_own_node`; `node_to_peer` is already pure) so the async methods delegate logic to them, then add a `#[cfg(test)] mod tests` that builds `k8s_openapi` `Node` structs in memory. No kube-API fake; `netlink.rs` stays integration-only.

**Tech Stack:** Rust, `cargo test`, `serde_json`, `k8s-openapi` (already a `flanneld` dependency).

**Spec:** `docs/superpowers/specs/2026-06-18-lease-backend-parity-design.md`

**Process note (lesson from spec 1):** the final gate MUST run the whole workspace suite (`cargo test --workspace`), not a filtered module — a per-module run there missed a cross-cutting regression.

---

## File structure

- `crates/flanneld/src/kube_mgr.rs` — extract `build_publish_patch` (Task 1) and `extract_own_node` (Task 2); add `#[cfg(test)] mod tests` covering encode (Task 1), own-node extraction (Task 2), and decode/`node_to_peer` (Task 3).

No other files change.

---

## Task 1: Extract `build_publish_patch` + encode test

**Files:**
- Modify: `crates/flanneld/src/kube_mgr.rs`

- [ ] **Step 1: Write the failing test**

Add a test module at the END of `crates/flanneld/src/kube_mgr.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_patch_sets_four_annotations_and_ssa_shape() {
        // parity: flannel pkg/subnet/kube — publishes backend-type/backend-data/
        // public-ip + kube-subnet-manager-managed via server-side apply.
        let p = build_publish_patch("172.18.0.2", "ae:11:22:33:44:55");
        assert_eq!(p["apiVersion"].as_str(), Some("v1"));
        assert_eq!(p["kind"].as_str(), Some("Node"));
        let ann = &p["metadata"]["annotations"];
        assert_eq!(
            ann.get(annotation::key("backend-type").as_str())
                .and_then(|v| v.as_str()),
            Some("vxlan")
        );
        assert_eq!(
            ann.get(annotation::key("backend-data").as_str())
                .and_then(|v| v.as_str()),
            Some(r#"{"VtepMAC":"ae:11:22:33:44:55"}"#)
        );
        assert_eq!(
            ann.get(annotation::key("public-ip").as_str())
                .and_then(|v| v.as_str()),
            Some("172.18.0.2")
        );
        assert_eq!(
            ann.get(annotation::key("kube-subnet-manager-managed").as_str())
                .and_then(|v| v.as_str()),
            Some("true")
        );
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p flanneld kube_mgr::tests`
Expected: FAIL to COMPILE — `cannot find function build_publish_patch in this scope`.

- [ ] **Step 3: Extract `build_publish_patch` and call it from `publish`**

In `crates/flanneld/src/kube_mgr.rs`, replace the existing `publish` method body so it delegates to a new free function. The `publish` method becomes:

```rust
    /// Server-side-apply patch own Node annotations: backend-type=vxlan,
    /// backend-data={"VtepMAC":mac}, public-ip, kube-subnet-manager-managed=true.
    pub async fn publish(&self, public_ip: &str, vtep_mac: &str) -> Result<()> {
        let nodes: Api<Node> = Api::all(self.client.clone());
        let patch = build_publish_patch(public_ip, vtep_mac);
        nodes
            .patch(
                &self.node_name,
                &PatchParams::apply("flanneld-rs").force(),
                &Patch::Apply(&patch),
            )
            .await
            .context("patch own annotations")?;
        Ok(())
    }
```

Then add this free function just above `fn node_to_peer` (near the bottom, before the test module):

```rust
/// Build the server-side-apply patch that publishes this node's flannel lease:
/// the four `flannel.alpha.coreos.com/*` annotations under a minimal Node object.
fn build_publish_patch(public_ip: &str, vtep_mac: &str) -> Value {
    let backend_data = BackendData {
        vtep_mac: vtep_mac.into(),
    }
    .to_json();

    let mut annotations = Map::new();
    annotations.insert(annotation::key("backend-type"), Value::from("vxlan"));
    annotations.insert(annotation::key("backend-data"), Value::from(backend_data));
    annotations.insert(annotation::key("public-ip"), Value::from(public_ip));
    annotations.insert(
        annotation::key("kube-subnet-manager-managed"),
        Value::from("true"),
    );

    json!({
        "apiVersion": "v1",
        "kind": "Node",
        "metadata": { "annotations": Value::Object(annotations) }
    })
}
```

(`Value`, `Map`, `json`, `BackendData`, and `annotation` are already imported at the top of the file.)

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p flanneld kube_mgr::tests`
Expected: PASS.

- [ ] **Step 5: Verify the crate is clean**

Run: `cargo clippy -p flanneld --all-targets -- -D warnings` and `cargo fmt -p flanneld -- --check`
Expected: both clean (no unused-import or dead-code warnings; `build_publish_patch` is used by `publish`).

- [ ] **Step 6: Commit**

```bash
git add crates/flanneld/src/kube_mgr.rs
git commit -m "test(flanneld): extract build_publish_patch; assert lease publish shape

parity: flannel pkg/subnet/kube — four flannel.alpha.coreos.com/* annotations
published via server-side apply."
```

---

## Task 2: Extract `extract_own_node` + own-node tests

**Files:**
- Modify: `crates/flanneld/src/kube_mgr.rs`

- [ ] **Step 1: Write the failing tests**

Add these imports and tests INSIDE the existing `mod tests` (after the `use super::*;` line, add the imports; place the tests after the Task 1 test):

```rust
    use k8s_openapi::api::core::v1::{NodeAddress, NodeSpec, NodeStatus};

    // Build a Node carrying only the fields extract_own_node reads.
    fn node_with(pod_cidr: Option<&str>, addresses: Vec<(&str, &str)>) -> Node {
        Node {
            spec: Some(NodeSpec {
                pod_cidr: pod_cidr.map(String::from),
                ..Default::default()
            }),
            status: Some(NodeStatus {
                addresses: Some(
                    addresses
                        .into_iter()
                        .map(|(t, a)| NodeAddress {
                            type_: t.into(),
                            address: a.into(),
                        })
                        .collect(),
                ),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    // parity: flannel pkg/subnet/kube — own lease = spec.podCIDR + status InternalIP.
    #[test]
    fn extract_own_node_reads_podcidr_and_internal_ip() {
        let n = node_with(Some("10.244.1.0/24"), vec![("InternalIP", "172.18.0.2")]);
        let own = extract_own_node(&n).unwrap();
        assert_eq!(own.pod_cidr, "10.244.1.0/24");
        assert_eq!(own.public_ip, "172.18.0.2");
    }

    #[test]
    fn extract_own_node_errors_without_podcidr() {
        let n = node_with(None, vec![("InternalIP", "172.18.0.2")]);
        let err = extract_own_node(&n).unwrap_err().to_string();
        assert!(err.contains("PodCIDR"), "got {err}");
    }

    #[test]
    fn extract_own_node_errors_without_internal_ip() {
        let n = node_with(Some("10.244.1.0/24"), vec![("ExternalIP", "1.2.3.4")]);
        let err = extract_own_node(&n).unwrap_err().to_string();
        assert!(err.contains("InternalIP"), "got {err}");
    }

    #[test]
    fn extract_own_node_prefers_internal_over_external_ip() {
        let n = node_with(
            Some("10.244.1.0/24"),
            vec![("ExternalIP", "1.2.3.4"), ("InternalIP", "172.18.0.2")],
        );
        let own = extract_own_node(&n).unwrap();
        assert_eq!(own.public_ip, "172.18.0.2");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p flanneld kube_mgr::tests::extract_own_node_reads_podcidr_and_internal_ip`
Expected: FAIL to COMPILE — `cannot find function extract_own_node in this scope`.

- [ ] **Step 3: Extract `extract_own_node` and call it from `own_node`**

In `crates/flanneld/src/kube_mgr.rs`, replace the `own_node` method body so it delegates:

```rust
    /// Get own Node: Spec.podCIDR + status InternalIP.
    pub async fn own_node(&self) -> Result<OwnNode> {
        let nodes: Api<Node> = Api::all(self.client.clone());
        let n = nodes.get(&self.node_name).await.context("get own node")?;
        extract_own_node(&n)
    }
```

Then add this free function just above `fn build_publish_patch`:

```rust
/// Extract this node's lease inputs from its Node object: the PodCIDR (from
/// spec) and the InternalIP (from status addresses). Both are required.
fn extract_own_node(n: &Node) -> Result<OwnNode> {
    let pod_cidr = n
        .spec
        .as_ref()
        .and_then(|s| s.pod_cidr.clone())
        .context("node has no PodCIDR")?;
    let public_ip = n
        .status
        .as_ref()
        .and_then(|s| s.addresses.as_ref())
        .and_then(|a| a.iter().find(|x| x.type_ == "InternalIP"))
        .map(|x| x.address.clone())
        .context("node has no InternalIP")?;
    Ok(OwnNode {
        pod_cidr,
        public_ip,
    })
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p flanneld kube_mgr::tests`
Expected: PASS (Task 1 test + 4 new own-node tests).

- [ ] **Step 5: Verify the crate is clean**

Run: `cargo clippy -p flanneld --all-targets -- -D warnings` and `cargo fmt -p flanneld -- --check`
Expected: both clean.

- [ ] **Step 6: Commit**

```bash
git add crates/flanneld/src/kube_mgr.rs
git commit -m "test(flanneld): extract extract_own_node; assert podCIDR + InternalIP rules

parity: flannel pkg/subnet/kube — own lease requires podCIDR and an InternalIP,
preferring InternalIP over ExternalIP."
```

---

## Task 3: `node_to_peer` decode tests (no production change)

**Files:**
- Modify: `crates/flanneld/src/kube_mgr.rs`

This task is test-only; `node_to_peer` already exists and is pure. The tests should PASS against the current code. If a test FAILS, STOP and report it (it indicates a real behaviour mismatch — do not weaken the test or change production code without flagging).

- [ ] **Step 1: Write the tests**

Add these imports and tests INSIDE the existing `mod tests` (add the imports next to the others at the top of the module; place the tests after the Task 2 tests):

```rust
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
    use std::collections::BTreeMap;

    // Build a Node with a name, optional podCIDR, and annotation key/value pairs.
    fn node_with_annotations(
        name: &str,
        pod_cidr: Option<&str>,
        annotations: Vec<(String, String)>,
    ) -> Node {
        let ann: BTreeMap<String, String> = annotations.into_iter().collect();
        Node {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                annotations: if ann.is_empty() { None } else { Some(ann) },
                ..Default::default()
            },
            spec: Some(NodeSpec {
                pod_cidr: pod_cidr.map(String::from),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn complete_annotations() -> Vec<(String, String)> {
        vec![
            (
                annotation::key("backend-data"),
                r#"{"VtepMAC":"ae:11:22:33:44:55"}"#.to_string(),
            ),
            (annotation::key("public-ip"), "172.18.0.3".to_string()),
        ]
    }

    // parity: flannel pkg/subnet/kube — a node with complete annotations + podCIDR
    // decodes to a lease (Peer); any missing/garbled piece yields no lease.
    #[test]
    fn node_to_peer_complete_node_yields_peer() {
        let n = node_with_annotations("n2", Some("10.244.2.0/24"), complete_annotations());
        let p = node_to_peer(&n).expect("peer");
        assert_eq!(p.node, "n2");
        assert_eq!(p.pod_cidr, "10.244.2.0/24");
        assert_eq!(p.public_ip, "172.18.0.3");
        assert_eq!(p.vtep_mac, "ae:11:22:33:44:55");
    }

    #[test]
    fn node_to_peer_missing_backend_data_is_skipped() {
        let ann = vec![(annotation::key("public-ip"), "172.18.0.3".to_string())];
        let n = node_with_annotations("n2", Some("10.244.2.0/24"), ann);
        assert!(node_to_peer(&n).is_none());
    }

    #[test]
    fn node_to_peer_missing_public_ip_is_skipped() {
        let ann = vec![(
            annotation::key("backend-data"),
            r#"{"VtepMAC":"aa:bb"}"#.to_string(),
        )];
        let n = node_with_annotations("n2", Some("10.244.2.0/24"), ann);
        assert!(node_to_peer(&n).is_none());
    }

    #[test]
    fn node_to_peer_missing_podcidr_is_skipped() {
        let n = node_with_annotations("n2", None, complete_annotations());
        assert!(node_to_peer(&n).is_none());
    }

    #[test]
    fn node_to_peer_no_annotations_is_skipped() {
        let n = node_with_annotations("n2", Some("10.244.2.0/24"), vec![]);
        assert!(node_to_peer(&n).is_none());
    }

    #[test]
    fn node_to_peer_malformed_backend_data_is_skipped() {
        let ann = vec![
            (annotation::key("backend-data"), "not json".to_string()),
            (annotation::key("public-ip"), "172.18.0.3".to_string()),
        ];
        let n = node_with_annotations("n2", Some("10.244.2.0/24"), ann);
        assert!(node_to_peer(&n).is_none());
    }
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p flanneld kube_mgr::tests`
Expected: PASS — all `kube_mgr::tests` (Task 1 + Task 2 + 6 new decode tests). If any of the 6 new tests FAIL, STOP and report with output (do not modify production code).

- [ ] **Step 3: Verify the crate is clean**

Run: `cargo clippy -p flanneld --all-targets -- -D warnings` and `cargo fmt -p flanneld -- --check`
Expected: both clean.

- [ ] **Step 4: Commit**

```bash
git add crates/flanneld/src/kube_mgr.rs
git commit -m "test(flanneld): cover node_to_peer lease decoding

parity: flannel pkg/subnet/kube — complete node decodes to a Peer; missing
backend-data/public-ip/podCIDR/annotations or malformed backend-data is skipped."
```

---

## Final verification (before opening a PR)

- [ ] **Run the full local CI gate (whole workspace, not a filtered module)**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace --locked
cargo test --workspace --locked
```
Expected: all green, 0 failures. flanneld test count up by 11 (1 encode + 4 own-node + 6 decode).

- [ ] **Open a PR** off `lease-backend-parity`, referencing the spec, once the gate is green.
