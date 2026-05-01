use serde::{Deserialize, Serialize};

use crate::NodeId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SkinId(pub u32);

/// A skeleton definition referenced by mesh nodes with per-vertex JOINTS_0 +
/// WEIGHTS_0 attributes. Joint order matches the index space used by those
/// attributes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skin {
    pub name: String,
    pub joints: Vec<NodeId>,
    /// Column-major 4x4 per joint (glTF convention): `inverse(world_bind(joint))`.
    pub inverse_bind_matrices: Vec<[[f32; 4]; 4]>,
    /// Per-joint envelope radius used by the automatic weight binder. Same
    /// length as `joints`. Not serialized to glTF — only the binder consumes it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub envelopes: Vec<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skeleton_root: Option<NodeId>,
    /// Canonical path of the imported `.mog` file this skin was lowered
    /// from. `None` when the skin was authored in the file currently being
    /// lowered. Used by tooling (e.g. MoGen Studio's inspector) to scope
    /// what's shown to the user — runtime export ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<std::path::PathBuf>,
}
