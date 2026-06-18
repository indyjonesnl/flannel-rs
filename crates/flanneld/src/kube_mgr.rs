use std::collections::HashMap;

use anyhow::{Context, Result};
use k8s_openapi::api::core::v1::Node;
use kube::api::{Patch, PatchParams};
use kube::{Api, Client};
use serde_json::{json, Map, Value};

use crate::annotation::{self, BackendData};
use crate::config::BackendType;
use crate::peer::Peer;

pub struct KubeMgr {
    client: Client,
    node_name: String,
}

#[derive(Debug)]
pub struct OwnNode {
    pub pod_cidr: String,
    pub public_ip: String,
}

impl KubeMgr {
    pub async fn new(node_name: String) -> Result<Self> {
        let client = Client::try_default().await.context("kube client")?;
        Ok(Self { client, node_name })
    }

    /// Get own Node: Spec.podCIDR + status InternalIP.
    pub async fn own_node(&self) -> Result<OwnNode> {
        let nodes: Api<Node> = Api::all(self.client.clone());
        let n = nodes.get(&self.node_name).await.context("get own node")?;
        extract_own_node(&n)
    }

    /// Server-side-apply patch own Node annotations: backend-type, public-ip,
    /// kube-subnet-manager-managed, and (vxlan only) backend-data={"VtepMAC":mac}.
    pub async fn publish(
        &self,
        backend: BackendType,
        public_ip: &str,
        vtep_mac: Option<&str>,
    ) -> Result<()> {
        let nodes: Api<Node> = Api::all(self.client.clone());
        let patch = build_publish_patch(backend, public_ip, vtep_mac);
        nodes
            .patch(
                &self.node_name,
                &PatchParams::apply("flanneld-rs").force(),
                &Patch::Apply(&patch),
            )
            .await
            .context("patch own annotations")?;
        Ok(())
    }

    /// Clone of the kube client, for callers that build their own watches.
    pub fn client(&self) -> Client {
        self.client.clone()
    }
}

/// Extract this node's lease inputs from its Node object: the PodCIDR (from
/// spec) and the InternalIP (from status addresses). Both are required.
fn extract_own_node(n: &Node) -> Result<OwnNode> {
    let pod_cidr = n
        .spec
        .as_ref()
        .and_then(|s| s.pod_cidr.clone())
        .context("node has no PodCIDR")?;
    let public_ip = n
        .status
        .as_ref()
        .and_then(|s| s.addresses.as_ref())
        .and_then(|a| a.iter().find(|x| x.type_ == "InternalIP"))
        .map(|x| x.address.clone())
        .context("node has no InternalIP")?;
    Ok(OwnNode {
        pod_cidr,
        public_ip,
    })
}

/// Build the server-side-apply patch that publishes this node's flannel lease:
/// the `flannel.alpha.coreos.com/*` annotations under a minimal Node object.
/// vxlan also publishes `backend-data` (the VtepMAC); host-gw does not.
fn build_publish_patch(backend: BackendType, public_ip: &str, vtep_mac: Option<&str>) -> Value {
    let backend_type = match backend {
        BackendType::Vxlan => "vxlan",
        BackendType::HostGw => "host-gw",
    };

    let mut annotations = Map::new();
    annotations.insert(annotation::key("backend-type"), Value::from(backend_type));
    if let Some(mac) = vtep_mac {
        let backend_data = BackendData {
            vtep_mac: mac.into(),
        }
        .to_json();
        annotations.insert(annotation::key("backend-data"), Value::from(backend_data));
    }
    annotations.insert(annotation::key("public-ip"), Value::from(public_ip));
    annotations.insert(
        annotation::key("kube-subnet-manager-managed"),
        Value::from("true"),
    );

    json!({
        "apiVersion": "v1",
        "kind": "Node",
        "metadata": { "annotations": Value::Object(annotations) }
    })
}

/// Build the desired peer map (node name -> Peer) from a set of Node objects,
/// excluding `self_name` and any node lacking complete lease data for `backend`.
pub fn desired_from_nodes(
    nodes: &[Node],
    self_name: &str,
    backend: BackendType,
) -> HashMap<String, Peer> {
    let mut out = HashMap::new();
    for n in nodes {
        let name = n.metadata.name.clone().unwrap_or_default();
        if name == self_name {
            continue;
        }
        if let Some(peer) = node_to_peer(n, backend) {
            out.insert(name, peer);
        }
    }
    out
}

