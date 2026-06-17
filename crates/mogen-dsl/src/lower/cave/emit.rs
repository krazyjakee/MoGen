//! Emit pass — turns a `CaveLayout` into SceneGraph geometry.
//!
//! The rock shell is meshed once: an additive bounding box minus the union of
//! every cavity carver (`box − ⋃ chambers/passages/entrances`), evaluated as a
//! smooth field and extracted with surface nets. The result is then roughened
//! with a bounded value-noise displacement for a natural stone finish.
//! Decorations are emitted separately by `decorate.rs`.

use anyhow::{bail, Result};
use glam::Vec3;

use mogen_core::{Mesh, NodeId, SceneGraph, Transform};
use mogen_geom::{blob_to_mesh, recompute_normals};

use crate::ast::Node;

use super::config::CaveCfg;
use super::generate::{rock_field, CaveLayout};
use super::materials::ROCK_MAT;
use super::rng::sub_seed;

pub(super) fn emit_rock(
    node: &Node,
    cfg: &CaveCfg,
    layout: &CaveLayout,
    parent: NodeId,
    graph: &mut SceneGraph,
) -> Result<()> {
    let children = rock_field(layout);

    // `lod_scale` trims the voxel grid to lower the polygon budget without
    // touching layout — a coarser grid meshes the same chambers with fewer
    // triangles. Floored well above the surface-nets minimum so a low-detail
    // bake still resolves every cavity.
    let res = ((cfg.resolution as f32 * cfg.lod_scale).round() as u32).clamp(24, 224);

    let mut mesh = blob_to_mesh(&children, cfg.blend, res);
    if mesh.indices.is_empty() {
        bail!(
            "cave produced no rock surface — the carvers may have hollowed the whole block. \
             Increase `size`/`margin`, lower `chamber_max`, or raise `resolution`."
        );
    }

    // Debug X-ray: drop the six outer bounding-box faces and keep only the
    // inner cavity walls, so the whole chamber/passage network is visible from
    // outside without flying the camera through the rock. The result is an open
    // (non-watertight) mesh — an inspection aid only.
    if cfg.debug_hide_shell {
        mesh = strip_outer_hull(&mesh, layout, cfg, res);
        if mesh.indices.is_empty() {
            bail!(
                "debug_hide_shell removed every face — no inner cavity walls were found. \
                 The carvers may not have opened up the interior; check `chambers`/`size`."
            );
        }
    }

    if cfg.roughness > 0.0 {
        roughen(&mut mesh, cfg.roughness, sub_seed(cfg.seed, 0x0CA7_FACE));
        mesh = recompute_normals(&mesh);
    }

    // Replace the surface-nets XZ planar UVs with a world-scale triplanar
    // mapping. The flat XZ projection smears the texture into vertical streaks
    // on near-vertical walls (constant XZ, varying Y); projecting each vertex
    // onto the plane its normal faces keeps texel density uniform on floors,
    // ceilings and walls alike. UVs tile in world space (the shared PBR sampler
    // is REPEAT and the maps are tileable), so the texture repeats at a fixed
    // real-world scale rather than stretching across the whole block.
    triplanar_uvs(&mut mesh, ROCK_UV_TILE);

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

/// Strip the outer bounding-box faces from the carved rock, keeping only the
/// interior cavity walls (`debug_hide_shell`). A triangle is part of the outer
/// hull when its centroid sits on one of the six block faces *and* its normal
/// points outward along that face's axis — the second test keeps interior walls
/// that merely sit near a face from being clipped. Chambers are placed `margin`
/// away from every face, so the near-plane tolerance stays well below `margin`.
fn strip_outer_hull(mesh: &Mesh, layout: &CaveLayout, cfg: &CaveCfg, res: u32) -> Mesh {
    let c = layout.block_center;
    let h = layout.block_half;
    let max_axis = (2.0 * h.x).max(2.0 * h.y).max(2.0 * h.z);
    let voxel = max_axis / (res.max(8) as f32 - 1.0);
    // Generous enough to catch blend-rounded edges, but strictly under `margin`
    // so an interior chamber wall is never mistaken for the hull.
    let eps = (cfg.blend + 2.0 * voxel).min(cfg.margin * 0.8).max(voxel);
    let comp = |v: Vec3, axis: usize| [v.x, v.y, v.z][axis];
    // (axis, plane coordinate, outward sign)
    let faces = [
        (0usize, c.x + h.x, 1.0f32),
        (0, c.x - h.x, -1.0),
        (1, c.y + h.y, 1.0),
        (1, c.y - h.y, -1.0),
        (2, c.z + h.z, 1.0),
        (2, c.z - h.z, -1.0),
    ];

    let mut keep: Vec<u32> = Vec::with_capacity(mesh.indices.len());
    for tri in mesh.indices.chunks_exact(3) {
        let (a, b, cc) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
        let p = |i: usize| Vec3::from(mesh.positions[i]);
        let centroid = (p(a) + p(b) + p(cc)) / 3.0;
        let nrm = (Vec3::from(mesh.normals[a])
            + Vec3::from(mesh.normals[b])
            + Vec3::from(mesh.normals[cc]))
        .normalize_or_zero();
        let on_hull = faces.iter().any(|&(axis, plane, sign)| {
            (comp(centroid, axis) - plane).abs() < eps && comp(nrm, axis) * sign > 0.4
        });
        if !on_hull {
            keep.extend_from_slice(tri);
        }
    }
    compact(mesh, &keep)
}

/// Build a new mesh containing only the triangles in `indices`, remapping to a
/// compact vertex range so the exporter sees no orphaned vertices.
fn compact(src: &Mesh, indices: &[u32]) -> Mesh {
    use std::collections::HashMap;
    debug_assert!(src.joints.is_empty(), "compact: joints would be lost");
    debug_assert!(src.weights.is_empty(), "compact: weights would be lost");
    debug_assert!(src.colors.is_empty(), "compact: colors would be lost");
    let mut remap: HashMap<u32, u32> = HashMap::new();
    let mut out = Mesh::default();
    let has_uv = !src.uvs.is_empty();
    out.indices = Vec::with_capacity(indices.len());
    for &i in indices {
        let ni = *remap.entry(i).or_insert_with(|| {
            let n = out.positions.len() as u32;
            out.positions.push(src.positions[i as usize]);
            out.normals.push(src.normals[i as usize]);
            if has_uv {
                out.uvs.push(src.uvs[i as usize]);
            }
            n
        });
        out.indices.push(ni);
    }
    out
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

/// World-space size (metres) of one texture tile on the rock. The texture
/// repeats every `ROCK_UV_TILE` metres along whichever plane each vertex
/// projects onto, giving consistent stone detail at any block size.
const ROCK_UV_TILE: f32 = 5.0;

/// Recompute the rock's UVs with a per-*triangle* triplanar projection: each
/// face projects all three of its vertices onto the world plane its geometric
/// normal most faces (XZ for floors/ceilings, XY or ZY for walls), scaled so
/// the texture tiles every `tile` metres.
///
/// The plane is chosen once per triangle (from the face normal) rather than
/// per vertex. A per-vertex choice lets a single triangle straddle two regimes
/// — one vertex on XZ, another on XY — and the exporter then interpolates two
/// incompatible projections across the face, shearing the texture into chevron
/// streaks along the wandering plane boundary. Picking per face keeps every
/// triangle's UVs internally consistent, so the only seams left are the hard
/// breaks where differently-projected faces meet (~45°), which the tileable
/// REPEAT maps hide.
///
/// Selecting per face means a vertex shared by faces on different planes needs
/// two different UVs, so the mesh is unwelded into independent triangles. The
/// smooth per-vertex normals are preserved (duplicated), so only the vertex
/// count grows — shading and watertight geometry are unchanged.
fn triplanar_uvs(mesh: &mut Mesh, tile: f32) {
    let inv = 1.0 / tile.max(1e-3);
    let n_tris = mesh.indices.len() / 3;
    let mut positions = Vec::with_capacity(n_tris * 3);
    let mut normals = Vec::with_capacity(n_tris * 3);
    let mut uvs = Vec::with_capacity(n_tris * 3);
    let mut colors: Vec<[f32; 4]> = if mesh.colors.is_empty() {
        Vec::new()
    } else {
        Vec::with_capacity(n_tris * 3)
    };
    let mut indices = Vec::with_capacity(n_tris * 3);

    // Cave rock is always static and never receives a gradient bake, so the
    // per-vertex ancillary buffers should be empty.  Assert here so a future
    // caller that populates them finds out before the unweld silently drops
    // the data.
    debug_assert!(mesh.joints.is_empty(), "triplanar_uvs: joints would be lost by unweld");
    debug_assert!(mesh.weights.is_empty(), "triplanar_uvs: weights would be lost by unweld");

    for tri in mesh.indices.chunks_exact(3) {
        let [i0, i1, i2] = [tri[0] as usize, tri[1] as usize, tri[2] as usize];
        let (p0, p1, p2) = (
            Vec3::from(mesh.positions[i0]),
            Vec3::from(mesh.positions[i1]),
            Vec3::from(mesh.positions[i2]),
        );
        // Geometric (face) normal — robust against the per-vertex smooth
        // normals disagreeing across the triangle. Degenerate tris fall back
        // to XZ; their zero area makes the choice invisible anyway.
        let face_normal = (p1 - p0).cross(p2 - p0).normalize_or_zero();
        let (ax, ay, az) = (face_normal.x.abs(), face_normal.y.abs(), face_normal.z.abs());
        let project = |p: Vec3| -> [f32; 2] {
            let (u, v) = if ay >= ax && ay >= az {
                (p.x, p.z) // floor / ceiling
            } else if ax >= az {
                (p.z, p.y) // wall facing ±X
            } else {
                (p.x, p.y) // wall facing ±Z
            };
            [u * inv, v * inv]
        };

        let base = positions.len() as u32;
        for &i in &[i0, i1, i2] {
            positions.push(mesh.positions[i]);
            normals.push(mesh.normals[i]);
            if !colors.is_empty() {
                colors.push(mesh.colors[i]);
            }
        }
        uvs.push(project(p0));
        uvs.push(project(p1));
        uvs.push(project(p2));
        indices.extend_from_slice(&[base, base + 1, base + 2]);
    }

    mesh.positions = positions;
    mesh.normals = normals;
    mesh.uvs = uvs;
    mesh.colors = colors;
    mesh.indices = indices;
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
