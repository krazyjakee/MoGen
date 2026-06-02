use std::f32::consts::TAU;

use anyhow::Result;
use glam::{Quat, Vec3};

use mogen_core::{MaterialId, NodeId, SceneGraph, Transform, UvMode};
use mogen_geom::{leaf_card_mesh, spline_tube_mesh};

use crate::ast::{Node, Value};

use super::cfg;
use super::helpers::inherit_material_from_ancestor;
use super::procedural::{begin_procedural, finish_procedural};
use super::rng::rand_pm;

/// Coarse growth habit for the procedural tree generator. Each form picks a
/// bundle of sensible defaults *and* selects which top-level emission path
/// runs (single trunk, multi-stem cluster, or trunk-plus-rosette). User
/// attributes still override every individual default.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Form {
    /// Default: every segment forks into `splits` children at its tip. Reads
    /// as a typical broadleaf tree (oak, maple, generic foliage prop).
    Decurrent,
    /// Conifer-like silhouette. Strong central leader, near-horizontal side
    /// branches, narrow needle leaves. Implemented via `leader_bias`.
    Excurrent,
    /// Willow-like. Long, lightly-angled branches with strong negative
    /// tropism so tips droop under gravity.
    Weeping,
    /// Bush. Multiple short trunks emerge from the base; no single leader.
    /// Driven by `multi_stem`.
    Shrub,
    /// Palm. Single straight trunk with no branching, plus a rosette of
    /// frond-shaped leaf cards at the top.
    Palm,
}

fn read_form(s: &str) -> Option<Form> {
    match s {
        "decurrent" => Some(Form::Decurrent),
        "excurrent" => Some(Form::Excurrent),
        "weeping" => Some(Form::Weeping),
        "shrub" => Some(Form::Shrub),
        "palm" => Some(Form::Palm),
        _ => None,
    }
}

/// Per-form default bundle. Layered under user attributes in `read_cfg`.
/// `branch_angle` is in degrees here so the literals read like the user-
/// facing units.
struct FormDefaults {
    length: f32,
    radius: f32,
    depth: u32,
    splits: u32,
    length_falloff: f32,
    radius_falloff: f32,
    branch_angle: f32,
    tropism: f32,
    bend: f32,
    leader_bias: f32,
    multi_stem: u32,
    leaf_size: f32,
    leaf_aspect: f32,
    leaf_cards: u32,
}

fn form_defaults(f: Form) -> FormDefaults {
    match f {
        Form::Decurrent => FormDefaults {
            length: 1.0,
            radius: 0.05,
            depth: 4,
            splits: 2,
            length_falloff: 0.7,
            radius_falloff: 0.6,
            branch_angle: 35.0,
            tropism: 0.0,
            bend: 10.0,
            leader_bias: 0.0,
            multi_stem: 1,
            leaf_size: 0.35,
            leaf_aspect: 1.0,
            leaf_cards: 2,
        },
        Form::Excurrent => FormDefaults {
            length: 1.6,
            radius: 0.08,
            depth: 5,
            splits: 5,
            length_falloff: 0.55,
            radius_falloff: 0.55,
            branch_angle: 78.0,
            tropism: 0.05,
            bend: 4.0,
            leader_bias: 0.85,
            multi_stem: 1,
            leaf_size: 0.18,
            leaf_aspect: 0.18,
            leaf_cards: 6,
        },
        Form::Weeping => FormDefaults {
            length: 1.4,
            radius: 0.07,
            depth: 5,
            splits: 2,
            length_falloff: 0.78,
            radius_falloff: 0.58,
            branch_angle: 18.0,
            tropism: -0.4,
            bend: 18.0,
            leader_bias: 0.0,
            multi_stem: 1,
            leaf_size: 0.55,
            leaf_aspect: 0.35,
            leaf_cards: 2,
        },
        Form::Shrub => FormDefaults {
            length: 0.45,
            radius: 0.025,
            depth: 3,
            splits: 3,
            length_falloff: 0.6,
            radius_falloff: 0.55,
            branch_angle: 42.0,
            tropism: 0.05,
            bend: 14.0,
            leader_bias: 0.0,
            multi_stem: 4,
            leaf_size: 0.22,
            leaf_aspect: 1.0,
            leaf_cards: 2,
        },
        Form::Palm => FormDefaults {
            // Palms ignore depth/splits/falloff at emission time, but we
            // still surface sensible defaults so authors can tweak with
            // user-friendly numbers. `depth=0` makes the recursion a no-op
            // even when an author overrides `form` after the fact.
            length: 4.0,
            radius: 0.18,
            depth: 0,
            splits: 1,
            length_falloff: 0.95,
            radius_falloff: 0.85,
            branch_angle: 0.0,
            tropism: 0.0,
            bend: 6.0,
            leader_bias: 0.0,
            multi_stem: 1,
            leaf_size: 1.4,
            leaf_aspect: 0.18,
            leaf_cards: 8,
        },
    }
}

