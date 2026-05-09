//! Allowed-kind tables and per-kind defaults for the conform child slot.
//! Path mode and patch mode have different vertex-layout assumptions, so
//! the same primitive may be valid in one but not the other.

use anyhow::{bail, Result};

use mogen_geom::Axis;

/// Primitives whose vertex layout is compatible with **path-mode** conform
/// (strips and tubes stretched between two connectors).
pub(super) const PATH_ALLOWED_KINDS: &[&str] = &[
    // Flat strips
    "box",
    "plane",
    "quad",
    "curved_plane",
    "slab",
    "post",
    "panel",
    "wall",
    // Tubes
    "cylinder",
    "capsule",
    "tube",
    "spline_tube",
    "spline_ribbon",
    // Imported meshes — `along=` is required.
    "mesh",
];

/// Primitives whose vertex layout is compatible with **patch-mode** conform
/// (decals / discs laid down at a single anchor). More permissive than
/// path mode because patch only needs a clear "up" axis, not an along axis.
pub(super) const PATCH_ALLOWED_KINDS: &[&str] = &[
    // Flat decals
    "disc",
    "plane",
    "quad",
    "curved_plane",
    "leaf_card",
    "decal",
    // Box-likes — must be thin along the up axis to make sense as a decal,
    // but we allow them and let the user choose an appropriate up axis.
    "box",
    "slab",
    "panel",
    "wall",
    // Round primitives that have a clear flat side or thin axis.
    "cylinder",
    "hemisphere",
    "half_cylinder",
    // Imported meshes — `up=` is required.
    "mesh",
];

/// Primitives that don't fit either mode — closed shapes with no canonical
/// along OR up axis, plus structural / replicator nodes.
pub(super) const REJECTED_KINDS: &[&str] = &[
    "sphere",
    "ellipsoid",
    "icosphere",
    "torus",
    "torus_arc",
    "superellipsoid",
    "pyramid",
    "cone",
    "frustum",
    "lathe",
    "prism",
    "rounded_box",
    "wedge",
    "union",
    "difference",
    "intersect",
    "group",
    "scene",
    "solid",
    "branch",
    "branch_seg",
];

#[derive(Clone, Copy)]
pub(super) enum ConformModeKind {
    Path,
    Patch,
}

pub(super) fn check_kind_allowed(
    kind: &str,
    _child_name: &str,
    mode: ConformModeKind,
) -> Result<()> {
    let (allowed, this_label, other_label, other_allowed) = match mode {
        ConformModeKind::Path => (
            PATH_ALLOWED_KINDS,
            "path",
            "patch",
            PATCH_ALLOWED_KINDS,
        ),
        ConformModeKind::Patch => (
            PATCH_ALLOWED_KINDS,
            "patch",
            "path",
            PATH_ALLOWED_KINDS,
        ),
    };
    if allowed.contains(&kind) {
        return Ok(());
    }
    let mut hint = String::new();
    if other_allowed.contains(&kind) {
        let switch = match mode {
            ConformModeKind::Path => "try patch mode (at=)",
            ConformModeKind::Patch => "try path mode (from=/to=)",
        };
        hint.push_str(&format!(" — {switch}"));
    } else if REJECTED_KINDS.contains(&kind) {
        hint.push_str(" (closed shape with no canonical surface axis)");
    }
    bail!(
        "conform: cannot mould a \"{kind}\" in {this_label} mode{hint} — \
        supported {this_label}-mode kinds: {} (other mode: {})",
        allowed.join(", "),
        other_label,
    );
}

pub(super) fn default_along_for(kind: &str) -> Axis {
    // Tubes are authored along Y; flat strips along X (for box/quad/plane
    // the long axis is conventionally X, matching the default `size=[X, Y, Z]`).
    match kind {
        "cylinder" | "capsule" | "tube" => Axis::Y,
        _ => Axis::X,
    }
}

pub(super) fn default_up_for(kind: &str) -> Axis {
    // The patch's "up" axis is the one that should align with the surface
    // outward normal — i.e., the direction the primitive faces in its local
    // space.
    match kind {
        // Quad/leaf_card/decal face +Z by convention (decal is internally a
        // quad with a synthesized image material, oriented the same way).
        "quad" | "leaf_card" | "decal" => Axis::Z,
        // Disc, plane, curved_plane, hemisphere, half_cylinder, cylinder,
        // and the box-likes (slab/panel/wall) all face +Y when used as flat
        // decals; cylinders are also Y-axial so a thin one used as a disc
        // shares the same up axis.
        _ => Axis::Y,
    }
}
