use cni::result::CniResult;
use ipnetwork::Ipv4Network;
use std::net::Ipv4Addr;

/// Deterministic host-side veth name from the container id, within the 15-char
/// IFNAMSIZ limit. Format: "veth" + first 11 alphanumeric chars of the id.
pub fn host_veth_name(container_id: &str) -> String {
    let id: String = container_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(11)
        .collect();
    format!("veth{id}")
}

/// The pod's address+prefix, gateway, and route destinations from the IPAM result.
pub struct IpPlan {
    pub addr: Ipv4Addr,
    pub prefix: u8,
    pub gateway: Option<Ipv4Addr>,
    pub routes: Vec<Ipv4Network>,
}

pub fn ip_plan(result: &CniResult) -> anyhow::Result<IpPlan> {
    let ip = result
        .ips
        .first()
        .ok_or_else(|| anyhow::anyhow!("IPAM result has no IPs"))?;
    let net: Ipv4Network = ip
        .address
        .parse()
        .map_err(|_| anyhow::anyhow!("bad IPAM address {}", ip.address))?;
    let gateway = match &ip.gateway {
        Some(g) => Some(
            g.parse()
                .map_err(|_| anyhow::anyhow!("bad IPAM gateway {g}"))?,
        ),
        None => None,
    };
    let mut routes = Vec::new();
    for r in &result.routes {
        routes.push(
            r.dst
                .parse()
                .map_err(|_| anyhow::anyhow!("bad route dst {}", r.dst))?,
        );
    }
    Ok(IpPlan {
        addr: net.ip(),
        prefix: net.prefix(),
        gateway,
        routes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_veth_name_is_bounded_and_deterministic() {
        let n = host_veth_name("ec5a938858dce08f4179b48658de7bbd");
        assert!(n.len() <= 15, "len {}", n.len());
        assert_eq!(n, host_veth_name("ec5a938858dce08f4179b48658de7bbd"));
        assert!(n.starts_with("veth"));
    }

    #[test]
    fn ip_plan_extracts_from_result() {
        let r = CniResult::parse(r#"{"cniVersion":"0.3.1","ips":[{"version":"4","address":"10.244.1.2/24","gateway":"10.244.1.1"}],"routes":[{"dst":"10.244.0.0/16"}]}"#).unwrap();
        let p = ip_plan(&r).unwrap();
        assert_eq!(p.addr, "10.244.1.2".parse::<Ipv4Addr>().unwrap());
        assert_eq!(p.prefix, 24);
        assert_eq!(p.gateway, Some("10.244.1.1".parse().unwrap()));
        assert_eq!(p.routes[0].to_string(), "10.244.0.0/16");
    }
}
