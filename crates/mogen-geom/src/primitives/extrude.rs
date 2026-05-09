//! Linear extrude: push a 2D polygon (with optional inner holes) to 3D along
//! +Y, with optional uniform top-section taper and total twist over the
//! height. Useful for I-beams, gear teeth, picture frames, custom tile
//! profiles — anything whose silhouette is a hand-authored polygon and
//! whose 3D form is just that polygon swept up.
//!
//! The cap polygon is triangulated by [`earcutr`] (Mapbox earcut port) so
//! arbitrary simple polygons with holes work correctly. Side ribs are emitted
//! as quads between consecutive bottom/top samples of every contour; each
//! side vertex is duplicated per ring so per-edge normals can be flat-shaded
//! correctly by the downstream normal recompute pass.
//!
//! Mesh sits centered at `y=0`, spanning `[-h/2, +h/2]`. Default connectors
//! `top`/`bottom` live at the polygon's centroid on each end cap.


use mogen_core::{Mesh, UvMode};

use crate::cleanup::recompute_normals;

/// One closed contour. CCW for outer outlines, CW for holes (by earcut
/// convention). Points are 2D in the polygon's local XZ frame; Z is the
/// "in" axis on the printed page (the polygon lies flat at y=0 before
/// extrusion).
pub type Contour = Vec<[f32; 2]>;

/// Linear extrusion of a closed 2D polygon (`outer`) along +Y by `height`.
/// `holes` cut inner contours through the cap polygons.
///
/// - `taper`: ratio applied to the top section relative to the bottom
///   (1.0 = straight extrude, 0.5 = top half-size, 2.0 = top double-size).
///   Vertices are scaled radially around the polygon centroid so any
///   silhouette tapers uniformly.
/// - `twist_radians`: total roll around +Y over the height. Applied
///   linearly between bottom and top.
/// - `caps`: emit end caps. Side ribs are always emitted.
///
/// Output mesh is centered at `y=0` (range `[-h/2, +h/2]`). Vertex
/// normals are recomputed after triangle assembly so the caps and side
/// ribs both shade correctly.
pub fn extrude_mesh(
    outer: &[[f32; 2]],
    holes: &[Contour],
    height: f32,
    taper: f32,
    twist_radians: f32,
    caps: bool,
    mode: UvMode,
) -> Mesh {
    if outer.len() < 3 {
        return Mesh::default();
    }

    let half_h = height * 0.5;
    let centroid = polygon_centroid(outer);

    // Per-ring transform of one 2D polygon point to a 3D position.
    // `t` ∈ [0, 1] runs bottom→top.
    let transform = |p: [f32; 2], t: f32| -> [f32; 3] {
        let scale = 1.0 + (taper - 1.0) * t;
        let dx = (p[0] - centroid[0]) * scale;
        let dz = (p[1] - centroid[1]) * scale;
        let angle = twist_radians * t;
        let (s, c) = angle.sin_cos();
        let rx = dx * c - dz * s + centroid[0];
        let rz = dx * s + dz * c + centroid[1];
        let y = -half_h + height * t;
        [rx, y, rz]
    };

    // Stitch every contour into one mesh: for each contour, push duplicated
    // bottom + top rings (so each side rib has its own vertex normals) plus
    // a quad strip between them. Caps come after, fed through earcut.
    let mut mesh = Mesh::default();

    // Side ribs — outer contour and every hole.
    push_side_strip(&mut mesh, outer, transform, mode, height);
    for hole in holes {
        push_side_strip(&mut mesh, hole, transform, mode, height);
    }

    if caps {
        // Build the flat triangulation once (in 2D) — earcut understands the
        // outer-CCW + hole-CW convention via the `hole_indices` argument.
        let (flat, hole_idx) = pack_polygon_for_earcut(outer, holes);
        match earcutr::earcut(&flat, &hole_idx, 2) {
            Ok(tri) => {
                // Bottom cap (normal -Y) uses earcut's natural winding;
                // top cap (normal +Y) reverses it. See [`push_cap`].
                push_cap(
                    &mut mesh,
                    outer,
                    holes,
                    &tri,
                    /*top=*/ false,
                    transform,
                    mode,
                    centroid,
                );
                push_cap(
                    &mut mesh,
                    outer,
                    holes,
                    &tri,
                    /*top=*/ true,
                    transform,
                    mode,
                    centroid,
                );
            }
            Err(_) => {
                // Self-intersecting outline or unrecoverable triangulator
                // failure. Skip caps; the side ribs still ship so the
                // author can see what went in. (Validation upstream rejects
                // < 3 points; everything else falls through here as
                // best-effort.)
            }
        }
    }

    recompute_normals(&mut mesh);
    mesh
}

