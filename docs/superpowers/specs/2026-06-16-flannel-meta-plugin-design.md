# flannel CNI meta-plugin (Rust) — Design

**Date:** 2026-06-16
**Status:** Approved (brainstorming)
**Milestone:** 2b — port Flannel's own remaining Go binary (the `flannel` CNI meta-plugin).

## Context

After M2a, the per-pod CNI chain is `flannel`(Go) → `bridge`(Go) → `host-local`(Rust).
The `flannel` meta-plugin (flannel-io/cni-plugin) is the only binary Flannel itself
ships besides `flanneld`, and it is still Go. This milestone ports it to Rust.
After the swap the chain becomes **flannel(Rust) → bridge(Go) → host-local(Rust)**,
and the `ghcr.io/flannel-io/flannel-cni-plugin` image dependency is eliminated from
the flannel-rs deployment.

## Scope

Flannel-subset, IPv4. The meta-plugin reads `/run/flannel/subnet.env` (written by
our flanneld), merges it with the conflist's `delegate{}` block, and execs the
delegate (default `bridge`). Out of scope (YAGNI): IPv6 / dual-stack
(`FLANNEL_IPV6_*`), runtime-config / capability plumbing beyond passthrough, and
persisting the generated delegate config (we reconstruct on DEL — see below).

## Crate structure

- `crates/cni-flannel/` — binary `flannel`, reusing `crates/cni` (`env::CniArgs`,
  `error::CniError`, `version::VersionResult`).
  - `subnetenv.rs` — parse `/run/flannel/subnet.env` →
    `SubnetEnv { network: String, subnet: String, mtu: u32, ipmasq: bool }`.
  - `delegate.rs` — pure `build_delegate(net_conf, &SubnetEnv) -> serde_json::Value`.
  - `exec.rs` — locate + exec the delegate binary via `CNI_PATH`, relay I/O.
  - `main.rs` — VERSION/ADD/DEL/CHECK dispatch.

## Behavior

### subnet.env (inverse of flanneld's writer)
Parse lines `FLANNEL_NETWORK`, `FLANNEL_SUBNET`, `FLANNEL_MTU`, `FLANNEL_IPMASQ`.

### build_delegate (pure)
Input: the flannel plugin's stdin netconf (`{ name, cniVersion, type:"flannel",
delegate:{...} }`) and the parsed `SubnetEnv`. Start from the `delegate{}` object
(a `serde_json::Value`), then set **only fields the user did not provide**:
- `name` = conf `name`; `cniVersion` = conf `cniVersion`.
- `type` = `"bridge"` if unset.
- `ipMasq` = `!subnet_env.ipmasq` if unset (flanneld already installs masq rules,
  so the delegate must NOT also masquerade).
- `mtu` = `subnet_env.mtu` if unset.
- `isGateway` = `true` if `type == "bridge"` and unset.
- `ipam` (always set): `{ "type":"host-local",
  "ranges":[[{"subnet": subnet_env.subnet}]],
  "routes":[{"dst": subnet_env.network}] }`.

User-provided delegate fields (e.g. our conflist's `hairpinMode`,
`isDefaultGateway`) pass through unchanged.

### Commands
- **VERSION**: print `VersionResult::supported()` (0.3.0/0.3.1).
- **ADD**: parse stdin + subnet.env → `build_delegate` → exec delegate `ADD` →
  relay the delegate's Result to stdout (the chained `portmap` consumes it).
- **DEL**: reconstruct the delegate config identically from stdin + subnet.env →
  exec delegate `DEL`. No on-disk state. (CNI passes the same netconf on DEL as
  ADD; teardown is keyed by containerID/netns, so subnet drift is irrelevant.)
- **CHECK**: same reconstruction → exec delegate `CHECK`.

### Delegate exec (`exec.rs`)
Split `CNI_PATH` on `:`; find the first directory containing an executable named
by the delegate `type`. Spawn it with the same `CNI_*` environment, write the
delegate JSON to its stdin, capture stdout + exit status. On non-zero status,
relay the child's stdout (its CNI error JSON) and exit non-zero; on success, relay
the Result. Binary not found in `CNI_PATH` → CNI error.

## Integration (swap one binary, verify by conformance)

- `Dockerfile`: build `cni-flannel`; bake `/opt/cni/bin/flannel`.
- `deploy/flannel-rs.yaml`: replace the `install-cni-plugin` initContainer (which
  pulls `ghcr.io/flannel-io/flannel-cni-plugin`) with one using `flannel-rs:dev`
  that installs the Rust `flannel`, folding in the existing `host-local` copy.
  Result: no upstream Go flannel-cni-plugin image is used. The Go `bridge`
  (installed by `tests/lib/cluster.sh` `install_cni_bridge`) remains.
- `flannel-go` variant untouched — parity baseline intact.

## Error handling

- Missing/unreadable `subnet.env` → CNI error code 11 (try again later); kubelet
  retries until flanneld writes it.
- Delegate binary not found in `CNI_PATH` → CNI error code 5.
- Delegate exits non-zero → relay its stdout + propagate non-zero exit.
- Never panic on malformed stdin/env/subnet.env — emit a CNI error JSON.

## Testing

- **Unit (pure, no root):**
  - `subnetenv`: parses a well-formed file; rejects/handles missing keys.
  - `build_delegate`: ipam ranges/routes from subnet/network; `mtu` from MTU;
    `ipMasq=false` when `FLANNEL_IPMASQ=true`; `isGateway=true` default;
    `type` defaults to `bridge`; user-set fields (mtu/type/ipMasq) are NOT
    overwritten.
- **Exec (no root):** a fake delegate shell script on a tempdir `CNI_PATH` that
  records its stdin + `CNI_*` env; assert `flannel` execs it with the right
  command/env and relays its stdout.
- **Integration:** existing smoke + conformance for the flannel-rs variant now
  exercise the Rust meta-plugin (47 `[sig-network] [Conformance]` specs).
  flannel-go baseline unchanged.

## Verification gate

- `cargo test` (all crates) pass; `cargo fmt --all -- --check` clean (added to the
  local pre-merge gate — M2a lesson); `cargo clippy --workspace --all-targets
  -- -D warnings` clean.
- `bash tests/smoke/run.sh flannel-rs` → `SMOKE PASSED`.
- `bash tests/conformance/run.sh flannel-rs` → conformance passes with the Rust
  meta-plugin in the chain.
- `bash tests/smoke/run.sh flannel-go` still green.
- CI green on the PR.

## Out of scope / later

- M2c: port `bridge` (heavy netlink: node bridge, veth, netns, IP assign, routes,
  hairpin). After that, also port/replace `portmap`, then no Go binary is exec'd
  on the data path.
- Daemon extensions (host-gw/wireguard backends, etcd, IPv6/dual-stack).
