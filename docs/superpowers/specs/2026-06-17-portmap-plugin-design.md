# portmap CNI plugin (Rust) — Design

**Date:** 2026-06-17
**Status:** Approved (brainstorming)
**Milestone:** 2d — port the `portmap` CNI plugin, the last Go binary on the data path.

## Context

After M2c the per-pod chain is `flannel`(Rust) → `bridge`(Rust) → `host-local`(Rust),
with `portmap`(Go) chained last for hostPort support. Porting portmap makes the
entire Flannel stack Rust — no Go binary is exec'd on the data path.

portmap is a *capability* plugin: it only acts when a pod declares a `hostPort`.
Our current smoke/conformance pods use no hostPort, so portmap is effectively
unverified by the existing suite. This milestone therefore also adds a hostPort
test so the Rust portmap is genuinely exercised, not rubber-stamped.

Reference: containernetworking/plugins `plugins/meta/portmap` (studied during
design): a chained plugin that reads `prevResult` + `runtimeConfig.portMappings`,
installs nat-table DNAT (+ hairpin SNAT) chains, and relays prevResult.

## Scope (flannel-subset)

Implement: DNAT `hostPort → podIP:containerPort` (tcp + udp, IPv4) via nat
PREROUTING + OUTPUT; a hairpin MASQUERADE rule so a pod can reach its own
hostPort; idempotent DEL; reuse the nft/legacy backend detection from flanneld's
ipmasq.

Out of scope (YAGNI): IPv6, `conditionsV4/V6`, `hostIP` filtering,
`externalSetMarkChain`, configurable `markMasqBit`, the `snat` toggle, and a full
CHECK (conflist is `cniVersion 0.3.1`, which never invokes CHECK).

## Crate structure

- `crates/cni-portmap/` — binary `portmap`, sync (pure iptables shelling, no
  netlink). Modules: `main` (dispatch), `config` (prevResult + portMappings
  parse), `rules` (build/apply/remove the nat rules).
- Reuses `cni`: `env::CniArgs`, `error::CniError`, `version::VersionResult`,
  `result::CniResult` (Deserialize, to read prevResult).

### Shared `cni::iptables` helper

Add `cni::iptables`: backend detection (pick `iptables-nft` vs `iptables-legacy`
to match kube-proxy's active backend, mirroring flanneld's ipmasq) plus thin
runners: `ensure_chain`, `ensure_rule` (`-C` then `-A`), `delete_rule`,
`flush_delete_chain`, all using `iptables --wait -t nat`. portmap uses it.

> flanneld's existing `crates/flanneld/src/ipmasq.rs` keeps its own backend
> detection for now; deduplicating it onto `cni::iptables` is a tracked
> follow-up (avoid touching the working daemon this milestone).

## Behavior (chained plugin)

### ADD
1. Parse stdin: `prevResult` (a `CniResult`) → pod IPv4 (`ips[0].address`);
   `runtimeConfig.portMappings: [{hostPort, containerPort, protocol}]`.
2. If `portMappings` is empty → relay `prevResult` unchanged, exit 0.
3. Detect the iptables backend.
4. In the nat table, mirroring Go's chain structure (clean, IP-free teardown):
   - ensure top chain `CNI-HOSTPORT-DNAT`, with jumps to it from `PREROUTING`
     and `OUTPUT`;
   - create per-container chain `CNI-DN-<short id>`; jump to it from
     `CNI-HOSTPORT-DNAT`; inside, one DNAT rule per mapping
     (`-p <proto> --dport <hostPort> -j DNAT --to-destination <podIP>:<containerPort>`);
   - install a hairpin MASQUERADE rule for traffic from the pod to its own
     hostPort (so a pod reaching its own published port is SNAT'd).
5. Relay `prevResult` unchanged (chained-plugin contract).

### DEL
Flush + delete the per-container chain and remove its jump from
`CNI-HOSTPORT-DNAT`; remove the hairpin rule. No IP needed. Idempotent (missing
chain/rule = success).

### CHECK
Minimal success stub (0.3.1 never calls it).

### VERSION
Advertise 0.3.0/0.3.1.

Protocols: tcp + udp. IPv4 only.

## hostPort verification (the milestone's distinguishing work)

Because the existing suite never exercises hostPort, add an explicit test that
runs for BOTH variants (so it verifies portmap AND Go/Rust parity):

- `tests/smoke/workload.yaml`: add a `hostport-server` pod (agnhost netexec,
  `containerPort: 80`) with `hostPort: 31180`.
- `tests/smoke/assert.sh`: **assert 5** — find the hostport pod's node and
  `docker exec <node> curl -sS --max-time 5 127.0.0.1:31180/hostname`, confirming
  it reaches the pod. The existing 4 asserts are unchanged.
- `bash tests/smoke/run.sh flannel-go` and `... flannel-rs` both run assert 5 →
  the Go node-shipped portmap and our Rust portmap must each satisfy it.
- Bonus: check whether any `[sig-network] [Conformance]` spec already exercises
  hostPort (extra coverage), but the smoke assert is the deterministic guarantee.

## Integration (swap the last Go binary)

- `Dockerfile`: build `cni-portmap`; bake `/opt/cni/bin/portmap`.
- `deploy/flannel-rs.yaml`: extend `install-cni-plugins-rs` to also install
  `portmap`: `cp -f /opt/cni/bin/flannel /opt/cni/bin/host-local
  /opt/cni/bin/bridge /opt/cni/bin/portmap /host/opt/cni/bin/`.
- `flannel-go` variant untouched (uses the kind node's Go portmap) — parity
  baseline intact.
- After M2d: zero Go CNI binaries on the data path; the whole stack is Rust.

## Error handling

- Empty `portMappings` → no-op success (relay prevResult).
- Missing pod IP in prevResult → CNI error (code 7).
- iptables backend not found / rule failure → CNI error (code 7) with stderr.
- DEL idempotent (ignore missing chains/rules).
- Never panic on stdin/env — emit a CNI error JSON.

## Testing

- **Unit (pure, no root):**
  - parse `prevResult` → pod IPv4 (from a real bridge/host-local result fixture).
  - parse `runtimeConfig.portMappings` (hostPort/containerPort/protocol).
  - DNAT rule-spec construction from (containerID, mapping, podIP) → the exact
    iptables argument vector; chain-name derivation (≤ iptables 28-char limit).
- **Integration:** smoke (now with assert 5) + conformance, BOTH variants. The
  hostPort assert directly drives the Rust portmap's DNAT path.

## Verification gate

- Full local CI gate green BEFORE any push: `cargo fmt --all -- --check`;
  `RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets -- -D warnings`;
  `cargo build --workspace --locked`; `cargo test --workspace --locked`.
- `bash tests/smoke/run.sh flannel-rs` and `... flannel-go` both green including
  assert 5.
- `bash tests/conformance/run.sh flannel-rs` green.
- CI green on the PR / main.

## Out of scope / later

- Dedupe flanneld's ipmasq backend detection onto `cni::iptables`.
- Bridge follow-ups (proper Result with `interfaces`; hairpin via sysfs).
- Daemon extensions (host-gw/wireguard backends, etcd, IPv6/dual-stack).