/// Push a quad strip between the bottom and top loops of one contour.
fn push_side_strip(
    mesh: &mut Mesh,
    contour: &[[f32; 2]],
    transform: impl Fn([f32; 2], f32) -> [f32; 3],
    mode: UvMode,
    height: f32,
) {
    if contour.len() < 2 {
        return;
    }
    // Cumulative arc length along the contour drives U in tile mode so
    // textures wrap evenly around the perimeter regardless of vertex
    // density.
    let mut arc = vec![0.0_f32];
    for w in contour.windows(2) {
        let last = *arc.last().unwrap();
        let dx = w[1][0] - w[0][0];
        let dz = w[1][1] - w[0][1];
        arc.push(last + (dx * dx + dz * dz).sqrt());
    }
    // Close the loop: distance from the last point back to the first.
    let close_dx = contour[0][0] - contour[contour.len() - 1][0];
    let close_dz = contour[0][1] - contour[contour.len() - 1][1];
    let last = *arc.last().unwrap();
    arc.push(last + (close_dx * close_dx + close_dz * close_dz).sqrt());

    let n = contour.len();
    let base = mesh.positions.len() as u32;

    // Emit (bottom, top) pairs for each contour vertex, with the closing
    // edge represented by a duplicate of vertex 0 at index n. This keeps
    // U coordinates monotonic across the seam.
    for i in 0..=n {
        let p = contour[i % n];
        let u = match mode {
            UvMode::Fit => arc[i] / arc[n].max(1e-6),
            UvMode::Tile => arc[i],
        };
        let v_bot = match mode {
            UvMode::Fit => 0.0,
            UvMode::Tile => 0.0,
        };
        let v_top = match mode {
            UvMode::Fit => 1.0,
            UvMode::Tile => height,
        };
        let p_bot = transform(p, 0.0);
        let p_top = transform(p, 1.0);
        mesh.positions.push(p_bot);
        mesh.normals.push([0.0, 0.0, 0.0]); // recomputed later
        mesh.uvs.push([u, v_bot]);
        mesh.positions.push(p_top);
        mesh.normals.push([0.0, 0.0, 0.0]);
        mesh.uvs.push([u, v_top]);
    }

    // Triangulate each rib quad. Winding chosen so the front face points
    // outward when `outer` is CCW and inward when `outer` is CW (holes).
    for i in 0..n {
        let a = base + (i as u32) * 2; // bottom_i
        let b = base + (i as u32) * 2 + 1; // top_i
        let c = base + (i as u32 + 1) * 2; // bottom_{i+1}
        let d = base + (i as u32 + 1) * 2 + 1; // top_{i+1}
        // Quad order: a-d-c-a-b-d. Two triangles a,d,c and a,b,d.
        // For a CCW outer contour the resulting normal opposes (b-a)×(c-a),
        // i.e. points outward away from the polygon's interior.
        mesh.indices.extend_from_slice(&[a, d, c, a, b, d]);
    }
}

/// Push one cap (top or bottom) using the earcut triangulation produced
/// by [`pack_polygon_for_earcut`].
#[allow(clippy::too_many_arguments)]
fn push_cap(
    mesh: &mut Mesh,
    outer: &[[f32; 2]],
    holes: &[Contour],
    tri: &[usize],
    top: bool,
    transform: impl Fn([f32; 2], f32) -> [f32; 3],
    mode: UvMode,
    centroid: [f32; 2],
) {
    let t = if top { 1.0 } else { 0.0 };
    let base = mesh.positions.len() as u32;

    // Bound the polygon for fit-mode UVs.
    let (mut min_x, mut max_x) = (f32::INFINITY, f32::NEG_INFINITY);
    let (mut min_z, mut max_z) = (f32::INFINITY, f32::NEG_INFINITY);
    let push_vert = |mesh: &mut Mesh, p: [f32; 2]| {
        let pos = transform(p, t);
        mesh.positions.push(pos);
        mesh.normals.push([0.0, 0.0, 0.0]);
    };
    for p in outer.iter().chain(holes.iter().flat_map(|h| h.iter())) {
        if p[0] < min_x { min_x = p[0]; }
        if p[0] > max_x { max_x = p[0]; }
        if p[1] < min_z { min_z = p[1]; }
        if p[1] > max_z { max_z = p[1]; }
    }
    let span_x = (max_x - min_x).max(1e-6);
    let span_z = (max_z - min_z).max(1e-6);

    // Push outer vertices, then each hole's vertices in the same order
    // earcut received them (hole_indices marks the boundaries).
    for p in outer.iter() {
        push_vert(mesh, *p);
        let uv = cap_uv(*p, mode, centroid, [min_x, min_z], [span_x, span_z]);
        mesh.uvs.push(uv);
    }
    for hole in holes {
        for p in hole.iter() {
            push_vert(mesh, *p);
            let uv = cap_uv(*p, mode, centroid, [min_x, min_z], [span_x, span_z]);
            mesh.uvs.push(uv);
        }
    }

    // Emit triangles — earcut returns indices into the packed vertex
    // sequence (outer first, then each hole concatenated). Earcut's CCW
    // output, read as 3D positions in the XZ plane, gives a -Y face
    // normal (CCW in [x,z] = CW when viewed from +Y). So earcut's natural
    // winding lands on the bottom cap, and the top cap reverses it.
    for c in tri.chunks(3) {
        let a = base + c[0] as u32;
        let b = base + c[1] as u32;
        let d = base + c[2] as u32;
        if top {
            mesh.indices.extend_from_slice(&[a, d, b]);
        } else {
            mesh.indices.extend_from_slice(&[a, b, d]);
        }
    }
}

