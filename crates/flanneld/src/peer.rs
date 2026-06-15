use std::collections::HashMap;

/// One remote node's VXLAN endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    pub node: String,
    pub pod_cidr: String,   // 10.244.2.0/24
    pub public_ip: String,  // underlay node IP
    pub vtep_mac: String,   // flannel.1 MAC on the peer
}

#[derive(Debug, PartialEq)]
pub enum Action {
    Add(Peer),
    Remove(Peer),
}

/// Diff installed vs desired, keyed by node name.
/// A peer whose fields changed yields Remove(old) then Add(new).
pub fn reconcile(installed: &HashMap<String, Peer>, desired: &HashMap<String, Peer>) -> Vec<Action> {
    let mut actions = Vec::new();
    for (node, old) in installed {
        match desired.get(node) {
            None => actions.push(Action::Remove(old.clone())),
            Some(new) if new != old => {
                actions.push(Action::Remove(old.clone()));
                actions.push(Action::Add(new.clone()));
            }
            Some(_) => {}
        }
    }
    for (node, new) in desired {
        if !installed.contains_key(node) {
            actions.push(Action::Add(new.clone()));
        }
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(node: &str, mac: &str) -> Peer {
        Peer { node: node.into(), pod_cidr: "10.244.2.0/24".into(),
               public_ip: "172.18.0.3".into(), vtep_mac: mac.into() }
    }

    #[test]
    fn adds_new_peer() {
        let installed = HashMap::new();
        let mut desired = HashMap::new();
        desired.insert("n2".into(), peer("n2", "aa:bb"));
        assert_eq!(reconcile(&installed, &desired), vec![Action::Add(peer("n2", "aa:bb"))]);
    }

    #[test]
    fn removes_gone_peer() {
        let mut installed = HashMap::new();
        installed.insert("n2".into(), peer("n2", "aa:bb"));
        let desired = HashMap::new();
        assert_eq!(reconcile(&installed, &desired), vec![Action::Remove(peer("n2", "aa:bb"))]);
    }

    #[test]
    fn replaces_changed_peer() {
        let mut installed = HashMap::new();
        installed.insert("n2".into(), peer("n2", "aa:bb"));
        let mut desired = HashMap::new();
        desired.insert("n2".into(), peer("n2", "cc:dd"));
        assert_eq!(
            reconcile(&installed, &desired),
            vec![Action::Remove(peer("n2", "aa:bb")), Action::Add(peer("n2", "cc:dd"))]
        );
    }

    #[test]
    fn unchanged_peer_no_action() {
        let mut installed = HashMap::new();
        installed.insert("n2".into(), peer("n2", "aa:bb"));
        let desired = installed.clone();
        assert!(reconcile(&installed, &desired).is_empty());
    }
}
