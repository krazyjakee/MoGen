use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};

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
