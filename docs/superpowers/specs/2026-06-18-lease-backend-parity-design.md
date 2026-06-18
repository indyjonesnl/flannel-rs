# flannel-rs lease/backend behaviour-parity tests (spec 2) — design

- **Date:** 2026-06-18
- **Status:** approved (design)
- **Scope:** second and final spec of the Go-Flannel parity effort. Follows `2026-06-18-flannel-parity-tests-design.md` (spec 1).

## Motivation

Spec 1 covered pure logic (config, IP math, CNI plugin rules). This spec covers
the kube subnet-manager lease/backend path: how flannel-rs reads each node's
lease from its annotations + PodCIDR, and how it publishes its own lease. We want
unit tests that lock this annotation/lease encode-decode contract against Go
Flannel's `pkg/subnet/kube` behaviour.

As in spec 1, tests are **reimplemented** in Rust (MIT) using the upstream
contract as reference — no Apache code is copied.

## Key decision: pure-function extraction, no kube-API fake

The lease/backend logic splits cleanly:

- **Pure transformation** (Node ↔ domain): decoding a `Node` into a `Peer`,
  extracting our own node's PodCIDR/InternalIP, and encoding the publish patch.
  This is flannel-rs's actual lease contract and is the parity target.
- **I/O** (`kube` client calls, the watch/poll loop) and **netlink** (VXLAN
  device, routes, neigh, fdb): framework/syscall behaviour, not flannel-rs logic.

A kube-API fake would mostly exercise the `kube` crate and overlaps the live
`reconcile` + smoke + conformance coverage. Instead we extract the pure functions
and test them by constructing `k8s_openapi` `Node` structs in memory — zero new
infrastructure. `netlink.rs` stays integration-only, exactly as bridge/netns did
in spec 1.

## §1 — Refactor for testability (`crates/flanneld/src/kube_mgr.rs`)

Separate the pure Node↔domain mapping from the kube I/O. The async methods keep
doing I/O and delegate the logic to pure helpers:

- `node_to_peer(&Node) -> Option<Peer>` — already a module-private free function;
  no signature change, just add tests (same-module tests can call it).
- Extract `extract_own_node(&Node) -> Result<OwnNode>` from `own_node`. `own_node`
  becomes: `nodes.get(name).await?` then `extract_own_node(&n)`.
- Extract `build_publish_patch(public_ip: &str, vtep_mac: &str) -> serde_json::Value`
  from `publish`. `publish` becomes: `build_publish_patch(...)` then `.patch(...)`.

This is a focused separation that also improves the file (logic vs I/O), not
speculative refactoring.

## §2 — Parity behaviours to assert

All unit-testable by building `k8s_openapi::api::core::v1::Node` structs in
memory (via `ObjectMeta` / `NodeSpec` / `NodeStatus` defaults).

**Decode — `node_to_peer` (flannel `pkg/subnet/kube` node→lease):**
- Complete node (backend-data + public-ip annotations + `spec.podCIDR`) →
  `Some(Peer)` with `node`, `pod_cidr`, `public_ip`, `vtep_mac` all correct.
- Missing `backend-data` annotation → `None`.
- Missing `public-ip` annotation → `None`.
- Missing `spec.podCIDR` → `None`.
- No annotations at all → `None`.
- Malformed `backend-data` JSON → `None` (skipped, not a hard error).

**Own-node extraction — `extract_own_node`:**
- podCIDR + `InternalIP` present → `OwnNode { pod_cidr, public_ip }` correct.
- No podCIDR → `Err` ("node has no PodCIDR").
- No `InternalIP` address → `Err` ("node has no InternalIP").
- A node with both `ExternalIP` and `InternalIP` → picks the `InternalIP`.

**Encode — `build_publish_patch`:**
- Result is a JSON object whose `metadata.annotations` contains exactly the four
  keys: `flannel.alpha.coreos.com/backend-type` = `"vxlan"`,
  `…/backend-data` = `{"VtepMAC":"<mac>"}`, `…/public-ip` = `"<ip>"`,
  `…/kube-subnet-manager-managed` = `"true"`.
- Top-level `apiVersion` = `"v1"` and `kind` = `"Node"` (server-side-apply shape).

## §3 — Out of scope

- `netlink.rs` VXLAN device / route / neigh / fdb — rtnetlink syscalls, no pure
  seam; covered live by smoke, conformance, and the `reconcile` CI scenario.
- The `kube` client itself, the watch/poll reconcile loop, and server-side-apply
  conflict semantics — `kube`-crate behaviour, not flannel-rs logic.
- `peer::reconcile` — already unit-tested (4 tests).
- No kube-API fake / mock HTTP server.

## §4 — Conventions

- Tests live in a `#[cfg(test)] mod tests` in `crates/flanneld/src/kube_mgr.rs`.
- Each test cites its upstream origin, e.g.
  `// parity: flannel pkg/subnet/kube — node with incomplete annotations yields no lease`.
- `Node` fixtures are built from `k8s_openapi` types (already a `flanneld`
  dependency) using `Default::default()` plus the specific fields under test; a
  small local helper (e.g. `node(name, pod_cidr, annotations, addresses)`) keeps
  the tests readable.
- Reimplementation only — no Apache-licensed code copied.

## Verification

- `cargo test --workspace --locked` green (run the **whole** suite, not a filtered
  module — a lesson from spec 1, where a per-module run missed a cross-cutting
  regression).
- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`.

## Outcome

Completes the two-spec parity effort: spec 1 (pure logic) + spec 2 (lease/backend
encode-decode). The remaining flannel-rs behaviour (VXLAN datapath) is asserted
by the existing live harnesses rather than unit tests.
