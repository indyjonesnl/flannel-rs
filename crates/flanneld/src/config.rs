use serde::Deserialize;

#[derive(Debug, Deserialize, PartialEq)]
pub struct NetConf {
    #[serde(rename = "Network")]
    pub network: String,
    #[serde(rename = "Backend")]
    pub backend: Backend,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct Backend {
    #[serde(rename = "Type")]
    pub kind: String,
}

impl NetConf {
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        let nc: Self = serde_json::from_str(s)?;
        // Network (the cluster CIDR) must be a well-formed IPv4 CIDR. The per-node
        // subnet still comes from the kube PodCIDR, not from this field.
        nc.network
            .parse::<ipnetwork::Ipv4Network>()
            .map_err(|e| anyhow::anyhow!("invalid Network CIDR '{}': {e}", nc.network))?;
        Ok(nc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_vxlan_net_conf() {
        let nc =
            NetConf::parse(r#"{"Network":"10.244.0.0/16","Backend":{"Type":"vxlan"}}"#).unwrap();
        assert_eq!(nc.network, "10.244.0.0/16");
        assert_eq!(nc.backend.kind, "vxlan");
    }

    // parity: flannel pkg/subnet/config_test.go — flannel-rs only honours
    // Network + Backend.Type (per-node subnet comes from kube PodCIDR).
    #[test]
    fn rejects_invalid_network_cidr() {
        assert!(NetConf::parse(r#"{"Network":"not-a-cidr","Backend":{"Type":"vxlan"}}"#).is_err());
        assert!(
            NetConf::parse(r#"{"Network":"10.244.0.0/33","Backend":{"Type":"vxlan"}}"#).is_err()
        );
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
}
