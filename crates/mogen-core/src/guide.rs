use crate::{NodeId, Span};
use serde::{Deserialize, Serialize};

/// An authored semantic surface curve, sampled independently of target mesh
/// triangles. Coordinates and normals live in target-local space.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuideCurve {
    pub name: String,
    pub target: NodeId,
    pub section: String,
    pub use_id: Option<u32>,
    pub closed: bool,
    pub tolerance: f32,
    pub points: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub source_span: Span,
}
