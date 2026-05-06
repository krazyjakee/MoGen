use mogen_core::{Mesh, UvMode};

use super::cuboid::box_mesh;

/// Box of `size` with each of the 12 cube edges replaced by a flat 45° bevel
/// strip of width `radius`, and each of the 8 cube corners replaced by a flat
/// triangle joining its three adjacent bevels. Topologically the sharp-edge
/// counterpart to `rounded_box_mesh`: same six face rectangles (shrunken to
/// the bevel boundary), but the 12 quarter-cylinder edge strips become flat
/// quads and the 8 sphere-octant corner patches become single triangles.
///
/// `radius` is clamped to ≤ ½ of the smallest extent so the bevels never
/// meet in the middle of a face. A radius below `1e-6` falls back to a plain
/// box — useful when an author types `radius=0` and still expects a clean
/// mesh.
///
/// UVs are face-local in `Fit` mode (unit square per face/strip/corner) and
/// world-space in `Tile` mode using the same triplanar projection
/// `rounded_box` uses, so the same texture matches at the bevel seam.
pub fn chamfered_box_mesh(size: [f32; 3], radius: f32, mode: UvMode) -> Mesh {
    let sx = size[0].max(0.0);
    let sy = size[1].max(0.0);
    let sz = size[2].max(0.0);
    let r = radius.min(sx * 0.5).min(sy * 0.5).min(sz * 0.5).max(0.0);
    if r < 1e-6 {
        return box_mesh([sx, sy, sz], mode);
    }
    let hx = sx * 0.5;
    let hy = sy * 0.5;
    let hz = sz * 0.5;
    let cx = hx - r;
    let cy = hy - r;
    let cz = hz - r;

    let mut mesh = Mesh::default();

    // Six flat face rectangles, shrunken so they end at the bevel boundary
    // along both perpendicular axes (matches the rounded_box layout exactly).
    let flat_faces: [([f32; 3], [[f32; 3]; 4]); 6] = [
        ([1.0, 0.0, 0.0],  [[ hx, -cy, -cz], [ hx,  cy, -cz], [ hx,  cy,  cz], [ hx, -cy,  cz]]),
        ([-1.0, 0.0, 0.0], [[-hx, -cy,  cz], [-hx,  cy,  cz], [-hx,  cy, -cz], [-hx, -cy, -cz]]),
        ([0.0, 1.0, 0.0],  [[-cx,  hy,  cz], [ cx,  hy,  cz], [ cx,  hy, -cz], [-cx,  hy, -cz]]),
        ([0.0, -1.0, 0.0], [[-cx, -hy, -cz], [ cx, -hy, -cz], [ cx, -hy,  cz], [-cx, -hy,  cz]]),
        ([0.0, 0.0, 1.0],  [[-cx, -cy,  hz], [ cx, -cy,  hz], [ cx,  cy,  hz], [-cx,  cy,  hz]]),
        ([0.0, 0.0, -1.0], [[ cx, -cy, -hz], [-cx, -cy, -hz], [-cx,  cy, -hz], [ cx,  cy, -hz]]),
    ];
    for (n, quad) in flat_faces {
        let base = mesh.positions.len() as u32;
        for v in quad {
            mesh.positions.push(v);
            mesh.normals.push(n);
        }
        mesh.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    // Twelve flat bevel quads, one per cube edge. Each quad sits on the plane
    // bisecting the two faces it joins, so its outward normal is the
    // (normalised) sum of the two adjacent face normals — i.e. an axis-aligned
    // 45° bevel for a cube. `axis` names the edge's direction (the axis the
    // bevel quad is parallel to); `s1`/`s2` are the signs of the two
    // perpendicular axes that pick which of the four edges in this orbit
    // we're on.
    let edges: [(usize, f32, f32); 12] = [
        (0,  1.0,  1.0), (0, -1.0,  1.0), (0, -1.0, -1.0), (0,  1.0, -1.0),
        (1,  1.0,  1.0), (1, -1.0,  1.0), (1, -1.0, -1.0), (1,  1.0, -1.0),
        (2,  1.0,  1.0), (2, -1.0,  1.0), (2, -1.0, -1.0), (2,  1.0, -1.0),
    ];
    let inv_sqrt2 = std::f32::consts::FRAC_1_SQRT_2;
    for (axis, s1, s2) in edges {
        // For an edge running along `axis`, the two perpendicular axes
        // (perp1, perp2) are the other two cube axes in cyclic order. The
        // bevel runs from (perp1=±c, perp2=±h) to (perp1=±h, perp2=±c) —
        // a single flat quad. Each end of the quad sits at one of the cube
        // edges' two extents along `axis` (±along_half).
        let (along_half, cp1, cp2) = match axis {
            0 => (cx, cy, cz),
            1 => (cy, cx, cz),
            2 => (cz, cx, cy),
            _ => unreachable!(),
        };
        // Two corners of the bevel quad in each cap (low and high along axis):
        //   `a` sits flush with the perp1 face: perp1 = s1 * h, perp2 = s2 * c
        //   `b` sits flush with the perp2 face: perp1 = s1 * c, perp2 = s2 * h
        let make = |along: f32, on_perp1_face: bool| -> [f32; 3] {
            let (p1, p2) = if on_perp1_face {
                (s1 * (cp1 + r), s2 * cp2)
            } else {
                (s1 * cp1, s2 * (cp2 + r))
            };
            match axis {
                0 => [along, p1, p2],
                1 => [p1, along, p2],
                2 => [p1, p2, along],
                _ => unreachable!(),
            }
        };
        // Outward normal: average of the two adjacent face normals,
        // normalised. Both face normals point along the perp axes with
        // magnitudes (s1, 0) and (0, s2) respectively; their sum has length
        // √2 so we scale by 1/√2.
        let mut normal = [0.0_f32; 3];
        match axis {
            0 => {
                normal[1] = s1 * inv_sqrt2;
                normal[2] = s2 * inv_sqrt2;
            }
            1 => {
                normal[0] = s1 * inv_sqrt2;
                normal[2] = s2 * inv_sqrt2;
            }
            2 => {
                normal[0] = s1 * inv_sqrt2;
                normal[1] = s2 * inv_sqrt2;
            }
            _ => unreachable!(),
        };
        // CCW winding viewed from outside: pick the order so the cross
        // product (b-a)×(c-a) points along `normal`. With the quad spanning
        // axis=[-along_half, +along_half] and the two perp orientations
        // above, the order [(-, on_p1), (-, on_p2), (+, on_p2), (+, on_p1)]
        // is CCW for the (s1, s2) = (+,+) case; flipping either sign flips
        // the normal so we conditionally reverse winding.
        let p_a = make(-along_half, true);
        let p_b = make(-along_half, false);
        let p_c = make(along_half, false);
        let p_d = make(along_half, true);
        let base = mesh.positions.len() as u32;
        // axis=1's perp pair (X, Z) forms a left-handed frame relative to Y,
        // so the natural perp1→perp2 sweep is CW instead of CCW; xor with
        // `axis == 1` to compensate.
        let ccw = ((s1 * s2) > 0.0) ^ (axis == 1);
        let quad = if ccw { [p_a, p_b, p_c, p_d] } else { [p_b, p_a, p_d, p_c] };
        for v in quad {
            mesh.positions.push(v);
            mesh.normals.push(normal);
        }
        mesh.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    // Eight flat corner triangles. Each connects one vertex from each of the
    // three adjacent bevel quads — the three points where the corner's three
    // incident face rectangles end. Outward normal is the unit (1/√3, 1/√3,
    // 1/√3) vector pointing away from the cube interior.
    let corners: [[f32; 3]; 8] = [
        [ 1.0,  1.0,  1.0], [-1.0,  1.0,  1.0], [-1.0, -1.0,  1.0], [ 1.0, -1.0,  1.0],
        [ 1.0,  1.0, -1.0], [-1.0,  1.0, -1.0], [-1.0, -1.0, -1.0], [ 1.0, -1.0, -1.0],
    ];
    let inv_sqrt3 = 1.0_f32 / 3.0_f32.sqrt();
    for s in corners {
        let sx_s = s[0]; let sy_s = s[1]; let sz_s = s[2];
        // The three bevel-end vertices at this corner: each sits on one of
        // the three cube faces meeting here, at the edge of that face's
        // shrunken rectangle.
        let p_x = [sx_s * hx,         sy_s * cy,         sz_s * cz        ];
        let p_y = [sx_s * cx,         sy_s * hy,         sz_s * cz        ];
        let p_z = [sx_s * cx,         sy_s * cy,         sz_s * hz        ];
        let normal = [sx_s * inv_sqrt3, sy_s * inv_sqrt3, sz_s * inv_sqrt3];
        let base = mesh.positions.len() as u32;
        // Pick CCW winding viewed from outside. For the (+,+,+) corner the
        // cycle x→y→z is CCW; flipping any sign flips orientation, so we
        // count the number of negative signs and swap two verts when odd.
        let parity = (sx_s < 0.0) as i32 + (sy_s < 0.0) as i32 + (sz_s < 0.0) as i32;
        let tri = if parity % 2 == 0 { [p_x, p_y, p_z] } else { [p_x, p_z, p_y] };
        for v in tri {
            mesh.positions.push(v);
            mesh.normals.push(normal);
        }
        mesh.indices.extend_from_slice(&[base, base + 1, base + 2]);
    }

    mesh.uvs = triplanar_uvs_for_box(&mesh.positions, &mesh.normals, [sx, sy, sz], mode);
    mesh
}

/// Triplanar UV projection — copy of the rounded_box helper so chamfered_box
/// stays self-contained (and so we can tweak later without affecting the
/// curved-bevel kernel).
fn triplanar_uvs_for_box(
    positions: &[[f32; 3]],
    normals: &[[f32; 3]],
    size: [f32; 3],
    mode: UvMode,
) -> Vec<[f32; 2]> {
    let inv_sx = if size[0] > 1e-6 { 1.0 / size[0] } else { 1.0 };
    let inv_sy = if size[1] > 1e-6 { 1.0 / size[1] } else { 1.0 };
    let inv_sz = if size[2] > 1e-6 { 1.0 / size[2] } else { 1.0 };
    let mut uvs = Vec::with_capacity(positions.len());
    for (p, n) in positions.iter().zip(normals.iter()) {
        let abs = [n[0].abs(), n[1].abs(), n[2].abs()];
        let dominant = if abs[0] >= abs[1] && abs[0] >= abs[2] {
            0
        } else if abs[1] >= abs[2] {
            1
        } else {
            2
        };
        let (u, v) = match dominant {
            0 => (p[2], p[1]),
            1 => (p[0], p[2]),
            _ => (p[0], p[1]),
        };
        let uv = match mode {
            UvMode::Tile => [u, v],
            UvMode::Fit => match dominant {
                0 => [(u * inv_sz) + 0.5, (v * inv_sy) + 0.5],
                1 => [(u * inv_sx) + 0.5, (v * inv_sz) + 0.5],
                _ => [(u * inv_sx) + 0.5, (v * inv_sy) + 0.5],
            },
        };
        uvs.push(uv);
    }
    uvs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aabb(positions: &[[f32; 3]]) -> ([f32; 3], [f32; 3]) {
        let mut mn = [f32::INFINITY; 3];
        let mut mx = [f32::NEG_INFINITY; 3];
        for p in positions {
            for k in 0..3 {
                mn[k] = mn[k].min(p[k]);
                mx[k] = mx[k].max(p[k]);
            }
        }
        (mn, mx)
    }

    #[test]
    fn radius_zero_falls_back_to_plain_box() {
        let chamfered = chamfered_box_mesh([1.0, 1.0, 1.0], 0.0, UvMode::Fit);
        let plain = box_mesh([1.0, 1.0, 1.0], UvMode::Fit);
        assert_eq!(chamfered.positions.len(), plain.positions.len());
        assert_eq!(chamfered.indices.len(), plain.indices.len());
    }

    #[test]
    fn aabb_matches_input_size() {
        let m = chamfered_box_mesh([2.0, 1.0, 4.0], 0.2, UvMode::Fit);
        let (mn, mx) = aabb(&m.positions);
        assert!((mx[0] - 1.0).abs() < 1e-5 && (mn[0] + 1.0).abs() < 1e-5);
        assert!((mx[1] - 0.5).abs() < 1e-5 && (mn[1] + 0.5).abs() < 1e-5);
        assert!((mx[2] - 2.0).abs() < 1e-5 && (mn[2] + 2.0).abs() < 1e-5);
    }

    #[test]
    fn cube_chamfer_produces_44_triangles() {
        // 6 face rects (12) + 12 bevel quads (24) + 8 corner tris (8) = 44.
        let m = chamfered_box_mesh([1.0, 1.0, 1.0], 0.1, UvMode::Fit);
        assert_eq!(m.indices.len() / 3, 44);
    }

    #[test]
    fn radius_clamped_to_half_smallest_extent() {
        // Bevel radius 5.0 on a 1×1×1 cube would fold past the centre. The
        // kernel clamps to 0.5 so the chamfer collapses the cube into 8
        // corner triangles meeting at the centre — but never inverts.
        let m = chamfered_box_mesh([1.0, 1.0, 1.0], 5.0, UvMode::Fit);
        let (mn, mx) = aabb(&m.positions);
        for k in 0..3 {
            assert!(mx[k] <= 0.5 + 1e-5, "axis {k} max {} exceeds half-extent", mx[k]);
            assert!(mn[k] >= -0.5 - 1e-5, "axis {k} min {} below -half-extent", mn[k]);
        }
    }

    #[test]
    fn all_normals_are_unit_length() {
        let m = chamfered_box_mesh([1.0, 1.0, 1.0], 0.15, UvMode::Fit);
        for n in &m.normals {
            let len = (n[0].powi(2) + n[1].powi(2) + n[2].powi(2)).sqrt();
            assert!(
                (len - 1.0).abs() < 1e-5,
                "normal {n:?} not unit-length (len={len})"
            );
        }
    }

    #[test]
    fn winding_points_outward_for_every_face() {
        // For each tri, check that the geometric face normal (right-hand
        // rule on the index winding) agrees with the average vertex normal.
        // A flipped winding would dot to ~-1; a correct one dots to ~+1.
        let m = chamfered_box_mesh([1.0, 1.0, 1.0], 0.15, UvMode::Fit);
        for tri in m.indices.chunks_exact(3) {
            let p0 = m.positions[tri[0] as usize];
            let p1 = m.positions[tri[1] as usize];
            let p2 = m.positions[tri[2] as usize];
            let e1 = [p1[0] - p0[0], p1[1] - p0[1], p1[2] - p0[2]];
            let e2 = [p2[0] - p0[0], p2[1] - p0[1], p2[2] - p0[2]];
            let face_n = [
                e1[1] * e2[2] - e1[2] * e2[1],
                e1[2] * e2[0] - e1[0] * e2[2],
                e1[0] * e2[1] - e1[1] * e2[0],
            ];
            let n0 = m.normals[tri[0] as usize];
            let dot = face_n[0] * n0[0] + face_n[1] * n0[1] + face_n[2] * n0[2];
            assert!(
                dot > 0.0,
                "tri {tri:?} winds against its outward normal (dot={dot})"
            );
        }
    }
}
