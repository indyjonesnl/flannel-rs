use fs2::FileExt;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

/// Disk-backed lease store, format-compatible with Go host-local:
/// `<data_dir>/<network>/<ip>` (contents `containerID\nifname`),
/// `last_reserved_ip.0`, and a `lock` file for `flock`.
pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(data_dir: &str, network: &str) -> io::Result<Self> {
        let dir = Path::new(data_dir).join(network);
        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    pub fn lock(&self) -> io::Result<File> {
        let f = File::create(self.dir.join("lock"))?;
        f.lock_exclusive()?;
        Ok(f)
    }

    pub fn leased(&self) -> io::Result<HashSet<Ipv4Addr>> {
        let mut set = HashSet::new();
        for entry in fs::read_dir(&self.dir)? {
            let name = entry?.file_name().to_string_lossy().to_string();
            if let Ok(ip) = name.parse::<Ipv4Addr>() {
                set.insert(ip);
            }
        }
        Ok(set)
    }

    pub fn last_reserved(&self) -> Option<Ipv4Addr> {
        fs::read_to_string(self.dir.join("last_reserved_ip.0"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
    }

    pub fn reserve(&self, ip: Ipv4Addr, container_id: &str, ifname: &str) -> io::Result<()> {
        fs::write(self.dir.join(ip.to_string()), format!("{container_id}\n{ifname}"))?;
        fs::write(self.dir.join("last_reserved_ip.0"), ip.to_string())?;
        Ok(())
    }

    pub fn release(&self, container_id: &str, ifname: &str) -> io::Result<()> {
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.parse::<Ipv4Addr>().is_err() {
                continue;
            }
            let content = fs::read_to_string(entry.path()).unwrap_or_default();
            let mut lines = content.lines();
            let cid = lines.next().unwrap_or("");
            let ifn = lines.next().unwrap_or("");
            if cid == container_id && ifn == ifname {
                fs::remove_file(entry.path())?;
            }
        }
        Ok(())
    }

    pub fn has(&self, container_id: &str, ifname: &str) -> io::Result<bool> {
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.parse::<Ipv4Addr>().is_err() {
                continue;
            }
            let content = fs::read_to_string(entry.path()).unwrap_or_default();
            let mut lines = content.lines();
            if lines.next() == Some(container_id) && lines.next() == Some(ifname) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> Ipv4Addr { s.parse().unwrap() }

    #[test]
    fn reserve_then_leased_and_last() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Store::new(tmp.path().to_str().unwrap(), "cbr0").unwrap();
        s.reserve(ip("10.244.1.2"), "cid1", "eth0").unwrap();
        assert!(s.leased().unwrap().contains(&ip("10.244.1.2")));
        assert_eq!(s.last_reserved(), Some(ip("10.244.1.2")));
        assert!(s.has("cid1", "eth0").unwrap());
    }

    #[test]
    fn release_is_idempotent_and_targeted() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Store::new(tmp.path().to_str().unwrap(), "cbr0").unwrap();
        s.reserve(ip("10.244.1.2"), "cid1", "eth0").unwrap();
        s.reserve(ip("10.244.1.3"), "cid2", "eth0").unwrap();
        s.release("cid1", "eth0").unwrap();
        assert!(!s.leased().unwrap().contains(&ip("10.244.1.2")));
        assert!(s.leased().unwrap().contains(&ip("10.244.1.3")));
        s.release("cid1", "eth0").unwrap();
        assert!(!s.has("cid1", "eth0").unwrap());
    }
}