/// Per-tree configuration assembled once from the user's `branch` attrs and
/// then read by every recursive expansion. Keeping this in one struct stops
/// the recursion signature from growing into a parameter zoo.
struct BranchCfg {
    form: Form,
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
    leader_bias: f32,
    multi_stem: u32,
    leaves: bool,
    leaf_size: f32,
    leaf_aspect: f32,
    leaf_cards: u32,
    leaf_material: Option<MaterialId>,
}

/// Top-level expander for `branch "tree" (...)`. Creates a wrapper group at the
/// user's pos/rot and dispatches to the appropriate growth routine for the
/// declared `form`. Every node produced below the wrapper is marked
/// non-editable: the geometry is a deterministic function of the seed and
/// can't be hand-edited piecewise (rebuilding wipes any tweaks).
pub(super) fn expand_branch(
    node: &Node,
    parent: Option<NodeId>,
    graph: &mut SceneGraph,
) -> Result<NodeId> {
    let cfg = read_cfg(node, graph);

    let (wrapper_id, pre_expand_count) = begin_procedural(node, parent, graph)?;

    let mut rng: u32 = node.attr_number("seed").map(|n| n as u32).unwrap_or(1).max(1);

    match cfg.form {
        Form::Palm => emit_palm(wrapper_id, &cfg, &mut rng, graph),
        Form::Shrub => emit_shrub(wrapper_id, &cfg, &mut rng, graph),
        _ => {
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
        }
    }

    // The whole tree below the wrapper is procedurally derived — there's no
    // single AST span to write back to for any individual segment. The wrapper
    // itself stays editable so the user can tweak `branch` attrs.
    finish_procedural(graph, pre_expand_count);

    Ok(wrapper_id)
}

