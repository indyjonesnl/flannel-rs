mod config;
mod rules;

use cni::env::CniArgs;
use cni::error::CniError;
use cni::iptables::Iptables;
use cni::version::VersionResult;
use config::PortmapConf;
use std::io::Read;
use std::process::ExitCode;

fn read_stdin() -> String {
    let mut s = String::new();
    let _ = std::io::stdin().read_to_string(&mut s);
    s
}

fn err(code: u32, msg: impl Into<String>) -> CniError {
    CniError::new(code, msg)
}

fn cmd_add(args: &CniArgs, stdin: &str) -> Result<String, CniError> {
    let conf = PortmapConf::parse(stdin)
        .map_err(|e| err(6, "decode config").with_details(e.to_string()))?;
    // A chained plugin MUST output the (possibly modified) prevResult as its
    // result — NOT the netconf. The runtime reads our stdout as the chain's final
    // CNI Result; emitting the whole config breaks pod sandbox setup.
    let prev = conf
        .prev_result
        .as_ref()
        .ok_or_else(|| err(7, "portmap requires prevResult"))?;

    let mappings = &conf.runtime_config.port_mappings;
    if !mappings.is_empty() {
        let pod_ip = config::pod_ipv4(prev).ok_or_else(|| err(7, "no IPv4 in prevResult"))?;
        let ipt = Iptables::detect();
        rules::apply(&ipt, &args.container_id, pod_ip, mappings)?;
    }

    // Relay prevResult unchanged (the chained-plugin contract), with or without mappings.
    serde_json::to_string(prev).map_err(|e| err(6, "encode result").with_details(e.to_string()))
}

fn cmd_del(args: &CniArgs, stdin: &str) -> Result<String, CniError> {
    // Parse best-effort; even with no mappings, removing chains is harmless.
    if let Ok(conf) = PortmapConf::parse(stdin) {
        if conf.runtime_config.port_mappings.is_empty() {
            return Ok(String::new());
        }
    }
    let ipt = Iptables::detect();
    rules::remove(&ipt, &args.container_id);
    Ok(String::new())
}

fn run() -> Result<(String, bool), CniError> {
    let args = CniArgs::from_env()?;
    match args.command.as_str() {
        "VERSION" => Ok((VersionResult::supported().to_json(), true)),
        "ADD" => cmd_add(&args, &read_stdin()).map(|s| (s, true)),
        "DEL" => cmd_del(&args, &read_stdin()).map(|s| (s, true)),
        "CHECK" => Ok((String::new(), true)),
        other => Err(err(4, format!("unknown CNI_COMMAND {other}"))),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok((out, true)) => {
            if !out.is_empty() {
                print!("{out}");
            }
            ExitCode::SUCCESS
        }
        Ok((out, false)) => {
            print!("{out}");
            ExitCode::FAILURE
        }
        Err(e) => {
            print!("{}", e.to_json());
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn add_args() -> CniArgs {
        let m: HashMap<String, String> = [
            ("CNI_COMMAND", "ADD"),
            ("CNI_CONTAINERID", "cid1"),
            ("CNI_IFNAME", "eth0"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        CniArgs::from_map(&m).unwrap()
    }

    // With no portMappings, ADD must output the prevResult (a CNI Result), not the
    // netconf. Emitting the config breaks sandbox setup ("failed to find network
    // info for sandbox"). No iptables is touched on this path.
    #[test]
    fn add_noop_relays_prevresult_not_config() {
        let stdin = r#"{"cniVersion":"0.3.1","name":"cbr0","type":"portmap","runtimeConfig":{"portMappings":[]},"prevResult":{"cniVersion":"0.3.1","ips":[{"version":"4","address":"10.244.1.5/24","gateway":"10.244.1.1"}]}}"#;
        let out = cmd_add(&add_args(), stdin).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(
            v.get("runtimeConfig").is_none(),
            "must not emit the netconf"
        );
        assert!(
            v.get("prevResult").is_none(),
            "must emit the result itself, not wrap it"
        );
        assert_eq!(v["ips"][0]["address"], "10.244.1.5/24");
    }
}
