# flanneld peer-reconcile smoke test Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A kind-based test that changes a peer node's VtepMAC at runtime and asserts every other node reconciles its overlay (fdb) to the new MAC while cross-node pod connectivity survives — exercising flanneld's `reconcile`/`del_peer`/`add_peer` path.

**Architecture:** A bash scenario (`tests/smoke/reconcile.sh`) that reuses the shared `cluster_up` from `tests/lib/cluster.sh`, applies the existing workload, perturbs one node's VTEP (delete `flannel.1` + restart its flanneld), and asserts peer fdb reconciliation + preserved connectivity. A dedicated CI job runs it for flannel-rs.

**Tech Stack:** bash, kind, kubectl, docker, python3 (annotation JSON parse). No Rust changes.

---

## Pre-flight: standing rule

No Rust changes here, so the cargo gate is unaffected. Still: before any `git push`, confirm `cargo fmt --all -- --check` (unchanged) and that YAML/bash lint. Use `dangerouslyDisableSandbox: true` for kind/docker while the safety classifier is flaky.

---

## File Structure

```
tests/smoke/reconcile.sh         # NEW: the reconcile scenario (flannel-rs only)
.github/workflows/ci.yml         # MODIFY: add `reconcile (flannel-rs)` job
```
Reuses `tests/lib/cluster.sh` (`cluster_up`/`cluster_down`) and `tests/smoke/workload.yaml` (server+client anti-affinity). Existing smoke/conformance scripts untouched.

---

## Task 1: reconcile.sh + local green run

**Files:**
- Create: `tests/smoke/reconcile.sh`

- [ ] **Step 1: Write reconcile.sh**

