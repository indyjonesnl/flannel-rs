use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct VersionResult {
    #[serde(rename = "cniVersion")]
    pub cni_version: String,
    #[serde(rename = "supportedVersions")]
    pub supported_versions: Vec<String>,
}

impl VersionResult {
    pub fn supported() -> Self {
        Self {
            cni_version: "0.3.1".into(),
            supported_versions: vec!["0.3.0".into(), "0.3.1".into()],
        }
    }
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("VersionResult serializes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertises_supported_versions() {
        let v: serde_json::Value =
            serde_json::from_str(&VersionResult::supported().to_json()).unwrap();
        assert_eq!(v["supportedVersions"][0], "0.3.0");
        assert_eq!(v["supportedVersions"][1], "0.3.1");
    }
}
