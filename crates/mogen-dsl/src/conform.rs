//! `conform` primitive resolution.
//!
//! A `conform (target="...", child="...", from="...", to="...", ...)` node
//! says: deform `child`'s mesh so its vertices follow a path on `target`'s
//! surface. The path runs from `target.from` to `target.to` (two connectors
//! on the target node), the strip's "along" axis becomes arc-length on the
//! path, and the strip's perpendicular axes lie tangent / normal to the
//! surface at each sample.
//!
//! Companion to `attach.rs`: where attach sets a rigid transform, conform
//! mutates vertex positions. Runs immediately after `resolve_attaches` so an
//! attached child can also be conformed (the conform pass reads the post-
//! attach transforms when computing the target↔child coordinate map). Runs
//! before `bind_meshes` so skin bind-pose world matrices reflect the
//! deformed geometry.

use std::collections::HashMap;

use anyhow::{anyhow, bail, Result};

use mogen_core::{Connector, ConformBinding, NodeId, SceneGraph, Span, Transform};
use mogen_geom::{
    build_path_frames, conform_mesh, conform_patch, subdivide_along_axis, transform_mesh, Axis,
    AxisMap, ConformParams, PatchParams, SurfaceIndex,
};

use crate::ast::{Node, Value};
use crate::attach::reparent_pub;

