//! Emit pass — turns a `CaveLayout` into SceneGraph geometry.
//!
//! The rock shell is meshed once: an additive bounding box minus the union of
//! every cavity carver (`box − ⋃ chambers/passages/entrances`), evaluated as a
//! smooth field and extracted with surface nets. The result is then roughened
//! with a bounded value-noise displacement for a natural stone finish.
//! Decorations are emitted separately by `decorate.rs`.

use anyhow::{bail, Result};
use glam::Mat4;

use mogen_core::{Mesh, NodeId, SceneGraph, Transform};
use mogen_geom::{blob_to_mesh, recompute_normals, BlobChild, SdfOp, SdfPrim};

use crate::ast::Node;

use super::config::CaveCfg;
use super::generate::CaveLayout;
use super::materials::ROCK_MAT;
use super::rng::sub_seed;

pub(super) fn emit_rock(
    node: &Node,
    cfg: &CaveCfg,
    layout: &CaveLayout,
    parent: NodeId,
    graph: &mut SceneGraph,
) -> Result<()> {
    let mut children: Vec<BlobChild> = Vec::with_capacity(layout.carvers.len() + 1);
    children.push(BlobChild::new(
        SdfPrim::Box {
            half: layout.block_half,
        },
        SdfOp::Add,
        Mat4::from_translation(layout.block_center),
    ));
    children.extend(layout.carvers.iter().cloned());

    let mut mesh = blob_to_mesh(&children, cfg.blend, cfg.resolution);
    if mesh.indices.is_empty() {
        bail!(
            "cave produced no rock surface — the carvers may have hollowed the whole block. \
             Increase `size`/`margin`, lower `chamber_max`, or raise `resolution`."
        );
    }

    if cfg.roughness > 0.0 {
        roughen(&mut mesh, cfg.roughness, sub_seed(cfg.seed, 0x0CA7_FACE));
        mesh = recompute_normals(&mesh);
    }

    let id = graph.add_child(parent, "rock".to_string(), "mesh", Transform::IDENTITY);
    graph.set_mesh(id, mesh);
    graph.nodes[id.0 as usize].origin = node.origin.clone();
    graph.nodes[id.0 as usize].role = Some("cave_rock".to_string());
    graph.nodes[id.0 as usize]
        .tags
        .extend(["cave".to_string(), "rock".to_string()]);
    bind_rock_material(id, node.origin.as_deref(), graph);
    Ok(())
}

/// Bind the rock mesh's material: a `mat=` the user put on the `cave` node
/// (inherited via the wrapper chain) wins; otherwise the `cave_rock` default.
fn bind_rock_material(id: NodeId, origin: Option<&std::path::Path>, graph: &mut SceneGraph) {
    let mut cur = graph.nodes[id.0 as usize].parent;
    while let Some(p) = cur {
        if let Some(m) = graph.nodes[p.0 as usize].material {
            graph.set_material(id, m);
            return;
        }
        cur = graph.nodes[p.0 as usize].parent;
    }
    if let Some(mid) = graph.find_material_scoped(ROCK_MAT, origin) {
        graph.set_material(id, mid);
    }
}

/// Displace every vertex along its normal by low-frequency value noise. The
/// magnitude is an absolute cap (≤ ~0.35 m) rather than the AABB-relative scale
/// `deform::noise` uses — a 12 m block would otherwise displace by metres and
/// shred the mesh. Low frequency keeps walls undulating instead of spiking, so
/// the surface stays watertight.
fn roughen(mesh: &mut Mesh, amount: f32, seed: u32) {
    let mag = amount.clamp(0.0, 1.0) * 0.35;
    if mag <= 0.0 {
        return;
    }
    const FREQ: f32 = 0.55; // cycles per metre
    for (p, n) in mesh.positions.iter_mut().zip(mesh.normals.iter()) {
        let h = value_noise(p[0] * FREQ, p[1] * FREQ, p[2] * FREQ, seed);
        let d = (h - 0.5) * 2.0 * mag;
        p[0] += n[0] * d;
        p[1] += n[1] * d;
        p[2] += n[2] * d;
    }
}

fn hash3(x: i32, y: i32, z: i32, seed: u32) -> f32 {
    let mut h = (x as u32)
        .wrapping_mul(0x8DA6_B343)
        ^ (y as u32).wrapping_mul(0xD816_3841)
        ^ (z as u32).wrapping_mul(0xCB1A_B31F)
        ^ seed.wrapping_mul(0x9E37_79B9);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    (h & 0xFFFF) as f32 / 65535.0
}

/// Trilinearly-interpolated value noise in [0, 1].
fn value_noise(x: f32, y: f32, z: f32, seed: u32) -> f32 {
    let xi = x.floor();
    let yi = y.floor();
    let zi = z.floor();
    let (fx, fy, fz) = (x - xi, y - yi, z - zi);
    // Smoothstep fade.
    let sx = fx * fx * (3.0 - 2.0 * fx);
    let sy = fy * fy * (3.0 - 2.0 * fy);
    let sz = fz * fz * (3.0 - 2.0 * fz);
    let (xi, yi, zi) = (xi as i32, yi as i32, zi as i32);
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let c000 = hash3(xi, yi, zi, seed);
    let c100 = hash3(xi + 1, yi, zi, seed);
    let c010 = hash3(xi, yi + 1, zi, seed);
    let c110 = hash3(xi + 1, yi + 1, zi, seed);
    let c001 = hash3(xi, yi, zi + 1, seed);
    let c101 = hash3(xi + 1, yi, zi + 1, seed);
    let c011 = hash3(xi, yi + 1, zi + 1, seed);
    let c111 = hash3(xi + 1, yi + 1, zi + 1, seed);
    let x00 = lerp(c000, c100, sx);
    let x10 = lerp(c010, c110, sx);
    let x01 = lerp(c001, c101, sx);
    let x11 = lerp(c011, c111, sx);
    let y0 = lerp(x00, x10, sy);
    let y1 = lerp(x01, x11, sy);
    lerp(y0, y1, sz)
}
