/// Inputs needed to render /run/flannel/subnet.env.
pub struct SubnetEnv {
    pub network: String, // cluster CIDR, e.g. 10.244.0.0/16
    pub subnet: String,  // this node's lease, e.g. 10.244.1.0/24
    pub mtu: u32,
    pub ipmasq: bool,
}

impl SubnetEnv {
    pub fn render(&self) -> String {
        format!(
            "FLANNEL_NETWORK={}\nFLANNEL_SUBNET={}\nFLANNEL_MTU={}\nFLANNEL_IPMASQ={}\n",
            self.network, self.subnet, self.mtu, self.ipmasq
        )
    }
}

/// VXLAN overhead is 50 bytes.
pub fn vxlan_mtu(link_mtu: u32) -> u32 {
    link_mtu.saturating_sub(50)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_subnet_env() {
        let e = SubnetEnv {
            network: "10.244.0.0/16".into(),
            subnet: "10.244.1.0/24".into(),
            mtu: 1450,
            ipmasq: true,
        };
        assert_eq!(
            e.render(),
            "FLANNEL_NETWORK=10.244.0.0/16\nFLANNEL_SUBNET=10.244.1.0/24\nFLANNEL_MTU=1450\nFLANNEL_IPMASQ=true\n"
        );
    }

    #[test]
    fn vxlan_mtu_subtracts_overhead() {
        assert_eq!(vxlan_mtu(1500), 1450);
    }

    #[test]
    fn vxlan_mtu_saturates_without_underflow() {
        assert_eq!(vxlan_mtu(40), 0);
    }
}
