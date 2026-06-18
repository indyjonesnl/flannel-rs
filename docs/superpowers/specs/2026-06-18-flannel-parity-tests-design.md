# flannel-rs ↔ Go Flannel behaviour-parity tests — design

- **Date:** 2026-06-18
- **Status:** approved (design)
- **Scope:** first spec of a two-spec effort. Pure-logic behaviour parity. Lease/backend parity is deferred to a follow-up spec.

## Motivation

flannel-rs reimplements the whole Flannel stack in Rust. We want unit tests that
assert flannel-rs reproduces Go Flannel's *observable behaviour*, so future
refactors can't silently drift from the reference implementation.

Both upstreams (`flannel-io/flannel`, `containernetworking/plugins`) are
Apache-2.0; flannel-rs is MIT. We therefore **reimplement** the tests in Rust —
reusing the upstream input vectors and expected outputs as a behavioural
reference — rather than copying any Go source. No Apache-licensed code enters the
repo.

## Key reframing (why this isn't "port their test files")

Inspecting the upstream sources changed the shape of the work:

- **`flannel pkg/subnet/config_test.go`** is almost entirely `SubnetLen` /
  `SubnetMin` / `SubnetMax` / IPv6 *defaulting* — flannel's etcd-mode subnet
  allocator. flannel-rs deliberately replaces that with the kube `PodCIDR`
  (kube-subnet-manager), so those tests assert behaviour flannel-rs intentionally
  does **not** have. Not portable as-is.
- **`flannel pkg/ip/*_test.go`** tests flannel's own `IP4` / `IP4Net` types.
  flannel-rs uses the `ipnetwork` crate + `std::net` instead; re-testing a
  third-party crate is not our job.

So the parity target is: **lock the observable behaviour flannel-rs actually
owns, using upstream vectors where they apply, and document each intentional
divergence.** Each test cites its upstream origin and, where relevant, the
divergence.

## Conventions

- Tests are `#[cfg(test)] mod` additions in the crate that owns the logic. No new
  test infrastructure, no Go toolchain, no FFI.
- Every parity test carries a one-line provenance comment, e.g.:
  `// parity: flannel pkg/subnet/config_test.go — divergence: subnet from kube PodCIDR, not SubnetLen`
- Vectors (inputs/expected outputs) are transcribed; Go code is not.

## §2 — Config parsing (`crates/flanneld/src/config.rs`)

Lock flannel-rs's `net-conf.json` contract.

**Small non-test additions (§5 decision: included):**
- `NetConf::parse` validates that `Network` is a syntactically valid IPv4 CIDR and
  that `Backend.Type` is present. (Non-vxlan rejection already exists via
  `classify_net_conf`.) Validation returns the existing error type; no new
  dependencies.

**Tests:**
1. Parses a well-formed vxlan config (`Network` + `Backend.Type`). *(have)*
2. Rejects malformed JSON. *(have, in main.rs `classify`)*
3. Rejects non-vxlan backend. *(have)*
4. Rejects missing `Backend` / missing `Type`.
5. Rejects a `Network` that is not a valid IPv4 CIDR (e.g. `"not-a-cidr"`,
   `"10.244.0.0/33"`).
6. **Divergence test:** a config carrying `SubnetLen` / `SubnetMin` / `SubnetMax`
   parses successfully and those fields are ignored — documenting that the
   per-node subnet comes from kube `PodCIDR`, not from this config.

## §3 — IP / subnet math (`crates/cni-host-local/src/alloc.rs`, `crates/flanneld/src/subnet.rs`)

Parity for the IP behaviours flannel-rs owns, using flannel `pkg/ip` intent
(`Contains`, `Overlaps`, `IncrementIP`, host enumeration) applied to our code.

**Small non-test addition (§5 decision: included):**
- A `subnet ⊆ network` containment helper (used to assert `FLANNEL_SUBNET` lies
  within `FLANNEL_NETWORK`). Pure function, unit-tested.

**Tests (gap-fill on top of existing `alloc` tests):**
1. Host enumeration order within a `/24`: first usable skips network + gateway.
   *(partly have)*
2. Network, gateway, and broadcast addresses are never handed out.
3. Edge prefixes: `/31` and `/32` ranges (degenerate host counts) behave sanely —
   no panic, and allocation returns "no free IP" rather than handing out the
   network/broadcast address.
4. Increment / wraparound to find the next free IP. *(have — confirm edges)*
5. Containment: a valid PodCIDR `/24` is reported inside its cluster `/16`; a
   `/24` outside the cluster network is reported outside (flannel `Contains`
   intent).

## §4 — CNI plugin gap-fill (`crates/cni-host-local`, `crates/cni-portmap`)

Fill gaps from `containernetworking/plugins` table-driven vectors. Anything
requiring root / network namespaces stays out (bridge remains at config/result-shape
level, as today).

**host-local:**
- Allocation respects an explicit range start/end (not just whole-subnet).
- Last IP in a range is allocatable; one past it is not.
- Reserved / `.0` / broadcast addresses excluded.

**portmap:**
- Exact iptables rule strings for DNAT, hairpin-mark, and localhost-mark across
  both `tcp` and `udp`. *(partly have — extend to udp + assert full arg vectors)*

## Out of scope (this spec)

- Lease/backend behaviour (subnet lease acquire/renew/expiry, VXLAN device, kube
  annotation round-trip). Needs a faked kube-API and is closer to integration; it
  overlaps with the existing `reconcile` CI scenario, smoke parity, and
  conformance. Deferred to a follow-up spec.
- IPv6 / dual-stack parity (flannel-rs is IPv4-only).
- Re-testing third-party crates (`ipnetwork`, `serde`).

## Verification

- `cargo test --workspace` green; new tests fail if the asserted behaviour
  regresses.
- Local CI gate before push: `cargo fmt`, `cargo clippy --all-targets -D warnings`,
  build, test.

## Follow-up

A second spec will cover lease/backend parity once a kube-API faking approach is
chosen.
