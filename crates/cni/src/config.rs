use serde::{Deserialize, Serialize};

/// Top-level network config passed to the IPAM plugin on stdin.
#[derive(Debug, Deserialize)]
pub struct NetConf {
    #[serde(rename = "cniVersion", default)]
    pub cni_version: String,
    #[serde(default)]
    pub name: String,
    pub ipam: IpamConfig,
}

#[derive(Debug, Deserialize)]
pub struct IpamConfig {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub ranges: Vec<Vec<RangeConfig>>,
    #[serde(default)]
    pub routes: Vec<Route>,
    #[serde(rename = "dataDir", default)]
    pub data_dir: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct RangeConfig {
    pub subnet: String,
    #[serde(default)]
    pub gateway: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Route {
    pub dst: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub gw: Option<String>,
}

impl NetConf {
    pub fn parse(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flannel_delegate_ipam() {
        let raw = r#"{
          "cniVersion": "0.3.1",
          "name": "cbr0",
          "ipam": {
            "type": "host-local",
            "ranges": [[{"subnet": "10.244.1.0/24"}]],
            "routes": [{"dst": "0.0.0.0/0"}],
            "dataDir": "/var/lib/cni/networks"
          }
        }"#;
        let nc = NetConf::parse(raw).unwrap();
        assert_eq!(nc.cni_version, "0.3.1");
        assert_eq!(nc.name, "cbr0");
        assert_eq!(nc.ipam.kind, "host-local");
        assert_eq!(nc.ipam.ranges[0][0].subnet, "10.244.1.0/24");
        assert_eq!(nc.ipam.routes[0].dst, "0.0.0.0/0");
        assert_eq!(nc.ipam.data_dir.as_deref(), Some("/var/lib/cni/networks"));
    }
}
