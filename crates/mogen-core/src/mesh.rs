use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Mesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
    /// Per-vertex UVs (`TEXCOORD_0` in glTF). Must be empty or the same length
    /// as `positions`. Empty means "no UV channel" — the exporter omits
    /// `TEXCOORD_0` entirely, and any material with texture slots will render
    /// as a solid colour.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uvs: Vec<[f32; 2]>,
    /// Per-vertex joint indices into the owning node's `Skin::joints`. Up to 4
    /// influences. Must be empty or the same length as `positions`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub joints: Vec<[u16; 4]>,
    /// Per-vertex weights matching `joints`. Each row must sum to ~1.0.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub weights: Vec<[f32; 4]>,
}

impl Mesh {
    pub fn new(positions: Vec<[f32; 3]>, normals: Vec<[f32; 3]>, indices: Vec<u32>) -> Self {
        Self {
            positions,
            normals,
            indices,
            uvs: Vec::new(),
            joints: Vec::new(),
            weights: Vec::new(),
        }
    }

    pub fn is_skinned(&self) -> bool {
        !self.joints.is_empty() && self.joints.len() == self.weights.len()
    }

    pub fn has_uvs(&self) -> bool {
        !self.uvs.is_empty() && self.uvs.len() == self.positions.len()
    }
}
