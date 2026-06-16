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

/// Build (delegate_type, delegate_json) from the stdin netconf and the loaded
/// subnet.env (None if absent). On DEL, a missing subnet.env is tolerated with a
/// placeholder env: CNI DEL must be best-effort, and the subnet/mtu values are
/// irrelevant to teardown (host-local frees by containerID, bridge by netns).
/// On ADD/CHECK a missing subnet.env is a try-again error (code 11).
fn resolve_delegate(
    stdin: &str,
    env: Option<subnetenv::SubnetEnv>,
    command: &str,
) -> Result<(String, String), CniError> {
    let conf = delegate::FlannelConf::parse(stdin).map_err(|e| {
        CniError::new(6, "failed to decode network config").with_details(e.to_string())
    })?;
    let env = match env {
        Some(e) => e,
        None if command == "DEL" => subnetenv::SubnetEnv {
            network: String::new(),
            subnet: String::new(),
            mtu: 0,
            ipmasq: false,
        },
        None => {
            return Err(CniError::new(
                11,
                "failed to read /run/flannel/subnet.env (flanneld not ready?)",
            ))
        }
    };
    let d = delegate::build_delegate(&conf, &env);
    let dtype = d
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or("bridge")
        .to_string();
    let json = serde_json::to_string(&d)
        .map_err(|e| CniError::new(6, "encode delegate").with_details(e.to_string()))?;
    Ok((dtype, json))
}

/// Returns (delegate_type, delegate_json), loading subnet.env from disk.
fn delegate_json(stdin: &str, command: &str) -> Result<(String, String), CniError> {
    let env = subnetenv::SubnetEnv::load(SUBNET_ENV_PATH).ok();
    resolve_delegate(stdin, env, command)
}

/// Returns (stdout_to_relay, success).
fn run() -> Result<(String, bool), CniError> {
    let args = CniArgs::from_env()?;
    match args.command.as_str() {
        "VERSION" => Ok((VersionResult::supported().to_json(), true)),
        "ADD" | "DEL" | "CHECK" => {
            let stdin = read_stdin();
            let (dtype, djson) = delegate_json(&stdin, &args.command)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> subnetenv::SubnetEnv {
        subnetenv::SubnetEnv {
            network: "10.244.0.0/16".into(),
            subnet: "10.244.1.0/24".into(),
            mtu: 1450,
            ipmasq: true,
        }
    }

    const CONF: &str = r#"{"name":"cbr0","cniVersion":"0.3.1","type":"flannel","delegate":{}}"#;

    #[test]
    fn add_requires_subnet_env() {
        let err = resolve_delegate(CONF, None, "ADD").unwrap_err();
        assert_eq!(err.code, 11);
    }

    #[test]
    fn del_tolerates_missing_subnet_env() {
        let (dtype, json) = resolve_delegate(CONF, None, "DEL").unwrap();
        assert_eq!(dtype, "bridge");
        // builds a valid delegate even without subnet.env, so bridge DEL can run
        assert!(json.contains("\"ipam\""));
    }

    #[test]
    fn add_with_subnet_env_builds_ipam() {
        let (_dtype, json) = resolve_delegate(CONF, Some(env()), "ADD").unwrap();
        assert!(json.contains("10.244.1.0/24"));
    }
}