#[inline]
fn cap_uv(
    p: [f32; 2],
    mode: UvMode,
    _centroid: [f32; 2],
    origin: [f32; 2],
    span: [f32; 2],
) -> [f32; 2] {
    match mode {
        UvMode::Fit => [
            (p[0] - origin[0]) / span[0],
            (p[1] - origin[1]) / span[1],
        ],
        UvMode::Tile => [p[0], p[1]],
    }
}

/// Pack outer + holes into a single flat `[x0, y0, x1, y1, …]` array plus
/// the start indices of each hole, as required by [`earcutr::earcut`].
fn pack_polygon_for_earcut(
    outer: &[[f32; 2]],
    holes: &[Contour],
) -> (Vec<f32>, Vec<usize>) {
    let mut flat: Vec<f32> = Vec::with_capacity(2 * (outer.len() + holes.iter().map(|h| h.len()).sum::<usize>()));
    let mut hole_idx: Vec<usize> = Vec::with_capacity(holes.len());
    for p in outer {
        flat.push(p[0]);
        flat.push(p[1]);
    }
    for hole in holes {
        if hole.len() < 3 {
            continue; // earcut would index past-end on a degenerate hole
        }
        hole_idx.push(flat.len() / 2);
        for p in hole {
            flat.push(p[0]);
            flat.push(p[1]);
        }
    }
    (flat, hole_idx)
}

