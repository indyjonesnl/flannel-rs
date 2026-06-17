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

/// One published hostPort -> containerPort mapping.
///
/// containerd's go-cni serializes its `PortMapping` struct with Go field names
/// as JSON keys (PascalCase): `HostPort`, `ContainerPort`, `Protocol`, `HostIP`,
/// with `HostIP` always present (empty string when unset). The CNI portmap
/// spec / reference plugin documents the camelCase form (`hostPort`, ...).
/// Accept BOTH casings via serde aliases so we parse the bytes containerd
/// actually sends as well as the spec form.
#[derive(Debug, Deserialize, Clone)]
pub struct PortMapping {
    #[serde(rename = "hostPort", alias = "HostPort")]
    pub host_port: u16,
    #[serde(rename = "containerPort", alias = "ContainerPort")]
    pub container_port: u16,
    #[serde(rename = "protocol", alias = "Protocol", default = "default_proto")]
    pub protocol: String,
    /// Optional host IP to bind the mapping to. containerd sends `"HostIP":""`
    /// (empty) when unspecified; treat empty as "no specific host IP".
    #[serde(rename = "hostIP", alias = "HostIP", alias = "hostIp", default)]
    pub host_ip: Option<String>,
}

fn default_proto() -> String {
    "tcp".to_string()
}

impl PortMapping {
    /// The host IP to match on, if a specific non-empty one was requested.
    pub fn host_ip_some(&self) -> Option<&str> {
        match self.host_ip.as_deref() {
            Some(ip) if !ip.is_empty() => Some(ip),
            _ => None,
        }
    }
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
        assert_eq!(m.host_ip_some(), None);
        let pip = pod_ipv4(c.prev_result.as_ref().unwrap()).unwrap();
        assert_eq!(pip, "10.244.1.5".parse::<Ipv4Addr>().unwrap());
    }

    // EXACT bytes captured from containerd's go-cni on stdin (PascalCase keys,
    // HostIP always present). Our struct previously only accepted camelCase, so
    // this failed with `missing field "hostPort"`.
    #[test]
    fn parses_containerd_pascalcase_runtime_config() {
        let raw = r#"{"capabilities":{"portMappings":true},"cniVersion":"0.3.1","name":"cbr0","prevResult":{"cniVersion":"0.3.1","dns":{},"ips":[{"address":"10.244.2.4/24","gateway":"10.244.2.1","version":"4"}],"routes":[{"dst":"10.244.0.0/16"}]},"runtimeConfig":{"portMappings":[{"HostPort":31180,"ContainerPort":80,"Protocol":"tcp","HostIP":""}]},"type":"portmap"}"#;
        let c = PortmapConf::parse(raw).unwrap();
        assert_eq!(c.runtime_config.port_mappings.len(), 1);
        let m = &c.runtime_config.port_mappings[0];
        assert_eq!(m.host_port, 31180);
        assert_eq!(m.container_port, 80);
        assert_eq!(m.protocol, "tcp");
        // Empty HostIP must be treated as "no specific host IP".
        assert_eq!(m.host_ip, Some(String::new()));
        assert_eq!(m.host_ip_some(), None);
        let pip = pod_ipv4(c.prev_result.as_ref().unwrap()).unwrap();
        assert_eq!(pip, "10.244.2.4".parse::<Ipv4Addr>().unwrap());
    }

    // A mapping with a specific HostIP (the conformance HostPort spec binds
    // distinct hostIPs) and udp protocol must round-trip.
    #[test]
    fn parses_mapping_with_host_ip_and_udp() {
        let raw = r#"{"cniVersion":"0.3.1","name":"cbr0","type":"portmap",
          "runtimeConfig":{"portMappings":[{"HostPort":54321,"ContainerPort":53,"Protocol":"udp","HostIP":"127.0.0.2"}]},
          "prevResult":{"cniVersion":"0.3.1","ips":[{"version":"4","address":"10.244.1.5/24"}]}}"#;
        let c = PortmapConf::parse(raw).unwrap();
        let m = &c.runtime_config.port_mappings[0];
        assert_eq!(m.host_port, 54321);
        assert_eq!(m.container_port, 53);
        assert_eq!(m.protocol, "udp");
        assert_eq!(m.host_ip_some(), Some("127.0.0.2"));
    }

    #[test]
    fn empty_runtime_config_yields_no_mappings() {
        let raw = r#"{"cniVersion":"0.3.1","prevResult":{"cniVersion":"0.3.1","ips":[]}}"#;
        let c = PortmapConf::parse(raw).unwrap();
        assert!(c.runtime_config.port_mappings.is_empty());
    }
}