`tests/smoke/reconcile.sh`:
```bash
#!/usr/bin/env bash
# Change a peer node's VtepMAC at runtime and assert other nodes reconcile their
# overlay (fdb) to the new MAC, with cross-node pod connectivity preserved.
# Exercises flanneld's reconcile/del_peer/add_peer path. flannel-rs only.
set -euo pipefail
VARIANT="${1:-flannel-rs}"
[ "$VARIANT" = "flannel-rs" ] || { echo "reconcile test is flannel-rs only"; exit 2; }
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
CTX="kind-flannel-rs"
k() { kubectl --context "$CTX" "$@"; }
# shellcheck source=../lib/cluster.sh
. "$ROOT/tests/lib/cluster.sh"

retry() {
  local timeout="$1"; shift
  local deadline=$((SECONDS + timeout))
  until "$@"; do
    if [ "$SECONDS" -ge "$deadline" ]; then
      echo "FAIL: \`$*\` not satisfied within ${timeout}s"; return 1
    fi
    sleep 3
  done
}

# VtepMAC from a node's flannel backend-data annotation ({"VtepMAC":"aa:bb:.."}).
vtep_mac() {
  k get node "$1" -o jsonpath='{.metadata.annotations.flannel\.alpha\.coreos\.com/backend-data}' \
    | python3 -c 'import sys,json; print(json.load(sys.stdin)["VtepMAC"])'
}
fdb_has()     { docker exec "$1" bridge fdb show dev flannel.1 2>/dev/null | grep -qi "$2"; }
fdb_missing() { ! fdb_has "$1" "$2"; }

dump_diag() {
  echo "================ RECONCILE DIAGNOSTICS ================"
  k get nodes -o custom-columns='NAME:.metadata.name,BACKEND:.metadata.annotations.flannel\.alpha\.coreos\.com/backend-data' || true
  for n in $(kind get nodes --name flannel-rs); do
    echo "--- $n flannel.1 fdb / neigh ---"
    docker exec "$n" bridge fdb show dev flannel.1 2>/dev/null || true
    docker exec "$n" ip neigh show dev flannel.1 2>/dev/null || true
  done
  k -n kube-flannel logs ds/kube-flannel-ds --tail=60 --all-containers || true
  echo "======================================================="
}
trap cluster_down EXIT
trap dump_diag ERR

cluster_up "$VARIANT"

k apply -f "$ROOT/tests/smoke/workload.yaml"
k rollout status deploy/smoke-server --timeout=240s
k rollout status deploy/smoke-client --timeout=240s

SRV_POD=$(k get pod -l app=smoke-server -o jsonpath='{.items[0].metadata.name}')
CLI_POD=$(k get pod -l app=smoke-client -o jsonpath='{.items[0].metadata.name}')
N=$(k get pod "$SRV_POD" -o jsonpath='{.spec.nodeName}')   # node whose VTEP we churn
M=$(k get pod "$CLI_POD" -o jsonpath='{.spec.nodeName}')   # peer that must reconcile
SRV_IP=$(k get pod "$SRV_POD" -o jsonpath='{.status.podIP}')
[ "$N" != "$M" ] || { echo "FAIL: pods co-located"; exit 1; }
echo "churn node N=$N ; peer M=$M ; server pod ip=$SRV_IP"

echo "== baseline: peer M has N's VtepMAC in flannel.1 fdb =="
OLD_MAC=$(vtep_mac "$N"); echo "OLD_MAC=$OLD_MAC"
retry 30 fdb_has "$M" "$OLD_MAC"
echo "OK: peer fdb has OLD_MAC"

echo "== perturb: delete flannel.1 on N + restart its flanneld =="
docker exec "$N" ip link delete flannel.1
FPOD=$(k -n kube-flannel get pod -l app=flannel --field-selector "spec.nodeName=$N" -o jsonpath='{.items[0].metadata.name}')
echo "restarting flanneld pod $FPOD on $N"
k -n kube-flannel delete pod "$FPOD"
k -n kube-flannel rollout status ds/kube-flannel-ds --timeout=180s

echo "== wait for N's VtepMAC to change =="
mac_changed() { local cur; cur=$(vtep_mac "$N" 2>/dev/null) || return 1; [ -n "$cur" ] && [ "$cur" != "$OLD_MAC" ]; }
retry 120 mac_changed
NEW_MAC=$(vtep_mac "$N"); echo "NEW_MAC=$NEW_MAC"

echo "== assert peer M reconciled: fdb has NEW_MAC and not OLD_MAC =="
retry 60 fdb_has "$M" "$NEW_MAC"
retry 30 fdb_missing "$M" "$OLD_MAC"
echo "OK: peer reconciled fdb to NEW_MAC"

echo "== assert cross-node connectivity preserved (same pod IP, new VTEP) =="
retry 60 k exec "$CLI_POD" -- curl -sS --max-time 5 "http://$SRV_IP:80/hostname"
echo "OK: cross-node connectivity preserved"

echo "RECONCILE PASSED: $VARIANT"
```

- [ ] **Step 2: Syntax check + chmod**

Run:
```bash
bash -n tests/smoke/reconcile.sh && echo "syntax OK"
chmod +x tests/smoke/reconcile.sh
```
Expected: `syntax OK`.

- [ ] **Step 3: Run locally to green**

Build the image and run the scenario (authorized to run kind/docker; set `dangerouslyDisableSandbox: true`):
```bash
docker build -t flannel-rs:dev .
bash tests/smoke/reconcile.sh flannel-rs
```
Expected: ends `RECONCILE PASSED: flannel-rs`, with the log showing `OLD_MAC=...`, `NEW_MAC=...` (different), `OK: peer reconciled fdb to NEW_MAC`, and `OK: cross-node connectivity preserved`.

If it fails, use `superpowers:systematic-debugging`. Likely points:
- `vtep_mac` parse: confirm the annotation key/format (`backend-data` = `{"VtepMAC":".."}`); `python3` is present.
- After deleting `flannel.1`, confirm the new flanneld pod recreates it with a different MAC (`docker exec $N ip -d link show flannel.1`) and re-publishes the annotation.
- Reconcile timing: peers poll every 10s — the `retry` windows (60s) should cover it; if not, widen.
- Connectivity: N's flanneld must re-add peers on restart (full resync) AND M must reconcile N's new MAC — both required for traffic. The ERR-trap diagnostics dump both nodes' fdb/neigh + DS logs.

