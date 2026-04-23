use glam::{Quat, Vec3};
use mogen_core::{Aabb, Mesh, Transform};

use crate::ast::{Node, Value};

pub(super) fn transform_from_attrs(node: &Node) -> Transform {
    let t = resolve_pos(node);
    let r = resolve_rot(node);
    let s = node.attr_scale("scale").unwrap_or(Vec3::ONE);
    Transform::from_trs(t, r, s)
}

/// Translation in the parent's frame, honoring `pos` plus the named shortcuts
/// `x`/`y`/`z` (per-component override) and `from`/`to` (corner form: the
/// midpoint of the two points). Shortcuts never silently mix with `pos` — a
/// component set via `x` wins over the same component of `pos` by design, so
/// the LLM can sprinkle `y=1.5` on a node without respelling `pos`.
pub(super) fn resolve_pos(node: &Node) -> Vec3 {
    if let (Some(a), Some(b)) = (node.attr_vec3("from"), node.attr_vec3("to")) {
        return (a + b) * 0.5;
    }
    let base = node.attr_vec3("pos").unwrap_or(Vec3::ZERO);
    Vec3::new(
        node.attr_number("x").unwrap_or(base.x),
        node.attr_number("y").unwrap_or(base.y),
        node.attr_number("z").unwrap_or(base.z),
    )
}

/// Rotation from `rot=[rx,ry,rz]` (degrees, XYZ Euler) plus the named
/// shortcuts `rx`/`ry`/`rz` for single-axis spins.
pub(super) fn resolve_rot(node: &Node) -> Quat {
    let base = node.attr_vec3("rot").unwrap_or(Vec3::ZERO);
    let rx = node.attr_number("rx").unwrap_or(base.x);
    let ry = node.attr_number("ry").unwrap_or(base.y);
    let rz = node.attr_number("rz").unwrap_or(base.z);
    Quat::from_euler(glam::EulerRot::XYZ, rx.to_radians(), ry.to_radians(), rz.to_radians())
}

/// Resolve a 3D `size=[w,h,d]` with four equivalent author forms:
///   `size=[1,2,3]`   — classic vec3
///   `size=1.5`       — scalar, expands to `[1.5, 1.5, 1.5]` (cube shorthand)
///   `w=1, h=2, d=3`  — per-axis shortcuts (missing axes fall back to `size`/default)
///   `from=[…], to=[…]` — corner form: size is `|to - from|`
/// Explicit `w`/`h`/`d` override individual components of `size` when mixed.
pub(super) fn resolve_size3(node: &Node, default: Vec3) -> Vec3 {
    if let (Some(a), Some(b)) = (node.attr_vec3("from"), node.attr_vec3("to")) {
        return (b - a).abs();
    }
    let base = match node.attr("size") {
        Some(Value::Number(n)) => Vec3::splat(*n),
        Some(Value::Vec3(v)) => Vec3::from_array(*v),
        _ => default,
    };
    Vec3::new(
        node.attr_number("w").unwrap_or(base.x),
        node.attr_number("h").unwrap_or(base.y),
        node.attr_number("d").unwrap_or(base.z),
    )
}

/// 2D size for XZ-aligned primitives (`plane`, `curved_plane`). Accepts
/// scalar, vec3 (Y ignored), or 2-element list. `w`/`d` override individual
/// components.
pub(super) fn resolve_size_xz(node: &Node, default: [f32; 2]) -> [f32; 2] {
    let base: [f32; 2] = match node.attr("size") {
        Some(Value::Number(n)) => [*n, *n],
        Some(Value::Vec3(v)) => [v[0], v[2]],
        Some(Value::List(v)) if v.len() == 2 => [v[0], v[1]],
        _ => default,
    };
    [
        node.attr_number("w").unwrap_or(base[0]),
        node.attr_number("d").unwrap_or(base[1]),
    ]
}