fn read_cfg(node: &Node, graph: &SceneGraph) -> BranchCfg {
    let form = match node.attr("form") {
        Some(Value::String(s)) | Some(Value::Ident(s)) => {
            read_form(s).unwrap_or(Form::Decurrent)
        }
        _ => Form::Decurrent,
    };
    let d = form_defaults(form);

    let length = cfg::scalar(node, "length", d.length, 1e-3);
    let radius = cfg::scalar(node, "radius", d.radius, 1e-4);
    let depth = cfg::count(node, "depth", d.depth as f32, 0.0);
    let splits = cfg::count(node, "splits", d.splits as f32, 1.0);
    let length_falloff = node.attr_number("length_falloff").unwrap_or(d.length_falloff);
    let radius_falloff = node.attr_number("radius_falloff").unwrap_or(d.radius_falloff);
    let branch_angle_rad = node
        .attr_number("branch_angle")
        .unwrap_or(d.branch_angle)
        .to_radians();
    // 137.5° (golden angle) gives the natural phyllotaxis spiral that keeps
    // successive forks from stacking on top of each other.
    let roll_rad = node.attr_number("roll").unwrap_or(137.5).to_radians();
    let tropism = node.attr_number("tropism").unwrap_or(d.tropism);
    let bend_rad = node.attr_number("bend").unwrap_or(d.bend).to_radians();
    let radial_segments = cfg::count(node, "segments", 8.0, 3.0);
    let samples_per_seg = cfg::count(node, "samples", 4.0, 1.0);
    let cps_per_seg = 4usize;
    let jitter = cfg::scalar_clamped(node, "jitter", 0.2, 0.0, 1.0);
    let leader_bias = cfg::scalar_clamped(node, "leader_bias", d.leader_bias, 0.0, 1.0);
    let multi_stem = cfg::count(node, "multi_stem", d.multi_stem as f32, 1.0);
    let leaves = node.attr_number("leaves").map(|n| n != 0.0).unwrap_or(true);
    let leaf_size = cfg::scalar(node, "leaf_size", d.leaf_size, 0.0);
    let leaf_aspect = cfg::scalar(node, "leaf_aspect", d.leaf_aspect, 0.05);
    let leaf_cards = cfg::count(node, "leaf_cards", d.leaf_cards as f32, 1.0);
    let leaf_material = match node.attr("leaf_mat") {
        Some(Value::String(s)) | Some(Value::Ident(s)) => {
            graph.find_material_scoped(s, node.origin.as_deref())
        }
        _ => None,
    };

    BranchCfg {
        form,
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
        leader_bias,
        multi_stem,
        leaves,
        leaf_size,
        leaf_aspect,
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
            // With leader_bias > 0 child 0 acts as a continuing leader: small
            // pitch off the parent tangent, near-full length, near-full
            // radius. That approximates a central-leader (pine-like) habit
            // without rewriting the recursion to track a dedicated trunk.
            let is_leader = cfg.leader_bias > 0.0 && i == 0;

            let pitch_jitter = rand_pm(rng) * cfg.jitter * cfg.branch_angle_rad * 0.5;
            let yaw_jitter = rand_pm(rng) * cfg.jitter * 30.0_f32.to_radians();
            let yaw = (i as f32) * (TAU / cfg.splits as f32) + accumulated_roll + yaw_jitter;
            let pitch = if is_leader {
                cfg.branch_angle_rad * (1.0 - cfg.leader_bias) + pitch_jitter * 0.2
            } else {
                cfg.branch_angle_rad + pitch_jitter
            };

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
            let next_length = if is_leader {
                // Lerp leader length from the (already-falloff'd) sibling
                // length up toward the parent length: bias=0 → falloff,
                // bias=1 → no shortening at all.
                let sibling_len = length * cfg.length_falloff;
                let leader_len = sibling_len + (length - sibling_len) * cfg.leader_bias;
                leader_len * length_jitter
            } else {
                length * cfg.length_falloff * length_jitter
            };
            let next_radius = if is_leader {
                let sibling_r = radius * cfg.radius_falloff;
                sibling_r + (radius - sibling_r) * cfg.leader_bias
            } else {
                // Side branches off a strong leader taper harder so they don't
                // visually compete with the trunk.
                let extra = if cfg.leader_bias > 0.0 { 0.7 } else { 1.0 };
                radius * cfg.radius_falloff * extra
            };

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
        // angles to the world. `leaf_card_mesh` is bottom-anchored (mesh
        // y=0..leaf_size), but the cutout texture pipeline produces a
        // foliage cluster centred in the frame with a transparent margin
        // around it — so we pull the leaf back by half its height along
        // the tangent, putting the texture's visible centre on the tip.
        // Without this, the cluster floats half a leaf_size past the end.
        let q = quat_from_y_to(tip_tangent);
        let leaf_origin = tip_pos - tip_tangent * cfg.leaf_size * 0.5;
        let leaf_id = graph.add_child(
            parent_id,
            "leaf",
            "leaf_card",
            Transform::from_trs(leaf_origin, q, Vec3::ONE),
        );
        let leaf_mesh = leaf_card_mesh(
            [cfg.leaf_size * cfg.leaf_aspect, cfg.leaf_size],
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

/// Shrub: emit `multi_stem` short trunks fanning out from the wrapper origin.
/// Each runs the standard recursive emission — only the base layout changes,
/// so the rest of the geometry pipeline is shared.
fn emit_shrub(parent_id: NodeId, cfg: &BranchCfg, rng: &mut u32, graph: &mut SceneGraph) {
    let stems = cfg.multi_stem.max(1);
    let outward_tilt = 18.0_f32.to_radians();
    for i in 0..stems {
        let yaw = (i as f32) / (stems as f32) * TAU
            + rand_pm(rng) * cfg.jitter * 0.5 * (TAU / stems as f32);
        let tilt = outward_tilt * (0.6 + 0.4 * (1.0 + rand_pm(rng)) * 0.5);
        let q = Quat::from_axis_angle(Vec3::Y, yaw) * Quat::from_axis_angle(Vec3::X, tilt);
        let stem_id = graph.add_child(
            parent_id,
            format!("stem_{i}"),
            "group",
            Transform::from_trs(Vec3::ZERO, q, Vec3::ONE),
        );
        let length_jitter = 1.0 + rand_pm(rng) * cfg.jitter * 0.3;
        emit_segment(
            stem_id,
            cfg.depth,
            cfg.length * length_jitter,
            cfg.radius,
            0.0,
            rng,
            cfg,
            graph,
        );
    }
}

/// Palm: a single straight tapered trunk with a fan of frond-shaped leaf
/// cards at the top. No recursive branching — the silhouette comes from the
/// rosette, not the geometry.
fn emit_palm(parent_id: NodeId, cfg: &BranchCfg, rng: &mut u32, graph: &mut SceneGraph) {
    let n_cps = cfg.cps_per_seg.max(2);
    let mut cps: Vec<[f32; 3]> = Vec::with_capacity(n_cps);
    let mut radii: Vec<f32> = Vec::with_capacity(n_cps);
    // Slight characteristic curve: most palm trunks lean a little, and the
    // bend reads better than a perfectly straight cylinder. Direction is
    // randomised via the rng so multiple palms in the same scene don't all
    // lean the same way.
    let lean_dir = if rand_pm(rng) >= 0.0 { 1.0 } else { -1.0 };
    let lean_amount = 0.06 * cfg.length * cfg.bend_rad.max(0.0).min(0.5);
    for i in 0..n_cps {
        let t = i as f32 / (n_cps - 1) as f32;
        let bend_x = lean_dir * lean_amount * t * t;
        cps.push([bend_x, cfg.length * t, 0.0]);
        radii.push(cfg.radius * (1.0 - t * (1.0 - cfg.radius_falloff)));
    }
    let tip_pos = Vec3::from_array(*cps.last().unwrap());
    let tip_tangent = (tip_pos - Vec3::from_array(cps[n_cps - 2])).normalize_or(Vec3::Y);

    let trunk_id = graph.add_child(parent_id, "trunk", "branch_seg", Transform::IDENTITY);
    let mesh = spline_tube_mesh(
        &cps,
        &radii,
        cfg.radial_segments,
        cfg.samples_per_seg,
        true,
        UvMode::Tile,
    );
    graph.set_mesh(trunk_id, mesh);
    inherit_material_from_ancestor(trunk_id, graph);

    if !cfg.leaves || cfg.leaf_size <= 0.0 {
        return;
    }

    // Frond rosette: spread `leaf_cards` (>= 4) evenly around the tip, each
    // pitched outward and slightly down so they arc away from the trunk like
    // a real palm crown. Each frond is a single tall card; `leaf_aspect` is
    // typically ~0.18 to read as a long narrow frond rather than a square
    // leaf cluster.
    let crown_align = quat_from_y_to(tip_tangent);
    let frond_count = cfg.leaf_cards.max(4);
    for i in 0..frond_count {
        let yaw = (i as f32) / (frond_count as f32) * TAU
            + rand_pm(rng) * cfg.jitter * 0.2;
        let pitch = (-30.0_f32).to_radians()
            + rand_pm(rng) * cfg.jitter * 12.0_f32.to_radians();
        let q = crown_align
            * Quat::from_axis_angle(Vec3::Y, yaw)
            * Quat::from_axis_angle(Vec3::X, pitch);
        let frond_origin = tip_pos
            - tip_tangent * cfg.leaf_size * cfg.leaf_aspect.min(1.0) * 0.25;
        let frond_id = graph.add_child(
            parent_id,
            format!("frond_{i}"),
            "leaf_card",
            Transform::from_trs(frond_origin, q, Vec3::ONE),
        );
        // 1 card per frond — the rosette itself is the multi-card construct.
        let frond_mesh = leaf_card_mesh(
            [cfg.leaf_size * cfg.leaf_aspect, cfg.leaf_size],
            1,
            UvMode::Fit,
        );
        graph.set_mesh(frond_id, frond_mesh);
        if let Some(mid) = cfg.leaf_material {
            graph.set_material(frond_id, mid);
        } else {
            inherit_material_from_ancestor(frond_id, graph);
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

