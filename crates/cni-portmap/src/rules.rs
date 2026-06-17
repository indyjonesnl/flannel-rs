use crate::config::PortMapping;
use cni::error::CniError;
use cni::iptables::Iptables;
use std::net::Ipv4Addr;

pub const TOP_CHAIN: &str = "CNI-HOSTPORT-DNAT";
/// Marks DNAT'd hostPort traffic that must be masqueraded on the way out
/// (hairpin: pod -> its own hostPort; and localhost: node -> 127.0.0.1:hostPort).
pub const SETMARK_CHAIN: &str = "CNI-HOSTPORT-SETMARK";
/// Masquerades anything the SETMARK chain marked. Mirrors the upstream Go
/// portmap plugin so the loopback/hairpin return path works.
pub const MASQ_CHAIN: &str = "CNI-HOSTPORT-MASQ";
/// Mark bit used to tag hostPort traffic for masquerade (matches Go portmap).
pub const MARK: &str = "0x2000/0x2000";

/// Per-container DNAT chain name, within iptables' 28-char chain limit.
pub fn dn_chain(container_id: &str) -> String {
    let id: String = container_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(16)
        .collect();
    format!("CNI-DN-{id}")
}

/// The DNAT rule args (after `-A <chain>`) for one mapping.
///
/// The match is keyed on protocol (`-p`), the published host port (`--dport`)
/// and, when the mapping requests a specific non-empty host IP, the destination
/// address (`-d <hostIP>/32`). Including hostIP + per-protocol matching means
/// two mappings sharing a hostPort but differing in hostIP or protocol install
/// distinct, non-conflicting rules (the HostPort conformance spec's case).
pub fn dnat_args(m: &PortMapping, pod_ip: Ipv4Addr) -> Vec<String> {
    let mut args = vec!["-p".into(), m.protocol.clone()];
    if let Some(host_ip) = m.host_ip_some() {
        args.push("-d".into());
        args.push(format!("{host_ip}/32"));
    }
    args.extend([
        "--dport".into(),
        m.host_port.to_string(),
        "-j".into(),
        "DNAT".into(),
        "--to-destination".into(),
        format!("{}:{}", pod_ip, m.container_port),
    ]);
    args
}

/// Args for the hairpin mark rule: traffic from the pod to its own published
/// hostPort must be SNAT'd so the reply returns via the host. Jumps to SETMARK.
pub fn hairpin_mark_args(m: &PortMapping, pod_ip: Ipv4Addr) -> Vec<String> {
    let mut args = vec![
        "-p".into(),
        m.protocol.clone(),
        "-s".into(),
        format!("{pod_ip}/32"),
    ];
    if let Some(host_ip) = m.host_ip_some() {
        args.push("-d".into());
        args.push(format!("{host_ip}/32"));
    }
    args.extend([
        "--dport".into(),
        m.host_port.to_string(),
        "-j".into(),
        SETMARK_CHAIN.into(),
    ]);
    args
}

/// Args for the localhost mark rule: node-local traffic to 127.0.0.1:hostPort is
/// DNAT'd to the pod, but keeps src 127.0.0.1 which the pod cannot route back to.
/// Mark it so it gets masqueraded to a host address. Jumps to SETMARK.
pub fn localhost_mark_args(m: &PortMapping) -> Vec<String> {
    vec![
        "-p".into(),
        m.protocol.clone(),
        "-s".into(),
        "127.0.0.1/32".into(),
        "--dport".into(),
        m.host_port.to_string(),
        "-j".into(),
        SETMARK_CHAIN.into(),
    ]
}

fn as_refs(v: &[String]) -> Vec<&str> {
    v.iter().map(|s| s.as_str()).collect()
}

/// Ensure the shared DNAT / SETMARK / MASQ chains and their hook jumps exist.
///
/// The hook jumps are INSERTED at the top of the built-in chains (not appended)
/// so they take precedence over flannel's masquerade/RETURN rules. In
/// particular, flannel appends a `RETURN` for the local pod subnet to
/// POSTROUTING; if our masquerade jump were appended after it, loopback hostPort
/// traffic (src 127.0.0.1, DNAT'd to a local pod IP) would hit that RETURN and
/// never get masqueraded, so the pod's reply would go to its own loopback and
/// the connection would hang. This mirrors the upstream Go portmap plugin.
fn ensure_top_chains(ipt: &Iptables) -> Result<(), CniError> {
    // Top DNAT chain + jumps from PREROUTING/OUTPUT (only for locally-addressed traffic).
    ipt.ensure_chain(TOP_CHAIN)?;
    ipt.insert_rule(
        "PREROUTING",
        &["-m", "addrtype", "--dst-type", "LOCAL", "-j", TOP_CHAIN],
    )?;
    ipt.insert_rule(
        "OUTPUT",
        &["-m", "addrtype", "--dst-type", "LOCAL", "-j", TOP_CHAIN],
    )?;

    // SETMARK chain: tag matched traffic for masquerade.
    ipt.ensure_chain(SETMARK_CHAIN)?;
    ipt.ensure_rule(SETMARK_CHAIN, &["-j", "MARK", "--set-xmark", MARK])?;

    // MASQ chain: masquerade tagged traffic, jumped from the TOP of POSTROUTING
    // so it runs before flannel's local-subnet RETURN.
    ipt.ensure_chain(MASQ_CHAIN)?;
    ipt.ensure_rule(
        MASQ_CHAIN,
        &["-m", "mark", "--mark", MARK, "-j", "MASQUERADE"],
    )?;
    ipt.insert_rule("POSTROUTING", &["-j", MASQ_CHAIN])?;
    Ok(())
}

