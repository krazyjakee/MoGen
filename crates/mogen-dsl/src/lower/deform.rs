//! Apply user-requested deformation modifiers to a primitive's mesh. Reads
//! `bend_x`/`bend_y`/`bend_z`/`twist_y`/`taper`/`droop`/`noise`/`jitter`/
//! `faceted`/`seed`/`style` from a `Node`, expands any `style="..."` preset to
//! fill in missing modifiers, then runs the geom kernel pipeline.
//!
//! Lives between `primitive_mesh` and `apply_anchor_to_mesh` in the lowering
//! path (see `lower/node.rs` and `lower/csg.rs`). Idempotent for nodes that
//! carry no deformation attrs.

use mogen_core::Mesh;
use mogen_geom::{bend, droop, jitter, noise, split_for_facets, taper, twist_y};
use mogen_geom::{recompute_normals, weld_vertices};

use crate::ast::{Node, Value};

#[derive(Default, Debug, Clone)]
struct Modifiers {
    seed: Option<u32>,
    noise: Option<f32>,
    jitter: Option<f32>,
    bend_x: Option<f32>,
    bend_y: Option<f32>,
    bend_z: Option<f32>,
    twist_y: Option<f32>,
    taper: Option<f32>,
    droop: Option<f32>,
    faceted: bool,
}

impl Modifiers {
    fn has_geometry_op(&self) -> bool {
        self.noise.is_some()
            || self.jitter.is_some()
            || self.bend_x.is_some()
            || self.bend_y.is_some()
            || self.bend_z.is_some()
            || self.twist_y.is_some()
            || self.taper.is_some()
            || self.droop.is_some()
    }
}

pub(super) fn apply_deform(mesh: &mut Mesh, node: &Node) {
    // Skinned meshes get their joints/weights filled in by the post-pass
    // `bind_meshes`, so at this point in lowering primitives never carry
    // skin data — but be paranoid and skip deformation if they ever do, so
    // that a future change to the lowering order doesn't silently strip
    // weights via `recompute_normals`/`weld_vertices`.
    if mesh.is_skinned() {
        return;
    }

    let mods = collect_modifiers(node);
    if !mods.has_geometry_op() && !mods.faceted {
        return;
    }

    // 1. Radial scale first: cheap, no topology change, gives later
    //    deformations a clean shape to work on.
    if let Some(r) = mods.taper {
        taper(mesh, r.max(0.0));
    }
    // 2. Axis-coupled deformations. Order is bend → twist → droop; these
    //    don't commute with each other, but in practice users rarely combine
    //    more than one and the chosen order matches author intuition
    //    (bend the beam, then twist it, then let it sag).
    if let Some(deg) = mods.bend_x {
        bend(mesh, 0, 1, deg.to_radians());
    }
    if let Some(deg) = mods.bend_y {
        // Rotation axis Y; length axis X (the natural "horizontal extent").
        bend(mesh, 1, 0, deg.to_radians());
    }
    if let Some(deg) = mods.bend_z {
        bend(mesh, 2, 1, deg.to_radians());
    }
    if let Some(deg) = mods.twist_y {
        twist_y(mesh, deg.to_radians());
    }
    if let Some(amount) = mods.droop {
        droop(mesh, amount);
    }

    // 3. High-frequency displacement runs last so it perturbs already-bent
    //    geometry. Jitter uses a derived seed so noise+jitter don't share a
    //    random stream — otherwise the two passes would correlate and look
    //    like "noise twice as strong" rather than two independent textures.
    let seed = mods.seed.unwrap_or(1);
    if let Some(amount) = mods.noise {
        noise(mesh, amount, seed);
    }
    if let Some(amount) = mods.jitter {
        jitter(mesh, amount, seed.wrapping_add(0x9E37_79B9));
    }

    // 4. Cleanup: recompute normals once; weld only if stochastic kernels
    //    ran (they can split seams when neighbouring verts displace
    //    differently across a UV cut).
    if mods.has_geometry_op() {
        *mesh = recompute_normals(mesh);
        if mods.noise.is_some() || mods.jitter.is_some() {
            *mesh = weld_vertices(mesh, 1e-4);
        }
    }

    // 5. Faceted shading is the last step so smooth normals from step 4 are
    //    discarded in favour of per-triangle face normals.
    if mods.faceted {
        *mesh = split_for_facets(mesh);
    }
}

fn collect_modifiers(node: &Node) -> Modifiers {
    let mut m = Modifiers::default();
    for (k, v) in &node.attrs {
        if let Value::Number(n) = v {
            set_modifier(&mut m, k, *n);
        }
    }
    m
}

fn set_modifier(m: &mut Modifiers, name: &str, value: f32) {
    match name {
        "seed" => m.seed = Some(value as u32),
        "noise" => m.noise = Some(value.clamp(0.0, 1.0)),
        "jitter" => m.jitter = Some(value.clamp(0.0, 1.0)),
        "bend_x" => m.bend_x = Some(value),
        "bend_y" => m.bend_y = Some(value),
        "bend_z" => m.bend_z = Some(value),
        "twist_y" => m.twist_y = Some(value),
        "taper" => m.taper = Some(value),
        "droop" => m.droop = Some(value),
        "faceted" => m.faceted = value != 0.0,
        _ => {}
    }
}
