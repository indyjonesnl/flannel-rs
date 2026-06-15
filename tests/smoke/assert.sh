#!/usr/bin/env bash
set -euo pipefail
CTX="kind-flannel-rs"
k() { kubectl --context "$CTX" "$@"; }

echo "== wait for workloads =="
k rollout status deploy/smoke-server --timeout=120s
k rollout status deploy/smoke-client --timeout=120s

SRV_POD=$(k get pod -l app=smoke-server -o jsonpath='{.items[0].metadata.name}')
CLI_POD=$(k get pod -l app=smoke-client -o jsonpath='{.items[0].metadata.name}')
SRV_IP=$(k get pod "$SRV_POD" -o jsonpath='{.status.podIP}')
SRV_NODE=$(k get pod "$SRV_POD" -o jsonpath='{.spec.nodeName}')
CLI_NODE=$(k get pod "$CLI_POD" -o jsonpath='{.spec.nodeName}')

echo "server=$SRV_POD@$SRV_NODE ip=$SRV_IP  client=$CLI_POD@$CLI_NODE"
[ "$SRV_NODE" != "$CLI_NODE" ] || { echo "FAIL: pods co-located"; exit 1; }

echo "== assert 1: pod IP in node PodCIDR + flannel.1 + routes =="
for node in $(k get nodes -o jsonpath='{.items[*].metadata.name}'); do
  docker exec "$node" ip -d link show flannel.1 >/dev/null \
    || { echo "FAIL: flannel.1 missing on $node"; exit 1; }
done
SRV_CIDR=$(k get node "$SRV_NODE" -o jsonpath='{.spec.podCIDR}')
python3 - "$SRV_IP" "$SRV_CIDR" <<'PY'
import sys, ipaddress
ip, cidr = sys.argv[1], sys.argv[2]
assert ipaddress.ip_address(ip) in ipaddress.ip_network(cidr), f"{ip} not in {cidr}"
print(f"OK: {ip} in {cidr}")
PY
docker exec "$CLI_NODE" ip route | grep -q "$SRV_CIDR" \
  || { echo "FAIL: no route to $SRV_CIDR on $CLI_NODE"; exit 1; }
echo "OK: route + device present"

echo "== assert 2: cross-node ping =="
k exec "$CLI_POD" -- ping -c3 -W2 "$SRV_IP"

echo "== assert 3: cross-node TCP/HTTP =="
k exec "$CLI_POD" -- curl -sS --max-time 5 "http://$SRV_IP:80/hostname"

echo "== assert 4: ClusterIP service =="
k exec "$CLI_POD" -- curl -sS --max-time 5 "http://smoke-server:80/hostname"

echo "ALL ASSERTS PASSED"