/// Install DNAT + hairpin/localhost-masquerade rules for all mappings.
pub fn apply(
    ipt: &Iptables,
    container_id: &str,
    pod_ip: Ipv4Addr,
    mappings: &[PortMapping],
) -> Result<(), CniError> {
    let dn = dn_chain(container_id);

    ensure_top_chains(ipt)?;

    // Per-container DNAT chain, jumped from the top chain.
    ipt.ensure_chain(&dn)?;
    ipt.ensure_rule(TOP_CHAIN, &["-j", &dn])?;

    for m in mappings {
        // Mark hairpin + localhost traffic for masquerade BEFORE the DNAT, so the
        // mark is set on the original (pre-DNAT) source match.
        let hp = hairpin_mark_args(m, pod_ip);
        ipt.ensure_rule(&dn, &as_refs(&hp))?;
        let lh = localhost_mark_args(m);
        ipt.ensure_rule(&dn, &as_refs(&lh))?;
        // The DNAT itself.
        let d = dnat_args(m, pod_ip);
        ipt.ensure_rule(&dn, &as_refs(&d))?;
    }
    Ok(())
}

/// Remove this container's chain and its jump (idempotent / best-effort).
/// The shared SETMARK/MASQ chains are left in place (they are reused by other
/// containers and harmless when empty of per-container references).
pub fn remove(ipt: &Iptables, container_id: &str) {
    let dn = dn_chain(container_id);
    ipt.delete_rule(TOP_CHAIN, &["-j", &dn]);
    ipt.flush_delete_chain(&dn);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapping() -> PortMapping {
        PortMapping {
            host_port: 31180,
            container_port: 80,
            protocol: "tcp".into(),
            host_ip: None,
        }
    }

    #[test]
    fn dn_chain_within_iptables_limit() {
        let c = dn_chain("ec5a938858dce08f4179b48658de7bbd");
        assert!(c.len() <= 28, "len {}", c.len());
        assert!(c.starts_with("CNI-DN-"));
    }

    #[test]
    fn dnat_args_target_pod_ip_and_port() {
        let a = dnat_args(&mapping(), "10.244.1.5".parse().unwrap());
        assert_eq!(
            a,
            vec![
                "-p",
                "tcp",
                "--dport",
                "31180",
                "-j",
                "DNAT",
                "--to-destination",
                "10.244.1.5:80"
            ]
        );
    }

    #[test]
    fn dnat_args_match_host_ip_when_set() {
        let m = PortMapping {
            host_port: 31180,
            container_port: 80,
            protocol: "udp".into(),
            host_ip: Some("127.0.0.2".into()),
        };
        let a = dnat_args(&m, "10.244.1.5".parse().unwrap());
        assert_eq!(
            a,
            vec![
                "-p",
                "udp",
                "-d",
                "127.0.0.2/32",
                "--dport",
                "31180",
                "-j",
                "DNAT",
                "--to-destination",
                "10.244.1.5:80"
            ]
        );
    }

    // An empty HostIP (containerd's default) must NOT add a `-d` match, so the
    // rule stays a plain port match.
    #[test]
    fn dnat_args_ignore_empty_host_ip() {
        let m = PortMapping {
            host_port: 31180,
            container_port: 80,
            protocol: "tcp".into(),
            host_ip: Some(String::new()),
        };
        let a = dnat_args(&m, "10.244.1.5".parse().unwrap());
        assert!(!a.iter().any(|s| s == "-d"), "empty hostIP must not add -d");
    }

    #[test]
    fn hairpin_mark_args_marks_pod_self_traffic() {
        let a = hairpin_mark_args(&mapping(), "10.244.1.5".parse().unwrap());
        assert_eq!(
            a,
            vec![
                "-p",
                "tcp",
                "-s",
                "10.244.1.5/32",
                "--dport",
                "31180",
                "-j",
                SETMARK_CHAIN,
            ]
        );
    }

    #[test]
    fn localhost_mark_args_marks_loopback_traffic() {
        let a = localhost_mark_args(&mapping());
        assert_eq!(
            a,
            vec![
                "-p",
                "tcp",
                "-s",
                "127.0.0.1/32",
                "--dport",
                "31180",
                "-j",
                SETMARK_CHAIN,
            ]
        );
    }
}
