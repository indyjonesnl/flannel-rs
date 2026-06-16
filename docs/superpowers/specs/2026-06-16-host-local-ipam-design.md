# host-local IPAM (Rust) — Design

**Date:** 2026-06-16
**Status:** Approved (brainstorming)
**Milestone:** 2a — first step of rustifying the per-pod CNI chain.

## Context

flannel-rs currently replaces only `flanneld` (the control plane: subnet lease,
VXLAN backend, ip-masq). The per-pod CNI data path is still upstream Go: the
`flannel` meta-plugin, `bridge`, and `host-local` IPAM. The end goal is a 100%
Rust Flannel stack. This milestone ports the **host-local IPAM plugin** to Rust —
the smallest, most self-contained piece — and proves it by swapping only that one
binary into the chain and keeping the smoke + conformance suites green.

## Scope

Flannel-subset only. flannel's delegate config hands host-local a single range
(the node `/24`), a gateway, and a default route. We implement exactly that.
Out of scope (YAGNI): multiple range-sets, requested-IP via `CNI_ARGS`/`ips`
capability, `resolvConf`, per-range gateways, GC command.

## Crate structure

- `crates/cni/` — CNI spec **library**, reused by future plugins (`bridge`,
  meta-plugin):
  - env parsing: `CNI_COMMAND`, `CNI_CONTAINERID`, `CNI_NETNS`, `CNI_IFNAME`,
    `CNI_ARGS`, `CNI_PATH`.
  - stdin types: `NetConf { cni_version, name, ipam: IpamConfig }`,
    `IpamConfig { type, ranges, routes, data_dir, gateway? }`.
  - output types: CNI `Result` and `Error` JSON encodings for spec 0.3.1.
  - `version` dispatch helper.
- `crates/cni-host-local/` — the `host-local` **binary**: allocation logic +
  command dispatch. `src/{main,alloc,store}.rs` (dispatch / pure allocation /
  disk-backed lease store).
- Workspace `Cargo.toml` gains both members.

## Behavior

Input (from stdin NetConf `ipam`): `ranges[0][0].subnet` = node `/24`, optional
`gateway`, `routes`, `dataDir` (default `/var/lib/cni/networks`). Single range;
extra fields parsed-but-unused.

- **Gateway**: config `gateway` if present, else the first usable address
  (`.1`), reserved and never allocated.
- **Excluded** from allocation: network address (`.0`), broadcast (last),
  gateway.
- **Allocation**: sequential from `last_reserved_ip.0`, wrapping around the
  range; the first address with no lease file is selected.
- **On-disk format** (matches Go host-local, under `<dataDir>/<network-name>/`):
  - `<ip>` — lease file, contents `containerID\nifname`.
  - `last_reserved_ip.0` — last allocated IP.
  - `lock` — `flock`ed during ADD/DEL for concurrency safety (parallel kubelet
    invocations).

### Commands

- **ADD**: allocate, write lease, return `Result` (0.3.1):
  `ips:[{version:"4", address:"<ip>/<prefix>", gateway:"<gw>"}]`,
  `routes` (from config), `dns` (from config if any).
- **DEL**: remove lease file(s) whose contents match `containerID`+`ifname`.
  Idempotent — success if none found.
- **CHECK**: succeed iff a lease for `containerID`+`ifname` exists.
- **VERSION**: print `{cniVersion, supportedVersions:["0.3.0","0.3.1"]}`.

## Integration (swap one binary, verify by conformance)

- `Dockerfile`: build `crates/cni-host-local`; stage the `host-local` binary in
  the flannel-rs image.
- `deploy/flannel-rs.yaml`: the `install-cni-plugin` initContainer copies the
  Rust `host-local` onto each node's `/opt/cni/bin/host-local`, overwriting the
  Go binary that `tests/lib/cluster.sh`'s `install_cni_bridge` laid down as the
  base. The upstream Go `bridge` then execs our Rust `host-local` for IPAM.
- `flannel-go` variant is untouched (keeps Go host-local) — the parity baseline
  remains a true reference.
- No changes to `tests/smoke/assert.sh` or the conformance focus.

## Error handling

- Idempotent DEL.
- `flock` on `<dataDir>/<network>/lock` around ADD/DEL.
- Create the dataDir tree if missing.
- Never panic on malformed stdin/env — emit a CNI `Error` JSON to stdout with an
  appropriate code (e.g. invalid environment variables; range exhausted) and
  exit non-zero.

## Testing

- **Unit (pure, no root):**
  - `cni-host-local` allocation core: next-IP given subnet + leased set +
    last_reserved; exclusion of network/broadcast/gateway; wraparound;
    exhaustion → error.
  - `cni` lib: env parsing; `Result`/`Error` JSON round-trip against known-good
    0.3.1 fixtures.
- **Integration:** existing `smoke` + `conformance` CI jobs, now exercising the
  Rust `host-local` for the flannel-rs variant. Green across all 47 sig-network
  conformance specs = correct allocation, release on delete, no collisions.

## Verification gate

- `cargo test` (existing 15 + new unit tests) pass; `cargo clippy`/`fmt` clean.
- `bash tests/smoke/run.sh flannel-rs` → `SMOKE PASSED`.
- `bash tests/conformance/run.sh flannel-rs` → conformance passes with Rust IPAM.
- `bash tests/smoke/run.sh flannel-go` still green (untouched baseline).
- CI: all jobs green on the PR.

## Out of scope / later milestones

- 2b: port `bridge`. 2c: port the `flannel` meta-plugin. After 2c: drop the
  upstream `flannel-cni-plugin` initContainer + containernetworking installs.
- Daemon extensions (host-gw/wireguard backends, etcd, IPv6/dual-stack).
