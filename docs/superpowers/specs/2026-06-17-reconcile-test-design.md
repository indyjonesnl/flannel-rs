# flanneld peer-reconcile smoke test — Design

**Date:** 2026-06-17
**Status:** Approved (brainstorming)

## Context

flanneld's reconcile loop — `peer::reconcile` emitting `Remove(old)+Add(new)`,
driving `netlink::del_peer`/`add_peer` (route + neigh + fdb) — is exercised at
startup but never tested when a peer's VTEP identity *changes* at runtime. That is
the resilience path: when a peer node's networking identity changes, every other
node must tear down the stale overlay entries and install the new ones, or
cross-node traffic to that node black-holes.

### How upstream handles this (researched, not assumed)

- **flannel-go** (`pkg/backend/vxlan/vxlan_network.go`): `handleSubnetEvents`
  reconciles **per-node subnet leases**, keyed on each peer's `PublicIP` +
  `VtepMAC`. Lease `Added` → `AddARP` (subnetIP→MAC) + `AddFDB` (MAC→PublicIP) +
  route; lease `Removed` → `DelARP`/`DelFDB`/`RouteDel`. A changed lease is
  Removed-then-Added. This is **node-lease-driven, never pod-IP-driven** — so a
  "kube-proxy restart" is not the trigger; a peer node's VTEP changing is.
  flannel-rs's reconcile mirrors this exactly.
- **kubernetes** (`test/e2e/network/conntrack.go`): the "endpoint gets a new IP,
  traffic reconciles" idea is tested as *"preserve UDP traffic when the server pod
  cycles for a ClusterIP service"* — a kube-proxy/conntrack + overlay-reachability
  test, at a different layer than flanneld's reconcile.

This test targets flanneld's own reconcile (the untested path).

## What it tests

A peer node's **VtepMAC changes at runtime** → every other node reconciles its
overlay (fdb/neigh/route) to the new MAC, and cross-node pod connectivity
survives.

### Why VtepMAC (not PublicIP)

A real "node gets a new IP" (PublicIP change) is the cloud trigger, but kind node
IPs are fixed and faking the `public-ip` annotation would break real traffic.
Recreating `flannel.1` forces a **new VtepMAC**, which drives the *identical*
reconcile branch (`Remove(old)+Add(new)` → `DelARP/DelFDB` + `AddARP/AddFDB`) with
traffic staying correct. Same code path flannel-go runs on a changed lease.

## Trigger mechanism

On one node N (a worker running a test pod):
1. `docker exec N ip link delete flannel.1`.
2. Restart flanneld on N: `kubectl -n kube-flannel delete pod <flannel-rs pod on N>`.
   The DaemonSet recreates it; `ensure_vxlan` builds a fresh `flannel.1` with a
   **new VtepMAC**; `publish()` re-writes node N's `backend-data` annotation.

Pod veths + `cbr0` persist (only `flannel.1` is deleted), so pod IPs are
unchanged — the only thing that changes is N's VTEP MAC, exactly the variable
under test. On restart, flanneld on N also re-adds all peers (full resync), and
peers' 10s poll detects N's changed annotation and reconciles.

## Assertions (new `tests/smoke/reconcile.sh`, flannel-rs only)

Stand up the cluster (reuse `tests/lib/cluster.sh` `cluster_up flannel-rs`), apply
`tests/smoke/workload.yaml` (server + client, anti-affinity → different nodes),
then:
1. **Baseline:** capture node N's (= server pod's node) `backend-data` VtepMAC
   (`OLD_MAC`); confirm a peer node M (= client's node) shows `OLD_MAC` in
   `bridge fdb show dev flannel.1`.
2. **Perturb:** delete `flannel.1` on N + restart N's flanneld pod; wait until N's
   annotation VtepMAC `!= OLD_MAC` (call it `NEW_MAC`).
3. **Reconcile:** assert (with retry for the ≤10s poll + churn) that peer M's
   `bridge fdb show dev flannel.1` now contains `NEW_MAC` and **not** `OLD_MAC`.
4. **Connectivity:** assert cross-node pod↔pod still works — `kubectl exec client
   -- curl <server-pod-ip>:80/hostname` succeeds (retry; same pod IP, new VTEP).

Existing smoke/conformance asserts are untouched.

## Components

- `tests/smoke/reconcile.sh` — sources `tests/lib/cluster.sh`, runs the scenario,
  EXIT-trap teardown + ERR-trap diagnostics (mirroring `run.sh`).
- `.github/workflows/ci.yml` — new `reconcile (flannel-rs)` job, `needs: test`,
  builds the image + loads br_netfilter (same prelude as the smoke jobs) and runs
  `reconcile.sh`. flannel-rs only (it validates *our* reconcile code).

## MAC parsing

`backend-data` is a JSON-string annotation `{"VtepMAC":"aa:bb:.."}`. Extract with
`kubectl ... -o jsonpath` for the annotation, then parse `VtepMAC` (python/jq).
fdb membership checked via `docker exec <node> bridge fdb show dev flannel.1`.

## Error handling / robustness

- Retry the post-restart annotation-change wait and the peer-fdb assertion (the
  poll is up to 10s; allow ~60s windows) using the existing `retry` helper pattern.
- On failure, dump diagnostics: both nodes' `bridge fdb`/`ip neigh`/`ip route` for
  `flannel.1`, node annotations, and the flannel-rs DS logs.
- Teardown the cluster on exit.

## Verification

- `bash tests/smoke/reconcile.sh flannel-rs` is green locally on kind: OLD_MAC seen
  on peer → after perturb, NEW_MAC replaces OLD_MAC in the peer's fdb → cross-node
  connectivity preserved.
- CI `reconcile (flannel-rs)` job green.
- Existing CI jobs unaffected (new job only).

## Out of scope

- PublicIP-change simulation (not reproducible in kind).
- The kube conntrack "server pod cycles" test (different layer; kube-proxy/overlay,
  not flanneld reconcile).
- Reconcile under flannel-go (it reconciles identically; this validates our code).
