mod delegate;
mod exec;
mod subnetenv;

use cni::env::CniArgs;
use cni::error::CniError;
use cni::version::VersionResult;
use std::io::Read;
use std::process::ExitCode;

const SUBNET_ENV_PATH: &str = "/run/flannel/subnet.env";

fn read_stdin() -> String {
    let mut s = String::new();
    let _ = std::io::stdin().read_to_string(&mut s);
    s
}

/// Returns (delegate_type, delegate_json).
fn delegate_json(stdin: &str) -> Result<(String, String), CniError> {
    let conf = delegate::FlannelConf::parse(stdin)
        .map_err(|e| CniError::new(6, "failed to decode network config").with_details(e.to_string()))?;
    let env = subnetenv::SubnetEnv::load(SUBNET_ENV_PATH)
        .map_err(|e| CniError::new(11, "failed to read /run/flannel/subnet.env (flanneld not ready?)").with_details(e))?;
    let d = delegate::build_delegate(&conf, &env);
    let dtype = d.get("type").and_then(|t| t.as_str()).unwrap_or("bridge").to_string();
    let json = serde_json::to_string(&d).map_err(|e| CniError::new(6, "encode delegate").with_details(e.to_string()))?;
    Ok((dtype, json))
}

/// Returns (stdout_to_relay, success).
fn run() -> Result<(String, bool), CniError> {
    let args = CniArgs::from_env()?;
    match args.command.as_str() {
        "VERSION" => Ok((VersionResult::supported().to_json(), true)),
        "ADD" | "DEL" | "CHECK" => {
            let stdin = read_stdin();
            let (dtype, djson) = delegate_json(&stdin)?;
            let out = exec::run_delegate(&dtype, &args, &djson)?;
            Ok((out.stdout, out.success))
        }
        other => Err(CniError::new(4, format!("unknown CNI_COMMAND {other}"))),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok((out, success)) => {
            if !out.is_empty() {
                print!("{out}");
            }
            if success {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(e) => {
            print!("{}", e.to_json());
            ExitCode::FAILURE
        }
    }
}
