#!/usr/bin/env bash
set -euo pipefail
VARIANT="${1:?usage: run.sh <flannel-go|flannel-rs>}"

# Shared kind cluster bring-up (create + CNI bridge + bridge netfilter + load
# image if rs + apply manifest + wait ds/nodes). Exposes ROOT, CTX, cluster_up,
# cluster_down. Also used by tests/conformance/run.sh.
# shellcheck source=../lib/cluster.sh
source "$(cd "$(dirname "$0")/../lib" && pwd)/cluster.sh"

# Workload image — preloaded into the cluster so pods never wait on a registry
# pull (slow/contended CI runners otherwise time out the rollout). Must match
# tests/smoke/workload.yaml.
AGNHOST="registry.k8s.io/e2e-test-images/agnhost:2.47"
preload_workload_image() {
  docker pull "$AGNHOST"
  kind load docker-image "$AGNHOST" --name "$CLUSTER_NAME"
  echo "OK: preloaded $AGNHOST into cluster"
}

dump_diagnostics() {
  echo "================ SMOKE DIAGNOSTICS ($VARIANT) ================"
  kubectl --context "$CTX" get nodes -o wide || true
  kubectl --context "$CTX" get pods -A -o wide || true
  echo "--- recent events ---"
  kubectl --context "$CTX" get events -A --sort-by=.lastTimestamp | tail -40 || true
  echo "--- flannel daemonset logs ---"
  kubectl --context "$CTX" -n kube-flannel logs ds/kube-flannel-ds --tail=80 --all-containers || true
  echo "--- coredns logs ---"
  kubectl --context "$CTX" -n kube-system logs -l k8s-app=kube-dns --tail=40 || true
  echo "============================================================="
}

trap cluster_down EXIT
trap dump_diagnostics ERR

cluster_up "$VARIANT"
preload_workload_image
kubectl --context "$CTX" apply -f "$ROOT/tests/smoke/workload.yaml"
# Images are preloaded, so the rollout only waits on scheduling + start.
kubectl --context "$CTX" rollout status deploy/smoke-server --timeout=300s
kubectl --context "$CTX" rollout status deploy/smoke-client --timeout=300s
# Assert 4 resolves a Service name via CoreDNS. CoreDNS only becomes Ready once
# the pod network is up, which lags the flannel rollout; without waiting, the
# in-pod resolver can time out on its first query and the assert flakes. This is
# variant-agnostic (applies to flannel-go and flannel-rs identically), like the
# image-pull buffer above.
kubectl --context "$CTX" -n kube-system rollout status deploy/coredns --timeout=180s
# Tell the asserts which backend is active (host-gw has no overlay device).
case "$VARIANT" in
  flannel-rs-hostgw) export BACKEND=host-gw ;;
  *) export BACKEND=vxlan ;;
esac
bash "$ROOT/tests/smoke/assert.sh"
echo "SMOKE PASSED: $VARIANT"
