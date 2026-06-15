use std::net::Ipv4Addr;
use anyhow::{Context, Result};
use futures::TryStreamExt;
use ipnetwork::Ipv4Network;
use rtnetlink::{Handle, new_connection};
use crate::peer::Peer;

pub struct Netlink {
    handle: Handle,
}

impl Netlink {
    pub fn new() -> Result<Self> {
        let (conn, handle, _) = new_connection()?;
        tokio::spawn(conn);
        Ok(Self { handle })
    }

    pub async fn ensure_vxlan(
        &self,
        name: &str,
        vni: u32,
        dstport: u16,
        local: Ipv4Addr,
        gateway: Ipv4Addr,
    ) -> Result<(String, u32)> {
        if let Some(idx) = self.link_index(name).await? {
            let mac = self.link_mac(idx).await?;
            self.bring_up(idx).await?;
            return Ok((mac, idx));
        }
        self.create_vxlan(name, vni, dstport, local).await?;
        let idx = self
            .link_index(name)
            .await?
            .context("vxlan link missing after create")?;
        self.add_address(idx, gateway).await.ok();
        self.bring_up(idx).await?;
        let mac = self.link_mac(idx).await?;
        Ok((mac, idx))
    }

    async fn create_vxlan(
        &self,
        name: &str,
        vni: u32,
        dstport: u16,
        local: Ipv4Addr,
    ) -> Result<()> {
        self.handle
            .link()
            .add()
            .vxlan(name.to_string(), vni)
            .port(dstport)
            .local(local)
            .learning(false)
            .up()
            .execute()
            .await
            .context("create vxlan link")?;
        Ok(())
    }

    async fn add_address(&self, idx: u32, gateway: Ipv4Addr) -> Result<()> {
        self.handle
            .address()
            .add(idx, gateway.into(), 32)
            .execute()
            .await?;
        Ok(())
    }

    async fn link_index(&self, name: &str) -> Result<Option<u32>> {
        let mut links = self.handle.link().get().match_name(name.to_string()).execute();
        match links.try_next().await {
            Ok(Some(l)) => Ok(Some(l.header.index)),
            Ok(None) => Ok(None),
            Err(rtnetlink::Error::NetlinkError(e)) if e.raw_code() == -19 => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn link_mac(&self, index: u32) -> Result<String> {
        use netlink_packet_route::link::LinkAttribute;
        let mut links = self.handle.link().get().match_index(index).execute();
        let link = links.try_next().await?.context("link disappeared")?;
        for attr in link.attributes {
            if let LinkAttribute::Address(bytes) = attr {
                return Ok(bytes
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<Vec<_>>()
                    .join(":"));
            }
        }
        anyhow::bail!("no MAC on link {index}")
    }

    async fn bring_up(&self, index: u32) -> Result<()> {
        self.handle.link().set(index).up().execute().await?;
        Ok(())
    }

    pub async fn add_route(&self, dev: u32, peer: &Peer) -> Result<()> {
        use netlink_packet_route::route::RouteFlag;

        let net: Ipv4Network = peer.pod_cidr.parse().context("parse peer cidr")?;
        // Route the peer's pod CIDR via the peer's VTEP IP (x.x.x.0) with the
        // ONLINK flag, matching upstream flannel. Without a gateway the route is
        // a connected route and the kernel ARPs for the individual destination
        // pod IP on flannel.1 (nolearning) -> resolution FAILS. Routing via the
        // VTEP IP makes the kernel resolve the gateway, which has a PERMANENT
        // neigh entry -> the inner frame gets the peer's flannel.1 MAC -> the
        // fdb maps that MAC to the peer's underlay IP -> packet is delivered.
        let mut r = self
            .handle
            .route()
            .add()
            .v4()
            .destination_prefix(net.network(), net.prefix())
            .output_interface(dev)
            .gateway(net.network());
        r.message_mut().header.flags.push(RouteFlag::Onlink);
        if let Err(rtnetlink::Error::NetlinkError(e)) = r.execute().await {
            if e.raw_code() != -17 {
                anyhow::bail!("add route: {e:?}");
            }
        }
        Ok(())
    }

    pub async fn add_peer_l2(&self, dev: u32, peer: &Peer) -> Result<()> {
        use netlink_packet_route::neighbour::NeighbourFlag;

        let mac = parse_mac(&peer.vtep_mac)?;
        let cidr: Ipv4Network = peer.pod_cidr.parse()?;
        let vtep_ip = cidr.network();
        let public: Ipv4Addr = peer.public_ip.parse()?;
        // neigh: peer overlay IP (x.x.x.0) -> peer flannel.1 MAC (resolves the
        // inner-frame destination MAC for the peer's subnet route).
        self.handle
            .neighbours()
            .add(dev, vtep_ip.into())
            .link_local_address(&mac)
            .execute()
            .await
            .ok();
        // fdb: peer flannel.1 MAC -> peer underlay (node) IP. This MUST carry
        // NTF_SELF (NeighbourFlag::Own); without it the kernel refuses to add
        // the entry on the vxlan device itself, so encapsulated frames have no
        // underlay destination and cross-node traffic is silently dropped.
        if let Err(e) = self
            .handle
            .neighbours()
            .add_bridge(dev, &mac)
            .destination(public.into())
            .flags(vec![NeighbourFlag::Own])
            .execute()
            .await
        {
            tracing::warn!("add fdb entry for peer {} failed: {e:?}", peer.public_ip);
        }
        Ok(())
    }

    pub async fn del_peer(&self, dev: u32, peer: &Peer) -> Result<()> {
        use netlink_packet_route::route::{RouteAddress, RouteAttribute};
        use netlink_packet_route::neighbour::{NeighbourAddress, NeighbourAttribute};

        let net: Ipv4Network = peer.pod_cidr.parse().context("parse peer cidr")?;
        let vtep_ip = net.network(); // x.x.x.0

        // Remove route to peer CIDR via dev (match by destination prefix).
        let mut routes = self.handle.route().get(rtnetlink::IpVersion::V4).execute();
        while let Some(route) = routes.try_next().await? {
            if route.header.destination_prefix_length != net.prefix() {
                continue;
            }
            let matches_dest = route.attributes.iter().any(|attr| {
                matches!(attr, RouteAttribute::Destination(RouteAddress::Inet(addr)) if *addr == vtep_ip)
            });
            if matches_dest {
                let _ = self.handle.route().del(route).execute().await;
                break;
            }
        }

        // Remove neigh (best-effort); entries otherwise age out.
        let mut neighbours = self.handle.neighbours().get().execute();
        while let Ok(Some(neigh)) = neighbours.try_next().await {
            if neigh.header.ifindex != dev {
                continue;
            }
            let matches_ip = neigh.attributes.iter().any(|attr| {
                matches!(attr, NeighbourAttribute::Destination(NeighbourAddress::Inet(addr)) if *addr == vtep_ip)
            });
            if matches_ip {
                let _ = self.handle.neighbours().del(neigh).execute().await;
                break;
            }
        }
        Ok(())
    }
}

fn parse_mac(s: &str) -> Result<[u8; 6]> {
    let parts: Vec<u8> = s
        .split(':')
        .map(|h| u8::from_str_radix(h, 16))
        .collect::<Result<_, _>>()
        .context("parse mac")?;
    let arr: [u8; 6] = parts
        .try_into()
        .map_err(|_| anyhow::anyhow!("mac not 6 bytes"))?;
    Ok(arr)
}
