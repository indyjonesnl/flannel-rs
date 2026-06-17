#!/usr/bin/env bash
set -euo pipefail
CTX="kind-flannel-rs"
k() { kubectl --context "$CTX" "$@"; }

# Retry a command until it succeeds or the window elapses. Network state
# (routes, ARP/fdb, kube-proxy/CoreDNS programming) converges asynchronously and
# is slower on contended CI runners; the assert must still ultimately succeed —
# this only tolerates first-attempt races, it does not weaken what is verified.
retry() {
  local timeout="$1"; shift
  local deadline=$((SECONDS + timeout))
  local attempt=1
  until "$@"; do
    if [ "$SECONDS" -ge "$deadline" ]; then
      echo "FAIL: \`$*\` did not succeed within ${timeout}s"
      return 1
    fi
    echo "  (attempt $attempt failed; retrying...)"
    attempt=$((attempt + 1))
    sleep 3
  done
}

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
retry 60 k exec "$CLI_POD" -- ping -c3 -W2 "$SRV_IP"

echo "== assert 3: cross-node TCP/HTTP =="
retry 60 k exec "$CLI_POD" -- curl -sS --max-time 5 "http://$SRV_IP:80/hostname"

echo "== assert 4: ClusterIP service =="
retry 90 k exec "$CLI_POD" -- curl -sS --max-time 5 "http://smoke-server:80/hostname"

echo "== assert 5: hostPort (portmap DNAT) =="
k rollout status deploy/hostport-server --timeout=120s
HP_POD=$(k get pod -l app=hostport-server -o jsonpath='{.items[0].metadata.name}')
HP_NODE=$(k get pod "$HP_POD" -o jsonpath='{.spec.nodeName}')
echo "hostport pod $HP_POD on node $HP_NODE"
# hostPort 31180 is published on the node the pod runs on; curl it from that node.
retry 60 docker exec "$HP_NODE" curl -sS --max-time 5 "http://127.0.0.1:31180/hostname"
echo "OK: hostPort reachable on $HP_NODE"

echo "ALL ASSERTS PASSED"
