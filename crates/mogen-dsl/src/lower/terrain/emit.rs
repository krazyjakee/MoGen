//! Emit pass — turns a `HeightField` into SceneGraph geometry.
//!
//! The patch is split into a `chunks × chunks` grid, and each chunk emits one
//! mesh node per LOD level (`cfg.lod_levels`): level 0 is the full-resolution
//! surface, each higher level doubles the sampling stride (≈¼ the triangles)
//! and carries a greater camera-distance band in `node.lod`. A viewer or engine
//! shows the one variant whose band contains the current distance, so distant
//! chunks cost a fraction of the triangles without popping out of view.
//!
//! Adjacent chunks sample their shared boundary grid line at identical heights,
//! so same-LOD seams are crack-free by construction. Across *different* LODs the
//! coarser edge can diverge from the finer one, so every chunk mesh is ringed by
//! a downward **skirt** as deep as the chunk's own relief — it hides any gap a
//! neighbouring LOD could open without leaving a hole in the surface.
//!
//! Surface normals are **analytic** — computed per grid sample from central
//! differences on the shared height field, not recomputed per chunk. A vertex at
//! grid `(i, j)` therefore gets the *same* normal no matter which chunk or LOD
//! emits it, so adjacent chunks shade identically across their shared edge with
//! no faceted boundary crease (a per-chunk `recompute_normals` would see only its
//! own triangles and leave a visible seam at every chunk line).
//!
//! A terrain surface is an open heightfield (not a closed solid), so the
//! watertight-by-construction rule applies to these seams rather than to a
//! sealed volume.

use glam::Vec3;

use mogen_core::{ColliderShape, Lod, Mesh, NodeId, SceneGraph, Transform};
use mogen_geom::recompute_normals;

use crate::ast::Node;

use super::config::{ColliderMode, TerrainCfg};
use super::field::HeightField;
use super::materials::{GROUND_MAT, WATER_MAT};

/// World-space size (metres) of one texture tile on the terrain.
const TERRAIN_UV_TILE: f32 = 4.0;

/// Camera distance (in chunk-widths) at which each LOD hands off to the next,
/// coarser one. With the default 3 levels and a chunk a few metres across, the
/// full-detail mesh covers the near field and coarser meshes take over as the
/// chunk recedes.
const LOD_STEP_CHUNKS: f32 = 3.0;

// --- surface colour bake ---------------------------------------------------
//
// Per-vertex COLOR_0 carries the terrain's grass/rock/sand/mud blend so the
// look survives as plain glTF (no engine shader needed). The ground material is
// white, so these are the colours that actually render; an engine can still add
// a runtime slope/height shader on top. Tones are linear-ish sRGB.

/// Grassy soil on gentle, dry ground (the old flat ground colour).
const COL_GRASS: [f32; 3] = [0.34, 0.42, 0.24];
/// Bare rock on steep slopes.
const COL_ROCK: [f32; 3] = [0.42, 0.40, 0.37];
/// Sand at the water's edge.
const COL_SAND: [f32; 3] = [0.76, 0.70, 0.50];
/// Dark mud on the submerged floor.
const COL_MUD: [f32; 3] = [0.28, 0.22, 0.16];

/// Width of the sandy shore band, as a fraction of the patch height `amp`. Land
/// within this height of the waterline reads as sand; below it fades to mud.
const SHORE_BAND_FRAC: f32 = 0.05;

