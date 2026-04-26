use std::f32::consts::TAU;

use anyhow::Result;
use glam::{Quat, Vec3};

use mogen_core::{MaterialId, NodeId, SceneGraph, Transform, UvMode};
use mogen_geom::{leaf_card_mesh, spline_tube_mesh};

use crate::ast::{Node, Value};

use super::helpers::{inherit_material_from_ancestor, transform_from_attrs};
use super::node::apply_metadata;

/// Per-tree configuration assembled once from the user's `branch` attrs and
/// then read by every recursive expansion. Keeping this in one struct stops
/// the recursion signature from growing into a parameter zoo.
struct BranchCfg {
    length: f32,
    radius: f32,
    depth: u32,
    splits: u32,
    length_falloff: f32,
    radius_falloff: f32,
    branch_angle_rad: f32,
    roll_rad: f32,
    tropism: f32,
    bend_rad: f32,
    radial_segments: u32,
    samples_per_seg: u32,
    cps_per_seg: usize,
    jitter: f32,
    leaves: bool,
    leaf_size: f32,
    leaf_cards: u32,
    leaf_material: Option<MaterialId>,
}

/// Top-level expander for `branch "tree" (...)`. Creates a wrapper group at the
/// user's pos/rot and recursively emits spline_tube branch segments + optional
/// alpha-cutout leaf cards at the tips. Every node produced here is marked
/// non-editable: the geometry is a deterministic function of the seed and
/// can't be hand-edited piecewise (rebuilding wipes any tweaks).
pub(super) fn expand_branch(
    node: &Node,
    parent: Option<NodeId>,
    graph: &mut SceneGraph,
) -> Result<NodeId> {
    let cfg = read_cfg(node, graph);

    let wrapper_name = node.name.clone().unwrap_or_else(|| node.kind.clone());
    let wrapper_transform = transform_from_attrs(node);
    let wrapper_id = match parent {
        None => graph.add_root(&wrapper_name, &node.kind, wrapper_transform),
        Some(p) => graph.add_child(p, &wrapper_name, &node.kind, wrapper_transform),
    };
    graph.set_source_span(wrapper_id, node.span);
    apply_metadata(node, wrapper_id, graph)?;

    let pre_expand_count = graph.nodes.len();

    let mut rng: u32 = node.attr_number("seed").map(|n| n as u32).unwrap_or(1).max(1);

    emit_segment(
        wrapper_id,
        cfg.depth,
        cfg.length,
        cfg.radius,
        0.0,
        &mut rng,
        &cfg,
        graph,
    );

    // The whole tree below the wrapper is procedurally derived — there's no
    // single AST span to write back to for any individual segment. The wrapper
    // itself stays editable so the user can tweak `branch` attrs.
    for i in pre_expand_count..graph.nodes.len() {
        graph.nodes[i].editable = false;
    }

    Ok(wrapper_id)
}

fn read_cfg(node: &Node, graph: &SceneGraph) -> BranchCfg {
    let length = node.attr_number("length").unwrap_or(1.0).max(1e-3);
    let radius = node.attr_number("radius").unwrap_or(0.05).max(1e-4);
    let depth = node.attr_number("depth").unwrap_or(4.0).max(0.0) as u32;
    let splits = node.attr_number("splits").unwrap_or(2.0).max(1.0) as u32;
    let length_falloff = node.attr_number("length_falloff").unwrap_or(0.7);
    let radius_falloff = node.attr_number("radius_falloff").unwrap_or(0.6);
    let branch_angle_rad = node.attr_number("branch_angle").unwrap_or(35.0).to_radians();
    // 137.5° (golden angle) gives the natural phyllotaxis spiral that keeps
    // successive forks from stacking on top of each other.
    let roll_rad = node.attr_number("roll").unwrap_or(137.5).to_radians();
    let tropism = node.attr_number("tropism").unwrap_or(0.0);
    let bend_rad = node.attr_number("bend").unwrap_or(10.0).to_radians();
    let radial_segments = node.attr_number("segments").map(|n| n as u32).unwrap_or(8).max(3);
    let samples_per_seg = node.attr_number("samples").map(|n| n as u32).unwrap_or(4).max(1);
    let cps_per_seg = 4usize;
    let jitter = node.attr_number("jitter").unwrap_or(0.2).clamp(0.0, 1.0);
    let leaves = node.attr_number("leaves").map(|n| n != 0.0).unwrap_or(true);
    let leaf_size = node.attr_number("leaf_size").unwrap_or(0.35).max(0.0);
    let leaf_cards = node.attr_number("leaf_cards").unwrap_or(2.0).max(1.0) as u32;
    let leaf_material = match node.attr("leaf_mat") {
        Some(Value::String(s)) | Some(Value::Ident(s)) => graph.find_material(s),
        _ => None,
    };

    BranchCfg {
        length,
        radius,
        depth,
        splits,
        length_falloff,
        radius_falloff,
        branch_angle_rad,
        roll_rad,
        tropism,
        bend_rad,
        radial_segments,
        samples_per_seg,
        cps_per_seg,
        jitter,
        leaves,
        leaf_size,
        leaf_cards,
        leaf_material,
    }
}

