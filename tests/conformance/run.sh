#!/usr/bin/env bash
set -euo pipefail
VARIANT="${1:?usage: run.sh <flannel-go|flannel-rs> [sig-network|sig-node]}"
SUITE="${2:-sig-network}"

# Shared kind cluster bring-up (create + CNI bridge + bridge netfilter + load
# image if rs + apply manifest + wait ds/nodes). Same setup the smoke harness
# uses. Exposes ROOT, CTX, CLUSTER_NAME, cluster_up, cluster_down.
# shellcheck source=../lib/cluster.sh
source "$(cd "$(dirname "$0")/../lib" && pwd)/cluster.sh"

# Conformance image MUST match the cluster's k8s version (node image v1.35.0).
CONFORMANCE_IMAGE="registry.k8s.io/conformance:v1.35.0"

# Focus/skip per suite. Each suite gates a distinct slice of the upstream
# conformance set. Skips are regex (alternated); each entry MUST carry a
# one-line justification. Keep skip lists minimal so coverage is not silently
# reduced.
case "$SUITE" in
  sig-network)
    # All networking conformance tests: pod-to-pod connectivity, Services,
    # DNS, endpoints.
    FOCUS='\[sig-network\].*\[Conformance\]'
    # NOTE: cloud LoadBalancer tests are NOT tagged [Conformance], so the
    # focus already excludes them; no skip needed for those.
    SKIP=""
    ;;
  sig-node)
    # Node-level conformance: pod lifecycle, probes, init/ephemeral
    # containers, env from Secret/ConfigMap, downward API, runtime. Proves a
    # CNI swap doesn't regress kubelet/runtime pod handling.
    FOCUS='\[sig-node\].*\[Conformance\]'
    SKIP=""
    ;;
  *)
    echo "unknown suite: $SUITE (want sig-network|sig-node)" >&2
    exit 2
    ;;
esac

OUTPUT_DIR="$(cd "$(dirname "$0")" && pwd)/results/$SUITE"
mkdir -p "$OUTPUT_DIR"

# Hydrophone uses the current kubeconfig context. kind sets the current context
# to kind-flannel-rs on create, but be explicit: export a dedicated kubeconfig
# scoped to this cluster and point hydrophone at it.
KCFG="$OUTPUT_DIR/kubeconfig"

dump_diagnostics() {
  echo "============ CONFORMANCE DIAGNOSTICS ($SUITE/$VARIANT) ============"
  kubectl --context "$CTX" get nodes -o wide || true
  kubectl --context "$CTX" get pods -A -o wide || true
  echo "--- recent events ---"
  kubectl --context "$CTX" get events -A --sort-by=.lastTimestamp | tail -40 || true
  echo "--- flannel daemonset logs ---"
  kubectl --context "$CTX" -n kube-flannel logs ds/kube-flannel-ds --tail=80 --all-containers || true
  echo "--- coredns logs ---"
  kubectl --context "$CTX" -n kube-system logs -l k8s-app=kube-dns --tail=40 || true
  echo "--- conformance pod logs (tail) ---"
  kubectl --context "$CTX" -n conformance logs e2e-conformance-test --tail=80 || true
  echo "==========================================================="
}

trap cluster_down EXIT
trap dump_diagnostics ERR

cluster_up "$VARIANT"

# CoreDNS must be up before DNS conformance tests run; it lags the flannel
# rollout (pod network must be ready first).
kubectl --context "$CTX" -n kube-system rollout status deploy/coredns --timeout=180s

kind get kubeconfig --name "$CLUSTER_NAME" > "$KCFG"

# Parallelism: runner has 2 CPU; default to 2. Ginkgo/hydrophone runs [Serial]
# tests after the parallel phase, so this is safe.
PARALLEL="${CONFORMANCE_PARALLEL:-2}"

echo "Running hydrophone: image=$CONFORMANCE_IMAGE focus=$FOCUS parallel=$PARALLEL skip='${SKIP}'"

# NOTE: --cleanup is a standalone hydrophone command (mutually exclusive with
# --focus), not a post-run flag, so it is NOT passed here. Cluster teardown is
# handled by the EXIT trap (cluster_down); hydrophone removes its own
# conformance namespace between runs via --skip-preflight / fresh cluster.
HYDRO_ARGS=(
  --kubeconfig "$KCFG"
  --conformance-image "$CONFORMANCE_IMAGE"
  --focus "$FOCUS"
  --parallel "$PARALLEL"
  --output-dir "$OUTPUT_DIR"
)
if [ -n "$SKIP" ]; then
  HYDRO_ARGS+=(--skip "$SKIP")
fi

# Hydrophone exits non-zero if any focused test fails.
hydrophone "${HYDRO_ARGS[@]}"

echo "CONFORMANCE PASSED: $SUITE/$VARIANT"
