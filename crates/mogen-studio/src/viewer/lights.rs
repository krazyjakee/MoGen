//! Resolve `SceneGraph` lights into GPU-ready analytic punctual lights for the
//! viewer. Each light's world-space position and direction are derived from
//! the carrier node's animated world transform; intensity / color / range /
//! cone fields pass through unchanged. The renderer caps at [`MAX_LIGHTS`]
//! and clamps oversize scenes silently — the punctual lighting model isn't
//! meant for crowds, and the studio can flag the truncation in a follow-up.

use glam::{Mat4, Vec3};
use mogen_core::{LightKind, NodeId, SceneGraph};

/// Upper bound on punctual lights forwarded to the shader. Sized to fit a
/// three-point lighting rig plus a generous set of accent lights without
/// silent truncation. Stays well inside the GL 3.3 fragment-uniform budget
/// (~15 components per light). Keep in sync with `MAX_LIGHTS` in
/// `shaders/mesh.rs`.
pub const MAX_LIGHTS: usize = 16;

/// One light entry handed to the renderer. Packed into a layout the shader
/// can iterate without per-light branches: position is unused for directional,
/// direction is unused for point, cone is unused for non-spot. The kind value
/// (0=dir, 1=point, 2=spot) drives the shader's per-light path.
#[derive(Clone, Copy, Debug)]
pub struct ResolvedLight {
    pub node: NodeId,
    pub kind: LightKind,
    /// World-space position of the carrier node. Unused for directional.
    pub position: Vec3,
    /// World-space normalised direction the light points along (the carrier
    /// node's local `-Z` rotated by its world rotation). Unused for point.
    pub direction: Vec3,
    /// Linear-space RGB pre-multiplied by `intensity`. The shader uses this
    /// directly as scene-referred radiance feeding into the BRDF, then AgX
    /// tonemaps the sum so values much greater than 1 still produce a
    /// readable image.
    pub color: [f32; 3],
    /// Distance cutoff for point/spot. `0.0` = unlimited (directional, or a
    /// point/spot with no range cap).
    pub range: f32,
    /// Cosine of the spot inner cone half-angle. `1.0` for non-spot.
    pub inner_cos: f32,
    /// Cosine of the spot outer cone half-angle. `1.0` for non-spot.
    pub outer_cos: f32,
}

/// Walk the scene and resolve every `light` node into a `ResolvedLight` using
/// the supplied animated world transforms. Order matches scene traversal so
/// the shader sees a stable ordering across frames.
pub fn collect_lights(scene: &SceneGraph, worlds: &[Mat4]) -> Vec<ResolvedLight> {
    let mut out = Vec::new();
    for (i, node) in scene.nodes.iter().enumerate() {
        let Some(light) = &node.light else {
            continue;
        };
        if out.len() >= MAX_LIGHTS {
            break;
        }
        let world = worlds.get(i).copied().unwrap_or(Mat4::IDENTITY);
        let position = world.w_axis.truncate();
        // glTF lights point along the carrier's local -Z, transformed by the
        // node's world rotation. Strip translation/scale via `transform_vector3`
        // so the result depends only on orientation.
        let direction = world
            .transform_vector3(Vec3::NEG_Z)
            .normalize_or_zero();
        let color = [
            light.color[0] * light.intensity,
            light.color[1] * light.intensity,
            light.color[2] * light.intensity,
        ];
        let range = light.range.unwrap_or(0.0);
        let (inner_cos, outer_cos) = match light.kind {
            LightKind::Spot => (
                light.inner_cone_rad.cos(),
                light.outer_cone_rad.cos(),
            ),
            _ => (1.0, 1.0),
        };
        out.push(ResolvedLight {
            node: NodeId(i as u32),
            kind: light.kind,
            position,
            direction,
            color,
            range,
            inner_cos,
            outer_cos,
        });
    }
    out
}

/// Map [`LightKind`] to the `int` the shader switches on.
pub fn kind_to_int(kind: LightKind) -> i32 {
    match kind {
        LightKind::Directional => 0,
        LightKind::Point => 1,
        LightKind::Spot => 2,
    }
}
