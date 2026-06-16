use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct BridgeConf {
    #[serde(rename = "cniVersion", default)]
    pub cni_version: String,
    #[serde(alias = "name", default = "default_bridge")]
    pub bridge: String,
    #[serde(rename = "isGateway", default)]
    pub is_gateway: bool,
    #[serde(rename = "isDefaultGateway", default)]
    pub is_default_gateway: bool,
    #[serde(rename = "hairpinMode", default)]
    pub hairpin_mode: bool,
    #[serde(default)]
    pub mtu: Option<u32>,
    pub ipam: Ipam,
}

#[derive(Debug, Deserialize)]
pub struct Ipam {
    #[serde(rename = "type")]
    pub kind: String,
}

fn default_bridge() -> String {
    "cni0".to_string()
}

impl BridgeConf {
    pub fn parse(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flannel_bridge_delegate() {
        let raw = r#"{"name":"cbr0","cniVersion":"0.3.1","type":"bridge","mtu":1450,"isGateway":true,"isDefaultGateway":true,"hairpinMode":true,"ipMasq":false,"ipam":{"type":"host-local","ranges":[[{"subnet":"10.244.1.0/24"}]]}}"#;
        let c = BridgeConf::parse(raw).unwrap();
        assert_eq!(c.bridge, "cbr0");
        assert_eq!(c.mtu, Some(1450));
        assert!(c.is_gateway && c.is_default_gateway && c.hairpin_mode);
        assert_eq!(c.ipam.kind, "host-local");
    }

    #[test]
    fn bridge_name_defaults_to_cni0() {
        let raw = r#"{"cniVersion":"0.3.1","ipam":{"type":"host-local"}}"#;
        assert_eq!(BridgeConf::parse(raw).unwrap().bridge, "cni0");
    }
}
