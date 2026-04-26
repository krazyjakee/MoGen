use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};

use crate::Span;

/// An oriented frame exposed by a node for attaching other parts.
/// `rotation` turns the canonical +Y axis into the connector's `dir`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Connector {
    pub name: String,
    pub pos: Vec3,
    pub rotation: Quat,
    pub tag: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub radius: Option<f32>,
    /// Byte range of the AST `connector` declaration that produced this
    /// frame. `None` for synthesized defaults (primitive faces, AABB
    /// fallbacks). The viewport's gizmo redirect path uses this to rewrite
    /// the connector's `at=` when an attach-bound child is translated —
    /// without a span there's no DSL slice to mutate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_span: Option<Span>,
}

impl Connector {
    /// Build a connector from an anchor point and a direction vector.
    /// The direction is rotated from +Y to the given `dir`; callers that
    /// need a specific roll around `dir` can modify `rotation` afterwards.
    pub fn from_at_dir(
        name: impl Into<String>,
        at: Vec3,
        dir: Vec3,
        tag: impl Into<String>,
        radius: Option<f32>,
    ) -> Self {
        let rotation = rotation_from_up(dir);
        Self {
            name: name.into(),
            pos: at,
            rotation,
            tag: tag.into(),
            radius,
            source_span: None,
        }
    }
}

fn rotation_from_up(dir: Vec3) -> Quat {
    let len = dir.length();
    if len < 1e-6 {
        return Quat::IDENTITY;
    }
    Quat::from_rotation_arc(Vec3::Y, dir / len)
}
