use crate::alloc::Allocator;
use crate::store::Store;
use cni::config::NetConf;
use cni::env::CniArgs;
use cni::error::CniError;
use cni::result::{CniResult, IpResult};
use ipnetwork::Ipv4Network;
use std::net::Ipv4Addr;

const DEFAULT_DATA_DIR: &str = "/var/lib/cni/networks";

fn load(stdin: &str) -> Result<NetConf, CniError> {
    NetConf::parse(stdin).map_err(|e| CniError::new(6, "failed to decode network config").with_details(e.to_string()))
}

fn allocator_for(nc: &NetConf) -> Result<Allocator, CniError> {
    let range = nc.ipam.ranges.first().and_then(|r| r.first())
        .ok_or_else(|| CniError::new(7, "ipam.ranges is empty"))?;
    let net: Ipv4Network = range.subnet.parse()
        .map_err(|_| CniError::new(7, format!("invalid subnet {}", range.subnet)))?;
    // Guard against a misconfigured wide range: allocation materializes the host
    // set, so cap at /16 (flannel hands host-local a per-node /24).
    if net.prefix() < 16 {
        return Err(CniError::new(7, format!("subnet {} too large (min prefix /16)", range.subnet)));
    }
    let gw: Option<Ipv4Addr> = match &range.gateway {
        Some(g) => Some(g.parse().map_err(|_| CniError::new(7, format!("invalid gateway {g}")))?),
        None => None,
    };
    Ok(Allocator::new(net, gw))
}

fn store_for(nc: &NetConf) -> Result<Store, CniError> {
    let data_dir = nc.ipam.data_dir.as_deref().unwrap_or(DEFAULT_DATA_DIR);
    Store::new(data_dir, &nc.name).map_err(|e| CniError::new(5, "failed to open data dir").with_details(e.to_string()))
}

pub fn cmd_add(args: &CniArgs, stdin: &str) -> Result<String, CniError> {
    let nc = load(stdin)?;
    let alloc = allocator_for(&nc)?;
    let store = store_for(&nc)?;
    let _lock = store.lock().map_err(|e| CniError::new(11, "failed to lock data dir").with_details(e.to_string()))?;

    let leased = store.leased().map_err(|e| CniError::new(5, "read leases").with_details(e.to_string()))?;
    let ip = alloc.next_ip(&leased, store.last_reserved())
        .ok_or_else(|| CniError::new(7, "no IP addresses available in range"))?;
    store.reserve(ip, &args.container_id, &args.ifname)
        .map_err(|e| CniError::new(5, "write lease").with_details(e.to_string()))?;

    let result = CniResult {
        cni_version: if nc.cni_version.is_empty() { "0.3.1".into() } else { nc.cni_version.clone() },
        ips: vec![IpResult {
            version: "4".into(),
            address: format!("{}/{}", ip, alloc.prefix()),
            gateway: Some(alloc.gateway().to_string()),
        }],
        routes: nc.ipam.routes.clone(),
    };
    Ok(result.to_json())
}

pub fn cmd_del(args: &CniArgs, stdin: &str) -> Result<String, CniError> {
    let nc = load(stdin)?;
    let store = store_for(&nc)?;
    let _lock = store.lock().map_err(|e| CniError::new(11, "failed to lock data dir").with_details(e.to_string()))?;
    store.release(&args.container_id, &args.ifname)
        .map_err(|e| CniError::new(5, "release lease").with_details(e.to_string()))?;
    Ok(String::new())
}

pub fn cmd_check(args: &CniArgs, stdin: &str) -> Result<String, CniError> {
    let nc = load(stdin)?;
    let store = store_for(&nc)?;
    if store.has(&args.container_id, &args.ifname).map_err(|e| CniError::new(5, "check lease").with_details(e.to_string()))? {
        Ok(String::new())
    } else {
        Err(CniError::new(7, "no allocation found for container"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn args(cmd: &str, cid: &str) -> CniArgs {
        let m: HashMap<String, String> = [("CNI_COMMAND", cmd), ("CNI_CONTAINERID", cid), ("CNI_IFNAME", "eth0")]
            .iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        CniArgs::from_map(&m).unwrap()
    }

    fn conf(data_dir: &str) -> String {
        format!(r#"{{"cniVersion":"0.3.1","name":"cbr0","ipam":{{"type":"host-local","ranges":[[{{"subnet":"10.244.1.0/24"}}]],"routes":[{{"dst":"0.0.0.0/0"}}],"dataDir":"{data_dir}"}}}}"#)
    }

    #[test]
    fn add_allocates_first_host_and_persists() {
        let tmp = tempfile::tempdir().unwrap();
        let c = conf(tmp.path().to_str().unwrap());
        let out = cmd_add(&args("ADD", "cid1"), &c).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["ips"][0]["address"], "10.244.1.2/24");
        assert_eq!(v["ips"][0]["gateway"], "10.244.1.1");
        assert_eq!(v["routes"][0]["dst"], "0.0.0.0/0");
    }

    #[test]
    fn second_add_gets_next_ip() {
        let tmp = tempfile::tempdir().unwrap();
        let c = conf(tmp.path().to_str().unwrap());
        cmd_add(&args("ADD", "cid1"), &c).unwrap();
        let out = cmd_add(&args("ADD", "cid2"), &c).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["ips"][0]["address"], "10.244.1.3/24");
    }

    #[test]
    fn del_frees_ip_for_reuse() {
        let tmp = tempfile::tempdir().unwrap();
        let c = conf(tmp.path().to_str().unwrap());
        cmd_add(&args("ADD", "cid1"), &c).unwrap();
        cmd_del(&args("DEL", "cid1"), &c).unwrap();
        let out = cmd_add(&args("ADD", "cid3"), &c).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        // The freed .2 is now available again, but DEL does not rewind
        // last_reserved_ip (round-robin semantics, matching Go host-local),
        // so the next allocation continues from after .2 and hands out .3.
        assert_eq!(v["ips"][0]["address"], "10.244.1.3/24");
        // Confirm .2 was actually freed and is allocatable on a subsequent wrap.
        assert!(!crate::store::Store::new(tmp.path().to_str().unwrap(), "cbr0")
            .unwrap()
            .leased()
            .unwrap()
            .contains(&"10.244.1.2".parse().unwrap()));
    }

    #[test]
    fn check_reflects_allocation() {
        let tmp = tempfile::tempdir().unwrap();
        let c = conf(tmp.path().to_str().unwrap());
        assert!(cmd_check(&args("CHECK", "cid1"), &c).is_err());
        cmd_add(&args("ADD", "cid1"), &c).unwrap();
        assert!(cmd_check(&args("CHECK", "cid1"), &c).is_ok());
    }

    #[test]
    fn rejects_oversized_subnet() {
        let tmp = tempfile::tempdir().unwrap();
        let c = format!(
            r#"{{"cniVersion":"0.3.1","name":"cbr0","ipam":{{"type":"host-local","ranges":[[{{"subnet":"10.0.0.0/8"}}]],"dataDir":"{}"}}}}"#,
            tmp.path().to_str().unwrap()
        );
        let err = cmd_add(&args("ADD", "cid1"), &c).unwrap_err();
        assert_eq!(err.code, 7);
    }
}