#[inline]
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Blend the surface colour for one vertex from its slope (`normal_y`: 1 flat,
/// 0 vertical) and world height `y` relative to the `waterline`. With no sea
/// (`has_water == false`) only the grass→rock slope blend applies.
fn surface_color(y: f32, normal_y: f32, waterline: f32, amp: f32, has_water: bool) -> [f32; 4] {
    let grass = Vec3::from(COL_GRASS);
    let rock = Vec3::from(COL_ROCK);

    // Slope: rock fades in across roughly 25°–55° of tilt.
    let steep = (1.0 - normal_y).clamp(0.0, 1.0);
    let rock_t = smoothstep(0.18, 0.55, steep);
    let mut col = grass.lerp(rock, rock_t);

    if has_water {
        let band = (SHORE_BAND_FRAC * amp).max(1e-4);
        let dh = y - waterline; // >0 above water, <0 submerged
        // Sand hugs the waterline (strongest at dh=0), but cliffs stay rock.
        let sand_t = (1.0 - (dh.abs() / band).clamp(0.0, 1.0)) * (1.0 - rock_t);
        col = col.lerp(Vec3::from(COL_SAND), sand_t.clamp(0.0, 1.0));
        // Below the sand band, the floor turns to mud.
        if dh < -band {
            let mud_t = smoothstep(0.0, band, -dh - band) * (1.0 - rock_t);
            col = col.lerp(Vec3::from(COL_MUD), mud_t.clamp(0.0, 1.0));
        }
    }

    [col.x, col.y, col.z, 1.0]
}

pub(super) fn emit_chunks(
    node: &Node,
    cfg: &TerrainCfg,
    field: &HeightField,
    parent: NodeId,
    graph: &mut SceneGraph,
) {
    let segments = field.n - 1;
    let chunks = cfg.chunks.max(1) as usize;
    let spc = segments / chunks; // segments per chunk (build() guarantees it divides)
    let levels = cfg.lod_levels.clamp(1, 4) as usize;

    let w = cfg.size[0];
    let d = cfg.size[2];
    let amp = cfg.size[1];
    let half_w = w * 0.5;
    let half_d = d * 0.5;

    let world_x = |i: usize| -half_w + (i as f32 / segments as f32) * w;
    let world_z = |j: usize| -half_d + (j as f32 / segments as f32) * d;

    // World spacing between adjacent fine-grid samples, for analytic normals.
    let dx = w / segments as f32;
    let dz = d / segments as f32;

    // Camera-distance scale for the LOD bands: one chunk's largest XZ extent.
    let chunk_extent = (w.max(d) / chunks as f32).max(0.001);

    // Waterline in world Y for the surface-colour bake (sand/mud blend).
    let has_water = cfg.sea_level > 0.0;
    let waterline = cfg.sea_level * amp;

    for cj in 0..chunks {
        for ci in 0..chunks {
            let i0 = ci * spc;
            let j0 = cj * spc;
            // Chunk centre in world XZ — the node transform — so each chunk's
            // local geometry stays small and its bounding sphere tight. Shared
            // across this chunk's LOD variants so they overlap in world space.
            let cx = 0.5 * (world_x(i0) + world_x(i0 + spc));
            let cz = 0.5 * (world_z(j0) + world_z(j0 + spc));

            // Skirt depth = the largest gap a coarser neighbour LOD can actually
            // open along this chunk's shared edges — far shallower than the full
            // relief, so the skirt hides under the surface instead of showing as
            // a wall at every LOD boundary.
            let skirt_depth = skirt_depth_for_chunk(field, i0, j0, spc, levels, amp);

            for level in 0..levels {
                let stride = 1usize << level;
                let mesh = build_chunk_lod_mesh(
                    field,
                    i0,
                    j0,
                    spc,
                    stride,
                    amp,
                    dx,
                    dz,
                    &world_x,
                    &world_z,
                    cx,
                    cz,
                    skirt_depth,
                    waterline,
                    has_water,
                );

                let (min_distance, max_distance) =
                    lod_band(level, levels, chunk_extent);

                let id = graph.add_child(
                    parent,
                    format!("chunk_{ci}_{cj}_lod{level}"),
                    "mesh",
                    Transform::from_translation(Vec3::new(cx, 0.0, cz)),
                );
                graph.set_mesh(id, mesh);
                let n = &mut graph.nodes[id.0 as usize];
                n.origin = node.origin.clone();
                n.role = Some("terrain".to_string());
                n.tags
                    .extend(["terrain".to_string(), "chunk".to_string()]);
                n.lod = Some(Lod {
                    level: level as u32,
                    min_distance,
                    max_distance,
                });
                // The collider is the physics surface, so it lives on the
                // full-detail variant only — coarser LODs are visual stand-ins
                // and would otherwise stack overlapping trimesh colliders.
                if level == 0 && cfg.colliders == ColliderMode::All {
                    n.collider = Some(ColliderShape::Trimesh);
                }
                bind_material(id, GROUND_MAT, node.origin.as_deref(), graph);
            }
        }
    }

    if cfg.sea_level > 0.0 {
        emit_water(node, cfg, parent, graph);
    }
}

/// Camera-distance band `[min, max)` for LOD `level` of `levels` total. Bands
/// partition `[0, ∞)`: level 0 starts at the camera, each step out is
/// `LOD_STEP_CHUNKS` chunk-widths further, and the coarsest level runs to ∞.
fn lod_band(level: usize, levels: usize, chunk_extent: f32) -> (f32, f32) {
    let step = chunk_extent * LOD_STEP_CHUNKS;
    let min = if level == 0 {
        0.0
    } else {
        step * level as f32
    };
    let max = if level + 1 >= levels {
        f32::INFINITY
    } else {
        step * (level + 1) as f32
    };
    (min, max)
}

/// Build one chunk's surface at a given sampling `stride` (1 = full detail,
/// 2 = half, …), ringed by a downward skirt of depth `skirt_depth`. Positions
/// are local to the chunk centre `(cx, cz)`; UVs tile in world space so the
/// ground texture is continuous across chunks and LODs.
#[allow(clippy::too_many_arguments)]
fn build_chunk_lod_mesh(
    field: &HeightField,
    i0: usize,
    j0: usize,
    spc: usize,
    stride: usize,
    amp: f32,
    dx: f32,
    dz: f32,
    world_x: &impl Fn(usize) -> f32,
    world_z: &impl Fn(usize) -> f32,
    cx: f32,
    cz: f32,
    skirt_depth: f32,
    waterline: f32,
    has_water: bool,
) -> Mesh {
    let step = spc / stride; // cells per side at this LOD
    let nv = step + 1; // vertices per side

    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(nv * nv);
    let mut uvs: Vec<[f32; 2]> = Vec::with_capacity(nv * nv);
    let mut normals: Vec<[f32; 3]> = Vec::with_capacity(nv * nv);
    for lj in 0..nv {
        let j = j0 + lj * stride;
        let wz = world_z(j);
        for li in 0..nv {
            let i = i0 + li * stride;
            let wx = world_x(i);
            let y = field.at(i, j) * amp;
            positions.push([wx - cx, y, wz - cz]);
            uvs.push([wx / TERRAIN_UV_TILE, wz / TERRAIN_UV_TILE]);
            // Analytic normal from the fine field — identical for this (i, j)
            // in every chunk and LOD, so shared edges shade seamlessly.
            normals.push(field_normal(field, i, j, amp, dx, dz));
        }
    }

    let mut indices: Vec<u32> = Vec::with_capacity(step * step * 6 + step * 4 * 6);
    for lj in 0..step {
        for li in 0..step {
            let a = (lj * nv + li) as u32;
            let b = a + 1;
            let c = a + nv as u32;
            let dd = c + 1;
            // CCW from +Y, matching the `heightfield` primitive.
            indices.push(a);
            indices.push(c);
            indices.push(dd);
            indices.push(a);
            indices.push(dd);
            indices.push(b);
        }
    }

    if skirt_depth > 0.0 {
        // Walk the perimeter as four ordered edge runs whose vertex order makes
        // each skirt wall face outward (geometric normal away from the chunk),
        // so the default back-face cull keeps them visible from outside.
        let sample = |i: usize, j: usize| -> (f32, f32, f32) {
            (world_x(i), world_z(j), field.at(i, j) * amp)
        };
        let mut edge = |pts: Vec<(usize, usize)>| {
            let run: Vec<(f32, f32, f32)> =
                pts.into_iter().map(|(i, j)| sample(i, j)).collect();
            append_skirt(
                &mut positions,
                &mut uvs,
                &mut normals,
                &mut indices,
                &run,
                cx,
                cz,
                skirt_depth,
            );
        };
        let s = stride;
        // South (z min): +X tangent.
        edge((0..=step).map(|li| (i0 + li * s, j0)).collect());
        // East (x max): +Z tangent.
        edge((0..=step).map(|lj| (i0 + step * s, j0 + lj * s)).collect());
        // North (z max): -X tangent.
        edge((0..=step).rev().map(|li| (i0 + li * s, j0 + step * s)).collect());
        // West (x min): -Z tangent.
        edge((0..=step).rev().map(|lj| (i0, j0 + lj * s)).collect());
    }

    // Bake the grass/rock/sand/mud blend into COLOR_0 using the analytic normals
    // (slope) and each vertex's world height. Positions are local to the chunk
    // centre but Y is already world-space, so it pairs with waterline.
    let colors = positions
        .iter()
        .zip(&normals)
        .map(|(p, nrm)| surface_color(p[1], nrm[1], waterline, amp, has_water))
        .collect();

    Mesh {
        positions,
        uvs,
        normals,
        indices,
        colors,
        ..Default::default()
    }
}

/// Tight skirt depth for a chunk: the largest vertical gap any coarser LOD of
/// this chunk's boundary can open against the full-detail edge. Adjacent chunks
/// share their boundary heights, so a chunk's own edge deviation under coarse
/// sampling bounds every cross-LOD seam at that edge exactly. Returns `0` when
/// no coarser LOD exists (`levels == 1`) — same-LOD edges are already crack-free.
fn skirt_depth_for_chunk(
    field: &HeightField,
    i0: usize,
    j0: usize,
    spc: usize,
    levels: usize,
    amp: f32,
) -> f32 {
    // The four boundary edges as fine height runs (length spc + 1).
    let south: Vec<f32> = (0..=spc).map(|k| field.at(i0 + k, j0)).collect();
    let north: Vec<f32> = (0..=spc).map(|k| field.at(i0 + k, j0 + spc)).collect();
    let west: Vec<f32> = (0..=spc).map(|k| field.at(i0, j0 + k)).collect();
    let east: Vec<f32> = (0..=spc).map(|k| field.at(i0 + spc, j0 + k)).collect();
    let edges = [&south, &north, &west, &east];

    let mut max_dev = 0.0f32;
    // Each coarser LOD samples the edge at stride 2^level and lerps across the
    // skipped points; the gap is the deviation of the fine point from that lerp.
    for level in 1..levels {
        let stride = 1usize << level;
        for run in edges {
            for k in 0..=spc {
                let k0 = (k / stride) * stride;
                let k1 = (k0 + stride).min(spc);
                let lerp = if k1 == k0 {
                    run[k0]
                } else {
                    let t = (k - k0) as f32 / (k1 - k0) as f32;
                    run[k0] * (1.0 - t) + run[k1] * t
                };
                max_dev = max_dev.max((run[k] - lerp).abs());
            }
        }
    }
    if max_dev <= 0.0 {
        0.0
    } else {
        // A hair extra so floating-point rounding never reopens the seam.
        max_dev * amp + 0.01 * amp
    }
}

/// Analytic surface normal at fine-grid sample `(i, j)` via central differences
/// on the height field. Edges clamp to the border (one-sided difference). The
/// result depends only on `(i, j)`, so the same grid point yields the same
/// normal in every chunk and at every LOD — the key to seamless shading.
fn field_normal(field: &HeightField, i: usize, j: usize, amp: f32, dx: f32, dz: f32) -> [f32; 3] {
    let n = field.n;
    let xm = i.saturating_sub(1);
    let xp = (i + 1).min(n - 1);
    let zm = j.saturating_sub(1);
    let zp = (j + 1).min(n - 1);
    let dhx = (field.at(xp, j) - field.at(xm, j)) * amp;
    let dhz = (field.at(i, zp) - field.at(i, zm)) * amp;
    let spanx = ((xp - xm) as f32 * dx).max(1e-6);
    let spanz = ((zp - zm) as f32 * dz).max(1e-6);
    let v = Vec3::new(-dhx / spanx, 1.0, -dhz / spanz).normalize();
    [v.x, v.y, v.z]
}

/// Append a vertical skirt strip below an ordered run of surface points. Each
/// wall uses its own duplicated vertices (flat-shaded, no normals shared with
/// the surface rim) so the top surface keeps clean up-facing normals. The point
/// order is chosen by the caller so the emitted triangles face outward.
fn append_skirt(
    positions: &mut Vec<[f32; 3]>,
    uvs: &mut Vec<[f32; 2]>,
    normals: &mut Vec<[f32; 3]>,
    indices: &mut Vec<u32>,
    run: &[(f32, f32, f32)],
    cx: f32,
    cz: f32,
    depth: f32,
) {
    for pair in run.windows(2) {
        let (x0, z0, y0) = pair[0];
        let (x1, z1, y1) = pair[1];
        let base = positions.len() as u32;
        // T0, T1 (top), then D1, D0 (dropped). Triangles (T0,T1,D1),(T0,D1,D0).
        let t0 = Vec3::new(x0 - cx, y0, z0 - cz);
        let t1 = Vec3::new(x1 - cx, y1, z1 - cz);
        let d1 = Vec3::new(x1 - cx, y1 - depth, z1 - cz);
        let d0 = Vec3::new(x0 - cx, y0 - depth, z0 - cz);
        positions.push(t0.into());
        positions.push(t1.into());
        positions.push(d1.into());
        positions.push(d0.into());
        uvs.push([x0 / TERRAIN_UV_TILE, z0 / TERRAIN_UV_TILE]);
        uvs.push([x1 / TERRAIN_UV_TILE, z1 / TERRAIN_UV_TILE]);
        uvs.push([x1 / TERRAIN_UV_TILE, z1 / TERRAIN_UV_TILE]);
        uvs.push([x0 / TERRAIN_UV_TILE, z0 / TERRAIN_UV_TILE]);
        // Flat outward normal for the whole wall (the vertex order makes the
        // first triangle's geometric normal point away from the chunk).
        let nrm = (t1 - t0).cross(d1 - t0).normalize_or_zero();
        let nrm = [nrm.x, nrm.y, nrm.z];
        normals.extend([nrm, nrm, nrm, nrm]);
        indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
}

/// A single flat water quad spanning the whole patch at the sea-level height.
/// Decorative (no collider) so a player can wade; the terrain beneath keeps its
/// real shape and simply dips below this plane.
fn emit_water(node: &Node, cfg: &TerrainCfg, parent: NodeId, graph: &mut SceneGraph) {
    let w = cfg.size[0];
    let d = cfg.size[2];
    let y = cfg.sea_level * cfg.size[1];
    let half_w = w * 0.5;
    let half_d = d * 0.5;

    let positions = vec![
        [-half_w, y, -half_d],
        [half_w, y, -half_d],
        [-half_w, y, half_d],
        [half_w, y, half_d],
    ];
    let uvs = vec![
        [-half_w / TERRAIN_UV_TILE, -half_d / TERRAIN_UV_TILE],
        [half_w / TERRAIN_UV_TILE, -half_d / TERRAIN_UV_TILE],
        [-half_w / TERRAIN_UV_TILE, half_d / TERRAIN_UV_TILE],
        [half_w / TERRAIN_UV_TILE, half_d / TERRAIN_UV_TILE],
    ];
    // CCW from +Y.
    let indices = vec![0, 2, 3, 0, 3, 1];
    let mesh = recompute_normals(&Mesh {
        positions,
        uvs,
        indices,
        ..Default::default()
    });

    let id = graph.add_child(parent, "water".to_string(), "mesh", Transform::IDENTITY);
    graph.set_mesh(id, mesh);
    graph.nodes[id.0 as usize].origin = node.origin.clone();
    graph.nodes[id.0 as usize].role = Some("water".to_string());
    graph.nodes[id.0 as usize]
        .tags
        .extend(["terrain".to_string(), "water".to_string()]);
    // Water never inherits the wrapper's ground `mat=` — it always uses the
    // water finish.
    if let Some(mid) = graph.find_material_scoped(WATER_MAT, node.origin.as_deref()) {
        graph.set_material(id, mid);
    }
}

/// Bind a node to the nearest ancestor `mat=` if one exists (so the user can
/// theme the whole terrain with `mat=` on the wrapper), else fall back to the
/// named default material on this origin. Mirrors `cave::emit::bind_*`.
fn bind_material(
    id: NodeId,
    default_name: &str,
    origin: Option<&std::path::Path>,
    graph: &mut SceneGraph,
) {
    let mut cur = graph.nodes[id.0 as usize].parent;
    while let Some(p) = cur {
        if let Some(m) = graph.nodes[p.0 as usize].material {
            graph.set_material(id, m);
            return;
        }
        cur = graph.nodes[p.0 as usize].parent;
    }
    if let Some(mid) = graph.find_material_scoped(default_name, origin) {
        graph.set_material(id, mid);
    }
}

#[cfg(test)]
mod tests {
    use super::super::config::{ColliderMode, SourceKind, TerrainCfg};
    use super::super::field;
    use super::*;

    fn cfg() -> TerrainCfg {
        TerrainCfg {
            seed: 7,
            mat_style: String::new(),
            size: [60.0, 12.0, 60.0],
            source: SourceKind::Fbm,
            octaves: 5,
            frequency: 0.05,
            persistence: 0.5,
            resolution: 96,
            chunks: 4,
            lod_levels: 1,
            smooth: 1,
            terrace: 0,
            sea_level: 0.0,
            colliders: ColliderMode::None,
            peaks: 0,
            flat_spots: 0,
            shore_points: 0,
            lod_scale: 1.0,
            debug_show_poi: false,
        }
    }

    /// Reproduce the emit-time geometry for one chunk's full-detail (stride 1)
    /// surface so a test can inspect its per-vertex normals directly.
    fn chunk_mesh(c: &TerrainCfg, f: &field::HeightField, ci: usize, cj: usize) -> Mesh {
        let segments = f.n - 1;
        let chunks = c.chunks as usize;
        let spc = segments / chunks;
        let (w, d, amp) = (c.size[0], c.size[2], c.size[1]);
        let (half_w, half_d) = (w * 0.5, d * 0.5);
        let dx = w / segments as f32;
        let dz = d / segments as f32;
        let world_x = |i: usize| -half_w + (i as f32 / segments as f32) * w;
        let world_z = |j: usize| -half_d + (j as f32 / segments as f32) * d;
        let (i0, j0) = (ci * spc, cj * spc);
        let cx = 0.5 * (world_x(i0) + world_x(i0 + spc));
        let cz = 0.5 * (world_z(j0) + world_z(j0 + spc));
        build_chunk_lod_mesh(
            f, i0, j0, spc, 1, amp, dx, dz, &world_x, &world_z, cx, cz, 0.0, 0.0, false,
        )
    }

    #[test]
    fn adjacent_chunks_share_boundary_normals() {
        // The east edge of chunk (0,0) is the west edge of chunk (1,0). With
        // analytic normals those shared grid points must carry identical
        // normals so the surface shades with no faceted seam at the chunk line.
        let c = cfg();
        let f = field::build(&c);
        let spc = (f.n - 1) / c.chunks as usize;
        let nv = spc + 1; // stride 1
        let left = chunk_mesh(&c, &f, 0, 0);
        let right = chunk_mesh(&c, &f, 1, 0);
        for lj in 0..nv {
            let east = left.normals[lj * nv + (nv - 1)]; // li = last
            let west = right.normals[lj * nv]; // li = 0
            for k in 0..3 {
                assert!(
                    (east[k] - west[k]).abs() < 1e-6,
                    "boundary normal mismatch at row {lj}, comp {k}: {east:?} vs {west:?}"
                );
            }
        }
    }

    #[test]
    fn surface_normals_point_up() {
        let c = cfg();
        let f = field::build(&c);
        let m = chunk_mesh(&c, &f, 1, 1);
        let nv = (f.n - 1) / c.chunks as usize + 1;
        for v in &m.normals[..nv * nv] {
            assert!(v[1] > 0.0, "surface normal not upward: {v:?}");
            let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            assert!((len - 1.0).abs() < 1e-4, "normal not unit length: {len}");
        }
    }
}
