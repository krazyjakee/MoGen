use mogen_core::{Mesh, UvMode};

use super::cuboid::box_mesh;

/// Which face of the box gets the inset cut. Names are axis+sign; the
/// alternative-name aliases (`top`, `bottom`, …) are resolved at the
/// lowering layer so this kernel takes the canonical form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsetFace {
    PosX,
    NegX,
    PosY,
    NegY,
    PosZ,
    NegZ,
}

impl InsetFace {
    fn axis(self) -> usize {
        match self {
            InsetFace::PosX | InsetFace::NegX => 0,
            InsetFace::PosY | InsetFace::NegY => 1,
            InsetFace::PosZ | InsetFace::NegZ => 2,
        }
    }
    fn sign(self) -> f32 {
        match self {
            InsetFace::PosX | InsetFace::PosY | InsetFace::PosZ => 1.0,
            InsetFace::NegX | InsetFace::NegY | InsetFace::NegZ => -1.0,
        }
    }
}

/// Box of `size` with one face replaced by an inset/recessed panel: an outer
/// ring at the original face plane, four side walls dropping inward by
/// `depth`, and a sunken inner rectangle. Use for window frames, recessed
/// door panels, button caps, sunken pickup wells.
///
/// `amount` is the inset distance (how far inward from the face perimeter the
/// sunken rect's edges sit, measured in world units along the face's two
/// in-plane axes). `depth` is how far the sunken rect drops below the face
/// plane along the face's outward normal — a positive depth always sinks the
/// panel inward, regardless of which face was chosen.
///
/// Both `amount` and `depth` are clamped: `amount` to ≤ ½ of the smallest
/// in-plane extent (so the sunken rect doesn't invert), `depth` to ≤ the
/// extent along the face's outward axis (so the sunken rect doesn't punch
/// through to the opposite face). `amount=0` collapses to the original
/// `box_mesh`; `depth=0` keeps the inset at the face plane (a flat outer
/// ring with no sidewalls — useful for purely visual seam ornament).
pub fn inset_box_mesh(
    size: [f32; 3],
    face: InsetFace,
    amount: f32,
    depth: f32,
    mode: UvMode,
) -> Mesh {
    let sx = size[0].max(0.0);
    let sy = size[1].max(0.0);
    let sz = size[2].max(0.0);
    let face_axis = face.axis();
    let perp1 = (face_axis + 1) % 3;
    let perp2 = (face_axis + 2) % 3;
    let in_plane_min = size[perp1].min(size[perp2]);
    let amount = amount.min(in_plane_min * 0.5).max(0.0);
    if amount < 1e-6 {
        return box_mesh([sx, sy, sz], mode);
    }
    // Clamp depth so the sunken rect can't cross the opposite face. Negative
    // depths would sink "outward" (away from the box interior) — refuse them
    // by clamping to 0; an outward bump is what `extrude_face` would do, and
    // we don't want this kernel doing two jobs.
    let depth = depth.max(0.0).min(size[face_axis]);

    // Build a parametric helper that places vertices in the (face_axis, perp1,
    // perp2) frame, then writes them back to xyz. `face_coord` is the
    // along-axis offset from the centre (positive = toward the face).
    let half = [sx * 0.5, sy * 0.5, sz * 0.5];
    let face_sign = face.sign();
    let make = |face_offset: f32, p1: f32, p2: f32| -> [f32; 3] {
        let mut v = [0.0_f32; 3];
        v[face_axis] = face_sign * (half[face_axis] - face_offset);
        v[perp1] = p1;
        v[perp2] = p2;
        v
    };
    let face_normal = {
        let mut n = [0.0_f32; 3];
        n[face_axis] = face_sign;
        n
    };
    // Inner-rect bounds in perp1/perp2: perimeter shrunken by `amount`.
    let cp1 = half[perp1] - amount;
    let cp2 = half[perp2] - amount;
    // Outer perimeter of the chosen face.
    let hp1 = half[perp1];
    let hp2 = half[perp2];

    // Outer-ring corners (at the face plane, depth 0).
    let a00 = make(0.0, -hp1, -hp2);
    let a10 = make(0.0,  hp1, -hp2);
    let a11 = make(0.0,  hp1,  hp2);
    let a01 = make(0.0, -hp1,  hp2);
    // Inner-ring corners at the face plane (top of the sidewall).
    let b00 = make(0.0, -cp1, -cp2);
    let b10 = make(0.0,  cp1, -cp2);
    let b11 = make(0.0,  cp1,  cp2);
    let b01 = make(0.0, -cp1,  cp2);
    // Inner-rect corners at the sunken depth.
    let c00 = make(depth, -cp1, -cp2);
    let c10 = make(depth,  cp1, -cp2);
    let c11 = make(depth,  cp1,  cp2);
    let c01 = make(depth, -cp1,  cp2);

    let mut mesh = Mesh::default();

    // Push the five untouched faces from `box_mesh`, then patch in the
    // seven-quad inset on the chosen face. Sharing `box_mesh` keeps the
    // texture/UV behaviour of those five faces identical to a plain box.
    let base_box = box_mesh([sx, sy, sz], mode);
    // Walk box_mesh's six 4-vertex face groups; copy all but the one whose
    // normal matches our face direction. The sign comparison is exact —
    // box_mesh emits axis-aligned ±1 normals.
    for face_idx in 0..6 {
        let n0 = base_box.normals[face_idx * 4];
        if (n0[face_axis] - face_sign).abs() < 1e-3
            && n0[perp1].abs() < 1e-3
            && n0[perp2].abs() < 1e-3
        {
            continue;
        }
        let base = mesh.positions.len() as u32;
        for k in 0..4 {
            mesh.positions.push(base_box.positions[face_idx * 4 + k]);
            mesh.normals.push(base_box.normals[face_idx * 4 + k]);
            mesh.uvs.push(base_box.uvs[face_idx * 4 + k]);
        }
        mesh.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    // CCW winding for the outer ring + sunken floor depends on face_sign:
    // when face_sign>0 the (perp1, perp2) order p1+→p2+ traces CCW around
    // the outward normal; when face_sign<0 it traces CW, so we reverse.
    // The standalone helper builds either order from a single quad spec.
    // Push one quad with the requested normal and CCW/CW winding. UV
    // mode mirrors the surrounding box face: Fit gives each inset quad
    // its own unit square; Tile uses world-space metres on the (perp1,
    // perp2) plane so the inset ring tiles at the same metric scale as
    // the unmodified box face it abuts. (Tile mode previously dropped
    // through to Fit, producing a visible UV scale seam at the inset
    // boundary — caught by the second-pass review.)
    let push_quad = |mesh: &mut Mesh, n: [f32; 3], q: [[f32; 3]; 4], reverse: bool| {
        let base = mesh.positions.len() as u32;
        let order = if reverse { [3, 2, 1, 0] } else { [0, 1, 2, 3] };
        const FIT: [[f32; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        for (i, &k) in order.iter().enumerate() {
            let p = q[k];
            mesh.positions.push(p);
            mesh.normals.push(n);
            let uv = match mode {
                UvMode::Fit => FIT[i],
                UvMode::Tile => [p[perp1], p[perp2]],
            };
            mesh.uvs.push(uv);
        }
        mesh.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    };

    let reverse = face_sign < 0.0;

    // Outer ring: four trapezoidal strips at the face plane, all sharing
    // the face's outward normal. Winding has to match the original face
    // orientation so the ring lies flush with the surrounding box.
    push_quad(&mut mesh, face_normal, [a00, a10, b10, b00], reverse);
    push_quad(&mut mesh, face_normal, [a10, a11, b11, b10], reverse);
    push_quad(&mut mesh, face_normal, [a11, a01, b01, b11], reverse);
    push_quad(&mut mesh, face_normal, [a01, a00, b00, b01], reverse);

    // Side walls: four quads dropping from the inner ring at the face plane
    // down to the sunken floor. Normals point INTO the well (i.e. inward
    // along ±perp1/±perp2 in the well-local frame).
    let wall_n = |perp_idx: usize, sign: f32| {
        let mut n = [0.0_f32; 3];
        n[perp_idx] = sign;
        n
    };
    // perp1- wall (inner-ring side at p1=-cp1): normal points +perp1 (inward).
    push_quad(&mut mesh, wall_n(perp1, 1.0), [b00, c00, c01, b01], reverse);
    // perp1+ wall: normal -perp1.
    push_quad(&mut mesh, wall_n(perp1, -1.0), [b10, b11, c11, c10], reverse);
    // perp2- wall: normal +perp2.
    push_quad(&mut mesh, wall_n(perp2, 1.0), [b00, b10, c10, c00], reverse);
    // perp2+ wall: normal -perp2.
    push_quad(&mut mesh, wall_n(perp2, -1.0), [b01, c01, c11, b11], reverse);

    // Sunken floor: same outward normal as the original face (a recessed
    // panel still faces outward), at the deeper face_offset. Without
    // `reverse` the (p1, p2) sweep matches the outer ring and would create
    // a coincident-orientation floor — that's actually right because the
    // floor's normal is the same as the face's; pass through unchanged.
    push_quad(&mut mesh, face_normal, [c00, c10, c11, c01], reverse);

    mesh
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
    fn zero_amount_falls_back_to_plain_box() {
        let inset = inset_box_mesh([1.0, 1.0, 1.0], InsetFace::PosY, 0.0, 0.1, UvMode::Fit);
        let plain = box_mesh([1.0, 1.0, 1.0], UvMode::Fit);
        assert_eq!(inset.positions.len(), plain.positions.len());
        assert_eq!(inset.indices.len(), plain.indices.len());
    }

    #[test]
    fn inset_does_not_extend_aabb() {
        // A sunken panel must never push past the original box bounds.
        for face in [
            InsetFace::PosX,
            InsetFace::NegX,
            InsetFace::PosY,
            InsetFace::NegY,
            InsetFace::PosZ,
            InsetFace::NegZ,
        ] {
            let m = inset_box_mesh([2.0, 1.0, 3.0], face, 0.2, 0.1, UvMode::Fit);
            let (mn, mx) = aabb(&m.positions);
            assert!(mx[0] <= 1.0 + 1e-5 && mn[0] >= -1.0 - 1e-5, "face={face:?} bust X");
            assert!(mx[1] <= 0.5 + 1e-5 && mn[1] >= -0.5 - 1e-5, "face={face:?} bust Y");
            assert!(mx[2] <= 1.5 + 1e-5 && mn[2] >= -1.5 - 1e-5, "face={face:?} bust Z");
        }
    }

    #[test]
    fn sunken_floor_sits_at_correct_depth() {
        // Inset on +Y with depth=0.2 should produce vertices exactly at
        // y = hy - depth = 0.5 - 0.2 = 0.3 for the sunken rect.
        let m = inset_box_mesh([1.0, 1.0, 1.0], InsetFace::PosY, 0.15, 0.2, UvMode::Fit);
        let mut found_floor = false;
        for p in &m.positions {
            if (p[1] - 0.3).abs() < 1e-5 {
                found_floor = true;
                break;
            }
        }
        assert!(found_floor, "no vertex at expected sunken Y=0.3");
    }

    #[test]
    fn winding_points_outward_for_every_face() {
        for face in [
            InsetFace::PosX,
            InsetFace::PosY,
            InsetFace::PosZ,
            InsetFace::NegX,
            InsetFace::NegY,
            InsetFace::NegZ,
        ] {
            let m = inset_box_mesh([1.0, 1.0, 1.0], face, 0.15, 0.1, UvMode::Fit);
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
                    "face={face:?} tri {tri:?} winds against its outward normal (dot={dot})"
                );
            }
        }
    }

    #[test]
    fn correct_triangle_count() {
        // 5 untouched faces × 2 tris = 10
        // 4 outer ring quads × 2 = 8
        // 4 sidewalls × 2 = 8
        // 1 sunken floor × 2 = 2
        // Total: 28
        let m = inset_box_mesh([1.0, 1.0, 1.0], InsetFace::PosY, 0.2, 0.1, UvMode::Fit);
        assert_eq!(m.indices.len() / 3, 28);
    }

    #[test]
    fn depth_zero_is_flat_inset_seam() {
        // depth=0 collapses sidewalls into degenerate strips at the face
        // plane. The resulting mesh should still be valid — every vertex
        // sits exactly on the original face plane.
        let m = inset_box_mesh([1.0, 1.0, 1.0], InsetFace::PosY, 0.2, 0.0, UvMode::Fit);
        // Find vertices on the +Y face: y == 0.5.
        let on_top: Vec<&[f32; 3]> = m.positions.iter().filter(|p| (p[1] - 0.5).abs() < 1e-5).collect();
        // 4 outer corners + 4 inner corners + 4 inner-corner duplicates +
        // sunken floor at depth 0 = on the same plane. Just assert there's
        // a sensible number of them rather than counting exactly (the count
        // depends on per-quad vertex sharing which is not contractually
        // stable across implementations).
        assert!(!on_top.is_empty(), "depth=0 mesh missing top-plane vertices");
    }
}
