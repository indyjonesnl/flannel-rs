use crate::config::Route;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct CniResult {
    #[serde(rename = "cniVersion")]
    pub cni_version: String,
    pub ips: Vec<IpResult>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<Route>,
}

#[derive(Debug, Serialize)]
pub struct IpResult {
    pub version: String,
    pub address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gateway: Option<String>,
}

impl CniResult {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("CniResult serializes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_0_3_1_result() {
        let r = CniResult {
            cni_version: "0.3.1".into(),
            ips: vec![IpResult {
                version: "4".into(),
                address: "10.244.1.2/24".into(),
                gateway: Some("10.244.1.1".into()),
            }],
            routes: vec![Route {
                dst: "0.0.0.0/0".into(),
                gw: None,
            }],
        };
        let v: serde_json::Value = serde_json::from_str(&r.to_json()).unwrap();
        assert_eq!(v["cniVersion"], "0.3.1");
        assert_eq!(v["ips"][0]["version"], "4");
        assert_eq!(v["ips"][0]["address"], "10.244.1.2/24");
        assert_eq!(v["ips"][0]["gateway"], "10.244.1.1");
        assert_eq!(v["routes"][0]["dst"], "0.0.0.0/0");
    }
}
