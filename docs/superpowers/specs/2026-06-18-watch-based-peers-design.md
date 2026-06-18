# flannel-rs watch-based peer updates — design

- **Date:** 2026-06-18
- **Status:** approved (design)

## Motivation

`flanneld` currently discovers peer changes by listing all Nodes every 10s
(`crates/flanneld/src/main.rs` reconcile loop). This adds up to ~10s of latency
before a new/changed/removed node's VXLAN route is installed, and re-lists the
whole node set on every tick. Replace the poll with a kube **watch** so peer
changes are reacted to near-instantly, while preserving the two things the 10s
cadence also provided: periodic ip-masq re-assertion and retry of failed netlink
operations.

## Upstream reference

Go flannel uses two independent loops (verified against `flannel-io/flannel`):

- **Leases/routes:** the kube subnet manager runs a client-go informer with a
  **5-minute resync period**; node add/update/delete enqueue onto a buffered
  channel that the vxlan backend consumes to install routes/ARP/FDB. The resync
  re-delivers all leases periodically (idempotent re-ensure = self-heal).
- **ip-masq:** a *separate* check-then-add loop on the `--iptables-resync` timer
  (**default 5s**), unrelated to lease events.

flannel-rs mirrors this shape, with one deliberate change: a **60s** peer
safety-net resync instead of 5 min, so a transient netlink failure recovers in
≤60s (today it is ≤10s); and ip-masq stays at the current **10s** (not 5s).

## Architecture

Replace the poll loop with a single async task running a `tokio::select!` over
three sources. Keeping one task means `nl` (the rtnetlink handle), `dev_idx`,
`installed`, and `masq` stay owned by that task — no cross-task `Send`/sharing of
the netlink handle.

- **Node watch:** `kube::runtime::watcher` over `Api::all::<Node>`, fed through a
  `reflector` into a `Store<Node>`. The reflector stream's initial emission is
  the full node list (this replaces the explicit startup reconcile); later
  emissions fire on each node change. `watcher` auto-relists on desync with
  backoff.
- **Peer-resync tick (60s):** re-reconcile from the store; retries any netlink op
  that failed on a prior attempt.
- **ip-masq tick (10s):** re-assert masquerade rules, exactly as today.

Both peer triggers (a watch event, the 60s tick) call the same
`reconcile_peers(...)`: build the desired peer map from the store, run the
existing `peer::reconcile` diff, and apply Add/Remove via the existing netlink
ops, updating the `installed` map.

### Data flow

```
watcher(Api::all::<Node>) ──▶ reflector(writer) ──▶ Store<Node>
        │ (stream of events; also updates Store as a side effect)
        ▼
   select! {
     event   = reflector_stream.next() => reconcile_peers(&store, &nl, dev_idx, &mut installed)
     _       = peer_resync_tick (60s)  => reconcile_peers(&store, &nl, dev_idx, &mut installed)
     _       = ipmasq_tick (10s)       => masq.ensure(&network, &subnet)
   }
```

## Components

- **`crates/flanneld/src/kube_mgr.rs`**
  - Add `pub fn desired_from_nodes(nodes: &[Node], self_name: &str) -> HashMap<String, Peer>`
    — filters out `self_name`, maps each remaining node via the existing private
    `node_to_peer`, collecting the `Some` results. This is the pure, unit-tested
    seam. (`Store::state()` yields `Vec<Arc<Node>>`; `main` adapts it to the
    `&[Node]` input — e.g. `&store.state().iter().map(|a| (**a).clone()).collect::<Vec<_>>()`
    — so the function stays trivially testable with plain `Node` values.)
  - Expose what `main` needs to build the watch: a `Client` accessor (e.g.
    `pub fn client(&self) -> Client`) so `main` can construct
    `Api::all(client)` + the `watcher`/`reflector`. `KubeMgr` keeps owning the
    client and `node_name`.
  - Remove the now-unused async `desired_peers()` (only the old poll loop used
    it). `publish` / `own_node` / `extract_own_node` / `build_publish_patch`
    are unchanged.

- **`crates/flanneld/src/main.rs`**
  - Replace the `loop { desired_peers().await; …; sleep(10s) }` block with the
    `select!` loop above. The reconcile/netlink/ip-masq bodies are reused
    unchanged; only the trigger mechanism changes.

- **`netlink.rs`, `peer.rs` (`reconcile`):** unchanged.

## Error handling

- `watcher` retries watch failures and relists on desync internally (built-in
  backoff). Reflector-stream `Err` items are logged and skipped; the stream
  self-heals.
- A failed `add_route` / `add_peer_l2` keeps the current behaviour: the peer is
  **not** inserted into `installed`, so it is retried on the next watch event or
  the 60s resync.
- A failed `del_peer` is logged; the peer is still dropped from `installed`.
- `masq.ensure` failure is logged and retried on the next 10s tick.

## Testability

- **`desired_from_nodes`** is the new pure unit-tested function: build a `Vec<Node>`
  in memory and assert the resulting peer map — includes complete peers, excludes
  `self_name`, excludes nodes with incomplete annotations/podCIDR. Reuses the
  spec-2 `node_with_annotations` fixture style and `node_to_peer`.
- The `select!` watch loop itself is integration-level (owns I/O): it is exercised
  by the existing **`reconcile` CI scenario** (`tests/smoke/reconcile.sh`, which
  changes a peer and asserts reconvergence) plus the smoke and conformance
  harnesses. No new test infrastructure.

## Out of scope

- netlink internals and ip-masq rule content (unchanged).
- Changing the ip-masq cadence to flannel's 5s (kept at 10s).
- Backend types other than vxlan.
- Advanced watch tuning (bookmarks, page sizes) beyond `watcher` defaults.

## Verification

- `cargo test --workspace --locked` green (run the whole suite, not a filtered
  module).
- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`.
- CI: the `reconcile` scenario + smoke + conformance jobs must pass — these
  validate the watch loop end-to-end (a Go-flannel-parity peer change is
  reconverged by the Rust watch path).