/// Decode a Node into a peer lease. All backends require public-ip + podCIDR +
/// name. vxlan additionally requires a valid `backend-data` VtepMAC (the node is
/// skipped without it); host-gw needs no VtepMAC, so `vtep_mac` is `None`.
fn node_to_peer(n: &Node, backend: BackendType) -> Option<Peer> {
    let ann = n.metadata.annotations.as_ref()?;
    let public_ip = ann.get(&annotation::key("public-ip"))?.clone();
    let pod_cidr = n.spec.as_ref()?.pod_cidr.clone()?;
    let vtep_mac = match backend {
        BackendType::Vxlan => {
            let bd = ann.get(&annotation::key("backend-data"))?;
            Some(BackendData::from_json(bd).ok()?.vtep_mac)
        }
        BackendType::HostGw => None,
    };
    Some(Peer {
        node: n.metadata.name.clone()?,
        pod_cidr,
        public_ip,
        vtep_mac,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::api::core::v1::{NodeAddress, NodeSpec, NodeStatus};
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
    use std::collections::BTreeMap;

    #[test]
    fn publish_patch_sets_four_annotations_and_ssa_shape() {
        // parity: flannel pkg/subnet/kube — publishes backend-type/backend-data/
        // public-ip + kube-subnet-manager-managed via server-side apply.
        let p = build_publish_patch(BackendType::Vxlan, "172.18.0.2", Some("ae:11:22:33:44:55"));
        assert_eq!(p["apiVersion"].as_str(), Some("v1"));
        assert_eq!(p["kind"].as_str(), Some("Node"));
        let ann = &p["metadata"]["annotations"];
        assert_eq!(
            ann.get(annotation::key("backend-type").as_str())
                .and_then(|v| v.as_str()),
            Some("vxlan")
        );
        assert_eq!(
            ann.get(annotation::key("backend-data").as_str())
                .and_then(|v| v.as_str()),
            Some(r#"{"VtepMAC":"ae:11:22:33:44:55"}"#)
        );
        assert_eq!(
            ann.get(annotation::key("public-ip").as_str())
                .and_then(|v| v.as_str()),
            Some("172.18.0.2")
        );
        assert_eq!(
            ann.get(annotation::key("kube-subnet-manager-managed").as_str())
                .and_then(|v| v.as_str()),
            Some("true")
        );
        assert_eq!(ann.as_object().map(|m| m.len()), Some(4));
    }

    // host-gw publishes backend-type=host-gw + public-ip, and NO backend-data.
    #[test]
    fn publish_patch_host_gw_omits_backend_data() {
        let p = build_publish_patch(BackendType::HostGw, "172.18.0.2", None);
        let ann = &p["metadata"]["annotations"];
        assert_eq!(
            ann.get(annotation::key("backend-type").as_str())
                .and_then(|v| v.as_str()),
            Some("host-gw")
        );
        assert_eq!(
            ann.get(annotation::key("public-ip").as_str())
                .and_then(|v| v.as_str()),
            Some("172.18.0.2")
        );
        assert!(ann.get(annotation::key("backend-data").as_str()).is_none());
        assert_eq!(ann.as_object().map(|m| m.len()), Some(3));
    }

    // Build a Node carrying only the fields extract_own_node reads.
    fn node_with(pod_cidr: Option<&str>, addresses: Vec<(&str, &str)>) -> Node {
        Node {
            spec: Some(NodeSpec {
                pod_cidr: pod_cidr.map(String::from),
                ..Default::default()
            }),
            status: Some(NodeStatus {
                addresses: Some(
                    addresses
                        .into_iter()
                        .map(|(t, a)| NodeAddress {
                            type_: t.into(),
                            address: a.into(),
                        })
                        .collect(),
                ),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    // parity: flannel pkg/subnet/kube — own lease = spec.podCIDR + status InternalIP.
    #[test]
    fn extract_own_node_reads_podcidr_and_internal_ip() {
        let n = node_with(Some("10.244.1.0/24"), vec![("InternalIP", "172.18.0.2")]);
        let own = extract_own_node(&n).unwrap();
        assert_eq!(own.pod_cidr, "10.244.1.0/24");
        assert_eq!(own.public_ip, "172.18.0.2");
    }

    #[test]
    fn extract_own_node_errors_without_podcidr() {
        let n = node_with(None, vec![("InternalIP", "172.18.0.2")]);
        let err = extract_own_node(&n).unwrap_err().to_string();
        assert!(err.contains("PodCIDR"), "got {err}");
    }

    #[test]
    fn extract_own_node_errors_without_internal_ip() {
        let n = node_with(Some("10.244.1.0/24"), vec![("ExternalIP", "1.2.3.4")]);
        let err = extract_own_node(&n).unwrap_err().to_string();
        assert!(err.contains("InternalIP"), "got {err}");
    }

    #[test]
    fn extract_own_node_prefers_internal_over_external_ip() {
        let n = node_with(
            Some("10.244.1.0/24"),
            vec![("ExternalIP", "1.2.3.4"), ("InternalIP", "172.18.0.2")],
        );
        let own = extract_own_node(&n).unwrap();
        assert_eq!(own.public_ip, "172.18.0.2");
    }

    // Build a Node with a name, optional podCIDR, and annotation key/value pairs.
    fn node_with_annotations(
        name: &str,
        pod_cidr: Option<&str>,
        annotations: Vec<(String, String)>,
    ) -> Node {
        let ann: BTreeMap<String, String> = annotations.into_iter().collect();
        Node {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                annotations: if ann.is_empty() { None } else { Some(ann) },
                ..Default::default()
            },
            spec: Some(NodeSpec {
                pod_cidr: pod_cidr.map(String::from),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    // Only the two annotations node_to_peer reads (backend-data + public-ip).
    fn complete_annotations() -> Vec<(String, String)> {
        vec![
            (
                annotation::key("backend-data"),
                r#"{"VtepMAC":"ae:11:22:33:44:55"}"#.to_string(),
            ),
            (annotation::key("public-ip"), "172.18.0.3".to_string()),
        ]
    }

    // parity: flannel pkg/subnet/kube — a node with complete annotations + podCIDR
    // decodes to a lease (Peer); any missing/garbled piece yields no lease.
    #[test]
    fn node_to_peer_complete_node_yields_peer() {
        let n = node_with_annotations("n2", Some("10.244.2.0/24"), complete_annotations());
        let p = node_to_peer(&n, BackendType::Vxlan).expect("peer");
        assert_eq!(p.node, "n2");
        assert_eq!(p.pod_cidr, "10.244.2.0/24");
        assert_eq!(p.public_ip, "172.18.0.3");
        assert_eq!(p.vtep_mac.as_deref(), Some("ae:11:22:33:44:55"));
    }

    // host-gw decodes a peer from public-ip + podCIDR with NO backend-data; vtep_mac is None.
    #[test]
    fn node_to_peer_host_gw_decodes_without_vtep_mac() {
        let ann = vec![(annotation::key("public-ip"), "172.18.0.3".to_string())];
        let n = node_with_annotations("n2", Some("10.244.2.0/24"), ann);
        let p = node_to_peer(&n, BackendType::HostGw).expect("peer");
        assert_eq!(p.node, "n2");
        assert_eq!(p.pod_cidr, "10.244.2.0/24");
        assert_eq!(p.public_ip, "172.18.0.3");
        assert_eq!(p.vtep_mac, None);
    }

    #[test]
    fn node_to_peer_host_gw_skips_missing_public_ip() {
        let n = node_with_annotations("n2", Some("10.244.2.0/24"), vec![]);
        assert!(node_to_peer(&n, BackendType::HostGw).is_none());
    }

    #[test]
    fn node_to_peer_missing_backend_data_is_skipped() {
        let ann = vec![(annotation::key("public-ip"), "172.18.0.3".to_string())];
        let n = node_with_annotations("n2", Some("10.244.2.0/24"), ann);
        assert!(node_to_peer(&n, BackendType::Vxlan).is_none());
    }

    #[test]
    fn node_to_peer_missing_public_ip_is_skipped() {
        let ann = vec![(
            annotation::key("backend-data"),
            r#"{"VtepMAC":"aa:bb"}"#.to_string(),
        )];
        let n = node_with_annotations("n2", Some("10.244.2.0/24"), ann);
        assert!(node_to_peer(&n, BackendType::Vxlan).is_none());
    }

    #[test]
    fn node_to_peer_missing_podcidr_is_skipped() {
        let n = node_with_annotations("n2", None, complete_annotations());
        assert!(node_to_peer(&n, BackendType::Vxlan).is_none());
    }

    #[test]
    fn node_to_peer_no_annotations_is_skipped() {
        let n = node_with_annotations("n2", Some("10.244.2.0/24"), vec![]);
        assert!(node_to_peer(&n, BackendType::Vxlan).is_none());
    }

    #[test]
    fn node_to_peer_malformed_backend_data_is_skipped() {
        let ann = vec![
            (annotation::key("backend-data"), "not json".to_string()),
            (annotation::key("public-ip"), "172.18.0.3".to_string()),
        ];
        let n = node_with_annotations("n2", Some("10.244.2.0/24"), ann);
        assert!(node_to_peer(&n, BackendType::Vxlan).is_none());
    }

    #[test]
    fn node_to_peer_missing_name_is_skipped() {
        // Complete lease data but no metadata.name yields no peer.
        let mut n = node_with_annotations("n2", Some("10.244.2.0/24"), complete_annotations());
        n.metadata.name = None;
        assert!(node_to_peer(&n, BackendType::Vxlan).is_none());
    }

    #[test]
    fn desired_from_nodes_includes_peers_excludes_self_and_incomplete() {
        let self_node =
            node_with_annotations("self", Some("10.244.0.0/24"), complete_annotations());
        let peer = node_with_annotations("n2", Some("10.244.2.0/24"), complete_annotations());
        let incomplete = node_with_annotations("n3", Some("10.244.3.0/24"), vec![]);
        let nodes = vec![self_node, peer, incomplete];
        let desired = desired_from_nodes(&nodes, "self", BackendType::Vxlan);
        assert_eq!(desired.len(), 1);
        assert!(desired.contains_key("n2"));
        assert!(!desired.contains_key("self"));
        assert!(!desired.contains_key("n3"));
        assert_eq!(desired["n2"].pod_cidr, "10.244.2.0/24");
        assert_eq!(desired["n2"].vtep_mac.as_deref(), Some("ae:11:22:33:44:55"));
        assert_eq!(desired["n2"].public_ip, "172.18.0.3");
    }

    #[test]
    fn desired_from_nodes_empty_input_is_empty() {
        let desired = desired_from_nodes(&[], "self", BackendType::Vxlan);
        assert!(desired.is_empty());
    }
}