/// Centroid of a closed polygon — used as the fixed point for taper/twist
/// so the operation is well-defined regardless of where the polygon
/// happens to sit in 2D space. Falls back to a simple vertex average for
/// degenerate (zero-area) polygons.
fn polygon_centroid(points: &[[f32; 2]]) -> [f32; 2] {
    let mut area = 0.0_f32;
    let mut cx = 0.0_f32;
    let mut cz = 0.0_f32;
    let n = points.len();
    for i in 0..n {
        let p0 = points[i];
        let p1 = points[(i + 1) % n];
        let cross = p0[0] * p1[1] - p1[0] * p0[1];
        area += cross;
        cx += (p0[0] + p1[0]) * cross;
        cz += (p0[1] + p1[1]) * cross;
    }
    if area.abs() < 1e-8 {
        let mut sx = 0.0_f32;
        let mut sz = 0.0_f32;
        for p in points {
            sx += p[0];
            sz += p[1];
        }
        return [sx / n as f32, sz / n as f32];
    }
    [cx / (3.0 * area), cz / (3.0 * area)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    fn unit_square() -> Vec<[f32; 2]> {
        // CCW square at the origin, side 1.
        vec![[-0.5, -0.5], [0.5, -0.5], [0.5, 0.5], [-0.5, 0.5]]
    }

    #[test]
    fn extrudes_unit_square_to_box() {
        let mesh = extrude_mesh(&unit_square(), &[], 1.0, 1.0, 0.0, true, UvMode::default());
        // 4 side ribs (2 tris each) + 2 caps (2 tris each) = 12 tris.
        assert_eq!(mesh.indices.len() / 3, 12, "expected 12 tris, got {}", mesh.indices.len() / 3);
        // Bounding box ≈ [-0.5, 0.5] on X and Z, [-0.5, 0.5] on Y.
        let (min, max) = aabb(&mesh.positions);
        assert!((min[1] + 0.5).abs() < 1e-5);
        assert!((max[1] - 0.5).abs() < 1e-5);
        assert!((min[0] + 0.5).abs() < 1e-5);
        assert!((max[0] - 0.5).abs() < 1e-5);
    }

    #[test]
    fn extrude_with_hole_keeps_outer_intact() {
        let outer = unit_square();
        let hole: Contour = vec![
            // CW small square in the middle.
            [-0.1, -0.1],
            [-0.1, 0.1],
            [0.1, 0.1],
            [0.1, -0.1],
        ];
        let mesh = extrude_mesh(&outer, &[hole], 1.0, 1.0, 0.0, true, UvMode::default());
        // The hole punches through both caps; tri count strictly larger
        // than the no-hole baseline (12 tris).
        assert!(
            mesh.indices.len() / 3 > 12,
            "extrude with hole should produce more tris than without (got {})",
            mesh.indices.len() / 3,
        );
        // Outer bounds untouched by the hole.
        let (min, max) = aabb(&mesh.positions);
        assert!((min[0] + 0.5).abs() < 1e-5);
        assert!((max[0] - 0.5).abs() < 1e-5);
    }

    #[test]
    fn taper_shrinks_top_ring() {
        let mesh = extrude_mesh(&unit_square(), &[], 1.0, 0.5, 0.0, true, UvMode::default());
        // Top ring (y > 0) X span should be half the bottom ring (y < 0).
        let mut top_x_max = f32::NEG_INFINITY;
        let mut bot_x_max = f32::NEG_INFINITY;
        for p in &mesh.positions {
            if p[1] > 0.4 && p[0] > top_x_max {
                top_x_max = p[0];
            }
            if p[1] < -0.4 && p[0] > bot_x_max {
                bot_x_max = p[0];
            }
        }
        assert!((top_x_max - 0.25).abs() < 1e-3, "top extent should be 0.25 (got {top_x_max})");
        assert!((bot_x_max - 0.5).abs() < 1e-3, "bottom extent should be 0.5 (got {bot_x_max})");
    }

    #[test]
    fn twist_rotates_top_ring() {
        // Twist by 45° — chosen so the rotation leaves a visible footprint
        // (a square is its own image under 90° rotation, so PI/2 wouldn't
        // detect anything). At 45°, (0.5, -0.5) rotates to ~(0.707, 0).
        let mesh = extrude_mesh(&unit_square(), &[], 1.0, 1.0, PI * 0.25, true, UvMode::default());
        let mut found_rotation = false;
        for p in &mesh.positions {
            if p[1] > 0.4 && (p[0] - 0.707).abs() < 0.05 && p[2].abs() < 0.05 {
                found_rotation = true;
                break;
            }
        }
        assert!(found_rotation, "twist should rotate the top ring 45°");
    }

    #[test]
    fn caps_face_outward() {
        // Bottom cap should have -Y face normals; top cap should have +Y.
        let mesh = extrude_mesh(&unit_square(), &[], 1.0, 1.0, 0.0, true, UvMode::default());
        let mut top_ny = 0.0f32;
        let mut bot_ny = 0.0f32;
        let mut top_n = 0;
        let mut bot_n = 0;
        for tri in mesh.indices.chunks_exact(3) {
            let pa = mesh.positions[tri[0] as usize];
            let pb = mesh.positions[tri[1] as usize];
            let pc = mesh.positions[tri[2] as usize];
            let avg_y = (pa[1] + pb[1] + pc[1]) / 3.0;
            let e1 = [pb[0]-pa[0], pb[1]-pa[1], pb[2]-pa[2]];
            let e2 = [pc[0]-pa[0], pc[1]-pa[1], pc[2]-pa[2]];
            let ny = e1[2]*e2[0] - e1[0]*e2[2];
            if avg_y > 0.4 { top_ny += ny; top_n += 1; }
            if avg_y < -0.4 { bot_ny += ny; bot_n += 1; }
        }
        assert!(top_n > 0 && bot_n > 0, "expected both caps present");
        assert!(top_ny > 0.0, "top cap normal should be +Y, got Ny sum {top_ny}");
        assert!(bot_ny < 0.0, "bottom cap normal should be -Y, got Ny sum {bot_ny}");
    }

    fn aabb(positions: &[[f32; 3]]) -> ([f32; 3], [f32; 3]) {
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        for p in positions {
            for i in 0..3 {
                if p[i] < min[i] { min[i] = p[i]; }
                if p[i] > max[i] { max[i] = p[i]; }
            }
        }
        (min, max)
    }
}
