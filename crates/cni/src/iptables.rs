use crate::error::CniError;
use std::process::Command;

/// Thin wrapper over the iptables CLI, pinned to the backend (nft/legacy) that
/// matches kube-proxy's active rules. All operations target the `nat` table.
pub struct Iptables {
    backend: String,
}

impl Iptables {
    /// Pick the backend whose `nat` table holds kube-proxy's `KUBE-` chains.
    /// Falls back to plain `iptables`.
    pub fn detect() -> Self {
        for b in ["iptables-nft", "iptables-legacy"] {
            if let Ok(out) = Command::new(b).args(["-t", "nat", "-S"]).output() {
                if out.status.success() && String::from_utf8_lossy(&out.stdout).contains("KUBE-") {
                    return Self {
                        backend: b.to_string(),
                    };
                }
            }
        }
        Self {
            backend: "iptables".to_string(),
        }
    }

    /// Explicit backend (used by tests and when detection is not needed).
    pub fn with_backend(backend: impl Into<String>) -> Self {
        Self {
            backend: backend.into(),
        }
    }

    fn run(&self, args: &[&str]) -> Result<std::process::Output, CniError> {
        Command::new(&self.backend)
            .arg("--wait")
            .args(args)
            .output()
            .map_err(|e| {
                CniError::new(7, format!("exec {}", self.backend)).with_details(e.to_string())
            })
    }

    /// Create a nat chain if absent.
    pub fn ensure_chain(&self, chain: &str) -> Result<(), CniError> {
        let out = self.run(&["-t", "nat", "-N", chain])?;
        if out.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("exists") {
            return Ok(()); // already created
        }
        Err(CniError::new(7, format!("create chain {chain}")).with_details(stderr.to_string()))
    }

    /// Append a rule to a nat chain if not already present (`-C` then `-A`).
    pub fn ensure_rule(&self, chain: &str, rule: &[&str]) -> Result<(), CniError> {
        let mut check = vec!["-t", "nat", "-C", chain];
        check.extend_from_slice(rule);
        if self.run(&check)?.status.success() {
            return Ok(());
        }
        let mut add = vec!["-t", "nat", "-A", chain];
        add.extend_from_slice(rule);
        let out = self.run(&add)?;
        if out.status.success() {
            Ok(())
        } else {
            Err(CniError::new(7, format!("append rule to {chain}"))
                .with_details(String::from_utf8_lossy(&out.stderr).to_string()))
        }
    }

    /// Delete a rule (best-effort; missing is fine).
    pub fn delete_rule(&self, chain: &str, rule: &[&str]) {
        let mut del = vec!["-t", "nat", "-D", chain];
        del.extend_from_slice(rule);
        let _ = self.run(&del);
    }

    /// Flush and delete a chain (best-effort).
    pub fn flush_delete_chain(&self, chain: &str) {
        let _ = self.run(&["-t", "nat", "-F", chain]);
        let _ = self.run(&["-t", "nat", "-X", chain]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::Path;

    // A fake iptables that records each invocation's args to $FAKE_LOG and
    // exits 0, except `-C` (check) exits 1 so ensure_rule proceeds to `-A`.
    fn fake_iptables(dir: &Path) -> String {
        use std::io::Write;
        let p = dir.join("fake-iptables");
        // Create with the exec bit set and fully close (sync + drop) the write
        // handle before returning, so a later exec can't race an open fd
        // (ETXTBSY "Text file busy") on a busy test host.
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o755)
            .open(&p)
            .unwrap();
        f.write_all(
            b"#!/bin/sh\necho \"$@\" >> \"$FAKE_LOG\"\nfor a in \"$@\"; do [ \"$a\" = \"-C\" ] && exit 1; done\nexit 0\n",
        )
        .unwrap();
        f.sync_all().unwrap();
        drop(f);
        p.to_str().unwrap().to_string()
    }

    #[test]
    fn ensure_rule_checks_then_appends() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("log");
        std::env::set_var("FAKE_LOG", &log);
        let ipt = Iptables::with_backend(fake_iptables(tmp.path()));
        ipt.ensure_rule("CNI-HOSTPORT-DNAT", &["-j", "CNI-DN-abc"])
            .unwrap();
        let recorded = std::fs::read_to_string(&log).unwrap();
        assert!(
            recorded.contains("-C CNI-HOSTPORT-DNAT -j CNI-DN-abc"),
            "{recorded}"
        );
        assert!(
            recorded.contains("-A CNI-HOSTPORT-DNAT -j CNI-DN-abc"),
            "{recorded}"
        );
        std::env::remove_var("FAKE_LOG");
    }
}
