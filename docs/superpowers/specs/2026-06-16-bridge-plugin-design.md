# bridge CNI plugin (Rust) — Design

**Date:** 2026-06-16
**Status:** Approved (brainstorming)
**Milestone:** 2c — port the `bridge` CNI plugin (the heavy per-pod netlink piece).

## Context

After M2b the per-pod chain is `flannel`(Rust) → `bridge`(Go) → `host-local`(Rust).
`bridge` (containernetworking/plugins) is the remaining heavy binary: it wires
each pod's network — node bridge, veth pair, namespace entry, IP assignment,
routes, gateway. Porting it leaves only `portmap` (M2d) in Go. Every pod and
every conformance spec exercises bridge, so the existing harness verifies it
fully — the risk is in the code, not the proof.

Reference: containernetworking/plugins `plugins/main/bridge/bridge.go` (studied
during design). The flannel meta-plugin (M2b) hands bridge a narrow delegate
config, so we implement only that subset.

## Scope (flannel-subset)

Implement what flannel's delegate uses: `name` (bridge), `mtu`, `isGateway`,
`isDefaultGateway`, `hairpinMode`, and `ipam` (delegate to host-local). The
delegate sets `ipMasq:false` (flanneld installs masquerade rules), so **bridge
does no iptables at all**.

Out of scope (YAGNI): `vlan`/`vlanTrunk`, `promiscMode`, `macspoofchk`,
`portIsolation`, `forceAddress`, IPv6 / DAD, `disableContainerInterface`,
`ipMasq` inside bridge, and a full CHECK (our conflist is `cniVersion 0.3.1`,
which never invokes CHECK — CHECK is a documented minimal success stub).

## Crate structure

- `crates/cni-bridge/` — binary `bridge`, a **tokio** binary (rtnetlink is async).
  Suggested modules: `main` (dispatch), `config` (NetConf parse), `bridge`
  (ensure bridge), `veth` (veth pair + names), `netns` (setns/thread runner),
  `ipam` (parse the delegate Result + apply addr/routes).

### Shared `cni` library refactors (reused by bridge)

- Move `run_delegate` + `DelegateOutput` from `cni-flannel` into `cni`
  (`cni::delegate`); update cni-flannel to import from there. bridge uses it to
  exec the `host-local` IPAM plugin.
- Add `Deserialize` to `cni::result::{CniResult, IpResult}` so bridge can parse
  host-local's Result (allocated IP, gateway, routes).

## Behavior

### ADD
1. Parse config: `name` (default `cni0`; flannel passes `cbr0`), `mtu`,
   `isGateway`, `isDefaultGateway`, `hairpinMode`, `ipam.type`.
2. `ensure_bridge(name, mtu)` — create the bridge link if absent, set MTU, bring
   up. Idempotent.
3. Exec IPAM ADD (`host-local`) via the shared delegate runner → parse
   `CniResult` → pod IP (CIDR), gateway, routes.
4. Set up veth + container interface (see "Netns mechanics").
5. `isGateway` → assign the gateway IP (`.1`) to the bridge; enable
   `net.ipv4.ip_forward`.
6. `ipMasq` is false → no iptables.
7. Emit the CNI Result (interfaces: bridge, host veth, container veth; ips;
   routes) to stdout for the chained `portmap`.

### DEL
Exec IPAM DEL; enter `CNI_NETNS` (if provided) and delete `CNI_IFNAME` (removes
the veth pair, host end auto-deleted). Best-effort/idempotent (ignore
not-found). No iptables.

### CHECK
Minimal success stub (0.3.1 never calls it).

### VERSION
Advertise 0.3.0/0.3.1.

## Netns mechanics (the risk center)

`CNI_NETNS` is a path (bind-mounted netns or `/proc/<pid>/ns/net`). Netlink
sockets bind to the netns of the creating thread, so container-side work runs on
a dedicated OS thread that `setns`'d in:

- **Host ns (main thread):** ensure bridge; create the veth pair (temp names,
  both ends in host ns); move the container end into `CNI_NETNS` via
  `setns_by_fd`; bring the host end up; `set master` to the bridge; set hairpin
  on that bridge port.
