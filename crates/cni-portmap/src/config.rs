use cni::result::CniResult;
use ipnetwork::Ipv4Network;
use serde::Deserialize;
use std::net::Ipv4Addr;

#[derive(Debug, Deserialize)]
pub struct PortmapConf {
    // Part of the netconf schema; parsed but not consumed by the dispatch.
    #[allow(dead_code)]
    #[serde(rename = "cniVersion", default)]
    pub cni_version: String,
    #[serde(rename = "prevResult", default)]
    pub prev_result: Option<CniResult>,
    #[serde(rename = "runtimeConfig", default)]
    pub runtime_config: RuntimeConfig,
}

#[derive(Debug, Deserialize, Default)]
pub struct RuntimeConfig {
    #[serde(rename = "portMappings", default)]
    pub port_mappings: Vec<PortMapping>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct PortMapping {
    #[serde(rename = "hostPort")]
    pub host_port: u16,
    #[serde(rename = "containerPort")]
    pub container_port: u16,
    #[serde(default = "default_proto")]
    pub protocol: String,
}

fn default_proto() -> String {
    "tcp".to_string()
}

impl PortmapConf {
    pub fn parse(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }
}

/// The first IPv4 address from the prevResult.
pub fn pod_ipv4(result: &CniResult) -> Option<Ipv4Addr> {
    for ip in &result.ips {
        if let Ok(net) = ip.address.parse::<Ipv4Network>() {
            return Some(net.ip());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_portmap_conf_with_mappings() {
        let raw = r#"{
          "cniVersion":"0.3.1","name":"cbr0","type":"portmap",
          "runtimeConfig":{"portMappings":[{"hostPort":31180,"containerPort":80,"protocol":"tcp"}]},
          "prevResult":{"cniVersion":"0.3.1","ips":[{"version":"4","address":"10.244.1.5/24","gateway":"10.244.1.1"}]}
        }"#;
        let c = PortmapConf::parse(raw).unwrap();
        assert_eq!(c.runtime_config.port_mappings.len(), 1);
        let m = &c.runtime_config.port_mappings[0];
        assert_eq!(m.host_port, 31180);
        assert_eq!(m.container_port, 80);
        assert_eq!(m.protocol, "tcp");
        let pip = pod_ipv4(c.prev_result.as_ref().unwrap()).unwrap();
        assert_eq!(pip, "10.244.1.5".parse::<Ipv4Addr>().unwrap());
    }

    #[test]
    fn empty_runtime_config_yields_no_mappings() {
        let raw = r#"{"cniVersion":"0.3.1","prevResult":{"cniVersion":"0.3.1","ips":[]}}"#;
        let c = PortmapConf::parse(raw).unwrap();
        assert!(c.runtime_config.port_mappings.is_empty());
    }
}
