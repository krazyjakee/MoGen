//! Versioned asset-front convention shared by capture hosts.
use crate::SceneGraph;
use glam::Vec3;
use serde::{Deserialize, Serialize};

pub const CAMERA_CONVENTION_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureInfo {
    pub convention_version: u32,
    pub revision: String,
    pub view: String,
    pub yaw: f32,
    pub pitch: f32,
    pub target: [f32; 3],
    pub distance: f32,
    pub out_of_frame_vertices: usize,
    pub cropped_parts: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetView {
    Front,
    Side,
    Back,
    #[serde(alias = "front_three_quarter")]
    ThreeQuarter,
    Presentation,
    RearThreeQuarter,
}
impl AssetView {
    pub const ALL: [Self; 5] = [
        Self::Front,
        Self::Side,
        Self::Back,
        Self::ThreeQuarter,
        Self::Presentation,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Front => "front",
            Self::Side => "side",
            Self::Back => "back",
            Self::ThreeQuarter => "three_quarter",
            Self::Presentation => "presentation",
            Self::RearThreeQuarter => "rear_three_quarter",
        }
    }
    /// Default asset faces -Z. Yaw locates the camera, not its look direction.
    pub fn camera(self) -> (f32, f32) {
        self.camera_from_front(std::f32::consts::PI)
    }
    pub fn camera_from_front(self, front_yaw: f32) -> (f32, f32) {
        use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, PI};
        match self {
            Self::Front => (front_yaw, 0.0),
            Self::Side => (front_yaw - FRAC_PI_2, 0.0),
            Self::Back => (front_yaw - PI, 0.0),
            Self::ThreeQuarter | Self::Presentation => (front_yaw - FRAC_PI_4, 0.5),
            Self::RearThreeQuarter => (front_yaw - 3.0 * FRAC_PI_4, 0.5),
        }
    }
    pub fn camera_for(self, scene: &SceneGraph) -> Result<(f32, f32), String> {
        Ok(self.camera_from_front(asset_front_yaw(scene)?))
    }
}

/// Meta front is world-space unless front_node names a unique node, in which
/// case its complete parent transform maps that node-local axis into world.
pub fn asset_front_yaw(scene: &SceneGraph) -> Result<f32, String> {
    let meta = scene.meta.as_ref();
    let direction = meta.and_then(|m| m.front.as_deref()).unwrap_or("-z");
    let mut front = match direction {
        "-z" => -Vec3::Z,
        "+z" | "z" => Vec3::Z,
        "-x" => -Vec3::X,
        "+x" | "x" => Vec3::X,
        _ => return Err("E0140: meta.front expects -z, +z, -x or +x".into()),
    };
    if let Some(name) = meta.and_then(|m| m.front_node.as_deref()) {
        let nodes: Vec<_> = scene
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.name == name)
            .collect();
        if nodes.len() != 1 {
            return Err(format!(
                "E0140: front_node {name:?} must identify one unique node"
            ));
        }
        front = scene.world_transforms()[nodes[0].0].transform_vector3(front);
    }
    front.y = 0.0;
    if !front.is_finite() || front.length_squared() < 1e-12 {
        return Err(
            "E0140: declared front is vertical/collapsed; choose a horizontal asset front".into(),
        );
    }
    Ok(front.x.atan2(front.z).rem_euclid(std::f32::consts::TAU))
}