- **Container ns (dedicated thread):** open `/proc/self/ns/net` (to restore),
  `setns(netns_fd, CLONE_NEWNET)`, build a **current-thread** tokio runtime there
  (so the rtnetlink socket is in the container ns), then: rename temp →
  `CNI_IFNAME`, bring up, `addr add` the pod IP, add routes + a default route via
  the gateway. Restore the host ns and join the thread before returning. Mirrors
  Go's `ns.Do` (lock-thread + setns).

**Netlink ops:** `link().add().bridge(name)` / `.veth(a,b)`; `link().set(idx)`
with `.mtu/.up/.master/.setns_by_fd/.name`; hairpin on the bridge port;
`address().add()`; `route().add()` (incl. default `0.0.0.0/0 via <gw>`);
`net.ipv4.ip_forward` written via `/proc/sys`. Host veth name is derived
deterministically from the container ID, truncated to the 15-char `IFNAMSIZ`
limit.

**Crates:** `rtnetlink`, `netlink-packet-route`, `tokio`, and `nix` (`sched` for
`setns`, `fcntl`/`fs` for opening the netns).

## Error handling

- `ensure_bridge` / veth ops idempotent where sensible.
- On ADD failure *after* veth creation, tear down the created veth so a retry is
  not blocked by a leftover interface (mirrors Go's deferred cleanup).
- DEL is best-effort: a missing interface / netns is success.
- The container-ns thread propagates errors back via `join()` → mapped to a CNI
  error.
- Never panic on stdin/env input — emit a CNI error JSON with an appropriate
  code.

## Testing

- **Unit (pure, no root):**
  - `config` parse: bridge name default `cni0`; `isGateway`/`isDefaultGateway`/
    `hairpinMode`/`mtu` read correctly.
  - `CniResult` **deserialize** from a real host-local 0.3.1 result fixture →
    extracts ip (CIDR), gateway, routes.
  - veth host-name derivation: deterministic from containerID, ≤ 15 chars.
  - gateway / default-route derivation.
- **Integration:** smoke + conformance for the flannel-rs variant now exercise
  the Rust bridge for **every pod** — cross-node ping/HTTP/ClusterIP plus all 47
  `[sig-network] [Conformance]` specs. Bridge bugs surface as connectivity
  failures. flannel-go baseline unchanged.
- Netns/netlink code needs root + a real netns, so it is verified by the
  integration suite on kind, not unit tests.

## Integration (swap one binary)

- `Dockerfile`: build `cni-bridge`; bake `/opt/cni/bin/bridge`.
- `deploy/flannel-rs.yaml`: extend the single `install-cni-plugins-rs`
  initContainer to also install `bridge`:
  `cp -f /opt/cni/bin/flannel /opt/cni/bin/host-local /opt/cni/bin/bridge /host/opt/cni/bin/`.
- `tests/lib/cluster.sh` `install_cni_bridge` stays (provides the Go base for the
  flannel-go variant; the rs DaemonSet overwrites all three Rust binaries).
- `flannel-go` variant untouched — parity baseline intact.
- After M2c the rs chain is **flannel(Rust) → bridge(Rust) → host-local(Rust)**;
  only `portmap`(Go) remains.

## Verification gate

- Full local CI gate green BEFORE any push (standing rule): `cargo fmt --all --
  --check`; `RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets --
  -D warnings`; `cargo build --workspace --locked`; `cargo test --workspace
  --locked`.
- `bash tests/smoke/run.sh flannel-rs` → `SMOKE PASSED`.
- `bash tests/conformance/run.sh flannel-rs` → 47 specs pass with the Rust bridge.
- `bash tests/smoke/run.sh flannel-go` still green.
- CI green on the PR / main.

## Out of scope / later

- M2d: port `portmap` (chained plugin; needs a new hostPort test to verify, since
  current conformance doesn't exercise hostPort).
- Daemon extensions (host-gw/wireguard backends, etcd, IPv6/dual-stack).
