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
    let mappings = &conf.runtime_config.port_mappings;
    if mappings.is_empty() {
        return Ok(stdin.to_string()); // nothing to do; relay prevResult (the whole config carries it)
    }
    let prev = conf
        .prev_result
        .as_ref()
        .ok_or_else(|| err(7, "portmap requires prevResult"))?;
    let pod_ip = config::pod_ipv4(prev).ok_or_else(|| err(7, "no IPv4 in prevResult"))?;

    let ipt = Iptables::detect();
    rules::apply(&ipt, &args.container_id, pod_ip, mappings)?;

    // Relay prevResult unchanged for any later chained plugin.
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
