use std::collections::HashMap;

use anyhow::{Context, Result};
use k8s_openapi::api::core::v1::Node;
use kube::api::{Patch, PatchParams};
use kube::{Api, Client};
use serde_json::{json, Map, Value};

use crate::annotation::{self, BackendData};
use crate::peer::Peer;

pub struct KubeMgr {
    client: Client,
    node_name: String,
}

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

    /// Server-side-apply patch own Node annotations: backend-type=vxlan,
    /// backend-data={"VtepMAC":mac}, public-ip, kube-subnet-manager-managed=true.
    pub async fn publish(&self, public_ip: &str, vtep_mac: &str) -> Result<()> {
        let nodes: Api<Node> = Api::all(self.client.clone());
        let patch = build_publish_patch(public_ip, vtep_mac);
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

    /// Build desired peer map (node name -> Peer) for all nodes except self that
    /// have complete annotations (backend-data + public-ip) and a podCIDR. Nodes
    /// with missing data are skipped.
    pub async fn desired_peers(&self) -> Result<HashMap<String, Peer>> {
        let nodes: Api<Node> = Api::all(self.client.clone());
        let list = nodes
            .list(&Default::default())
            .await
            .context("list nodes")?;
        let mut out = HashMap::new();
        for n in list {
            let name = n.metadata.name.clone().unwrap_or_default();
            if name == self.node_name {
                continue;
            }
            let Some(peer) = node_to_peer(&n) else {
                continue;
            };
            out.insert(name, peer);
        }
        Ok(out)
    }
}

/// Build the server-side-apply patch that publishes this node's flannel lease:
/// the four `flannel.alpha.coreos.com/*` annotations under a minimal Node object.
fn build_publish_patch(public_ip: &str, vtep_mac: &str) -> Value {
    let backend_data = BackendData {
        vtep_mac: vtep_mac.into(),
    }
    .to_json();

    let mut annotations = Map::new();
    annotations.insert(annotation::key("backend-type"), Value::from("vxlan"));
    annotations.insert(annotation::key("backend-data"), Value::from(backend_data));
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

fn node_to_peer(n: &Node) -> Option<Peer> {
    let ann = n.metadata.annotations.as_ref()?;
    let bd = ann.get(&annotation::key("backend-data"))?;
    let vtep_mac = BackendData::from_json(bd).ok()?.vtep_mac;
    let public_ip = ann.get(&annotation::key("public-ip"))?.clone();
    let pod_cidr = n.spec.as_ref()?.pod_cidr.clone()?;
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

    #[test]
    fn publish_patch_sets_four_annotations_and_ssa_shape() {
        // parity: flannel pkg/subnet/kube — publishes backend-type/backend-data/
        // public-ip + kube-subnet-manager-managed via server-side apply.
        let p = build_publish_patch("172.18.0.2", "ae:11:22:33:44:55");
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
}
