# flannel-rs

Flannel's `flanneld`, reimplemented in Rust — VXLAN backend, kube-subnet-manager.

![CI](https://github.com/indyjonesnl/flannel-rs/actions/workflows/ci.yml/badge.svg)

A drop-in replacement for the upstream Go [`flanneld`](https://github.com/flannel-io/flannel)
daemon. It speaks the same node annotations, writes the same `/run/flannel/subnet.env`,
and delegates per-pod wiring to the unchanged upstream CNI plugins — so it swaps in
behind the standard Flannel CNI config with no other changes.

## What it does

Per node, the daemon:

- leases the node's `PodCIDR` (kube-subnet-manager — reads `Node.Spec.PodCIDR`),
- creates the `flannel.1` VXLAN device and publishes its VTEP MAC + public IP to node
  annotations,
- watches all nodes and installs the route + neigh + fdb entries for each peer,
- writes `/run/flannel/subnet.env` for the CNI plugins,
- installs flannel-style `ip-masq` iptables rules.

```
flannel-rs (DaemonSet, hostNetwork, NET_ADMIN)
├── subnet manager (kube)   read Node.Spec.PodCIDR; lease /24 for self
│                           write annotations: backend-type=vxlan,
│                           backend-data={VtepMAC}, public-ip
├── node watcher (kube)     poll Nodes → peer lease events
├── vxlan backend (netlink) create flannel.1; per peer:
│                             route  peer-podCIDR via flannel.1
│                             neigh  peer-vtep-ip -> peer-VtepMAC
│                             fdb    peer-VtepMAC -> peer-public-ip
├── ip-masq (iptables)      MASQUERADE pod egress leaving the pod network
└── subnet writer           write /run/flannel/subnet.env
```

Per-pod networking (veth, bridge, IPAM) is **not** reimplemented — it stays with the
upstream `flannel` / `bridge` / `host-local` CNI plugins, exactly as in a normal Flannel
install. See the [design doc](docs/superpowers/specs/2026-06-15-flannel-rs-design.md).

## Status

**Working milestone 1.** Single `flanneld` binary, VXLAN backend, in-cluster
kube-subnet-manager, ip-masq, real underlay-MTU detection, bootstrap retry/backoff, and
least-privilege RBAC (`nodes: get/list/patch` only).

| Module (`crates/flanneld/src/`) | Responsibility |
| --- | --- |
| `config.rs`     | parse `net-conf.json` |
| `subnet.rs`     | render `subnet.env`; compute VXLAN MTU |
| `annotation.rs` | node annotation keys + `backend-data` serde |
| `peer.rs`       | peer model + reconcile diff |
| `netlink.rs`    | VXLAN device + route/neigh/fdb ops |
| `kube_mgr.rs`   | own node, annotation patch, peer list |
| `ipmasq.rs`     | flannel-style MASQUERADE rules |
| `main.rs`       | bootstrap + reconcile loop |

## Evidence it works

flannel-rs is validated two ways, both gated in [CI](https://github.com/indyjonesnl/flannel-rs/actions/workflows/ci.yml)
on every push and PR:

- **Parity smoke harness** — the *same* `tests/smoke/assert.sh` runs against upstream Go
  flannel **and** flannel-rs and must pass identically: cross-node pod-to-pod ping,
  cross-node TCP/HTTP, ClusterIP service reachability, and pod-IP-in-`PodCIDR` +
  `flannel.1` device/routes present. The Go baseline is locked green first, then
  flannel-rs must match it.
- **Upstream conformance** — [Hydrophone](https://github.com/kubernetes-sigs/hydrophone)
  runs the Kubernetes e2e suite focused on `[sig-network] [Conformance]`:
  **47 specs, 0 failures, none skipped** — intra-pod & node-pod connectivity (http/udp),
  ClusterIP/NodePort/ExternalName Services, session affinity, cluster DNS, and
  Endpoints/EndpointSlices. flannel-rs passed all of them with no CNI changes.

CI jobs: `fmt + clippy + test` (15 unit tests) → `smoke (flannel-go)`,
`smoke (flannel-rs)`, `sig-network conformance (flannel-rs)`.

> Note: same-node pod→Service traffic only traverses iptables when `br_netfilter` is
> loaded (`net.bridge.bridge-nf-call-iptables=1`). The harness ensures this on each node;
> CI surfaced it on runners that don't load the module by default.

### Reproduce locally

Prerequisites: `kind`, `kubectl`, `docker`, `cargo` (plus `hydrophone` for conformance).

```sh
cargo test                              # unit tests
docker build -t flannel-rs:dev .        # build the image

bash tests/smoke/run.sh flannel-go      # baseline (upstream Go flannel)
bash tests/smoke/run.sh flannel-rs      # parity check
bash tests/conformance/run.sh flannel-rs # sig-network conformance
```

Each script creates a 3-node kind cluster, installs the CNI, runs its checks, and tears
the cluster down.

## Roadmap

Done: VXLAN backend · kube-subnet-manager · ip-masq · real MTU · bootstrap backoff ·
minimal RBAC · smoke parity · sig-network conformance in CI.

Next / not yet:

- additional backends (`host-gw`, `wireguard`),
- IPv6 / dual-stack,
- port the `bridge` + `host-local` IPAM to Rust (currently upstream binaries),
- watch-based peer updates (currently a 10s poll),
- NetworkPolicy, multi-arch images.

## Inspiration

- [flannel-io/flannel](https://github.com/flannel-io/flannel) — reference behavior:
  `subnet.env` format, annotation keys, the VXLAN backend, and masquerade rules.
- [rk8s libnetwork](https://github.com/rk8s-dev/rk8s/tree/main/project/libnetwork) —
  Rust CNI / networking reference.

## Where it's used

flannel-rs is built to be the CNI for
[indyjonesnl/rusternetes](https://github.com/indyjonesnl/rusternetes) ("kubernetes,
reimplemented in Rust"). That integration is the intended direction; rusternetes does not
wire it in yet.

## Repo layout

```
crates/flanneld/   the daemon (Rust)
deploy/            kind cluster config + flannel-go / flannel-rs manifests
tests/             smoke harness, sig-network conformance, shared cluster lib
.github/           CI workflow
docs/              design spec + implementation plan
```

## License

MIT — see [LICENSE](LICENSE).
