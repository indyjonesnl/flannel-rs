use ipnetwork::Ipv4Network;
use std::collections::HashSet;
use std::net::Ipv4Addr;

/// Pure IP allocator over a single subnet. Excludes the network address,
/// the broadcast address, and the gateway. No I/O.
pub struct Allocator {
    net: Ipv4Network,
    gateway: Ipv4Addr,
}

impl Allocator {
    pub fn new(net: Ipv4Network, gateway: Option<Ipv4Addr>) -> Self {
        let gateway = gateway.unwrap_or_else(|| first_usable(net));
        Self { net, gateway }
    }

    pub fn gateway(&self) -> Ipv4Addr {
        self.gateway
    }

    pub fn prefix(&self) -> u8 {
        self.net.prefix()
    }

    fn usable_hosts(&self) -> Vec<Ipv4Addr> {
        let network = self.net.network();
        let broadcast = self.net.broadcast();
        self.net
            .iter()
            .filter(|ip| *ip != network && *ip != broadcast && *ip != self.gateway)
            .collect()
    }

    pub fn next_ip(&self, leased: &HashSet<Ipv4Addr>, last: Option<Ipv4Addr>) -> Option<Ipv4Addr> {
        let hosts = self.usable_hosts();
        if hosts.is_empty() {
            return None;
        }
        let start = match last {
            Some(l) => hosts.iter().position(|h| *h == l).map(|i| i + 1).unwrap_or(0),
            None => 0,
        };
        for k in 0..hosts.len() {
            let ip = hosts[(start + k) % hosts.len()];
            if !leased.contains(&ip) {
                return Some(ip);
            }
        }
        None
    }
}

fn first_usable(net: Ipv4Network) -> Ipv4Addr {
    Ipv4Addr::from(u32::from(net.network()) + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(s: &str) -> Ipv4Network { s.parse().unwrap() }
    fn ip(s: &str) -> Ipv4Addr { s.parse().unwrap() }

    #[test]
    fn default_gateway_is_first_usable() {
        let a = Allocator::new(net("10.244.1.0/24"), None);
        assert_eq!(a.gateway(), ip("10.244.1.1"));
    }

    #[test]
    fn first_allocation_skips_network_and_gateway() {
        let a = Allocator::new(net("10.244.1.0/24"), None);
        assert_eq!(a.next_ip(&HashSet::new(), None), Some(ip("10.244.1.2")));
    }

    #[test]
    fn sequential_after_last_reserved() {
        let a = Allocator::new(net("10.244.1.0/24"), None);
        let leased: HashSet<_> = [ip("10.244.1.2")].into_iter().collect();
        assert_eq!(a.next_ip(&leased, Some(ip("10.244.1.2"))), Some(ip("10.244.1.3")));
    }

    #[test]
    fn wraps_around_to_find_free() {
        let a = Allocator::new(net("10.244.1.0/24"), None);
        let mut leased: HashSet<Ipv4Addr> = HashSet::new();
        for o in 3..=254 { leased.insert(ip(&format!("10.244.1.{o}"))); }
        assert_eq!(a.next_ip(&leased, Some(ip("10.244.1.254"))), Some(ip("10.244.1.2")));
    }

    #[test]
    fn exhausted_range_returns_none() {
        let a = Allocator::new(net("10.244.1.0/24"), None);
        let mut leased: HashSet<Ipv4Addr> = HashSet::new();
        for o in 2..=254 { leased.insert(ip(&format!("10.244.1.{o}"))); }
        assert_eq!(a.next_ip(&leased, None), None);
    }

    #[test]
    fn explicit_gateway_is_excluded() {
        let a = Allocator::new(net("10.244.1.0/24"), Some(ip("10.244.1.5")));
        let chosen = a.next_ip(&HashSet::new(), None).unwrap();
        assert_ne!(chosen, ip("10.244.1.5"));
        // With an explicit gateway of .5, only network/broadcast/.5 are excluded,
        // so the first usable host is .1 (not .2 — that skip only happens with the
        // default gateway, which is itself .1).
        assert_eq!(chosen, ip("10.244.1.1"));
    }
}
