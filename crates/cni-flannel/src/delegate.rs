use crate::subnetenv::SubnetEnv;
use serde_json::{json, Map, Value};

/// The flannel plugin's own stdin netconf.
pub struct FlannelConf {
    pub name: String,
    pub cni_version: String,
    pub delegate: Map<String, Value>,
}

impl FlannelConf {
    pub fn parse(s: &str) -> Result<Self, serde_json::Error> {
        let v: Value = serde_json::from_str(s)?;
        let name = v
            .get("name")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let cni_version = v
            .get("cniVersion")
            .and_then(|x| x.as_str())
            .unwrap_or("0.3.1")
            .to_string();
        let delegate = v
            .get("delegate")
            .and_then(|d| d.as_object())
            .cloned()
            .unwrap_or_default();
        Ok(Self {
            name,
            cni_version,
            delegate,
        })
    }
}

/// Fill flannel-derived fields the user did not set, then always inject the
/// host-local ipam derived from subnet.env.
pub fn build_delegate(conf: &FlannelConf, env: &SubnetEnv) -> Value {
    let mut d = conf.delegate.clone();
    d.insert("name".into(), json!(conf.name));
    d.insert("cniVersion".into(), json!(conf.cni_version));
    if !d.contains_key("type") {
        d.insert("type".into(), json!("bridge"));
    }
    if !d.contains_key("ipMasq") {
        d.insert("ipMasq".into(), json!(!env.ipmasq));
    }
    if !d.contains_key("mtu") {
        d.insert("mtu".into(), json!(env.mtu));
    }
    if d.get("type").and_then(|t| t.as_str()) == Some("bridge") && !d.contains_key("isGateway") {
        d.insert("isGateway".into(), json!(true));
    }
    d.insert(
        "ipam".into(),
        json!({
            "type": "host-local",
            "ranges": [[{"subnet": env.subnet}]],
            "routes": [{"dst": env.network}],
        }),
    );
    Value::Object(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> SubnetEnv {
        SubnetEnv {
            network: "10.244.0.0/16".into(),
            subnet: "10.244.1.0/24".into(),
            mtu: 1450,
            ipmasq: true,
        }
    }

    #[test]
    fn builds_bridge_delegate_from_flannel_conf() {
        let conf = FlannelConf::parse(r#"{"name":"cbr0","cniVersion":"0.3.1","type":"flannel","delegate":{"hairpinMode":true,"isDefaultGateway":true}}"#).unwrap();
        let d = build_delegate(&conf, &env());
        assert_eq!(d["name"], "cbr0");
        assert_eq!(d["cniVersion"], "0.3.1");
        assert_eq!(d["type"], "bridge");
        assert_eq!(d["mtu"], 1450);
        assert_eq!(d["ipMasq"], false);
        assert_eq!(d["isGateway"], true);
        assert_eq!(d["hairpinMode"], true);
        assert_eq!(d["isDefaultGateway"], true);
        assert_eq!(d["ipam"]["type"], "host-local");
        assert_eq!(d["ipam"]["ranges"][0][0]["subnet"], "10.244.1.0/24");
        assert_eq!(d["ipam"]["routes"][0]["dst"], "10.244.0.0/16");
    }

    #[test]
    fn does_not_overwrite_user_fields() {
        let conf = FlannelConf::parse(r#"{"name":"cbr0","cniVersion":"0.3.1","delegate":{"type":"ptp","mtu":9000,"ipMasq":true}}"#).unwrap();
        let d = build_delegate(&conf, &env());
        assert_eq!(d["type"], "ptp");
        assert_eq!(d["mtu"], 9000);
        assert_eq!(d["ipMasq"], true);
        assert!(d.get("isGateway").is_none());
    }
}
