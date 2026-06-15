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
        Ok(serde_json::from_str(s)?)
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
}
