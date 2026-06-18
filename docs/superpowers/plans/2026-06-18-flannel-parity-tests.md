# flannel-rs behaviour-parity tests (spec 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add Rust unit tests (and the minimal supporting code) that assert flannel-rs reproduces Go Flannel's observable behaviour for config parsing, IP/subnet math, and CNI plugin rule/allocation logic.

**Architecture:** Reimplement upstream behaviour as `#[cfg(test)] mod` additions in the crates that own each piece of logic. Two tasks add tiny non-test code (NetConf CIDR validation; a `subnet ⊆ network` helper wired into bootstrap); two tasks are test-only, locking current behaviour and covering untested branches. No Apache code is copied — only input/expected vectors, each cited in a provenance comment.

**Tech Stack:** Rust, `cargo test`, `serde_json`, the `ipnetwork` crate (already a `flanneld` dependency).

**Spec:** `docs/superpowers/specs/2026-06-18-flannel-parity-tests-design.md`

**Note on scope refinement:** §4's "host-local range start/end bounds" is intentionally dropped. flannel-rs's host-local allocates over the whole node subnet (its conflist sets no `rangeStart`/`rangeEnd`), matching flannel's real usage; adding range support would be a new feature, not a parity test. §4 host-local is therefore covered as edge/bounds tests in Task 3.

---

## File structure

- `crates/flanneld/src/config.rs` — `NetConf` parse + validation (Task 1).
- `crates/flanneld/src/subnet.rs` — `subnet_in_network` helper + tests (Task 2).
- `crates/flanneld/src/main.rs` — wire the helper into `try_bootstrap` (Task 2).
- `crates/cni-host-local/src/alloc.rs` — `Allocator` edge tests (Task 3).
- `crates/cni-portmap/src/rules.rs` — portmap rule-arg tests (Task 4).

---

## Task 1: NetConf config-contract validation + tests

**Files:**
- Modify: `crates/flanneld/src/config.rs`

- [ ] **Step 1: Write the failing tests**

Add these tests inside the existing `mod tests` in `crates/flanneld/src/config.rs` (after `parses_vxlan_net_conf`):

```rust
    // parity: flannel pkg/subnet/config_test.go — flannel-rs only honours
    // Network + Backend.Type (per-node subnet comes from kube PodCIDR).
    #[test]
    fn rejects_invalid_network_cidr() {
        assert!(NetConf::parse(r#"{"Network":"not-a-cidr","Backend":{"Type":"vxlan"}}"#).is_err());
        assert!(NetConf::parse(r#"{"Network":"10.244.0.0/33","Backend":{"Type":"vxlan"}}"#).is_err());
    }

    #[test]
    fn rejects_missing_backend_or_type() {
        assert!(NetConf::parse(r#"{"Network":"10.244.0.0/16"}"#).is_err());
        assert!(NetConf::parse(r#"{"Network":"10.244.0.0/16","Backend":{}}"#).is_err());
    }

    // Divergence test: flannel's SubnetLen/SubnetMin/SubnetMax drive its etcd-mode
    // allocator. flannel-rs ignores them (subnet = kube PodCIDR); they must parse
    // without error and not affect the result.
    #[test]
    fn ignores_subnet_allocator_fields() {
        let nc = NetConf::parse(
            r#"{"Network":"10.244.0.0/16","Backend":{"Type":"vxlan"},"SubnetLen":28,"SubnetMin":"10.244.5.0","SubnetMax":"10.244.8.0"}"#,
        )
        .unwrap();
        assert_eq!(nc.network, "10.244.0.0/16");
        assert_eq!(nc.backend.kind, "vxlan");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p flanneld config::tests::rejects_invalid_network_cidr`
