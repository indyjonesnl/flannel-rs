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

# kindest/node images no longer bundle the CNI "bridge" plugin, which flannel's
# default delegate requires. Install the canonical upstream plugin (pinned +
# checksum-verified) onto every node. Idempotent.
CNI_VER="v1.6.2"
CNI_TGZ="cni-plugins-linux-amd64-${CNI_VER}.tgz"
CNI_SHA256="b8e811578fb66023f90d2e238d80cec3bdfca4b44049af74c374d4fae0f9c090"
install_cni_bridge() {
  local tmp; tmp="$(mktemp -d)"
  curl -fsSL -o "$tmp/$CNI_TGZ" \
    "https://github.com/containernetworking/plugins/releases/download/${CNI_VER}/${CNI_TGZ}"
  echo "${CNI_SHA256}  $tmp/$CNI_TGZ" | sha256sum -c - \
    || { echo "FAIL: CNI plugins checksum mismatch"; rm -rf "$tmp"; exit 1; }
  tar -xzf "$tmp/$CNI_TGZ" -C "$tmp" ./bridge ./host-local
  for node in $(kind get nodes --name flannel-rs); do
    docker cp "$tmp/bridge"     "$node:/opt/cni/bin/bridge"
    docker cp "$tmp/host-local" "$node:/opt/cni/bin/host-local"
    docker exec "$node" chmod +x /opt/cni/bin/bridge /opt/cni/bin/host-local
  done
  rm -rf "$tmp"
  echo "OK: CNI bridge/host-local installed on all nodes"
}

cleanup() { kind delete cluster --name flannel-rs >/dev/null 2>&1 || true; }
trap cleanup EXIT

kind create cluster --config "$ROOT/deploy/kind-cluster.yaml"
install_cni_bridge
[ "$VARIANT" = "flannel-rs" ] && kind load docker-image flannel-rs:dev --name flannel-rs
kubectl --context "$CTX" apply -f "$MANIFEST"
kubectl --context "$CTX" -n kube-flannel rollout status ds/kube-flannel-ds --timeout=180s
kubectl --context "$CTX" wait --for=condition=Ready nodes --all --timeout=180s
kubectl --context "$CTX" apply -f "$ROOT/tests/smoke/workload.yaml"
# Give workloads room to pull images + start on a cold node before asserting,
# so assert.sh's shorter rollout-status timeouts don't race the first pull.
kubectl --context "$CTX" rollout status deploy/smoke-server --timeout=240s
kubectl --context "$CTX" rollout status deploy/smoke-client --timeout=240s
# Assert 4 resolves a Service name via CoreDNS. CoreDNS only becomes Ready once
# the pod network is up, which lags the flannel rollout; without waiting, the
# in-pod resolver can time out on its first query and the assert flakes. This is
# variant-agnostic (applies to flannel-go and flannel-rs identically), like the
# image-pull buffer above.
kubectl --context "$CTX" -n kube-system rollout status deploy/coredns --timeout=180s
bash "$ROOT/tests/smoke/assert.sh"
echo "SMOKE PASSED: $VARIANT"
