mod alloc;
mod command;
mod store;

use cni::env::CniArgs;
use cni::error::CniError;
use cni::version::VersionResult;
use std::io::Read;
use std::process::ExitCode;

fn read_stdin() -> String {
    let mut s = String::new();
    let _ = std::io::stdin().read_to_string(&mut s);
    s
}

fn run() -> Result<String, CniError> {
    let args = CniArgs::from_env()?;
    match args.command.as_str() {
        "VERSION" => Ok(VersionResult::supported().to_json()),
        "ADD" => command::cmd_add(&args, &read_stdin()),
        "DEL" => command::cmd_del(&args, &read_stdin()),
        "CHECK" => command::cmd_check(&args, &read_stdin()),
        other => Err(CniError::new(4, format!("unknown CNI_COMMAND {other}"))),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(out) => {
            if !out.is_empty() { print!("{out}"); }
            ExitCode::SUCCESS
        }
        Err(e) => {
            print!("{}", e.to_json());
            ExitCode::FAILURE
        }
    }
}
