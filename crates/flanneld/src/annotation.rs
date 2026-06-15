use serde::{Deserialize, Serialize};

pub const PREFIX: &str = "flannel.alpha.coreos.com";

pub fn key(suffix: &str) -> String {
    format!("{PREFIX}/{suffix}")
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct BackendData {
    #[serde(rename = "VtepMAC")]
    pub vtep_mac: String,
}

impl BackendData {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("BackendData serializes")
    }
    pub fn from_json(s: &str) -> anyhow::Result<Self> {
        Ok(serde_json::from_str(s)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_prefixed_key() {
        assert_eq!(key("public-ip"), "flannel.alpha.coreos.com/public-ip");
    }

    #[test]
    fn roundtrips_backend_data() {
        let b = BackendData { vtep_mac: "ae:11:22:33:44:55".into() };
        let j = b.to_json();
        assert_eq!(j, r#"{"VtepMAC":"ae:11:22:33:44:55"}"#);
        assert_eq!(BackendData::from_json(&j).unwrap(), b);
    }
}
