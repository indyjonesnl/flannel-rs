use crate::config::PortMapping;
use cni::error::CniError;
use cni::iptables::Iptables;
use std::net::Ipv4Addr;

pub const TOP_CHAIN: &str = "CNI-HOSTPORT-DNAT";

/// Per-container DNAT chain name, within iptables' 28-char chain limit.
pub fn dn_chain(container_id: &str) -> String {
    let id: String = container_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(16)
        .collect();
    format!("CNI-DN-{id}")
}

/// Per-container hairpin-masquerade chain name.
pub fn hm_chain(container_id: &str) -> String {
    let id: String = container_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(16)
        .collect();
    format!("CNI-HM-{id}")
}

/// The DNAT rule args (after `-A <chain>`) for one mapping.
pub fn dnat_args(m: &PortMapping, pod_ip: Ipv4Addr) -> Vec<String> {
    vec![
        "-p".into(),
        m.protocol.clone(),
        "--dport".into(),
        m.host_port.to_string(),
        "-j".into(),
        "DNAT".into(),
        "--to-destination".into(),
        format!("{}:{}", pod_ip, m.container_port),
    ]
}

/// The hairpin masquerade rule args for one mapping (pod reaching its own hostPort).
pub fn hairpin_args(m: &PortMapping, pod_ip: Ipv4Addr) -> Vec<String> {
    vec![
        "-p".into(),
        m.protocol.clone(),
        "-s".into(),
        format!("{pod_ip}/32"),
        "-d".into(),
        format!("{pod_ip}/32"),
        "--dport".into(),
        m.container_port.to_string(),
        "-j".into(),
        "MASQUERADE".into(),
    ]
}

fn as_refs(v: &[String]) -> Vec<&str> {
    v.iter().map(|s| s.as_str()).collect()
}

/// Install DNAT + hairpin chains/rules for all mappings.
pub fn apply(
    ipt: &Iptables,
    container_id: &str,
    pod_ip: Ipv4Addr,
    mappings: &[PortMapping],
) -> Result<(), CniError> {
    let dn = dn_chain(container_id);
    let hm = hm_chain(container_id);

    // Top DNAT chain + jumps from PREROUTING/OUTPUT (only for locally-addressed traffic).
    ipt.ensure_chain(TOP_CHAIN)?;
    ipt.ensure_rule(
        "PREROUTING",
        &["-m", "addrtype", "--dst-type", "LOCAL", "-j", TOP_CHAIN],
    )?;
    ipt.ensure_rule(
        "OUTPUT",
        &["-m", "addrtype", "--dst-type", "LOCAL", "-j", TOP_CHAIN],
    )?;

    // Per-container DNAT chain, jumped from the top chain.
    ipt.ensure_chain(&dn)?;
    ipt.ensure_rule(TOP_CHAIN, &["-j", &dn])?;

    // Hairpin masq chain, jumped from POSTROUTING.
    ipt.ensure_chain(&hm)?;
    ipt.ensure_rule("POSTROUTING", &["-j", &hm])?;

    for m in mappings {
        let d = dnat_args(m, pod_ip);
        ipt.ensure_rule(&dn, &as_refs(&d))?;
        let h = hairpin_args(m, pod_ip);
        ipt.ensure_rule(&hm, &as_refs(&h))?;
    }
    Ok(())
}

/// Remove this container's chains and their jumps (idempotent / best-effort).
pub fn remove(ipt: &Iptables, container_id: &str) {
    let dn = dn_chain(container_id);
    let hm = hm_chain(container_id);
    ipt.delete_rule(TOP_CHAIN, &["-j", &dn]);
    ipt.flush_delete_chain(&dn);
    ipt.delete_rule("POSTROUTING", &["-j", &hm]);
    ipt.flush_delete_chain(&hm);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapping() -> PortMapping {
        PortMapping {
            host_port: 31180,
            container_port: 80,
            protocol: "tcp".into(),
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
            vec!["-p", "tcp", "--dport", "31180", "-j", "DNAT", "--to-destination", "10.244.1.5:80"]
        );
    }

    #[test]
    fn hairpin_args_masquerade_self_traffic() {
        let a = hairpin_args(&mapping(), "10.244.1.5".parse().unwrap());
        assert_eq!(
            a,
            vec![
                "-p",
                "tcp",
                "-s",
                "10.244.1.5/32",
                "-d",
                "10.244.1.5/32",
                "--dport",
                "80",
                "-j",
                "MASQUERADE"
            ]
        );
    }
}
