#!/usr/bin/env bash
# Shared kind cluster bring-up / teardown for flannel-rs test harnesses.
# Source this file, then call cluster_up <variant> / cluster_down.
#
# Exposes (set by cluster_up, usable by callers):
#   CLUSTER_NAME  - kind cluster name (flannel-rs)
#   CTX           - kubectl context (kind-flannel-rs)
#   ROOT          - repo root
#
# Sourcing scripts are expected to run under `set -euo pipefail`.

CLUSTER_NAME="flannel-rs"
CTX="kind-flannel-rs"
# ROOT resolves to the repo root relative to this lib file (tests/lib -> ..  -> ..).
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

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
  for node in $(kind get nodes --name "$CLUSTER_NAME"); do
    docker cp "$tmp/bridge"     "$node:/opt/cni/bin/bridge"
    docker cp "$tmp/host-local" "$node:/opt/cni/bin/host-local"
    docker exec "$node" chmod +x /opt/cni/bin/bridge /opt/cni/bin/host-local
  done
  rm -rf "$tmp"
  echo "OK: CNI bridge/host-local installed on all nodes"
}

# Same-node pod -> Service -> pod traffic is bridged and only hits iptables
# (kube-proxy DNAT/masq) when bridge netfilter is enabled. Ensure it on each
# node so in-cluster DNS works regardless of pod placement. Best-effort: the
# module must be loaded on the host (CI loads it; dev machines usually have it).
ensure_bridge_netfilter() {
  for node in $(kind get nodes --name "$CLUSTER_NAME"); do
    docker exec "$node" sh -c \
      'modprobe br_netfilter 2>/dev/null || true; \
       sysctl -w net.bridge.bridge-nf-call-iptables=1 2>/dev/null || true; \
       sysctl -w net.bridge.bridge-nf-call-ip6tables=1 2>/dev/null || true'
  done
  echo "OK: bridge-nf-call-iptables ensured on all nodes"
}

# Stand up the kind cluster with the requested flannel variant fully rolled out
# and all nodes Ready. Resolves $MANIFEST as a side effect.
#   cluster_up <flannel-go|flannel-rs|flannel-rs-hostgw>
cluster_up() {
  local variant="${1:?cluster_up: variant required}"
  case "$variant" in
    flannel-go) MANIFEST="$ROOT/deploy/flannel-go.yaml" ;;
    flannel-rs) MANIFEST="$ROOT/deploy/flannel-rs.yaml" ;;
    flannel-rs-hostgw)
      # Same image + manifest as flannel-rs, but with net-conf.json's backend
      # switched to host-gw so the daemon boots host-gw from the start.
      MANIFEST="$(mktemp --suffix=-flannel-rs-hostgw.yaml)"
      sed 's/"Type": "vxlan"/"Type": "host-gw"/' "$ROOT/deploy/flannel-rs.yaml" > "$MANIFEST"
      ;;
    *) echo "unknown variant $variant"; exit 2 ;;
  esac

  kind create cluster --config "$ROOT/deploy/kind-cluster.yaml"
  install_cni_bridge
  ensure_bridge_netfilter
  case "$variant" in
    flannel-rs | flannel-rs-hostgw)
      kind load docker-image flannel-rs:dev --name "$CLUSTER_NAME" ;;
  esac
  kubectl --context "$CTX" apply -f "$MANIFEST"
  kubectl --context "$CTX" -n kube-flannel rollout status ds/kube-flannel-ds --timeout=180s
  kubectl --context "$CTX" wait --for=condition=Ready nodes --all --timeout=180s
  echo "OK: cluster up ($variant)"
}

cluster_down() {
  kind delete cluster --name "$CLUSTER_NAME" >/dev/null 2>&1 || true
}
