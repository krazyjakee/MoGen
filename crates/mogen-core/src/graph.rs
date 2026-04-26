use std::path::Path;

use serde::{Deserialize, Serialize};

use glam::Mat4;

use crate::{Clip, Connector, Joint, Material, MaterialId, Mesh, Skin, SkinId, Span, Transform};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub u32);

/// Records that a node's local transform was set by an `attach` pass and
/// is therefore overwritten on every recompile. Stored on the child so the
/// viewport editor can redirect a gizmo translate into the bound socket's
/// `at=` instead of writing a `pos=` that would just be clobbered.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachBinding {
    pub parent: NodeId,
    pub socket: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Byte range of the AST node that produced this scene node, when the
    /// scene node corresponds 1:1 to a single DSL declaration. `None` for
    /// nodes synthesised by replicators (array/mirror/stack/grid) or CSG
    /// ops — those have no canonical source to rewrite.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_span: Option<Span>,
    /// Whether this node's transform/attributes can be rewritten back into
    /// the `.mog` source via span-based text mutation. `false` for
    /// replicator/CSG-synthesised children; the viewport editor gates its
    /// gizmo + inspector writeback on this flag.
    #[serde(default = "default_editable", skip_serializing_if = "is_default_editable")]
    pub editable: bool,
    /// Set when the node's translation was derived by `apply_relative_placement`
    /// (one of `above`/`below`/`left_of`/`right_of`/`in_front_of`/`behind`).
    /// The viewport gizmo refuses to edit these nodes: a `pos=` writeback
    /// would double-shift on recompile because the layout pass re-adds its
    /// offset. Authors detach by removing the relative-placement attr.
    #[serde(default, skip_serializing_if = "is_false")]
    pub relative_placed: bool,
    /// Set when this node's transform was overridden by an `attach` pass.
    /// Names the parent + socket connector that controls its placement, so
    /// the gizmo can redirect a translate into the connector's `at=` rather
    /// than writing a `pos=` that the next compile would overwrite.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attach_binding: Option<AttachBinding>,
}

fn default_editable() -> bool {
    true
}

fn is_default_editable(b: &bool) -> bool {
    *b
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Default for SceneNode {
    fn default() -> Self {
        Self {
            name: String::new(),
            transform: Transform::default(),
            mesh: None,
            material: None,
            skin: None,
            parent: None,
            children: Vec::new(),
            connectors: Vec::new(),
            tags: Vec::new(),
            role: None,
            kind: String::new(),
            source_span: None,
            editable: true,
            relative_placed: false,
            attach_binding: None,
        }
    }
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

    /// Attach an AST source span to a node so the viewport editor can rewrite
    /// the `.mog` slice that produced it.
    pub fn set_source_span(&mut self, id: NodeId, span: Span) {
        self.nodes[id.0 as usize].source_span = Some(span);
    }

    /// Mark this node as non-editable (produced by array/mirror/CSG
    /// expansion, no single AST node to rewrite).
    pub fn set_not_editable(&mut self, id: NodeId) {
        self.nodes[id.0 as usize].editable = false;
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

    /// Rewrite every relative texture path on every material so it's anchored
    /// at `base` (typically the directory containing the source `.mog` file).
    /// Absolute paths are left untouched. Callers invoke this after lowering
    /// but before export so the exporter only ever sees paths it can resolve
    /// from process cwd.
    pub fn resolve_texture_paths(&mut self, base: &Path) {
        for mat in &mut self.materials {
            for slot in mat.texture_slots_mut() {
                if let Some(t) = slot {
                    if !t.path.is_absolute() {
                        t.path = base.join(&t.path);
                    }
                }
            }
        }
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

    /// All nodes whose `name` equals `name`. Used by animation templates so a
    /// single `target="rotor"` expands to every replicated rotor produced by
    /// an `array`/`mirror`.
    pub fn find_nodes_by_name(&self, name: &str) -> Vec<NodeId> {
        self.nodes
            .iter()
            .enumerate()
            .filter_map(|(i, n)| (n.name == name).then_some(NodeId(i as u32)))
            .collect()
    }

    /// All nodes whose `role` equals `role`.
    pub fn find_nodes_by_role(&self, role: &str) -> Vec<NodeId> {
        self.nodes
            .iter()
            .enumerate()
            .filter_map(|(i, n)| (n.role.as_deref() == Some(role)).then_some(NodeId(i as u32)))
            .collect()
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
