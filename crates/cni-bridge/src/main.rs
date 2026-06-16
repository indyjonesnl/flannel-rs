mod config;
mod contns;
mod hostns;
mod plan;

use cni::env::CniArgs;
use cni::error::CniError;
use cni::result::CniResult;
use cni::version::VersionResult;
use config::BridgeConf;
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

async fn cmd_add(args: &CniArgs, conf: &BridgeConf, stdin: &str) -> Result<String, CniError> {
    // host-side netlink connection
    let (conn, h, _) =
        rtnetlink::new_connection().map_err(|e| err(5, "netlink").with_details(e.to_string()))?;
    tokio::spawn(conn);

    // 1. ensure bridge
    let bridge_idx = hostns::ensure_bridge(&h, &conf.bridge, conf.mtu)
        .await
        .map_err(|e| err(7, "ensure bridge").with_details(format!("{e:#}")))?;

    // 2. IPAM ADD
    let out = cni::delegate::run_delegate(&conf.ipam.kind, args, stdin)?;
    if !out.success {
        // relay IPAM error verbatim
        return Err(err(7, "ipam add failed").with_details(out.stdout));
    }
    let ipam = CniResult::parse(&out.stdout)
        .map_err(|e| err(6, "parse ipam result").with_details(e.to_string()))?;
    let ipplan =
        plan::ip_plan(&ipam).map_err(|e| err(7, "ipam plan").with_details(e.to_string()))?;

    // 3. veth pair
    let host_veth = plan::host_veth_name(&args.container_id);
    let temp_cont = plan::temp_cont_name(&args.container_id);
    let (host_idx, peer_idx) = hostns::create_veth(&h, &host_veth, &temp_cont, conf.mtu)
        .await
        .map_err(|e| err(5, "create veth").with_details(format!("{e:#}")))?;

    // 4. move container end into the netns, configure it
    let netns_fd =
        open_netns(&args.netns).map_err(|e| err(5, "open netns").with_details(e.to_string()))?;
    if let Err(e) = hostns::move_to_netns(&h, peer_idx, netns_fd).await {
        hostns::del_link(&h, host_idx).await;
        return Err(err(5, "move veth to netns").with_details(format!("{e:#}")));
    }
    if let Err(e) = contns::configure_container_iface(
        args.netns.clone(),
        temp_cont.clone(),
        args.ifname.clone(),
        ipplan,
        conf.is_default_gateway,
    ) {
        hostns::del_link(&h, host_idx).await;
        return Err(err(7, "configure container iface").with_details(format!("{e:#}")));
    }

    // 5. attach host veth to bridge + hairpin
    if let Err(e) = hostns::attach_host_veth(&h, host_idx, bridge_idx, conf.hairpin_mode).await {
        hostns::del_link(&h, host_idx).await;
        return Err(err(5, "attach host veth").with_details(format!("{e:#}")));
    }

    // 6. isGateway: bridge IP + ip_forward
    if conf.is_gateway {
        if let Some(gw) = ipam
            .ips
            .first()
            .and_then(|i| i.gateway.as_ref())
            .and_then(|g| g.parse().ok())
        {
            let prefix = ipam.ips[0]
                .address
                .split('/')
                .nth(1)
                .and_then(|p| p.parse().ok())
                .unwrap_or(24);
            let _ = hostns::set_bridge_gateway(&h, bridge_idx, gw, prefix).await;
        }
        let _ = std::fs::write("/proc/sys/net/ipv4/ip_forward", "1");
    }

    // 7. relay the IPAM result as our result (0.3.1 chain: portmap consumes it)
    Ok(out.stdout)
}

fn cmd_del(args: &CniArgs, conf: &BridgeConf, stdin: &str) -> Result<String, CniError> {
    // IPAM DEL (best-effort)
    let _ = cni::delegate::run_delegate(&conf.ipam.kind, args, stdin);
    // remove container iface (removes veth pair)
    if !args.netns.is_empty() {
        let _ = contns::delete_container_iface(args.netns.clone(), args.ifname.clone());
    }
    Ok(String::new())
}

fn open_netns(path: &str) -> std::io::Result<std::os::fd::RawFd> {
    use std::os::fd::IntoRawFd;
    Ok(std::fs::File::open(path)?.into_raw_fd())
}

fn run(rt: &tokio::runtime::Runtime) -> Result<(String, bool), CniError> {
    let args = CniArgs::from_env()?;
    match args.command.as_str() {
        "VERSION" => Ok((VersionResult::supported().to_json(), true)),
        "ADD" => {
            let stdin = read_stdin();
            let conf = BridgeConf::parse(&stdin)
                .map_err(|e| err(6, "decode config").with_details(e.to_string()))?;
            rt.block_on(cmd_add(&args, &conf, &stdin))
                .map(|s| (s, true))
        }
        "DEL" => {
            let stdin = read_stdin();
            let conf = BridgeConf::parse(&stdin)
                .map_err(|e| err(6, "decode config").with_details(e.to_string()))?;
            cmd_del(&args, &conf, &stdin).map(|s| (s, true))
        }
        "CHECK" => Ok((String::new(), true)), // 0.3.1 never calls CHECK
        other => Err(err(4, format!("unknown CNI_COMMAND {other}"))),
    }
}

fn main() -> ExitCode {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            print!(
                "{}",
                err(5, "build runtime")
                    .with_details(e.to_string())
                    .to_json()
            );
            return ExitCode::FAILURE;
        }
    };
    match run(&rt) {
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
