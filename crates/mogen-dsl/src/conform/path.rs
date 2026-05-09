//! Path-mode conform: stretch a strip / tube child along a path between
//! two connectors on the target's surface.

use anyhow::{anyhow, bail, Result};

use mogen_core::{ConformBinding, NodeId, SceneGraph};
use mogen_geom::{
    build_path_frames, conform_mesh, subdivide_along_axis, Axis, AxisMap, ConformParams,
};

use super::kinds::{check_kind_allowed, default_along_for, ConformModeKind};
use super::place::{
    aabb_extent, bake_local_rs_into_mesh, build_target_surface, list_connector_names,
    place_deformed_mesh,
};
use super::spec::{ConformMode, ConformSpec};

pub(super) fn apply_path(
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
    // Honor the user's local rotation/scale before path conform — see
    // `apply_patch` for the rationale. Translation is dropped because the
    // strip's positioning comes from `from=`/`to=` on the target.
    let child_mesh = bake_local_rs_into_mesh(&graph.nodes[child_id.0 as usize].transform, &child_mesh);
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
