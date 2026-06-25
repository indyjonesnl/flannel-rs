# flannel-rs

Flannel, reimplemented in Rust — the **whole stack**: the `flanneld` daemon (VXLAN
backend, kube-subnet-manager) *and* the per-pod CNI plugin chain.

[![CI](https://github.com/indyjonesnl/flannel-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/indyjonesnl/flannel-rs/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/indyjonesnl/flannel-rs?logo=github&label=release)](https://github.com/indyjonesnl/flannel-rs/releases/latest)
[![sig-network conformance](https://img.shields.io/badge/sig--network%20conformance-47%2F0-brightgreen)](https://github.com/indyjonesnl/flannel-rs/actions/workflows/ci.yml)
[![sig-node conformance](https://img.shields.io/badge/sig--node%20conformance-105%2F0-brightgreen)](https://github.com/indyjonesnl/flannel-rs/actions/workflows/ci.yml)

A drop-in replacement for upstream Go [Flannel](https://github.com/flannel-io/flannel).
It speaks the same node annotations, writes the same `/run/flannel/subnet.env`, and
uses the same CNI conflist — so it swaps in behind a standard Flannel install with no
other changes. The difference: **no Go binary runs on the data path** — every piece is
Rust.

## What it does

**Control plane** — `flanneld` (DaemonSet, `hostNetwork`, `NET_ADMIN`): leases the
node's `PodCIDR`, creates the `flannel.1` VXLAN device, publishes its VTEP MAC + public
IP to node annotations, watches peers and installs route + neigh + fdb, writes
`subnet.env`, and installs `ip-masq` iptables rules.

**Data path** — the CNI chain kubelet invokes per pod, now all Rust:

```
flannel (meta)  → reads subnet.env, builds the delegate config, execs ↓
bridge          → node bridge + veth pair + container-netns setup, execs ↓
host-local      → allocates the pod IP from the node /24
portmap         → hostPort DNAT (+ hairpin), chained last
```

```
flannel-rs (DaemonSet)
├── flanneld (control plane)
│   ├── subnet manager (kube)   read Node.Spec.PodCIDR; lease /24; node annotations
│   ├── vxlan backend (netlink) flannel.1; per peer: route + neigh + fdb
│   ├── ip-masq (iptables)      MASQUERADE pod egress leaving the pod network
│   └── subnet writer           /run/flannel/subnet.env
└── CNI plugins (installed to /opt/cni/bin)
    flannel → bridge → host-local → portmap   (all Rust)
```

## Install

The released DaemonSet manifest is self-contained (namespace, RBAC, ConfigMap, DS):

```sh
kubectl apply -f https://raw.githubusercontent.com/indyjonesnl/flannel-rs/main/deploy/flannel-rs-release.yaml
```

It pulls the multi-arch image `ghcr.io/indyjonesnl/flannel-rs` (linux/amd64 + arm64).
The cluster must have a pod CIDR and its default CNI disabled (flannel-rs is the CNI).

- **Image:** `ghcr.io/indyjonesnl/flannel-rs:v0.1.0` (also `:latest`)
- **Binaries:** static-musl tarballs (amd64/arm64) on the
  [Releases](https://github.com/indyjonesnl/flannel-rs/releases) page — for bare-metal /
  non-DaemonSet installs (the five binaries go in `/opt/cni/bin` + `flanneld`).

## Status

**Complete for IPv4.** The entire Flannel stack is Rust and gated in CI by smoke parity
(vs Go flannel) plus the upstream `[sig-network] [Conformance]` and `[sig-node]
[Conformance]` suites, and a curated `[sig-network]` slice (overlay MTU + ip-masq).

| Crate | Role | Lang |
| --- | --- | --- |
| `crates/flanneld`     | daemon: subnet lease, VXLAN, ip-masq, `subnet.env` | Rust |
| `crates/cni`          | shared CNI lib (env/config/result/error/version/delegate/iptables) | Rust |
| `crates/cni-flannel`  | `flannel` meta-plugin (reads `subnet.env`, delegates) | Rust |
| `crates/cni-bridge`   | `bridge` (node bridge + veth + container netns) | Rust |
| `crates/cni-host-local` | `host-local` IPAM | Rust |
| `crates/cni-portmap`  | `portmap` (hostPort DNAT + hairpin) | Rust |

## Evidence it works

Both gated in [CI](https://github.com/indyjonesnl/flannel-rs/actions/workflows/ci.yml) on
every push and PR:

- **Parity smoke harness** — the *same* `tests/smoke/assert.sh` runs against upstream Go
  flannel **and** flannel-rs and must pass identically: cross-node pod-to-pod ping,
  cross-node TCP/HTTP, ClusterIP service, pod-IP-in-`PodCIDR` + `flannel.1` device/routes,
  and a **hostPort** check (exercises `portmap`). Go baseline is locked green first, then
  flannel-rs must match.
- **Upstream sig-network conformance** — [Hydrophone](https://github.com/kubernetes-sigs/hydrophone)
  runs `[sig-network] [Conformance]`: **47 specs, 0 failures, none skipped** — intra-pod &
  node-pod connectivity (http/udp), ClusterIP/NodePort/ExternalName Services, session
  affinity, cluster DNS, Endpoints/EndpointSlices, and HostPort. flannel-rs passes the
  same set as Go flannel.
- **Upstream sig-node conformance** — Hydrophone runs `[sig-node] [Conformance]`: **105
  specs, 0 failures, none skipped** — pod lifecycle, liveness/readiness/startup probes,
  init & ephemeral containers, Secret/ConfigMap env, downward API, and runtime handling.
  Proves the Rust CNI swap doesn't regress kubelet/runtime pod handling.
- **sig-network extra (MTU + ip-masq)** — Hydrophone runs the non-`[Conformance]`
  `[sig-network]` specs that the conformance subset under-tests: **4 specs, 0 failures** —
  "large requests: http/udp" (large payloads over the VXLAN overlay → exercises the
  computed `subnet.env` MTU) and "Internet connection for containers"
  (`Feature:Networking-IPv4`/`-DNS` → exercises `flanneld`'s ip-masq MASQUERADE on pod
  egress to off-cluster destinations).

CI jobs: `fmt + clippy + test` → `smoke (flannel-go/flannel-rs/flannel-rs-hostgw)`,
`sig-network conformance (flannel-rs/flannel-rs-hostgw)`,
`sig-node conformance (flannel-rs/flannel-rs-hostgw)`,
`sig-network extra (MTU + ip-masq) (flannel-rs/flannel-rs-hostgw)`.

> Note: same-node pod→Service traffic only traverses iptables when `br_netfilter` is
> loaded (`net.bridge.bridge-nf-call-iptables=1`). The harness ensures it on each node.

### Reproduce locally

Prerequisites: `kind`, `kubectl`, `docker`, `cargo` (plus `hydrophone` for conformance).

```sh
cargo test                                # unit tests
docker build -t flannel-rs:dev .          # build the dev image

bash tests/smoke/run.sh flannel-go        # baseline (upstream Go flannel)
bash tests/smoke/run.sh flannel-rs        # parity check (all-Rust chain)
bash tests/smoke/run.sh flannel-rs-hostgw # host-gw (no overlay)
bash tests/conformance/run.sh flannel-rs sig-network            # vxlan sig-network
bash tests/conformance/run.sh flannel-rs-hostgw sig-network     # host-gw sig-network
bash tests/conformance/run.sh flannel-rs sig-node               # vxlan sig-node
bash tests/conformance/run.sh flannel-rs-hostgw sig-node        # host-gw sig-node
bash tests/conformance/run.sh flannel-rs sig-network-extra      # vxlan MTU + ip-masq
bash tests/conformance/run.sh flannel-rs-hostgw sig-network-extra # host-gw MTU + ip-masq
```

Each script creates a 3-node kind cluster, installs the CNI, runs its checks, and tears
the cluster down.

## Releasing

Push a `vX.Y.Z` tag → the
[release workflow](.github/workflows/release.yml) cross-compiles static-musl binaries
(amd64 + arm64 via `cargo-zigbuild`), publishes checksummed tarballs to the GitHub
Release, and pushes the multi-arch image to GHCR. `workflow_dispatch` with
`publish=false` is a build-only dry run.

## Roadmap

Done: VXLAN backend · **`host-gw` backend** · kube-subnet-manager · ip-masq · real MTU ·
bootstrap backoff · minimal RBAC · **all four CNI plugins in Rust** · smoke parity ·
sig-network conformance · sig-node conformance · watch-based peer updates ·
multi-arch image + binary releases.

> **Backends:** `vxlan` (default, overlay) and `host-gw` (direct routing, no overlay —
> requires all nodes on one L2 subnet). Select via `net-conf.json` `Backend.Type`.

Next / not yet:

- IPv6 / dual-stack ([#5](https://github.com/indyjonesnl/flannel-rs/issues/5)),
- additional backends (`wireguard`),
- bridge: emit a full `Result` with `interfaces`; hairpin via sysfs,
- NetworkPolicy; image/SBOM signing (cosign).

## Inspiration

- [flannel-io/flannel](https://github.com/flannel-io/flannel) — reference behavior
  (`subnet.env`, annotation keys, VXLAN backend, masquerade) and
  [cni-plugin](https://github.com/flannel-io/cni-plugin) / the
  [containernetworking plugins](https://github.com/containernetworking/plugins) the Rust
  ports follow.
- [rk8s libnetwork](https://github.com/rk8s-dev/rk8s/tree/main/project/libnetwork) — Rust
  CNI / networking reference.

## Where it's used

Built to be the CNI for
[indyjonesnl/rusternetes](https://github.com/indyjonesnl/rusternetes) ("kubernetes,
reimplemented in Rust"). That integration is the intended direction; rusternetes does not
wire it in yet.

## Repo layout

```
crates/flanneld/      the daemon (Rust)
crates/cni/           shared CNI library
crates/cni-*/         the Rust CNI plugins (flannel, bridge, host-local, portmap)
deploy/               kind config; flannel-go (baseline), flannel-rs (:dev), flannel-rs-release (GHCR)
tests/                smoke harness, sig-network + sig-node conformance, shared cluster lib
.github/workflows/    ci.yml (test+smoke+conformance), release.yml (binaries+image)
docs/                 design specs + implementation plans
```

## License

MIT — see [LICENSE](LICENSE).
