# flannel-rs — agent guide

Flannel reimplemented in Rust: the `flanneld` control-plane daemon **and** the full
per-pod CNI plugin chain. A drop-in replacement for upstream Go
[flannel](https://github.com/flannel-io/flannel) — same node annotations, same
`/run/flannel/subnet.env`, same CNI conflist — with no Go binary on the data path.

## Golden rules (read first)

1. **Never push red.** Before every push run the full local gate (below) AND, for any
   change to daemon runtime behaviour, the kind smoke harness. CI minutes are scarce.
2. **`cargo test` green ≠ it works.** The daemon's cluster behaviour (RBAC, kube access,
   netlink, the reconcile loop) is only proven by the kind jobs. Two real incidents this
   project hit (a missing `watch` RBAC verb; a vxlan-only test assertion) passed the cargo
   gate and failed only at runtime. **Run smoke locally for daemon-behaviour changes.**
3. **Parity, reimplemented — not copied.** Upstream flannel / containernetworking are
   Apache-2.0; this repo is MIT. Reuse their *behaviour and test vectors*, write original
   Rust. Cite the upstream source in a comment (e.g. `// parity: flannel pkg/...`).
4. **TDD.** Write the failing test first for pure logic; implement to green.

## Architecture

Single Cargo workspace (edition 2021, version inherited from `[workspace.package]`).

| Crate | Role |
|---|---|
| `crates/flanneld` | daemon: kube subnet-manager (leases node `PodCIDR`), VXLAN **or** host-gw backend, ip-masq, writes `subnet.env`, watches nodes → reconciles peers |
| `crates/cni` | shared CNI lib (env/config/result/error/version/delegate/iptables) |
| `crates/cni-flannel` | `flannel` meta-plugin (reads `subnet.env`, delegates) |
| `crates/cni-bridge` | `bridge` (node bridge + veth + container netns) |
| `crates/cni-host-local` | `host-local` IPAM |
| `crates/cni-portmap` | `portmap` (hostPort DNAT + hairpin) |

Data path (per pod, all Rust): `flannel → bridge → host-local → portmap`.

**Backends** (`net-conf.json` `Backend.Type`): `vxlan` (default, overlay) and `host-gw`
(direct routes via the host NIC, no overlay; needs all nodes on one L2 subnet). The peer
watch/reconcile loop and netlink layer are shared; the backend only changes the per-peer
action and the MTU.

Scope: **IPv4 only** (no IPv6/dual-stack yet).

## Build & test — the local gate

Run all four before any push (mirrors CI's `fmt + clippy + test` job, which gates the rest):

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings   # -D warnings is enforced in CI
cargo build --workspace --locked
cargo test --workspace --locked
```

Common gotcha clippy catches: a `#[cfg(test)] mod tests` placed before other items
(`items-after-test-module`) — put the test module **last** in the file.

## Daemon-behaviour changes need the kind harness too

If you touch `flanneld`'s runtime (RBAC, kube access patterns, netlink, bootstrap, the
reconcile loop, backends), the cargo gate is insufficient. Build the image and run smoke:

```sh
docker build -t flannel-rs:dev .
bash tests/smoke/run.sh flannel-go            # upstream baseline (lock green first)
bash tests/smoke/run.sh flannel-rs            # vxlan parity (all-Rust chain)
bash tests/smoke/run.sh flannel-rs-hostgw     # host-gw (no overlay)
bash tests/conformance/run.sh flannel-rs sig-network        # 47 specs
bash tests/conformance/run.sh flannel-rs sig-node           # 105 specs
bash tests/conformance/run.sh flannel-rs sig-network-extra  # MTU + ip-masq
bash tests/smoke/reconcile.sh flannel-rs                    # vxlan peer reconverge
bash tests/smoke/reconcile.sh flannel-rs-hostgw             # host-gw route reconverge
```

Each script creates a 3-node kind cluster, runs its checks, and tears down. Expect
`ALL ASSERTS PASSED` / `CONFORMANCE PASSED` / `RECONCILE PASSED`. Conformance needs
`hydrophone`; all need `kind`, `kubectl`, `docker`.

**RBAC gotcha:** the flannel `ClusterRole` verbs live in **two** manifests that must stay
in sync — `deploy/flannel-rs.yaml` (used by CI) and `deploy/flannel-rs-release.yaml`
(released installs). A change to kube access (e.g. adding `watch`) must update both.

## CI jobs (`.github/workflows/ci.yml`)

`fmt + clippy + test` → then (all `needs: test`): `kind smoke` matrix
(flannel-go / flannel-rs / flannel-rs-hostgw) · `sig-network conformance` ·
`sig-node conformance` · `sig-network extra (MTU + ip-masq)` ·
`flanneld reconcile` (flannel-rs + flannel-rs-hostgw). All gated on every push/PR.

## Testing conventions

- Unit tests are inline `#[cfg(test)] mod tests` at the **end** of the file (`use super::*;`).
- Pure logic (config parsing, IP/route decisions, MTU, annotation encode/decode) is the
  unit-test seam — extract pure functions from I/O so they test without a cluster. The
  netlink/kube I/O and the watch loop stay integration-tested by the kind harness.
- Smoke `assert.sh` is backend-aware via the `BACKEND` env var; host-gw asserts *no*
  `flannel.1` device.

## Contributing workflow

- Branch off `main` (don't commit to `main` directly). One feature per branch.
- Run the local gate (+ smoke for daemon changes) → push → open a PR; let CI validate.
- Commit messages: Conventional Commits (`feat(flanneld): …`, `test(host-gw): …`).
- Releases: push a `vX.Y.Z` tag → `release.yml` builds static-musl binaries + multi-arch
  GHCR image. The tag MUST equal `[workspace.package] version` (a release-job guard
  enforces this) — bump `Cargo.toml` before tagging.

## Roadmap (not yet done)

IPv6/dual-stack ([#5](https://github.com/indyjonesnl/flannel-rs/issues/5)); `wireguard`
backend; bridge full `Result` with `interfaces` + hairpin via sysfs; NetworkPolicy;
image/SBOM signing.

<!-- SPECKIT START -->
For additional context about technologies to be used, project structure,
shell commands, and other important information, read the current plan
<!-- SPECKIT END -->
