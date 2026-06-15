//! Install flannel-style ip-masq (source-NAT) iptables rules.
//!
//! The daemon advertises `FLANNEL_IPMASQ=true` in subnet.env, but historically
//! never installed the corresponding MASQUERADE rules. Without them, pod traffic
//! to host-network / external destinations (e.g. a pod reaching the apiserver via
//! its ClusterIP, whose endpoint is host-network) is not source-NATed and stalls
//! in SYN_RECV when the pod is not co-located with the endpoint.
//!
//! Upstream Go flannel (run with `--ip-masq`) installs four rules in the `nat`
//! table `POSTROUTING` chain. We mirror them exactly.
//!
//! ## Backend selection
//! kube nodes can use either `iptables-legacy` or `iptables-nft`. Our rules must
//! land in the SAME backend kube-proxy uses, otherwise they sit in tables that do
//! not process the traffic. We mirror the upstream `iptables-wrapper` heuristic:
//! whichever backend's `*-save` output contains the `KUBE-` chains installed by
//! kube-proxy is the active one. If neither shows KUBE chains we fall back to the
//! one with more rules, then to plain `iptables`.

use std::process::Command;

use anyhow::{anyhow, Context, Result};
use tracing::debug;

/// Installs/ensures flannel's masquerade rule set idempotently using a concrete
/// iptables backend binary that matches the one kube-proxy uses.
pub struct IpMasq {
    /// Concrete binary: "iptables-nft", "iptables-legacy", or "iptables".
    backend: String,
}

/// Build the four masquerade rules, in append order, as argument vectors.
///
/// `n` = FLANNEL_NETWORK (cluster CIDR, e.g. `10.244.0.0/16`).
/// `sn` = FLANNEL_SUBNET (this node's pod CIDR, e.g. `10.244.1.0/24`).
///
/// Order matters: the RETURN rules are appended before the broad MASQUERADE so
/// that `-C`/`-A` (append) preserves first-match-wins semantics — pod-to-pod and
/// inbound-to-local-pod traffic is exempted before the catch-all masquerade.
fn masq_rules(n: &str, sn: &str) -> Vec<Vec<String>> {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<String>>();
    vec![
        // pod -> pod: no masq
        s(&["-s", n, "-d", n, "-j", "RETURN"]),
        // pod -> external (non-multicast): masq
        s(&["-s", n, "!", "-d", "224.0.0.0/4", "-j", "MASQUERADE", "--random-fully"]),
        // !pod -> local pods: no masq
        s(&["!", "-s", n, "-d", sn, "-j", "RETURN"]),
        // !pod -> remote pods: masq
        s(&["!", "-s", n, "-d", n, "-j", "MASQUERADE", "--random-fully"]),
    ]
}

