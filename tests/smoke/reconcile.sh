#!/usr/bin/env bash
# Change a peer node's VtepMAC at runtime and assert other nodes reconcile their
# overlay (fdb) to the new MAC, with cross-node pod connectivity preserved.
# Exercises flanneld's reconcile/del_peer/add_peer path. flannel-rs only.
set -euo pipefail
VARIANT="${1:-flannel-rs}"
case "$VARIANT" in
  flannel-rs | flannel-rs-hostgw) ;;
  *) echo "reconcile test is flannel-rs / flannel-rs-hostgw only"; exit 2 ;;
esac
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

if [ "$VARIANT" = "flannel-rs-hostgw" ]; then
  # host-gw reconvergence: M must hold a direct route to N's pod CIDR via N's
  # node IP. Drop it and restart M's flanneld; flanneld must re-install it.
  SRV_CIDR=$(k get node "$N" -o jsonpath='{.spec.podCIDR}')
  N_IP=$(k get node "$N" -o jsonpath='{.status.addresses[?(@.type=="InternalIP")].address}')
  echo "host-gw: peer M=$M must route $SRV_CIDR via N($N) IP $N_IP"
  route_ok() { docker exec "$M" ip route show "$SRV_CIDR" | grep -q "via $N_IP"; }

  echo "== baseline: M has a direct route to N's pod CIDR via N's node IP =="
  retry 30 route_ok
  echo "OK: baseline route present"

  echo "== perturb: delete that route on M, then restart M's flanneld =="
  docker exec "$M" ip route del "$SRV_CIDR" || true
  ! route_ok || { echo "FAIL: route still present right after delete"; exit 1; }
  FPOD=$(k -n kube-flannel get pod -l app=flannel --field-selector "spec.nodeName=$M" -o jsonpath='{.items[0].metadata.name}')
  echo "restarting flanneld pod $FPOD on $M"
  k -n kube-flannel delete pod "$FPOD"
  k -n kube-flannel rollout status ds/kube-flannel-ds --timeout=180s

  echo "== assert M reconverged: route to N's pod CIDR via N's node IP is back =="
  retry 120 route_ok
  echo "OK: host-gw route reconverged"

  echo "== assert cross-node connectivity preserved =="
  retry 60 k exec "$CLI_POD" -- curl -sS --max-time 5 "http://$SRV_IP:80/hostname"
  echo "OK: cross-node connectivity preserved"
  echo "RECONCILE PASSED: $VARIANT"
  exit 0
fi

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