fn emit_segment(
    parent_id: NodeId,
    depth_remaining: u32,
    length: f32,
    radius: f32,
    accumulated_roll: f32,
    rng: &mut u32,
    cfg: &BranchCfg,
    graph: &mut SceneGraph,
) {
    // Build a centerline along local +Y, with optional sideways bend in +X
    // and optional tropism droop in -Y near the tip. Authoring control points
    // densely lets `spline_tube_mesh`'s parallel-transport frames stay stable
    // even when bend is large.
    let n_cps = cfg.cps_per_seg.max(2);
    let mut cps: Vec<[f32; 3]> = Vec::with_capacity(n_cps);
    let mut radii: Vec<f32> = Vec::with_capacity(n_cps);
    let trop = cfg.tropism * length;
    for i in 0..n_cps {
        let t = i as f32 / (n_cps - 1) as f32;
        let (px, py) = if cfg.bend_rad.abs() < 1e-5 {
            (0.0, length * t)
        } else {
            // Arc parameterisation: tangent at t=0 is +Y, the curve bends
            // smoothly in +X. R = length / θ keeps total arc length = `length`.
            let r = length / cfg.bend_rad;
            let phi = t * cfg.bend_rad;
            (r * (1.0 - phi.cos()), r * phi.sin())
        };
        // Tropism accumulates as t² so the droop is concentrated near the
        // tip — that's what real branches look like under their own weight.
        let droop = trop * t * t;
        cps.push([px, py + droop, 0.0]);
        // Linear taper from R at base to R*radius_falloff at tip.
        let r_here = radius * (1.0 - t * (1.0 - cfg.radius_falloff));
        radii.push(r_here);
    }

    let tip_pos = Vec3::from_array(*cps.last().unwrap());
    let tip_tangent = (tip_pos - Vec3::from_array(cps[n_cps - 2])).normalize_or(Vec3::Y);

    // Add the segment as a child mesh node. `branch_seg` is a synthetic kind
    // — it never appears in the grammar, so AST validation never sees it.
    let seg_name = format!("seg_d{depth_remaining}");
    let seg_id = graph.add_child(parent_id, seg_name, "branch_seg", Transform::IDENTITY);
    let mesh = spline_tube_mesh(
        &cps,
        &radii,
        cfg.radial_segments,
        cfg.samples_per_seg,
        true,
        UvMode::Tile,
    );
    graph.set_mesh(seg_id, mesh);
    inherit_material_from_ancestor(seg_id, graph);

    if depth_remaining > 0 {
        for i in 0..cfg.splits {
            let pitch_jitter = rand_pm(rng) * cfg.jitter * cfg.branch_angle_rad * 0.5;
            let yaw_jitter = rand_pm(rng) * cfg.jitter * 30.0_f32.to_radians();
            let yaw = (i as f32) * (TAU / cfg.splits as f32) + accumulated_roll + yaw_jitter;
            let pitch = cfg.branch_angle_rad + pitch_jitter;

            // Build the child orientation in three steps:
            //   (1) yaw around local +Y so each split lands at a different
            //       azimuth around the parent tip,
            //   (2) pitch around local +X to lean the branch off the parent
            //       axis by `branch_angle`,
            //   (3) align the resulting frame's +Y to the parent's actual tip
            //       tangent so bent parents pass their direction down the chain.
            let align_to_tangent = quat_from_y_to(tip_tangent);
            let yaw_q = Quat::from_axis_angle(Vec3::Y, yaw);
            let pitch_q = Quat::from_axis_angle(Vec3::X, pitch);
            let q = align_to_tangent * yaw_q * pitch_q;

            let child_id = graph.add_child(
                parent_id,
                format!("fork_{i}"),
                "group",
                Transform::from_trs(tip_pos, q, Vec3::ONE),
            );

            // Per-fork length jitter — small variations stop sibling forks from
            // looking like a manufactured fork (every branch at the same length
            // reads as artificial).
            let length_jitter = 1.0 + rand_pm(rng) * cfg.jitter * 0.3;
            let next_length = length * cfg.length_falloff * length_jitter;
            let next_radius = radius * cfg.radius_falloff;

            emit_segment(
                child_id,
                depth_remaining - 1,
                next_length,
                next_radius,
                accumulated_roll + cfg.roll_rad,
                rng,
                cfg,
                graph,
            );
        }
    } else if cfg.leaves && cfg.leaf_size > 0.0 {
        // Leaf at the tip, oriented so its +Y matches the branch tangent —
        // the leaf "grows out" of the branch rather than sitting at right
        // angles to the world.
        let q = quat_from_y_to(tip_tangent);
        let leaf_id = graph.add_child(
            parent_id,
            "leaf",
            "leaf_card",
            Transform::from_trs(tip_pos, q, Vec3::ONE),
        );
        let leaf_mesh = leaf_card_mesh(
            [cfg.leaf_size, cfg.leaf_size],
            cfg.leaf_cards,
            UvMode::Fit,
        );
        graph.set_mesh(leaf_id, leaf_mesh);
        if let Some(mid) = cfg.leaf_material {
            graph.set_material(leaf_id, mid);
        } else {
            inherit_material_from_ancestor(leaf_id, graph);
        }
    }
}

/// Quaternion that rotates +Y onto `target`. Falls back to a 180° flip around
/// +X when `target` is anti-parallel to +Y, which `Quat::from_rotation_arc`
/// can't express directly.
fn quat_from_y_to(target: Vec3) -> Quat {
    let t = target.normalize_or(Vec3::Y);
    if t.y < -0.9999 {
        Quat::from_axis_angle(Vec3::X, std::f32::consts::PI)
    } else {
        Quat::from_rotation_arc(Vec3::Y, t)
    }
}

/// Linear-congruential RNG returning a [-1, 1] float. Stateful but cheap and
/// fully deterministic given the seed — the whole point of a procedural tree
/// is that the same seed regrows the same shape on every compile.
fn rand_pm(state: &mut u32) -> f32 {
    *state = state.wrapping_mul(1664525).wrapping_add(1013904223);
    let bits = (*state >> 8) & 0x00FF_FFFF;
    (bits as f32 / (1u32 << 24) as f32) * 2.0 - 1.0
}
