use crate::{NodeId, Span};
use serde::{Deserialize, Serialize};

/// Resolved semantic anchors. Coordinates remain local to their owning nodes
/// so inspection can remeasure after a transform, rather than reusing a gap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relationship {
    pub mode: String,
    pub child: NodeId,
    pub target: NodeId,
    pub socket: String,
    pub endpoint: Option<String>,
    pub child_anchor: [f32; 3],
    pub target_anchor: [f32; 3],
    pub target_normal: [f32; 3],
    pub insertion: f32,
    pub clearance: f32,
    pub tolerance: f32,
    pub source_span: Span,
}
