# flannel-rs — Design

**Date:** 2026-06-15
**Status:** Approved (brainstorming)

## Goal

A drop-in replacement for Flannel's `flanneld` daemon, written in Rust. It must
be swappable for upstream Go Flannel in a Kubernetes cluster with no change to
the per-pod CNI data path. Success is defined by a single smoke-test harness
that passes identically against upstream Go Flannel and against flannel-rs.

### Locked decisions

| Decision | Choice |
|----------|--------|
| Backend | VXLAN (default flannel backend) |
| Lease store | kube-subnet-manager (read `Node.Spec.PodCIDR`, peer info in node annotations) |
| CNI scope | Daemon only. Reuse upstream `flannel` meta-plugin + `bridge` + `host-local` binaries |
| Smoke asserts | cross-node ping, cross-node TCP/HTTP, ClusterIP service, pod-IP-in-CIDR + routes/device |
| Cluster | `kind`, 1 control-plane + 2 workers, default CNI disabled |
| Delivery | Parity-harness first: lock Go-flannel baseline, then make flannel-rs match |

## Architecture (flanneld port only)

```
flannel-rs (DaemonSet, hostNetwork, NET_ADMIN)
├── subnet manager (kube)   read Node.Spec.PodCIDR; lease /24 for self
│                           write annotations: backend-type=vxlan,
│                           backend-data={VtepMAC}, public-ip
├── node watcher (kube)     watch Nodes → peer lease events
├── vxlan backend (netlink) create flannel.1 (vxlan, learning off);
│                           on peer add/del:
│                             route   peer-podCIDR via flannel.1
│                             neigh   peer-vtep-ip -> peer-VtepMAC (PERMANENT)
│                             fdb     peer-VtepMAC -> peer-public-ip
└── subnet writer           write /run/flannel/subnet.env:
                              FLANNEL_NETWORK, FLANNEL_SUBNET,
                              FLANNEL_MTU, FLANNEL_IPMASQ
```

Per-pod wiring stays upstream: `/etc/cni/net.d/10-flannel.conflist`
(flannel meta-plugin → `bridge` + `host-local`) reads `subnet.env`. flannel-rs
never touches individual pods.

## Stack

- `kube` + `k8s-openapi` — Node get/watch, annotation patch, PodCIDR.
- `rtnetlink` + `netlink-packet-route` — vxlan link, routes, neigh, fdb.
- `tokio` — async runtime.
- `serde` / `serde_json` — backend-data annotation + net-conf.json.
- `anyhow` / `thiserror` — errors. `tracing` — structured logs.
- Single binary `flanneld`. Cargo workspace so bridge/IPAM may be added later.

## Data flow (startup → steady state)

1. Read env: `NODE_NAME` (downward API); cluster net config from ConfigMap
   `kube-flannel-cfg` (`net-conf.json`: `Network`, `Backend.Type=vxlan`).
2. Get own Node → `Spec.PodCIDR` = my lease subnet.
3. Create/ensure `flannel.1` vxlan dev (VNI 1, dstport 8472, local = node IP).
   Read its MAC.
4. PATCH own Node annotations:
   `flannel.alpha.coreos.com/backend-type=vxlan`,
   `backend-data={"VtepMAC":"<mac>"}`,
   `public-ip=<nodeIP>`,
   `kube-subnet-manager-managed=true`.
5. Assign `<podCIDR network addr>/32` to flannel.1, bring up.
6. Write `/run/flannel/subnet.env`.
7. Watch Nodes. For each peer (not self) with complete annotations → install
   route + neigh + fdb. On delete → remove. On change → reconcile.

## Reconciliation / error handling

- **Idempotent installs** — `EEXIST` on link/route/neigh/fdb treated as success;
  diffs reconciled each event.
- **Watch restart** — on watcher error/desync, re-list all Nodes, rebuild peer
  set, prune stale entries (full resync, not just deltas).
- **Annotation race** — peer missing VtepMAC/public-ip → skip, retry on next
  update.
- **Fatal vs retry** — netlink/kube errors retry with backoff; missing PodCIDR
  or malformed net-conf = fatal exit (DaemonSet restarts, surfaces misconfig).
- **Crash safety** — kernel state survives restart; full resync on boot makes
  restarts safe.

## Repo layout

```
flannel-rs/
├── Cargo.toml              # workspace
├── crates/flanneld/        # the binary
│   └── src/{main,config,subnet,vxlan,netlink,watch}.rs
├── deploy/
│   ├── kind-cluster.yaml   # 1 cp + 2 workers, default CNI disabled, podSubnet
│   ├── flannel-go.yaml     # upstream reference DaemonSet
│   └── flannel-rs.yaml     # our DaemonSet (same configmap/RBAC, our image)
├── Dockerfile              # static musl build + cni-plugins binaries baked in
├── tests/smoke/            # harness scripts (bash + kubectl)
└── docs/superpowers/specs/
```

## Test strategy (the contract)

Harness `tests/smoke/run.sh <flannel-go|flannel-rs>`:

1. `kind create` (CNI disabled, `podSubnet=10.244.0.0/16`, 3 nodes).
2. Apply chosen flannel manifest; wait DaemonSet ready + Nodes `Ready`.
3. Deploy `server` (HTTP) + `client` Deployments with pod-anti-affinity →
   forced cross-node.
4. Asserts (all four):
   - **route/device**: on each kind node (`docker exec`) `flannel.1` exists,
     routes to peer CIDRs present; each pod IP ∈ its node `PodCIDR`.
   - **ping**: `kubectl exec client -- ping -c3 <server pod IP>` (cross-node).
   - **TCP/HTTP**: `kubectl exec client -- curl <server pod IP>:80`.
   - **ClusterIP**: `kubectl exec client -- curl <svc VIP>:80`.
5. Teardown. Exit non-zero on any fail.

Same script, two args → identical asserts. flannel-rs is done when both pass.

## Milestones

1. Harness + kind config; **Go flannel green** (baseline locked).
2. flanneld skeleton: config load, own-lease, subnet.env write, annotations
   (pods get IPs, same-node works).
3. VXLAN backend: device + peer route/neigh/fdb. **Cross-node green**.
4. Resync/restart robustness; flannel-rs passes full harness = Go flannel.

## Out of scope (YAGNI)

host-gw / wireguard / ipip backends, etcd, IPv6 / dual-stack, Windows,
multi-cluster, the Rust bridge/IPAM port (later milestone), NetworkPolicy.