Expected: FAIL — `rejects_invalid_network_cidr` panics (parse currently returns `Ok` for `"not-a-cidr"`). (`rejects_missing_backend_or_type` and `ignores_subnet_allocator_fields` will already pass via serde; that's fine — they lock behaviour.)

- [ ] **Step 3: Add Network CIDR validation to `NetConf::parse`**

Replace the `parse` method in `crates/flanneld/src/config.rs`:

```rust
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        let nc: Self = serde_json::from_str(s)?;
        // Network (the cluster CIDR) must be a well-formed IPv4 CIDR. The per-node
        // subnet still comes from the kube PodCIDR, not from this field.
        nc.network
            .parse::<ipnetwork::Ipv4Network>()
            .map_err(|e| anyhow::anyhow!("invalid Network CIDR {:?}: {e}", nc.network))?;
        Ok(nc)
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p flanneld config::tests`
Expected: PASS — all `config::tests` green (existing + 3 new).

- [ ] **Step 5: Commit**

```bash
git add crates/flanneld/src/config.rs
git commit -m "test(flanneld): lock net-conf contract; validate Network CIDR

parity: flannel pkg/subnet/config_test.go (scoped to honoured fields).
Rejects invalid Network CIDR + missing Backend/Type; documents that
SubnetLen/SubnetMin/SubnetMax are ignored (subnet from kube PodCIDR)."
```

---

## Task 2: `subnet ⊆ network` containment helper + bootstrap wiring + tests

**Files:**
- Modify: `crates/flanneld/src/subnet.rs`
- Modify: `crates/flanneld/src/main.rs`

- [ ] **Step 1: Write the failing tests**

Add to the `mod tests` in `crates/flanneld/src/subnet.rs`:

```rust
    // parity: flannel pkg/ip ipnet_test.go (IP4Net.Contains intent) — the node
    // lease (FLANNEL_SUBNET) must sit inside the cluster CIDR (FLANNEL_NETWORK).
    #[test]
    fn subnet_within_network_is_contained() {
        let net: ipnetwork::Ipv4Network = "10.244.0.0/16".parse().unwrap();
        let sub: ipnetwork::Ipv4Network = "10.244.1.0/24".parse().unwrap();
        assert!(subnet_in_network(sub, net));
    }

    #[test]
    fn subnet_outside_network_is_not_contained() {
        let net: ipnetwork::Ipv4Network = "10.244.0.0/16".parse().unwrap();
        let sub: ipnetwork::Ipv4Network = "10.245.1.0/24".parse().unwrap();
        assert!(!subnet_in_network(sub, net));
    }

    #[test]
    fn supernet_is_not_contained_in_subnet() {
        // A /16 cannot fit inside a /24 — its broadcast falls outside.
        let small: ipnetwork::Ipv4Network = "10.244.1.0/24".parse().unwrap();
        let big: ipnetwork::Ipv4Network = "10.244.0.0/16".parse().unwrap();
        assert!(!subnet_in_network(big, small));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p flanneld subnet::tests::subnet_within_network_is_contained`
Expected: FAIL — `cannot find function subnet_in_network in this scope`.

- [ ] **Step 3: Add the helper**

At the top of `crates/flanneld/src/subnet.rs`, add the import and function (above `pub struct SubnetEnv`):

```rust
use ipnetwork::Ipv4Network;

/// True if `subnet` lies entirely within `network` (both IPv4 CIDRs): both its
/// network and broadcast addresses are inside `network`.
pub fn subnet_in_network(subnet: Ipv4Network, network: Ipv4Network) -> bool {
    network.contains(subnet.network()) && network.contains(subnet.broadcast())
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p flanneld subnet::tests`
Expected: PASS — all `subnet::tests` green.

- [ ] **Step 5: Wire the helper into bootstrap so it isn't dead code**

In `crates/flanneld/src/main.rs`, inside `try_bootstrap`, immediately after the line:

```rust
    let cidr: Ipv4Network = own.pod_cidr.parse().context("parse own PodCIDR")?;
```

insert:

```rust
    // Sanity check (parity: flannel verifies the lease sits within the network).
    // Non-fatal: kube assigns the PodCIDR, so just surface a misconfiguration.
    if let Ok(net) = nc.network.parse::<Ipv4Network>() {
        if !crate::subnet::subnet_in_network(cidr, net) {
            warn!(subnet = %own.pod_cidr, network = %nc.network,
                  "node PodCIDR is not within the flannel Network");
        }
    }
```

(`nc`, `cidr`, `warn`, and `Ipv4Network` are already in scope in `try_bootstrap`.)

- [ ] **Step 6: Verify the workspace builds with no warnings**

Run: `cargo clippy -p flanneld --all-targets -- -D warnings`
Expected: PASS — no `dead_code` warning for `subnet_in_network`.

- [ ] **Step 7: Commit**

```bash
git add crates/flanneld/src/subnet.rs crates/flanneld/src/main.rs
git commit -m "feat(flanneld): assert node PodCIDR lies within flannel Network

parity: flannel pkg/ip IP4Net.Contains intent. Adds subnet_in_network
helper (unit-tested) and a non-fatal bootstrap sanity check."
```

---

## Task 3: Allocator edge-prefix + last-IP tests (test-only)

**Files:**
- Modify: `crates/cni-host-local/src/alloc.rs`

- [ ] **Step 1: Write the failing tests**

Add to the `mod tests` in `crates/cni-host-local/src/alloc.rs` (the `net` / `ip` helpers already exist there):

```rust
    // parity: containernetworking host-local — degenerate prefixes have no usable
    // host; allocation must return None, never the network/broadcast address.
    #[test]
    fn slash31_has_no_usable_host() {
        let a = Allocator::new(net("10.0.0.0/31"), None);
        assert_eq!(a.next_ip(&HashSet::new(), None), None);
    }

    #[test]
    fn slash32_has_no_usable_host() {
        let a = Allocator::new(net("10.0.0.0/32"), None);
        assert_eq!(a.next_ip(&HashSet::new(), None), None);
    }

    // Last usable host in a /24 (.254) is allocatable.
    #[test]
    fn last_usable_host_is_allocatable() {
        let a = Allocator::new(net("10.244.1.0/24"), None);
        let mut leased: HashSet<Ipv4Addr> = HashSet::new();
        for o in 2..=253 {
            leased.insert(ip(&format!("10.244.1.{o}")));
        }
        assert_eq!(a.next_ip(&leased, None), Some(ip("10.244.1.254")));
    }
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p cni-host-local alloc::tests`
Expected: PASS — current `Allocator` already excludes network/broadcast/gateway and returns `None` when no host is free, so these lock that behaviour. (If any fail, that is a real regression to investigate, not a test to weaken.)

- [ ] **Step 3: Commit**

```bash
git add crates/cni-host-local/src/alloc.rs
git commit -m "test(host-local): lock /31,/32 edge prefixes and last-usable IP

parity: containernetworking host-local allocation bounds."
```

---

## Task 4: portmap rule-arg gap-fill tests (test-only)

**Files:**
- Modify: `crates/cni-portmap/src/rules.rs`

- [ ] **Step 1: Write the failing tests**

Add to the `mod tests` in `crates/cni-portmap/src/rules.rs` (the `mapping()` helper already exists there):

```rust
    // parity: containernetworking portmap — hairpin rule with an explicit hostIP
    // must include the `-d <hostIP>/32` match (the host_ip branch of
    // hairpin_mark_args, otherwise untested).
    #[test]
    fn hairpin_mark_args_match_host_ip_when_set() {
        let m = PortMapping {
            host_port: 31180,
            container_port: 80,
            protocol: "tcp".into(),
            host_ip: Some("127.0.0.2".into()),
        };
        let a = hairpin_mark_args(&m, "10.244.1.5".parse().unwrap());
        assert_eq!(
            a,
            vec![
                "-p",
                "tcp",
                "-s",
                "10.244.1.5/32",
                "-d",
                "127.0.0.2/32",
                "--dport",
                "31180",
                "-j",
                SETMARK_CHAIN,
            ]
        );
    }

    // localhost mark rule honours the mapping protocol (udp here).
    #[test]
    fn localhost_mark_args_udp() {
        let m = PortMapping {
            host_port: 31180,
            container_port: 80,
            protocol: "udp".into(),
            host_ip: None,
        };
        let a = localhost_mark_args(&m);
        assert_eq!(
            a,
            vec![
                "-p",
                "udp",
                "-s",
                "127.0.0.1/32",
                "--dport",
                "31180",
                "-j",
                SETMARK_CHAIN,
            ]
        );
    }
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p cni-portmap rules::tests`
Expected: PASS — these exercise existing branches (`host_ip_some()` in `hairpin_mark_args`, protocol passthrough in `localhost_mark_args`).

- [ ] **Step 3: Commit**

```bash
git add crates/cni-portmap/src/rules.rs
git commit -m "test(portmap): cover hairpin hostIP match and udp localhost mark

parity: containernetworking portmap rule shapes."
```

---

## Final verification (before opening a PR)

- [ ] **Run the full local CI gate**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace --locked
cargo test --workspace --locked
```
Expected: all green, 0 failures. Test count up by 11 (3 config + 3 subnet + 3 alloc + 2 portmap).

- [ ] **Open a PR** off a feature branch (`flannel-parity-tests`), referencing the spec, once the gate is green.