/// 2D size for XY-aligned primitives (`quad`). Accepts scalar, vec3 (Z
/// ignored), or 2-element list. `w`/`h` override individual components.
pub(super) fn resolve_size_xy(node: &Node, default: [f32; 2]) -> [f32; 2] {
    let base: [f32; 2] = match node.attr("size") {
        Some(Value::Number(n)) => [*n, *n],
        Some(Value::Vec3(v)) => [v[0], v[1]],
        Some(Value::List(v)) if v.len() == 2 => [v[0], v[1]],
        _ => default,
    };
    [
        node.attr_number("w").unwrap_or(base[0]),
        node.attr_number("h").unwrap_or(base[1]),
    ]
}

/// The effective anchor for this node. Reads the explicit `anchor` attr, then
/// falls back to the kind's default (e.g. `slab` → `bottom`). `None` means
/// "center" — the mesh is left at the primitive's natural origin.
pub(super) fn anchor_for(node: &Node) -> Option<String> {
    let explicit = match node.attr("anchor") {
        Some(Value::String(s)) | Some(Value::Ident(s)) => Some(s.clone()),
        _ => None,
    };
    explicit.or_else(|| default_anchor(&node.kind).map(|s| s.to_string()))
}

/// Default anchor per primitive kind. Box aliases with a natural "resting
/// face" set it here so authors get the intuitive placement without typing
/// `anchor=` on every row. Returns `None` for kinds that should stay
/// centered.
pub(super) fn default_anchor(kind: &str) -> Option<&'static str> {
    match kind {
        "slab" | "post" => Some("bottom"),
        "panel" => Some("back"),
        _ => None,
    }
}

/// Compute where the named anchor point sits within an AABB. `"center"` (or
/// an empty/unrecognised token) leaves a component at the AABB centre;
/// underscore-separated tokens override individual axes
/// (`bottom`, `top`, `left`, `right`, `front`, `back`). Combined anchors like
/// `bottom_left` or `top_front_right` work by overriding one axis per token.
fn anchor_point(aabb: &Aabb, anchor: &str) -> Vec3 {
    let mut p = aabb.center();
    for token in anchor.split('_') {
        match token {
            "" | "center" => {}
            "top" => p.y = aabb.max.y,
            "bottom" => p.y = aabb.min.y,
            "left" => p.x = aabb.min.x,
            "right" => p.x = aabb.max.x,
            "front" => p.z = aabb.min.z,
            "back" => p.z = aabb.max.z,
            _ => {}
        }
    }
    p
}

/// Shift every vertex in `mesh` so that the anchor face/corner/centre lands
/// at the mesh's local origin. Returns the applied offset so callers can
/// translate default connectors by the same amount (they are authored in the
/// natural frame and need to move with the mesh).
pub(super) fn apply_anchor_to_mesh(mesh: &mut Mesh, anchor: Option<&str>) -> Vec3 {
    let Some(anchor) = anchor else { return Vec3::ZERO };
    if mesh.positions.is_empty() {
        return Vec3::ZERO;
    }
    let aabb = Aabb::from_mesh(mesh);
    let shift = -anchor_point(&aabb, anchor);
    if shift == Vec3::ZERO {
        return shift;
    }
    for p in &mut mesh.positions {
        p[0] += shift.x;
        p[1] += shift.y;
        p[2] += shift.z;
    }
    shift
}

pub(super) fn axis_vec3(v: &Value) -> Option<Vec3> {
    match v {
        Value::Ident(s) | Value::String(s) => match s.as_str() {
            "x" | "X" => Some(Vec3::X),
            "y" | "Y" => Some(Vec3::Y),
            "z" | "Z" => Some(Vec3::Z),
            _ => None,
        },
        Value::Vec3(v) => Some(Vec3::from_array(*v)),
        _ => None,
    }
}

pub(super) fn string_or_ident(v: Option<&Value>) -> Option<&str> {
    match v? {
        Value::String(s) | Value::Ident(s) => Some(s.as_str()),
        _ => None,
    }
}
