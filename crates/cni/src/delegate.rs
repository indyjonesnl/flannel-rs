use crate::env::CniArgs;
use crate::error::CniError;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// stdout of the delegate, plus whether it exited 0.
#[derive(Debug)]
pub struct DelegateOutput {
    pub stdout: String,
    pub success: bool,
}

/// Find `name` in CNI_PATH (colon-separated), exec it with the same CNI_* env and
/// `stdin_json` piped to stdin. `Err` only for our own failures (not found / spawn).
pub fn run_delegate(
    name: &str,
    args: &CniArgs,
    stdin_json: &str,
) -> Result<DelegateOutput, CniError> {
    let bin = find_in_path(name, &args.path).ok_or_else(|| {
        CniError::new(5, format!("delegate plugin {name:?} not found in CNI_PATH"))
    })?;
    let mut child = Command::new(&bin)
        .env("CNI_COMMAND", &args.command)
        .env("CNI_CONTAINERID", &args.container_id)
        .env("CNI_NETNS", &args.netns)
        .env("CNI_IFNAME", &args.ifname)
        .env("CNI_ARGS", &args.args)
        .env("CNI_PATH", &args.path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| {
            CniError::new(5, format!("exec delegate {name} failed")).with_details(e.to_string())
        })?;
    // Write stdin on a separate thread while wait_with_output() drains the
    // child's stdout, so a delegate that fills its stdout pipe buffer before
    // reading all of stdin can't deadlock against us.
    let mut stdin = child.stdin.take().expect("stdin piped");
    let payload = stdin_json.as_bytes().to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&payload);
        // stdin dropped here -> EOF to the child.
    });
    let out = child
        .wait_with_output()
        .map_err(|e| CniError::new(5, "wait for delegate").with_details(e.to_string()))?;
    let _ = writer.join();
    Ok(DelegateOutput {
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        success: out.status.success(),
    })
}

fn find_in_path(name: &str, cni_path: &str) -> Option<PathBuf> {
    for dir in cni_path.split(':').filter(|d| !d.is_empty()) {
        let p = Path::new(dir).join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::os::unix::fs::PermissionsExt;

    fn args(path: &str) -> CniArgs {
        let m: HashMap<String, String> = [
            ("CNI_COMMAND", "ADD"),
            ("CNI_CONTAINERID", "cid1"),
            ("CNI_IFNAME", "eth0"),
            ("CNI_PATH", path),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        CniArgs::from_map(&m).unwrap()
    }

    fn write_script(dir: &Path, name: &str, body: &str) {
        use std::io::Write;
        let p = dir.join(name);
        // Create executable + fully close the write fd before any exec, or a
        // concurrent test can hit ETXTBSY ("Text file busy") execing it.
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o755)
            .open(&p)
            .unwrap();
        f.write_all(body.as_bytes()).unwrap();
        f.sync_all().unwrap();
        drop(f);
    }

    #[test]
    fn execs_delegate_and_relays_stdout() {
        let tmp = tempfile::tempdir().unwrap();
        write_script(tmp.path(), "bridge", "#!/bin/sh\ncat > \"$CNI_PATH/received_stdin\"\necho '{\"cniVersion\":\"0.3.1\",\"ips\":[]}'\n");
        let path = tmp.path().to_str().unwrap();
        let out = run_delegate("bridge", &args(path), r#"{"type":"bridge","x":1}"#).unwrap();
        assert!(out.success);
        let v: serde_json::Value = serde_json::from_str(out.stdout.trim()).unwrap();
        assert_eq!(v["cniVersion"], "0.3.1");
        let received = std::fs::read_to_string(tmp.path().join("received_stdin")).unwrap();
        assert!(received.contains("\"type\":\"bridge\""));
    }

    #[test]
    fn failing_delegate_reports_unsuccessful() {
        let tmp = tempfile::tempdir().unwrap();
        write_script(
            tmp.path(),
            "bridge",
            "#!/bin/sh\necho '{\"code\":7,\"msg\":\"boom\"}'\nexit 1\n",
        );
        let path = tmp.path().to_str().unwrap();
        let out = run_delegate("bridge", &args(path), "{}").unwrap();
        assert!(!out.success);
        assert!(out.stdout.contains("boom"));
    }

    #[test]
    fn missing_delegate_is_error() {
        let tmp = tempfile::tempdir().unwrap();
        let err =
            run_delegate("nonexistent", &args(tmp.path().to_str().unwrap()), "{}").unwrap_err();
        assert_eq!(err.code, 5);
    }
}
