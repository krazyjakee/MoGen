use serde::{Deserialize, Serialize};

/// glTF `KHR_lights_punctual` light kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum LightKind {
    #[default]
    Directional,
    Point,
    Spot,
}

/// A punctual light attached to a [`SceneNode`].
///
/// Direction is implicit: glTF lights point along the node's local `-Z`,
/// transformed by the node's world rotation. Intensity is in **candela** for
/// point/spot lights and **lux** for directional lights, matching the glTF
/// `KHR_lights_punctual` spec — the DSL passes the value through unchanged.
///
/// `range` only applies to point/spot lights (`None` = no attenuation cutoff).
/// `inner_cone_rad` / `outer_cone_rad` only apply to spot lights.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Light {
    pub kind: LightKind,
    /// Linear-space RGB; default `[1, 1, 1]`.
    pub color: [f32; 3],
    /// Candela (point/spot) or lux (directional). Default `1.0` per spec.
    pub intensity: f32,
    /// Distance cutoff for point/spot. `None` = unlimited (spec default).
    pub range: Option<f32>,
    /// Inner cone half-angle in **radians**. Spot-only; ignored otherwise.
    /// Default `0.0`.
    pub inner_cone_rad: f32,
    /// Outer cone half-angle in **radians**. Spot-only; ignored otherwise.
    /// Default `PI / 4` (matches the glTF spec default).
    pub outer_cone_rad: f32,
}

impl Default for Light {
    fn default() -> Self {
        Self {
            kind: LightKind::Directional,
            color: [1.0, 1.0, 1.0],
            intensity: 1.0,
            range: None,
            inner_cone_rad: 0.0,
            outer_cone_rad: std::f32::consts::FRAC_PI_4,
        }
    }
}
