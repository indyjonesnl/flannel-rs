use serde::Serialize;

/// CNI error result. Codes: 4 invalid env, 5 I/O, 6 decode, 7 invalid config, 11 try again.
#[derive(Debug, Serialize, thiserror::Error)]
#[error("CNI error {code}: {msg}")]
pub struct CniError {
    #[serde(rename = "cniVersion")]
    pub cni_version: String,
    pub code: u32,
    pub msg: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

impl CniError {
    pub fn new(code: u32, msg: impl Into<String>) -> Self {
        Self {
            cni_version: "0.3.1".into(),
            code,
            msg: msg.into(),
            details: None,
        }
    }
    pub fn with_details(mut self, details: impl Into<String>) -> Self {
        self.details = Some(details.into());
        self
    }
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("CniError serializes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_error() {
        let e = CniError::new(7, "no IP addresses available in range").with_details("range full");
        let v: serde_json::Value = serde_json::from_str(&e.to_json()).unwrap();
        assert_eq!(v["code"], 7);
        assert_eq!(v["msg"], "no IP addresses available in range");
        assert_eq!(v["details"], "range full");
        assert_eq!(v["cniVersion"], "0.3.1");
    }
}