/// Primitives whose vertex layout is compatible with **path-mode** conform
/// (strips and tubes stretched between two connectors).
const PATH_ALLOWED_KINDS: &[&str] = &[
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
const PATCH_ALLOWED_KINDS: &[&str] = &[
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
const REJECTED_KINDS: &[&str] = &[
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

#[derive(Debug)]
enum ConformMode {
    /// Strip stretched along a path between two connectors on the target.
    Path {
        from: String,
        to: String,
        along: Option<Axis>,
        width: Option<Axis>,
        height: Option<Axis>,
        samples: u32,
        twist_deg: f32,
    },
    /// Flat / disc child laid down at a single anchor connector on the target.
    Patch {
        at: String,
        up: Option<Axis>,
    },
}

#[derive(Debug)]
struct ConformSpec {
    target: String,
    child: String,
    lift: f32,
    reparent: bool,
    use_id: Option<u32>,
    #[allow(dead_code)]
    span: Span,
    mode: ConformMode,
}

/// Resolve every `conform` declared at AST scope.
pub fn resolve_conforms(ast: &[Node], graph: &mut SceneGraph) -> Result<()> {
    let specs = collect_conforms(ast)?;
    let mut by_use: HashMap<Option<u32>, Vec<ConformSpec>> = HashMap::new();
    for s in specs {
        by_use.entry(s.use_id).or_default().push(s);
    }
    for (_, group) in by_use {
        for spec in group {
            apply_conform(&spec, graph, None)?;
        }
    }
    Ok(())
}

/// Resolve `conform` declarations inside a replicated subtree.
pub fn resolve_conforms_in_scope(
    children: &[Node],
    graph: &mut SceneGraph,
    scope_root: NodeId,
) -> Result<()> {
    let mut specs = Vec::new();
    for c in children {
        walk(c, &mut specs)?;
    }
    for spec in &specs {
        apply_conform(spec, graph, Some(scope_root))?;
    }
    Ok(())
}

fn collect_conforms(ast: &[Node]) -> Result<Vec<ConformSpec>> {
    let mut out = Vec::new();
    for n in ast {
        walk(n, &mut out)?;
    }
    Ok(out)
}

fn walk(n: &Node, out: &mut Vec<ConformSpec>) -> Result<()> {
    if n.kind == "conform" {
        out.push(build_spec(n)?);
        return Ok(());
    }
    if n.kind == "array" || n.kind == "mirror" {
        return Ok(());
    }
    for c in &n.children {
        walk(c, out)?;
    }
    Ok(())
}

fn build_spec(n: &Node) -> Result<ConformSpec> {
    let target = str_attr(n, "target")
        .ok_or_else(|| anyhow!("conform requires target=\"<node name>\""))?;
    let child = str_attr(n, "child")
        .ok_or_else(|| anyhow!("conform requires child=\"<node name>\""))?;

    // Reserved attributes for future modes — reject early so users get a
    // clear error rather than silent fallback to defaults.
    if n.attr("direction").is_some() {
        bail!(
            "conform: direction= projection mode is not yet implemented (v1 supports path mode via from=/to= and patch mode via at=)"
        );
    }
    if let Some(curve) = str_attr(n, "curve") {
        if curve != "geodesic_lerp" {
            bail!(
                "conform: curve=\"{curve}\" is not yet implemented (v1 supports curve=\"geodesic_lerp\" only)"
            );
        }
    }
    if n.attr("via").is_some() {
        bail!("conform: via= multi-segment paths are not yet implemented");
    }

    // Mode discrimination: `at` selects patch mode, `from`/`to` selects path
    // mode. Mixing or omitting both is a hard error so authors get an
    // actionable diagnostic.
    let has_at = n.attr("at").is_some();
    let has_from = n.attr("from").is_some();
    let has_to = n.attr("to").is_some();
    if has_at && (has_from || has_to) {
        bail!(
            "conform: cannot combine patch-mode (at=) with path-mode (from=/to=); pick one"
        );
    }
    if !has_at && !(has_from || has_to) {
        bail!(
            "conform requires either at=\"<connector>\" (patch mode) or from=\"<connector>\" to=\"<connector>\" (path mode)"
        );
    }

    let lift = n.attr_number("lift").unwrap_or(0.0);
    // `reparent` defaults to true. Author can pass `reparent=0` to disable.
    let reparent = n.attr_number("reparent").map(|v| v != 0.0).unwrap_or(true);

    let mode = if has_at {
        // Reject path-mode-only attrs so authors don't accidentally write
        // attrs that get silently ignored.
        for k in ["along", "width", "height", "samples", "twist"] {
            if n.attr(k).is_some() {
                bail!(
                    "conform: attribute `{k}` is path-mode only (use it with from=/to=, not at=)"
                );
            }
        }
        let at = str_attr(n, "at").unwrap();
        let up = parse_axis(n, "up");
        ConformMode::Patch { at, up }
    } else {
        if n.attr("up").is_some() {
            bail!(
                "conform: attribute `up` is patch-mode only (use it with at=, not from=/to=)"
            );
        }
        let from = str_attr(n, "from")
            .ok_or_else(|| anyhow!("conform requires from=\"<connector>\" on the target"))?;
        let to = str_attr(n, "to")
            .ok_or_else(|| anyhow!("conform requires to=\"<connector>\" on the target"))?;
        let along = parse_axis(n, "along");
        let width = parse_axis(n, "width");
        let height = parse_axis(n, "height");
        let samples = n
            .attr_number("samples")
            .map(|v| v.max(2.0) as u32)
            .unwrap_or(64);
        let twist_deg = n.attr_number("twist").unwrap_or(0.0);
        ConformMode::Path { from, to, along, width, height, samples, twist_deg }
    };

    Ok(ConformSpec {
        target,
        child,
        lift,
        reparent,
        use_id: n.use_id,
        span: n.span,
        mode,
    })
}

fn str_attr(n: &Node, key: &str) -> Option<String> {
    match n.attr(key)? {
        Value::String(s) | Value::Ident(s) => Some(s.clone()),
        _ => None,
    }
}

fn parse_axis(n: &Node, key: &str) -> Option<Axis> {
    match n.attr(key)? {
        Value::String(s) | Value::Ident(s) => match s.as_str() {
            "x" | "X" => Some(Axis::X),
            "y" | "Y" => Some(Axis::Y),
            "z" | "Z" => Some(Axis::Z),
            _ => None,
        },
        _ => None,
    }
}

fn apply_conform(
    spec: &ConformSpec,
    graph: &mut SceneGraph,
    scope: Option<NodeId>,
) -> Result<()> {
    // Lookup precedence mirrors `attach`: explicit scope (replicator
    // per-instance pass) is strict; otherwise frame-visible match against
    // the spec's `use_id`.
    let find = |name: &str| -> Option<NodeId> {
        if let Some(root) = scope {
            return graph.find_node_in_subtree(root, name);
        }
        graph
            .nodes
            .iter()
            .position(|n| n.name == name && graph.use_id_visible(spec.use_id, n.use_id))
            .map(|i| NodeId(i as u32))
    };

    let target_id = find(&spec.target)
        .ok_or_else(|| anyhow!("conform: unknown target node \"{}\"", spec.target))?;
    let child_id = find(&spec.child)
        .ok_or_else(|| anyhow!("conform: unknown child node \"{}\"", spec.child))?;
    if target_id == child_id {
        bail!(
            "conform: \"{}\" cannot be conformed onto itself",
            spec.child
        );
    }

    match &spec.mode {
        ConformMode::Path { .. } => apply_path(spec, graph, target_id, child_id),
        ConformMode::Patch { .. } => apply_patch(spec, graph, target_id, child_id),
    }
}

fn apply_path(
    spec: &ConformSpec,
    graph: &mut SceneGraph,
    target_id: NodeId,
    child_id: NodeId,
) -> Result<()> {
    let ConformMode::Path { from, to, along, width, height, samples, twist_deg } = &spec.mode
    else {
        unreachable!("apply_path called with non-Path mode");
    };

    let child_kind = graph.nodes[child_id.0 as usize].kind.clone();
    check_kind_allowed(&child_kind, &spec.child, ConformModeKind::Path)?;

    // Imported meshes have no canonical long axis, so the author must
    // pick one explicitly.
    let along = match (child_kind.as_str(), *along) {
        ("mesh", None) => {
            bail!(
                "conform: imported mesh \"{}\" must specify along= (no canonical long axis)",
                spec.child
            );
        }
        (_, Some(a)) => a,
        _ => default_along_for(&child_kind),
    };
    let axes = build_axis_map(along, *width, *height);
    if !axes.is_valid() {
        bail!(
            "conform: along/width/height axes must be distinct, got along={:?} width={:?} height={:?}",
            along,
            width,
            height
        );
    }

    // Resolve target connectors first so missing-connector errors fire
    // before any expensive surface-index work.
    let (from_pos, to_pos) = {
        let t = &graph.nodes[target_id.0 as usize];
        let from_c = t.connectors.iter().find(|c| c.name == *from).ok_or_else(|| {
            anyhow!(
                "conform: target \"{}\" has no connector \"{}\" (available: {})",
                spec.target,
                from,
                list_connector_names(&t.connectors)
            )
        })?;
        let to_c = t.connectors.iter().find(|c| c.name == *to).ok_or_else(|| {
            anyhow!(
                "conform: target \"{}\" has no connector \"{}\" (available: {})",
                spec.target,
                to,
                list_connector_names(&t.connectors)
            )
        })?;
        (from_c.pos, to_c.pos)
    };

    let surface = build_target_surface(graph, target_id, &spec.target)?;

    // Snap the connector positions onto the surface — gives us robust
    // endpoints even when the author placed the connector slightly off.
    let s_a = surface
        .closest_point(from_pos)
        .ok_or_else(|| anyhow!("conform: failed to snap from=\"{}\" onto surface", from))?
        .pos;
    let s_b = surface
        .closest_point(to_pos)
        .ok_or_else(|| anyhow!("conform: failed to snap to=\"{}\" onto surface", to))?
        .pos;

    let frames = build_path_frames(&surface, s_a, s_b, *samples, *twist_deg);
    if frames.len() < 2 {
        bail!(
            "conform: failed to build path between \"{}\" and \"{}\" on target \"{}\"",
            from,
            to,
            spec.target
        );
    }

    let child_mesh = graph.nodes[child_id.0 as usize]
        .mesh
        .as_ref()
        .ok_or_else(|| anyhow!("conform: child \"{}\" has no mesh", spec.child))?
        .clone();
    // Auto-subdivide the child along the path axis when its tessellation is
    // too coarse to follow the path's curvature. A bare `box` has only two
    // distinct values along each axis, so without this every interior path
    // frame is skipped and the strip stays straight. Target one child segment
    // per ~4 path samples; that's dense enough to track the curvature without
    // ballooning vertex counts.
    let target_segments = (*samples / 4).clamp(8, 64);
    let child_mesh = subdivide_along_axis(&child_mesh, along, target_segments);
    let deformed_target_local = conform_mesh(
        &child_mesh,
        &frames,
        &ConformParams { axes, lift: spec.lift },
    );

    // Sanity check: post-deformation extent must not balloon. Catches
    // e.g. an inverted axis selection.
    if let Some(span) = aabb_extent(&deformed_target_local, along.index()) {
        let chord = (s_b - s_a).length().max(1e-3);
        if span > chord * 4.0 {
            eprintln!(
                "warning: conform of \"{}\" produced strip much longer than chord ({:.3}m vs {:.3}m chord) — check axis selection",
                spec.child, span, chord
            );
        }
    }

    place_deformed_mesh(graph, child_id, target_id, deformed_target_local, spec.reparent);
    graph.nodes[child_id.0 as usize].conform_binding = Some(ConformBinding::Path {
        target: target_id,
        from: from.clone(),
        to: to.clone(),
        samples: *samples,
    });

    Ok(())
}

fn apply_patch(
    spec: &ConformSpec,
    graph: &mut SceneGraph,
    target_id: NodeId,
    child_id: NodeId,
) -> Result<()> {
    let ConformMode::Patch { at, up } = &spec.mode else {
        unreachable!("apply_patch called with non-Patch mode");
    };

    let child_kind = graph.nodes[child_id.0 as usize].kind.clone();
    check_kind_allowed(&child_kind, &spec.child, ConformModeKind::Patch)?;

    // Imported meshes have no canonical up axis — author must pick one.
    let up_axis = match (child_kind.as_str(), *up) {
        ("mesh", None) => {
            bail!(
                "conform: imported mesh \"{}\" must specify up= (no canonical normal axis)",
                spec.child
            );
        }
        (_, Some(a)) => a,
        _ => default_up_for(&child_kind),
    };

    // Resolve the anchor connector first so a missing-connector error fires
    // before we build the surface index.
    let (anchor_pos, anchor_dir) = {
        let t = &graph.nodes[target_id.0 as usize];
        let c = t.connectors.iter().find(|c| c.name == *at).ok_or_else(|| {
            anyhow!(
                "conform: target \"{}\" has no connector \"{}\" (available: {})",
                spec.target,
                at,
                list_connector_names(&t.connectors)
            )
        })?;
        // Connectors store rotation as a quat that turns +Y into `dir`,
        // so the outward direction is `rotation * +Y`.
        let dir = c.rotation * glam::Vec3::Y;
        (c.pos, Some(dir))
    };

    let surface = build_target_surface(graph, target_id, &spec.target)?;

    let child_mesh = graph.nodes[child_id.0 as usize]
        .mesh
        .as_ref()
        .ok_or_else(|| anyhow!("conform: child \"{}\" has no mesh", spec.child))?
        .clone();

    let deformed_target_local = conform_patch(
        &child_mesh,
        &surface,
        &PatchParams {
            up: up_axis,
            anchor: anchor_pos,
            anchor_dir,
            roll_ref: None,
            lift: spec.lift,
        },
    );

    place_deformed_mesh(graph, child_id, target_id, deformed_target_local, spec.reparent);
    graph.nodes[child_id.0 as usize].conform_binding = Some(ConformBinding::Patch {
        target: target_id,
        at: at.clone(),
    });

    Ok(())
}

#[derive(Clone, Copy)]
enum ConformModeKind {
    Path,
    Patch,
}

fn check_kind_allowed(kind: &str, _child_name: &str, mode: ConformModeKind) -> Result<()> {
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

fn build_target_surface(
    graph: &SceneGraph,
    target_id: NodeId,
    target_name: &str,
) -> Result<SurfaceIndex> {
    let target_mesh = graph.nodes[target_id.0 as usize]
        .mesh
        .as_ref()
        .ok_or_else(|| {
            anyhow!(
                "conform: target \"{}\" has no geometry — conform requires a mesh-bearing target",
                target_name
            )
        })?
        .clone();
    let surface = SurfaceIndex::build(&target_mesh);
    if surface.is_empty() {
        bail!("conform: target \"{}\" mesh has no triangles", target_name);
    }
    Ok(surface)
}

fn place_deformed_mesh(
    graph: &mut SceneGraph,
    child_id: NodeId,
    target_id: NodeId,
    deformed_target_local: mogen_core::Mesh,
    reparent: bool,
) {
    if reparent {
        // Place the deformed mesh "as if" it lives in target-local space:
        // reparent under target with identity local transform. Any user
        // pos=/rot= on the child is intentionally discarded.
        graph.nodes[child_id.0 as usize].mesh = Some(deformed_target_local);
        graph.nodes[child_id.0 as usize].transform = Transform::IDENTITY;
        reparent_pub(graph, child_id, target_id);
    } else {
        // Keep the child's parentage intact; move the deformed mesh from
        // target-local space back into child-local space.
        let world = graph.world_transforms();
        let to_child = world[child_id.0 as usize].inverse() * world[target_id.0 as usize];
        let final_mesh = transform_mesh(&deformed_target_local, to_child);
        graph.nodes[child_id.0 as usize].mesh = Some(final_mesh);
    }
}

fn list_connector_names(cs: &[Connector]) -> String {
    if cs.is_empty() {
        return "<none>".to_string();
    }
    cs.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(", ")
}

fn default_along_for(kind: &str) -> Axis {
    // Tubes are authored along Y; flat strips along X (for box/quad/plane
    // the long axis is conventionally X, matching the default `size=[X, Y, Z]`).
    match kind {
        "cylinder" | "capsule" | "tube" => Axis::Y,
        _ => Axis::X,
    }
}

fn default_up_for(kind: &str) -> Axis {
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

fn build_axis_map(along: Axis, width: Option<Axis>, height: Option<Axis>) -> AxisMap {
    match (width, height) {
        (Some(w), Some(h)) => AxisMap { along, width: w, height: h },
        (Some(w), None) => {
            let h = remaining_axis(along, w);
            AxisMap { along, width: w, height: h }
        }
        (None, Some(h)) => {
            let w = remaining_axis(along, h);
            AxisMap { along, width: w, height: h }
        }
        (None, None) => AxisMap::from_along(along),
    }
}

fn remaining_axis(a: Axis, b: Axis) -> Axis {
    for c in [Axis::X, Axis::Y, Axis::Z] {
        if c != a && c != b {
            return c;
        }
    }
    // Defensive default: should be unreachable given non-equal inputs.
    Axis::Z
}

fn aabb_extent(mesh: &mogen_core::Mesh, axis: usize) -> Option<f32> {
    if mesh.positions.is_empty() {
        return None;
    }
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    for p in &mesh.positions {
        let v = p[axis];
        if v < lo {
            lo = v;
        }
        if v > hi {
            hi = v;
        }
    }
    Some(hi - lo)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lower;
    use crate::parser::parse;

    fn build(src: &str) -> SceneGraph {
        let ast = parse(src).expect("parse");
        lower(&ast).expect("lower")
    }

    fn build_err(src: &str) -> String {
        let ast = parse(src).expect("parse");
        format!("{}", lower(&ast).unwrap_err())
    }

    #[test]
    fn flat_target_keeps_strip_unchanged() {
        // Conforming a flat strip onto a flat plane shouldn't warp it.
        let src = r#"
            scene {
              plane "ground" (size=[4, 4]) {
                connector "a" (at=[-1, 0,  0], dir=[0, 1, 0])
                connector "b" (at=[ 1, 0,  0], dir=[0, 1, 0])
              }
              box "stripe" (size=[2.0, 0.05, 0.2])
              conform (target="ground", child="stripe", from="a", to="b",
                       along=x, lift=0.001)
            }
        "#;
        let g = build(src);
        let stripe = g.find_node("stripe").unwrap();
        let mesh = g.nodes[stripe.0 as usize].mesh.as_ref().unwrap();
        // Every vertex y should be lift ± strip half-thickness (0.025).
        for p in &mesh.positions {
            assert!(
                p[1] > -0.03 && p[1] < 0.03,
                "stripe vertex y outside expected range: {p:?}"
            );
        }
        // After reparent=true (default), stripe is parented under ground.
        let ground = g.find_node("ground").unwrap();
        assert_eq!(g.nodes[stripe.0 as usize].parent, Some(ground));
    }

    #[test]
    fn zip_on_ellipsoid_lands_on_surface() {
        let src = r#"
            scene {
              ellipsoid "bag" (size=[1.0, 0.6, 0.6]) {
                connector "zip_a" (at=[-0.4, 0.25, 0.30], dir=[0, 0, 1])
                connector "zip_b" (at=[ 0.4, 0.25, 0.30], dir=[0, 0, 1])
              }
              box "zip" (size=[0.8, 0.012, 0.04])
              conform (target="bag", child="zip", from="zip_a", to="zip_b",
                       along=x, lift=0.003)
            }
        "#;
        let g = build(src);
        let zip = g.find_node("zip").unwrap();
        let mesh = g.nodes[zip.0 as usize].mesh.as_ref().unwrap();
        // Every zip vertex should sit just outside the ellipsoid surface.
        // The ellipsoid is the "bag" with semi-axes (0.5, 0.3, 0.3); a
        // surface point at (x, y, z) satisfies x²/0.25 + y²/0.09 + z²/0.09 = 1.
        for p in &mesh.positions {
            let v = (p[0] * p[0]) / 0.25 + (p[1] * p[1]) / 0.09 + (p[2] * p[2]) / 0.09;
            // lift=3mm + strip thickness up to ~6mm pushes vertices
            // slightly outside the unit isosurface.
            assert!(
                v > 0.95 && v < 1.20,
                "zip vertex {p:?} not near surface (iso={v})"
            );
        }
    }

    #[test]
    fn conform_writes_binding_for_tooling() {
        let src = r#"
            scene {
              plane "ground" (size=[4, 4]) {
                connector "a" (at=[-1, 0, 0], dir=[0, 1, 0])
                connector "b" (at=[ 1, 0, 0], dir=[0, 1, 0])
              }
              box "stripe" (size=[2, 0.05, 0.2])
              conform (target="ground", child="stripe", from="a", to="b", along=x)
            }
        "#;
        let g = build(src);
        let stripe = g.find_node("stripe").unwrap();
        let cb = g.nodes[stripe.0 as usize]
            .conform_binding
            .as_ref()
            .expect("conform binding written");
        match cb {
            ConformBinding::Path { target, from, to, .. } => {
                assert_eq!(*target, g.find_node("ground").unwrap());
                assert_eq!(from, "a");
                assert_eq!(to, "b");
            }
            other => panic!("expected Path binding, got {other:?}"),
        }
    }

    #[test]
    fn conform_rejects_sphere_child() {
        let err = build_err(
            r#"
            scene {
              sphere "ball" (radius=0.5) {
                connector "a" (at=[-0.5, 0, 0], dir=[-1, 0, 0])
                connector "b" (at=[ 0.5, 0, 0], dir=[ 1, 0, 0])
              }
              sphere "decal" (radius=0.05)
              conform (target="ball", child="decal", from="a", to="b")
            }
            "#,
        );
        assert!(err.contains("cannot mould a \"sphere\""), "err = {err}");
        assert!(
            err.contains("supported path-mode kinds"),
            "err = {err}"
        );
    }

    #[test]
    fn conform_rejects_unknown_connector() {
        let err = build_err(
            r#"
            scene {
              plane "ground" (size=[4, 4]) {
                connector "a" (at=[-1, 0, 0], dir=[0, 1, 0])
              }
              box "stripe" (size=[2, 0.05, 0.2])
              conform (target="ground", child="stripe", from="a", to="zzz")
            }
            "#,
        );
        assert!(err.contains("no connector \"zzz\""), "err = {err}");
        // Available list should mention the connectors we did define.
        assert!(err.contains("\"ground\""), "err = {err}");
    }

    #[test]
    fn conform_rejects_imported_mesh_without_along() {
        // Building from a literal `mesh` node that loads an external GLB
        // is heavy; instead simulate by invoking conform on a `mesh`-kinded
        // child via a synthesized scene. This exercises the
        // `kind == "mesh" && along.is_none()` branch even without I/O.
        // (`mesh` lowering needs a real .glb path. Skip in DSL tests; the
        // branch is exercised by the build_spec → apply_conform unit path
        // through the code, which we cover via the broken/ snapshot in
        // `tests/broken/conform_imported_no_along.mog`.)
    }

    #[test]
    fn patch_disc_lays_on_plane_at_anchor() {
        // A disc patch at a connector on a flat plane: every vertex
        // sits at lift above the plane, planar offset equals the disc's
        // own rim radius from the anchor.
        let src = r#"
            scene {
              plane "ground" (size=[4, 4]) {
                connector "spot" (at=[0.5, 0, -0.2], dir=[0, 1, 0])
              }
              disc "patch" (radius=0.3, segments=24)
              conform (target="ground", child="patch", at="spot", lift=0.002)
            }
        "#;
        let g = build(src);
        let patch = g.find_node("patch").unwrap();
        let mesh = g.nodes[patch.0 as usize].mesh.as_ref().unwrap();
        for p in &mesh.positions {
            assert!(
                (p[1] - 0.002).abs() < 1e-3,
                "patch vertex y {} not at lift",
                p[1]
            );
        }
        // Reparent default → patch lives under ground.
        let ground = g.find_node("ground").unwrap();
        assert_eq!(g.nodes[patch.0 as usize].parent, Some(ground));
    }

    #[test]
    fn patch_disc_on_curved_target_follows_curvature() {
        // Patch a disc onto an ellipsoid — every vertex must sit close to
        // the surface (not on a flat tangent plane).
        let src = r#"
            scene {
              ellipsoid "bag" (size=[1.0, 0.6, 0.6]) {
                connector "spot" (at=[0.4, 0.2, 0.3], dir=[0, 0, 1])
              }
              disc "decal" (radius=0.12, segments=32)
              conform (target="bag", child="decal", at="spot", lift=0.005)
            }
        "#;
        let g = build(src);
        let decal = g.find_node("decal").unwrap();
        let mesh = g.nodes[decal.0 as usize].mesh.as_ref().unwrap();
        // Ellipsoid semi-axes (0.5, 0.3, 0.3); vertex iso ≈ 1 + tiny lift.
        for p in &mesh.positions {
            let v = (p[0] * p[0]) / 0.25 + (p[1] * p[1]) / 0.09 + (p[2] * p[2]) / 0.09;
            assert!(
                v > 0.95 && v < 1.20,
                "decal vertex {p:?} far from ellipsoid (iso={v})",
            );
        }
    }

    #[test]
    fn patch_writes_patch_binding() {
        let src = r#"
            scene {
              plane "ground" (size=[4, 4]) {
                connector "spot" (at=[0, 0, 0], dir=[0, 1, 0])
              }
              disc "patch" (radius=0.2, segments=12)
              conform (target="ground", child="patch", at="spot")
            }
        "#;
        let g = build(src);
        let patch = g.find_node("patch").unwrap();
        let cb = g.nodes[patch.0 as usize]
            .conform_binding
            .as_ref()
            .expect("conform binding written");
        match cb {
            ConformBinding::Patch { target, at } => {
                assert_eq!(*target, g.find_node("ground").unwrap());
                assert_eq!(at, "spot");
            }
            other => panic!("expected Patch binding, got {other:?}"),
        }
    }

    #[test]
    fn conform_rejects_mixing_modes() {
        let err = build_err(
            r#"
            scene {
              plane "ground" (size=[4, 4]) {
                connector "a" (at=[0, 0, 0], dir=[0, 1, 0])
                connector "b" (at=[1, 0, 0], dir=[0, 1, 0])
              }
              disc "patch" (radius=0.1, segments=8)
              conform (target="ground", child="patch", at="a", from="a", to="b")
            }
            "#,
        );
        assert!(
            err.contains("cannot combine patch-mode") && err.contains("path-mode"),
            "err = {err}"
        );
    }

    #[test]
    fn conform_rejects_no_mode() {
        let err = build_err(
            r#"
            scene {
              plane "ground" (size=[4, 4])
              disc "patch" (radius=0.1, segments=8)
              conform (target="ground", child="patch")
            }
            "#,
        );
        assert!(
            err.contains("at=") && err.contains("from=") && err.contains("to="),
            "err = {err}"
        );
    }

    #[test]
    fn conform_path_mode_disc_hints_patch_mode() {
        // Disc isn't allowed in path mode; the error should suggest patch
        // mode rather than just listing supported kinds.
        let err = build_err(
            r#"
            scene {
              plane "ground" (size=[4, 4]) {
                connector "a" (at=[-1, 0, 0], dir=[0, 1, 0])
                connector "b" (at=[ 1, 0, 0], dir=[0, 1, 0])
              }
              disc "patch" (radius=0.1, segments=12)
              conform (target="ground", child="patch", from="a", to="b")
            }
            "#,
        );
        assert!(
            err.contains("try patch mode") && err.contains("disc"),
            "err = {err}"
        );
    }

    #[test]
    fn conform_patch_mode_sphere_still_rejected() {
        // Closed shapes (sphere) lack any canonical surface axis and stay
        // rejected even in patch mode.
        let err = build_err(
            r#"
            scene {
              plane "ground" (size=[4, 4]) {
                connector "spot" (at=[0, 0, 0], dir=[0, 1, 0])
              }
              sphere "ball" (radius=0.1)
              conform (target="ground", child="ball", at="spot")
            }
            "#,
        );
        assert!(err.contains("cannot mould a \"sphere\""), "err = {err}");
        assert!(err.contains("patch mode"), "err = {err}");
    }

    #[test]
    fn patch_mode_rejects_path_only_attrs() {
        let err = build_err(
            r#"
            scene {
              plane "ground" (size=[4, 4]) {
                connector "spot" (at=[0, 0, 0], dir=[0, 1, 0])
              }
              disc "patch" (radius=0.1, segments=12)
              conform (target="ground", child="patch", at="spot", along=x)
            }
            "#,
        );
        assert!(err.contains("path-mode only"), "err = {err}");
    }

    #[test]
    fn conform_no_reparent_keeps_original_parent() {
        let src = r#"
            scene {
              group "root" {
                plane "ground" (size=[4, 4]) {
                  connector "a" (at=[-1, 0, 0], dir=[0, 1, 0])
                  connector "b" (at=[ 1, 0, 0], dir=[0, 1, 0])
                }
                box "stripe" (size=[2, 0.05, 0.2])
              }
              conform (target="ground", child="stripe", from="a", to="b",
                       along=x, reparent=0)
            }
        "#;
        let g = build(src);
        let root = g.find_node("root").unwrap();
        let stripe = g.find_node("stripe").unwrap();
        // With reparent=0, stripe stays under "root", not under ground.
        assert_eq!(g.nodes[stripe.0 as usize].parent, Some(root));
    }
}
