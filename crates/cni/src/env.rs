use crate::error::CniError;
use std::collections::HashMap;

/// The CNI invocation, parsed from environment variables.
#[derive(Debug, Clone)]
pub struct CniArgs {
    pub command: String,
    pub container_id: String,
    pub netns: String,
    pub ifname: String,
    pub args: String,
    pub path: String,
}

impl CniArgs {
    pub fn from_map(env: &HashMap<String, String>) -> Result<Self, CniError> {
        let get = |k: &str| env.get(k).cloned().unwrap_or_default();
        let command = env
            .get("CNI_COMMAND")
            .cloned()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| CniError::new(4, "CNI_COMMAND missing"))?;
        Ok(Self {
            command,
            container_id: get("CNI_CONTAINERID"),
            netns: get("CNI_NETNS"),
            ifname: get("CNI_IFNAME"),
            args: get("CNI_ARGS"),
            path: get("CNI_PATH"),
        })
    }

    pub fn from_env() -> Result<Self, CniError> {
        let map: HashMap<String, String> = std::env::vars().collect();
        Self::from_map(&map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn parses_add_invocation() {
        let a = CniArgs::from_map(&map(&[
            ("CNI_COMMAND", "ADD"),
            ("CNI_CONTAINERID", "abc123"),
            ("CNI_IFNAME", "eth0"),
            ("CNI_NETNS", "/var/run/netns/x"),
        ]))
        .unwrap();
        assert_eq!(a.command, "ADD");
        assert_eq!(a.container_id, "abc123");
        assert_eq!(a.ifname, "eth0");
    }

    #[test]
    fn missing_command_is_error_code_4() {
        let err = CniArgs::from_map(&map(&[("CNI_CONTAINERID", "abc")])).unwrap_err();
        assert_eq!(err.code, 4);
    }
}