/// Count non-default rules in a backend's nat-table save output. We look for the
/// `KUBE-` chains kube-proxy installs; their presence is the strongest signal the
/// backend is active. Returns `(has_kube_chains, total_rule_lines)`.
fn backend_signal(backend: &str) -> (bool, usize) {
    let out = Command::new(backend).arg("-t").arg("nat").arg("-S").output();
    let Ok(out) = out else { return (false, 0) };
    if !out.status.success() {
        return (false, 0);
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let has_kube = text.lines().any(|l| l.contains("KUBE-"));
    // Count actual rule lines (-A ...), not chain declarations (-N/-P).
    let rules = text.lines().filter(|l| l.starts_with("-A")).count();
    (has_kube, rules)
}

impl IpMasq {
    pub fn new(backend: impl Into<String>) -> Self {
        Self {
            backend: backend.into(),
        }
    }

    /// Pick the backend that matches kube-proxy. Prefer one whose nat table holds
    /// the `KUBE-` chains; otherwise the one with more rules; otherwise `iptables`.
    pub fn detect() -> Result<Self> {
        let (nft_kube, nft_rules) = backend_signal("iptables-nft");
        let (legacy_kube, legacy_rules) = backend_signal("iptables-legacy");
        debug!(nft_kube, nft_rules, legacy_kube, legacy_rules, "iptables backend signals");

        let backend = match (nft_kube, legacy_kube) {
            (true, false) => "iptables-nft",
            (false, true) => "iptables-legacy",
            // Both or neither show KUBE chains: fall back to the busier table.
            _ => {
                if nft_rules >= legacy_rules && (nft_rules > 0 || nft_kube) {
                    "iptables-nft"
                } else if legacy_rules > 0 || legacy_kube {
                    "iptables-legacy"
                } else {
                    // Neither concrete binary responded usefully; try generic.
                    "iptables"
                }
            }
        };
        Ok(Self::new(backend))
    }

    pub fn backend(&self) -> &str {
        &self.backend
    }

    /// Ensure every masquerade rule is present, idempotently. For each rule we
    /// first `-C` (check); if absent we `-A` (append). Returns Ok regardless of
    /// whether anything changed; errors only if an append genuinely fails.
    pub fn ensure(&self, network: &str, subnet: &str) -> Result<()> {
        for rule in masq_rules(network, subnet) {
            self.ensure_rule(&rule)?;
        }
        Ok(())
    }

    fn ensure_rule(&self, rule: &[String]) -> Result<()> {
        if self.rule_exists(rule, "-C")? {
            return Ok(());
        }
        // Rule absent: append it.
        match self.append(rule) {
            Ok(()) => Ok(()),
            Err(e) => {
                // Some old backends lack `--random-fully`. Retry without it.
                if rule.iter().any(|a| a == "--random-fully") {
                    let stripped: Vec<String> =
                        rule.iter().filter(|a| *a != "--random-fully").cloned().collect();
                    if self.rule_exists(&stripped, "-C")? {
                        return Ok(());
                    }
                    return self.append(&stripped);
                }
                Err(e)
            }
        }
    }

    /// Run `<backend> --wait -t nat <op> POSTROUTING <rule...>`. `-C` returns the
    /// exit status (non-zero means "rule absent", not an error to propagate).
    fn rule_exists(&self, rule: &[String], op: &str) -> Result<bool> {
        let out = self
            .run(op, rule)
            .with_context(|| format!("spawn {} {}", self.backend, op))?;
        Ok(out.status.success())
    }

    fn append(&self, rule: &[String]) -> Result<()> {
        let out = self.run("-A", rule).with_context(|| format!("spawn {} -A", self.backend))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(anyhow!(
                "{} -A POSTROUTING {} failed: {}",
                self.backend,
                rule.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }

    fn run(&self, op: &str, rule: &[String]) -> std::io::Result<std::process::Output> {
        Command::new(&self.backend)
            .arg("--wait")
            .arg("-t")
            .arg("nat")
            .arg(op)
            .arg("POSTROUTING")
            .args(rule)
            .output()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_four_rules_in_order() {
        let n = "10.244.0.0/16";
        let sn = "10.244.1.0/24";
        let rules = masq_rules(n, sn);
        assert_eq!(rules.len(), 4);

        assert_eq!(rules[0], vec!["-s", n, "-d", n, "-j", "RETURN"]);
        assert_eq!(
            rules[1],
            vec!["-s", n, "!", "-d", "224.0.0.0/4", "-j", "MASQUERADE", "--random-fully"]
        );
        assert_eq!(rules[2], vec!["!", "-s", n, "-d", sn, "-j", "RETURN"]);
        assert_eq!(
            rules[3],
            vec!["!", "-s", n, "-d", n, "-j", "MASQUERADE", "--random-fully"]
        );
    }

    #[test]
    fn return_rules_precede_broad_masquerade() {
        // First-match-wins relies on RETURNs being appended before the catch-all
        // MASQUERADE. Assert the pod->pod RETURN comes before pod->external masq,
        // and the inbound RETURN comes before the remote-pod masq.
        let rules = masq_rules("10.244.0.0/16", "10.244.1.0/24");
        let is_return = |r: &Vec<String>| r.iter().any(|a| a == "RETURN");
        let is_masq = |r: &Vec<String>| r.iter().any(|a| a == "MASQUERADE");
        assert!(is_return(&rules[0]) && is_masq(&rules[1]));
        assert!(is_return(&rules[2]) && is_masq(&rules[3]));
    }
}
