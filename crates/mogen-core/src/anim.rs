use glam::Vec3;
use serde::{Deserialize, Serialize};

use crate::NodeId;

/// Kinematic degree-of-freedom advertised by a node. In v1 every joint is
/// lowered to a node-transform animation track; the type only picks which
/// channel (rotation/translation) a clip's keyframes drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JointKind {
    /// 1 DOF rotation around `axis`; `from`/`to` are degrees.
    Hinge,
    /// 1 DOF translation along `axis`; `from`/`to` are meters.
    Slider,
    /// 3 DOF rotation; `from`/`to` are vec3 Euler degrees.
    Ball,
    /// Continuous 1 DOF rotation around `axis`; typically driven by `spin`.
    Rotor,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Joint {
    pub name: String,
    pub kind: JointKind,
    pub axis: Vec3,
    /// Two-element range; meaning depends on `kind` (degrees or meters).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limits: Option<[f32; 2]>,
    /// Scene node whose transform this joint drives.
    pub pivot: NodeId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackProperty {
    Translation,
    Rotation,
    Scale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Interpolation {
    Linear,
    Step,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub node: NodeId,
    pub property: TrackProperty,
    pub interpolation: Interpolation,
    pub times: Vec<f32>,
    /// For Translation/Scale, each entry is `[x, y, z, 0]`.
    /// For Rotation, each entry is the quaternion `[x, y, z, w]`.
    pub values: Vec<[f32; 4]>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Clip {
    pub name: String,
    pub duration: f32,
    pub tracks: Vec<Track>,
    /// Canonical path of the imported `.mog` file this clip was lowered
    /// from. `None` when the clip was authored in the file currently being
    /// lowered. Used by tooling (e.g. MoGen Studio's inspector) to scope
    /// what's shown to the user — runtime export ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<std::path::PathBuf>,
}
