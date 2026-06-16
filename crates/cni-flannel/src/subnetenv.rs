/// Parsed /run/flannel/subnet.env (the file flanneld writes).
#[derive(Debug, PartialEq)]
pub struct SubnetEnv {
    pub network: String,
    pub subnet: String,
    pub mtu: u32,
    pub ipmasq: bool,
}

impl SubnetEnv {
    pub fn parse(s: &str) -> Result<Self, String> {
        let (mut network, mut subnet, mut mtu, mut ipmasq) = (None, None, None, None);
        for line in s.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let (k, v) = line.split_once('=').ok_or_else(|| format!("malformed line: {line}"))?;
            match k.trim() {
                "FLANNEL_NETWORK" => network = Some(v.trim().to_string()),
                "FLANNEL_SUBNET" => subnet = Some(v.trim().to_string()),
                "FLANNEL_MTU" => mtu = Some(v.trim().parse::<u32>().map_err(|e| e.to_string())?),
                "FLANNEL_IPMASQ" => ipmasq = Some(v.trim() == "true"),
                _ => {}
            }
        }
        Ok(Self {
            network: network.ok_or("missing FLANNEL_NETWORK")?,
            subnet: subnet.ok_or("missing FLANNEL_SUBNET")?,
            mtu: mtu.ok_or("missing FLANNEL_MTU")?,
            ipmasq: ipmasq.unwrap_or(false),
        })
    }

    pub fn load(path: &str) -> Result<Self, String> {
        let s = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        Self::parse(&s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_well_formed_subnet_env() {
        let raw = "FLANNEL_NETWORK=10.244.0.0/16\nFLANNEL_SUBNET=10.244.1.0/24\nFLANNEL_MTU=1450\nFLANNEL_IPMASQ=true\n";
        let e = SubnetEnv::parse(raw).unwrap();
        assert_eq!(e, SubnetEnv { network: "10.244.0.0/16".into(), subnet: "10.244.1.0/24".into(), mtu: 1450, ipmasq: true });
    }

    #[test]
    fn missing_required_key_errors() {
        let raw = "FLANNEL_NETWORK=10.244.0.0/16\nFLANNEL_MTU=1450\n";
        assert!(SubnetEnv::parse(raw).is_err());
    }
}
