use crate::ast::Node;

use super::LOD_SCALE;

/// Find a top-level `lod_scale (value=N)` declaration and return its multiplier.
/// Defaults to 1.0 when absent. Values <= 0 fall back to 1.0 so a malformed
/// setting can't silently destroy every mesh.
pub(super) fn extract_lod_scale(ast: &[Node]) -> f32 {
    for n in ast {
        if n.kind == "lod_scale" {
            if let Some(v) = n.attr_number("value") {
                if v > 0.0 {
                    return v;
                }
            }
        }
    }
    1.0
}

pub(super) fn current_lod_scale() -> f32 {
    LOD_SCALE.with(|s| s.get())
}

/// Scale a default segment/ring count by the active LOD multiplier.
/// Clamped to a sensible floor so circles still close.
pub(super) fn scaled_default(default: u32, min: u32) -> u32 {
    let scale = current_lod_scale();
    let scaled = (default as f32 * scale).round();
    if scaled < min as f32 {
        min
    } else {
        scaled as u32
    }
}

/// Icosphere subdivisions are exponential (4× tris per step), so a multiplier
/// translates to an additive offset of round(log2(scale)). Floor at 0.
pub(super) fn scaled_subdivisions(default: u32) -> u32 {
    let scale = current_lod_scale();
    if scale <= 0.0 {
        return default;
    }
    let offset = scale.log2().round() as i32;
    (default as i32 + offset).max(0) as u32
}
