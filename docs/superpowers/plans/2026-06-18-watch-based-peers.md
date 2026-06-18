# watch-based peer updates Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace flanneld's 10s node-list poll with a kube `watcher`+`reflector` store (near-instant peer reaction), a 60s safety-net peer resync, and a separate 10s ip-masq re-assert ticker — mirroring upstream Go flannel's two-loop shape.

**Architecture:** Task 1 extracts the pure `desired_from_nodes` mapping from the async `desired_peers` (so it's unit-tested while the poll still works). Task 2 rewrites the main loop into a single `tokio::select!` over the reflector stream + two `tokio::time::interval`s, reusing the existing `peer::reconcile` + netlink ops, and removes the now-unused `desired_peers`.

**Tech Stack:** Rust, tokio, `kube` 0.99 (`runtime` feature: `watcher`, `reflector`, `Store`), `k8s-openapi`, `futures`.

**Spec:** `docs/superpowers/specs/2026-06-18-watch-based-peers-design.md`

**Verified kube-runtime 0.99 API:** `kube::runtime::watcher(api, watcher::Config)`, `kube::runtime::reflector(writer, stream)`, `kube::runtime::reflector::store::<K>() -> (Store<K>, Writer<K>)`, `Store::state(&self) -> Vec<std::sync::Arc<K>>`. `.boxed()`/`.next()` come from `futures::StreamExt`.

**Process note (lesson from earlier specs):** the final gate runs the whole workspace suite (`cargo test --workspace`), not a filtered module.

---

## File structure

- `crates/flanneld/src/kube_mgr.rs` — add pure `desired_from_nodes` + tests; refactor `desired_peers` to delegate (Task 1); add `client()` accessor + remove `desired_peers` (Task 2).
- `crates/flanneld/src/main.rs` — add `reconcile_from_store` helper + rewrite the reconcile loop into a `select!` (Task 2).

---

## Task 1: Extract `desired_from_nodes` (pure) + tests

**Files:**
- Modify: `crates/flanneld/src/kube_mgr.rs`

- [ ] **Step 1: Write the failing tests**

Add these tests INSIDE the existing `#[cfg(test)] mod tests` in `crates/flanneld/src/kube_mgr.rs` (the helpers `node_with_annotations` and `complete_annotations` already exist there from a prior change; reuse them). Place after the existing tests:

```rust
    #[test]
    fn desired_from_nodes_includes_peers_excludes_self_and_incomplete() {
        let self_node =
            node_with_annotations("self", Some("10.244.0.0/24"), complete_annotations());
        let peer = node_with_annotations("n2", Some("10.244.2.0/24"), complete_annotations());
        let incomplete = node_with_annotations("n3", Some("10.244.3.0/24"), vec![]);
        let nodes = vec![self_node, peer, incomplete];
        let desired = desired_from_nodes(&nodes, "self");
        assert_eq!(desired.len(), 1);
        assert!(desired.contains_key("n2"));
        assert!(!desired.contains_key("self"));
        assert!(!desired.contains_key("n3"));
        assert_eq!(desired["n2"].pod_cidr, "10.244.2.0/24");
        assert_eq!(desired["n2"].vtep_mac, "ae:11:22:33:44:55");
    }

    #[test]
    fn desired_from_nodes_empty_input_is_empty() {
        let desired = desired_from_nodes(&[], "self");
        assert!(desired.is_empty());
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p flanneld kube_mgr::tests::desired_from_nodes_empty_input_is_empty`
Expected: FAIL to COMPILE — `cannot find function desired_from_nodes in this scope`.

- [ ] **Step 3: Add `desired_from_nodes` and make `desired_peers` delegate to it**

In `crates/flanneld/src/kube_mgr.rs`, add this free function just above `fn node_to_peer`:

```rust
/// Build the desired peer map (node name -> Peer) from a set of Node objects,
/// excluding `self_name` and any node lacking complete lease data.
pub fn desired_from_nodes(nodes: &[Node], self_name: &str) -> HashMap<String, Peer> {
    let mut out = HashMap::new();
    for n in nodes {
        let name = n.metadata.name.clone().unwrap_or_default();
        if name == self_name {
            continue;
        }
        if let Some(peer) = node_to_peer(n) {
            out.insert(name, peer);
        }
    }
    out
}
```

Then replace the body of `desired_peers` so it lists and delegates:

```rust
    /// Build desired peer map (node name -> Peer) for all nodes except self that
    /// have complete annotations (backend-data + public-ip) and a podCIDR. Nodes
    /// with missing data are skipped.
    pub async fn desired_peers(&self) -> Result<HashMap<String, Peer>> {
        let nodes: Api<Node> = Api::all(self.client.clone());
        let list = nodes
            .list(&Default::default())
            .await
            .context("list nodes")?;
        Ok(desired_from_nodes(&list.items, &self.node_name))
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p flanneld kube_mgr::tests`
Expected: PASS (existing kube_mgr tests + 2 new).

- [ ] **Step 5: Verify the crate is clean**

Run: `cargo clippy -p flanneld --all-targets -- -D warnings` and `cargo fmt -p flanneld -- --check`
Expected: both clean (`desired_from_nodes` is used by `desired_peers`, so no dead-code warning).

- [ ] **Step 6: Commit**

```bash
git add crates/flanneld/src/kube_mgr.rs
git commit -m "refactor(flanneld): extract pure desired_from_nodes from desired_peers

Pure node-list -> peer-map mapping, unit-tested (excludes self + incomplete
nodes). desired_peers now lists then delegates. Prep for watch-based loop."
```

---

## Task 2: Watch-based reconcile loop

**Files:**
- Modify: `crates/flanneld/src/kube_mgr.rs`
- Modify: `crates/flanneld/src/main.rs`

This task rewrites async control flow; the loop itself is integration-level (verified by the `reconcile` CI scenario + smoke), so it is not TDD-driven. Success criteria: clean build, clippy, fmt, and the whole unit-test suite still green.

- [ ] **Step 1: Add a `client()` accessor and remove the now-unused `desired_peers`**

In `crates/flanneld/src/kube_mgr.rs`, add this method inside `impl KubeMgr` (e.g. just after `pub async fn new`):

```rust
    /// Clone of the kube client, for callers that build their own watches.
    pub fn client(&self) -> Client {
        self.client.clone()
    }
```

Then DELETE the entire `desired_peers` method (the `pub async fn desired_peers(&self) -> Result<HashMap<String, Peer>> { ... }` block). `desired_from_nodes` and `node_to_peer` stay.

- [ ] **Step 2: Add imports to `main.rs`**

In `crates/flanneld/src/main.rs`, add these `use` lines with the existing imports near the top:

```rust
use futures::StreamExt;
use k8s_openapi::api::core::v1::Node;
use kube::runtime::{reflector, watcher};
use kube::Api;
```

- [ ] **Step 3: Add the `reconcile_from_store` helper**

In `crates/flanneld/src/main.rs`, add this free async function (e.g. directly above `#[tokio::main] async fn main`):

```rust
/// Reconcile installed VXLAN peers against the current node Store: build the
/// desired peer set, diff it against `installed`, and apply add/remove via
/// netlink. A failed add is left out of `installed` so it is retried on the next
/// watch event or resync tick.
async fn reconcile_from_store(
    store: &reflector::Store<Node>,
    self_name: &str,
    nl: &Netlink,
    dev_idx: u32,
    installed: &mut HashMap<String, crate::peer::Peer>,
) {
    let nodes: Vec<Node> = store.state().iter().map(|a| (**a).clone()).collect();
    let desired = crate::kube_mgr::desired_from_nodes(&nodes, self_name);
    let mut next = installed.clone();
    for action in reconcile(installed, &desired) {
        match action {
            Action::Add(p) => {
                let r1 = nl.add_route(dev_idx, &p).await;
                let r2 = nl.add_peer_l2(dev_idx, &p).await;
                match (r1, r2) {
                    (Ok(()), Ok(())) => {
                        info!(node = %p.node, cidr = %p.pod_cidr, "peer added");
                        next.insert(p.node.clone(), p);
                    }
                    (a, b) => {
                        if let Err(e) = a {
                            warn!(?e, node = %p.node, "add_route failed; will retry");
                        }
                        if let Err(e) = b {
                            warn!(?e, node = %p.node, "add_peer_l2 failed; will retry");
                        }
                        next.remove(&p.node);
                    }
                }
            }
            Action::Remove(p) => {
                if let Err(e) = nl.del_peer(dev_idx, &p).await {
                    warn!(?e, node = %p.node, "del_peer");
                }
                next.remove(&p.node);
                info!(node = %p.node, "peer removed");
            }
        }
    }
    *installed = next;
}
```

- [ ] **Step 4: Replace the poll loop with the `select!` loop**

In `crates/flanneld/src/main.rs`, replace the entire block from `let mut installed: HashMap<String, crate::peer::Peer> = HashMap::new();` through the end of the `loop { ... }` (the old poll loop, ending just before the `#[cfg(test)] mod tests`) with:

```rust
    let mut installed: HashMap<String, crate::peer::Peer> = HashMap::new();

    // Watch nodes via a reflector-backed store so peer changes reconcile
    // near-instantly; the watcher relists on desync with its own backoff.
    let api: Api<Node> = Api::all(mgr.client());
    let (store, writer) = reflector::store::<Node>();
    let mut stream = reflector(writer, watcher(api, watcher::Config::default())).boxed();

    // Safety-net peer resync (also retries failed netlink ops) and ip-masq
    // re-assertion run on independent timers. interval() fires immediately on the
    // first tick, giving an initial reconcile + masq ensure right away.
    let mut peer_resync = tokio::time::interval(Duration::from_secs(60));
    let mut masq_tick = tokio::time::interval(Duration::from_secs(10));

    loop {
        tokio::select! {
            ev = stream.next() => match ev {
                Some(Ok(_)) => {
                    reconcile_from_store(&store, &node_name, &nl, dev_idx, &mut installed).await;
                }
                Some(Err(e)) => warn!(?e, "node watch error; watcher will resync"),
                None => {
                    warn!("node watch stream ended unexpectedly");
                    return Ok(());
                }
            },
            _ = peer_resync.tick() => {
                reconcile_from_store(&store, &node_name, &nl, dev_idx, &mut installed).await;
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
```

- [ ] **Step 5: Build and verify clean**

Run: `cargo build -p flanneld` then `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all -- --check`
Expected: builds; clippy clean (no unused imports, no dead `desired_peers`); fmt clean.
If the build fails on a kube-runtime symbol path, adapt the `use`/call to the kube 0.99 API (the verified shapes are in the plan header) — do NOT change the loop's behaviour. If you cannot resolve it, STOP and report BLOCKED with the compiler error.

- [ ] **Step 6: Run the whole unit-test suite**

Run: `cargo test --workspace --locked`
Expected: all green, 0 failures (no unit test targets the loop directly; `desired_from_nodes` tests from Task 1 cover the pure mapping).

- [ ] **Step 7: Commit**

```bash
git add crates/flanneld/src/kube_mgr.rs crates/flanneld/src/main.rs
git commit -m "feat(flanneld): watch nodes instead of polling for peer updates

Replace the 10s node-list poll with a kube watcher+reflector store (peers
reconcile on change), a 60s safety-net resync, and a separate 10s ip-masq
re-assert ticker, combined in one tokio::select! loop. Mirrors upstream Go
flannel's two-loop shape. Removes the now-unused KubeMgr::desired_peers."
```

---

## Final verification (before opening a PR)

- [ ] **Run the full local CI gate (whole workspace)**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace --locked
cargo test --workspace --locked
```
Expected: all green, 0 failures. flanneld test count up by 2 (the `desired_from_nodes` tests).

- [ ] **Open a PR** off `watch-based-peers`, referencing the spec. The PR's CI must pass the `reconcile` scenario, smoke, and conformance jobs — these validate the watch loop end-to-end (a peer change is reconverged by the Rust watch path). Note in the PR that loop behaviour is verified by CI, not unit tests.