- [ ] **Step 4: Commit**

```bash
git add tests/smoke/reconcile.sh
git commit -m "test: flanneld peer-reconcile scenario (VtepMAC change -> fdb reconcile)"
```

---

## Task 2: CI job

**Files:**
- Modify: `.github/workflows/ci.yml`

- [ ] **Step 1: Add the reconcile job**

In `.github/workflows/ci.yml`, after the `conformance` job, add a `reconcile` job mirroring the smoke job's prelude (install kind/kubectl, load br_netfilter, build image):
```yaml
  reconcile:
    name: flanneld reconcile (flannel-rs)
    needs: test
    runs-on: ubuntu-latest
    timeout-minutes: 25
    steps:
      - uses: actions/checkout@v5

      - name: Install kind and kubectl
        run: |
          curl -fsSLo kind https://kind.sigs.k8s.io/dl/v0.31.0/kind-linux-amd64
          chmod +x kind && sudo mv kind /usr/local/bin/kind
          curl -fsSLo kubectl "https://dl.k8s.io/release/$(curl -fsSL https://dl.k8s.io/release/stable.txt)/bin/linux/amd64/kubectl"
          chmod +x kubectl && sudo mv kubectl /usr/local/bin/kubectl

      - name: Enable bridge netfilter
        run: |
          sudo modprobe br_netfilter
          sudo sysctl -w net.bridge.bridge-nf-call-iptables=1

      - name: Build flannel-rs image
        run: docker build -t flannel-rs:dev .

      - name: Run reconcile scenario
        run: bash tests/smoke/reconcile.sh flannel-rs
```

- [ ] **Step 2: Lint the YAML**

Run: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/ci.yml')); print('YAML OK')"`
Expected: `YAML OK`.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: add flanneld reconcile job (flannel-rs)"
```

---

## Task 3: Verify in CI

**Files:** none (operational — handled by the orchestrator at merge time).

- [ ] **Step 1: Full local gate + push (orchestrator)**

`cargo fmt --all -- --check` (unchanged — no Rust touched). Open a PR (or push to a branch) so the new `reconcile` job runs alongside the existing ones; confirm `reconcile (flannel-rs)` is green and the other jobs are unaffected.

- [ ] **Step 2: Confirm on main**

After merge, confirm the `main` CI run is green across `test`, `smoke (flannel-go)`, `smoke (flannel-rs)`, `sig-network conformance (flannel-rs)`, and the new `reconcile (flannel-rs)`.

---

## Self-Review

**Spec coverage:**
- Trigger (delete flannel.1 + restart flanneld → new VtepMAC) → Task 1 reconcile.sh "perturb". ✓
- Baseline (peer has OLD_MAC) → "baseline" block. ✓
- Reconcile assertion (peer fdb gains NEW_MAC, loses OLD_MAC) → "assert peer M reconciled". ✓
- Connectivity preserved → final curl assert. ✓
- Reuse cluster_up + workload; EXIT teardown + ERR diagnostics → reconcile.sh structure. ✓
- CI job, flannel-rs only, needs: test → Task 2. ✓
- MAC parse via jsonpath + python3 → `vtep_mac`. ✓
- Retry windows for the 10s poll → `retry` helper (30–120s). ✓
- Existing asserts/scripts untouched → only new file + additive CI job. ✓

**Placeholder scan:** no TBD/TODO; all bash/YAML concrete.

**Consistency:** `N` = server pod's node (churned), `M` = client's node (reconciling peer), `OLD_MAC`/`NEW_MAC` from `vtep_mac`, `fdb_has`/`fdb_missing` helpers, `cluster_up`/`cluster_down` from cluster.sh — used consistently. Job name `reconcile (flannel-rs)` matches the spec.

**Known risk:** reconcile timing on a slow CI runner — mitigated by generous `retry` windows + the ERR-trap diagnostics; the local run (Task 1 Step 3) validates the mechanism before CI.
