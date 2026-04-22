use serde::{Deserialize, Serialize};

use glam::Mat4;

use crate::{Clip, Connector, Joint, Material, MaterialId, Mesh, Skin, SkinId, Transform};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub u32);

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SceneNode {
    pub name: String,
    pub transform: Transform,
    pub mesh: Option<Mesh>,
    pub material: Option<MaterialId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skin: Option<SkinId>,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub connectors: Vec<Connector>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Original DSL node kind (box, cylinder, group, …). Useful in extras.
    pub kind: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SceneGraph {
    pub nodes: Vec<SceneNode>,
    pub roots: Vec<NodeId>,
    pub materials: Vec<Material>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub joints: Vec<Joint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clips: Vec<Clip>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skins: Vec<Skin>,
}

impl SceneGraph {
    pub fn new() -> Self {
        Self::default()
    }

    fn push_node(&mut self, node: SceneNode) -> NodeId {
        let id = NodeId(self.nodes.len() as u32);
        self.nodes.push(node);
        id
    }

    pub fn add_root(
        &mut self,
        name: impl Into<String>,
        kind: impl Into<String>,
        transform: Transform,
    ) -> NodeId {
        let id = self.push_node(SceneNode {
            name: name.into(),
            kind: kind.into(),
            transform,
            parent: None,
            ..Default::default()
        });
        self.roots.push(id);
        id
    }

    pub fn add_child(
        &mut self,
        parent: NodeId,
        name: impl Into<String>,
        kind: impl Into<String>,
        transform: Transform,
    ) -> NodeId {
        let id = self.push_node(SceneNode {
            name: name.into(),
            kind: kind.into(),
            transform,
            parent: Some(parent),
            ..Default::default()
        });
        self.nodes[parent.0 as usize].children.push(id);
        id
    }

    pub fn set_mesh(&mut self, id: NodeId, mesh: Mesh) {
        self.nodes[id.0 as usize].mesh = Some(mesh);
    }

    pub fn set_material(&mut self, id: NodeId, mat: MaterialId) {
        self.nodes[id.0 as usize].material = Some(mat);
    }

    pub fn add_material(&mut self, mat: Material) -> MaterialId {
        let id = MaterialId(self.materials.len() as u32);
        self.materials.push(mat);
        id
    }

    pub fn find_material(&self, name: &str) -> Option<MaterialId> {
        self.materials
            .iter()
            .position(|m| m.name == name)
            .map(|i| MaterialId(i as u32))
    }

    pub fn get(&self, id: NodeId) -> &SceneNode {
        &self.nodes[id.0 as usize]
    }

    pub fn find_node(&self, name: &str) -> Option<NodeId> {
        self.nodes
            .iter()
            .position(|n| n.name == name)
            .map(|i| NodeId(i as u32))
    }

    /// Find a node by name within the descendants of `root` (inclusive).
    ///
    /// Used by scoped `attach` resolution so that replicated subtrees (arrays,
    /// mirrors) can share names without their attach specs colliding.
    pub fn find_node_in_subtree(&self, root: NodeId, name: &str) -> Option<NodeId> {
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            let n = &self.nodes[id.0 as usize];
            if n.name == name {
                return Some(id);
            }
            stack.extend(n.children.iter().copied());
        }
        None
    }

    pub fn find_joint(&self, name: &str) -> Option<&Joint> {
        self.joints.iter().find(|j| j.name == name)
    }

    pub fn set_skin(&mut self, id: NodeId, skin: SkinId) {
        self.nodes[id.0 as usize].skin = Some(skin);
    }

    pub fn add_skin(&mut self, skin: Skin) -> SkinId {
        let id = SkinId(self.skins.len() as u32);
        self.skins.push(skin);
        id
    }

    pub fn find_skin(&self, name: &str) -> Option<SkinId> {
        self.skins
            .iter()
            .position(|s| s.name == name)
            .map(|i| SkinId(i as u32))
    }

    /// Compute world-space transforms for every node by walking from the
    /// roots. Returns a vector indexed by `NodeId.0`.
    pub fn world_transforms(&self) -> Vec<Mat4> {
        let mut out = vec![Mat4::IDENTITY; self.nodes.len()];
        for root in &self.roots {
            self.walk_world(*root, Mat4::IDENTITY, &mut out);
        }
        out
    }

    fn walk_world(&self, id: NodeId, parent_world: Mat4, out: &mut [Mat4]) {
        let node = &self.nodes[id.0 as usize];
        let world = parent_world * node.transform.to_mat4();
        out[id.0 as usize] = world;
        for c in &node.children {
            self.walk_world(*c, world, out);
        }
    }

    pub fn is_ancestor(&self, ancestor: NodeId, descendant: NodeId) -> bool {
        let mut cur = Some(descendant);
        while let Some(id) = cur {
            if id == ancestor {
                return true;
            }
            cur = self.nodes[id.0 as usize].parent;
        }
        false
    }
}
