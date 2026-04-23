use std::collections::HashMap;
use std::f32::consts::{PI, TAU};

use glam::Vec3;
use mgen_core::Mesh;

use crate::cleanup::recompute_normals;

pub fn box_mesh(size: [f32; 3]) -> Mesh {
    let hx = size[0] * 0.5;
    let hy = size[1] * 0.5;
    let hz = size[2] * 0.5;

    let faces: [([f32; 3], [[f32; 3]; 4]); 6] = [
        ([1.0, 0.0, 0.0],  [[ hx, -hy, -hz], [ hx,  hy, -hz], [ hx,  hy,  hz], [ hx, -hy,  hz]]),
        ([-1.0, 0.0, 0.0], [[-hx, -hy,  hz], [-hx,  hy,  hz], [-hx,  hy, -hz], [-hx, -hy, -hz]]),
        ([0.0, 1.0, 0.0],  [[-hx,  hy,  hz], [ hx,  hy,  hz], [ hx,  hy, -hz], [-hx,  hy, -hz]]),
        ([0.0, -1.0, 0.0], [[-hx, -hy, -hz], [ hx, -hy, -hz], [ hx, -hy,  hz], [-hx, -hy,  hz]]),
        ([0.0, 0.0, 1.0],  [[-hx, -hy,  hz], [ hx, -hy,  hz], [ hx,  hy,  hz], [-hx,  hy,  hz]]),
        ([0.0, 0.0, -1.0], [[ hx, -hy, -hz], [-hx, -hy, -hz], [-hx,  hy, -hz], [ hx,  hy, -hz]]),
    ];

    let mut positions = Vec::with_capacity(24);
    let mut normals = Vec::with_capacity(24);
    let mut uvs = Vec::with_capacity(24);
    let mut indices = Vec::with_capacity(36);
    // Each face occupies the full [0,1]² in UV space. Quad corners are wound
    // [0,1,2,3] → [0,0],[1,0],[1,1],[0,1] so U and V align with the first
    // edge and its perpendicular respectively.
    const QUAD_UVS: [[f32; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
    for (normal, verts) in faces {
        let base = positions.len() as u32;
        for (i, v) in verts.into_iter().enumerate() {
            positions.push(v);
            normals.push(normal);
            uvs.push(QUAD_UVS[i]);
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    Mesh { positions, normals, uvs, indices, ..Default::default() }
}

/// XZ plane centered at origin, facing +Y.
pub fn plane_mesh(size: [f32; 2]) -> Mesh {
    let hx = size[0] * 0.5;
    let hz = size[1] * 0.5;
    let n = [0.0, 1.0, 0.0];
    Mesh {
        positions: vec![[-hx, 0.0, -hz], [hx, 0.0, -hz], [hx, 0.0, hz], [-hx, 0.0, hz]],
        normals: vec![n; 4],
        // U follows +X, V follows +Z so "up" in texture space matches "back" in world space.
        uvs: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        indices: vec![0, 3, 2, 0, 2, 1],
        ..Default::default()
    }
}

/// XY quad centered at origin, facing +Z. Useful as a billboard / decal plane,
/// complements `plane_mesh` which lies flat.
pub fn quad_mesh(size: [f32; 2]) -> Mesh {
    let hx = size[0] * 0.5;
    let hy = size[1] * 0.5;
    let n = [0.0, 0.0, 1.0];
    Mesh {
        positions: vec![[-hx, -hy, 0.0], [hx, -hy, 0.0], [hx, hy, 0.0], [-hx, hy, 0.0]],
        normals: vec![n; 4],
        uvs: vec![[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]],
        indices: vec![0, 1, 2, 0, 2, 3],
        ..Default::default()
    }
}

/// Axis-aligned cylinder along Y, centered at origin.
pub fn cylinder_mesh(radius: f32, height: f32, segments: u32) -> Mesh {
    let segments = segments.max(3);
    let hy = height * 0.5;
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut uvs = Vec::new();
    let mut indices = Vec::new();

    // Side wall — duplicated ring verts so side normals are radial. U wraps
    // around the cylinder (0..1 over the full circumference); V = 0 at the
    // bottom ring, V = 1 at the top. Duplicated seam vert at i=segments has
    // U=1 so the texture closes without mirroring.
    let side_start = positions.len() as u32;
    for i in 0..=segments {
        let t = i as f32 / segments as f32;
        let a = t * TAU;
        let (sa, ca) = (a.sin(), a.cos());
        let n = [ca, 0.0, sa];
        positions.push([ca * radius, -hy, sa * radius]);
        normals.push(n);
        uvs.push([t, 0.0]);
        positions.push([ca * radius,  hy, sa * radius]);
        normals.push(n);
        uvs.push([t, 1.0]);
    }
    for i in 0..segments {
        let base = side_start + i * 2;
        // Wound CCW when viewed from outside so face normals point radially
        // outward — required for backface culling and boolean CSG.
        indices.extend_from_slice(&[base, base + 1, base + 3, base, base + 3, base + 2]);
    }

    // Top cap (fan around center, CCW from +Y). UV is a planar projection
    // onto the disc: centre = (0.5, 0.5), rim = unit circle.
    let top_center = positions.len() as u32;
    positions.push([0.0, hy, 0.0]);
    normals.push([0.0, 1.0, 0.0]);
    uvs.push([0.5, 0.5]);
    for i in 0..=segments {
        let a = (i as f32 / segments as f32) * TAU;
        let (sa, ca) = (a.sin(), a.cos());
        positions.push([ca * radius, hy, sa * radius]);
        normals.push([0.0, 1.0, 0.0]);
        uvs.push([ca * 0.5 + 0.5, sa * 0.5 + 0.5]);
    }
    for i in 0..segments {
        indices.extend_from_slice(&[top_center, top_center + 2 + i, top_center + 1 + i]);
    }

    // Bottom cap (CCW from -Y).
    let bot_center = positions.len() as u32;
    positions.push([0.0, -hy, 0.0]);
    normals.push([0.0, -1.0, 0.0]);
    uvs.push([0.5, 0.5]);
    for i in 0..=segments {
        let a = (i as f32 / segments as f32) * TAU;
        let (sa, ca) = (a.sin(), a.cos());
        positions.push([ca * radius, -hy, sa * radius]);
        normals.push([0.0, -1.0, 0.0]);
        uvs.push([ca * 0.5 + 0.5, sa * 0.5 + 0.5]);
    }
    for i in 0..segments {
        indices.extend_from_slice(&[bot_center, bot_center + 1 + i, bot_center + 2 + i]);
    }

    Mesh { positions, normals, uvs, indices, ..Default::default() }
}

/// Cone with apex at +height/2 and base at -height/2, aligned to Y.
pub fn cone_mesh(radius: f32, height: f32, segments: u32) -> Mesh {
    let segments = segments.max(3);
    let hy = height * 0.5;
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut uvs = Vec::new();
    let mut indices = Vec::new();

    let slant = (radius * radius + height * height).sqrt();
    let ny_side = radius / slant;
    let nh_side = height / slant;

    let side_start = positions.len() as u32;
    for i in 0..=segments {
        let t = i as f32 / segments as f32;
        let a = t * TAU;
        let (sa, ca) = (a.sin(), a.cos());
        // Unique apex per segment so side normals can differ around the cone.
        positions.push([ca * radius, -hy, sa * radius]);
        normals.push([ca * nh_side, ny_side, sa * nh_side]);
        uvs.push([t, 0.0]);
        positions.push([0.0, hy, 0.0]);
        normals.push([ca * nh_side, ny_side, sa * nh_side]);
        uvs.push([t, 1.0]);
    }
    for i in 0..segments {
        let base = side_start + i * 2;
        // base=bottom_i, base+1=apex, base+2=bottom_(i+1).
        // CCW from outside = bottom_i → apex → bottom_(i+1).
        indices.extend_from_slice(&[base, base + 1, base + 2]);
    }

    // Bottom cap (CCW from -Y so face normal is -Y). Disc UV projection.
    let bot_center = positions.len() as u32;
    positions.push([0.0, -hy, 0.0]);
    normals.push([0.0, -1.0, 0.0]);
    uvs.push([0.5, 0.5]);
    for i in 0..=segments {
        let a = (i as f32 / segments as f32) * TAU;
        let (sa, ca) = (a.sin(), a.cos());
        positions.push([ca * radius, -hy, sa * radius]);
        normals.push([0.0, -1.0, 0.0]);
        uvs.push([ca * 0.5 + 0.5, sa * 0.5 + 0.5]);
    }
    for i in 0..segments {
        indices.extend_from_slice(&[bot_center, bot_center + 1 + i, bot_center + 2 + i]);
    }

    Mesh { positions, normals, uvs, indices, ..Default::default() }
}

/// UV sphere centered at origin.
pub fn sphere_mesh(radius: f32, rings: u32, segments: u32) -> Mesh {
    let rings = rings.max(2);
    let segments = segments.max(3);
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut uvs = Vec::new();
    let mut indices = Vec::new();

    for ring in 0..=rings {
        let v = ring as f32 / rings as f32;
        let phi = v * PI; // 0 .. PI (north to south)
        let y = phi.cos();
        let r = phi.sin();
        for seg in 0..=segments {
            let u = seg as f32 / segments as f32;
            let theta = u * TAU;
            let x = r * theta.cos();
            let z = r * theta.sin();
            positions.push([x * radius, y * radius, z * radius]);
            normals.push([x, y, z]);
            // Equirectangular: U wraps longitudinally, V = 0 at north pole.
            uvs.push([u, v]);
        }
    }

    let row = segments + 1;
    for ring in 0..rings {
        for seg in 0..segments {
            let a = ring * row + seg;
            let b = a + row;
            indices.extend_from_slice(&[a, a + 1, b + 1, a, b + 1, b]);
        }
    }

    Mesh { positions, normals, uvs, indices, ..Default::default() }
}

/// Capsule aligned to Y: cylindrical body of length `height` with hemispherical
/// caps of `radius`. Total height = `height + 2 * radius`. `rings` is the
/// latitude count per hemisphere.
pub fn capsule_mesh(radius: f32, height: f32, rings: u32, segments: u32) -> Mesh {
    let rings = rings.max(2);
    let segments = segments.max(3);
    let hy = height * 0.5;
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut uvs = Vec::new();
    let mut indices = Vec::new();

    let row = segments + 1;
    let rows_per_hemi = rings + 1;
    // Arc-length fraction occupied by each hemisphere vs the cylinder body —
    // keeps V continuous (no pinch at the equator) when the body is short or
    // long. Shared denominator = 2r*(π/2) + height.
    let hemi_arc = radius * PI * 0.5;
    let total_arc = (2.0 * hemi_arc + height).max(1e-6);
    let v_top_equator = hemi_arc / total_arc;
    let v_bottom_equator = (hemi_arc + height) / total_arc;

    // Top hemisphere: phi in [0, PI/2]. y_unit = cos(phi), r_unit = sin(phi).
    // Shift verts up by +hy so the hemisphere sits on top of the cylinder body.
    // Spherical normals at the equator (phi=PI/2) are radial, matching the
    // cylinder body's normals automatically.
    for r in 0..rows_per_hemi {
        let frac = r as f32 / rings as f32;
        let phi = frac * (PI * 0.5);
        let y_unit = phi.cos();
        let r_unit = phi.sin();
        for s in 0..=segments {
            let u = s as f32 / segments as f32;
            let theta = u * TAU;
            let cx = theta.cos();
            let sz = theta.sin();
            positions.push([cx * r_unit * radius, y_unit * radius + hy, sz * r_unit * radius]);
            normals.push([cx * r_unit, y_unit, sz * r_unit]);
            uvs.push([u, frac * v_top_equator]);
        }
    }
    // Bottom hemisphere: phi in [PI/2, PI], shifted down by -hy.
    for r in 0..rows_per_hemi {
        let frac = r as f32 / rings as f32;
        let phi = (PI * 0.5) + frac * (PI * 0.5);
        let y_unit = phi.cos();
        let r_unit = phi.sin();
        for s in 0..=segments {
            let u = s as f32 / segments as f32;
            let theta = u * TAU;
            let cx = theta.cos();
            let sz = theta.sin();
            positions.push([cx * r_unit * radius, y_unit * radius - hy, sz * r_unit * radius]);
            normals.push([cx * r_unit, y_unit, sz * r_unit]);
            uvs.push([u, v_bottom_equator + frac * (1.0 - v_bottom_equator)]);
        }
    }

    // Strip between every pair of adjacent latitudes — including the transition
    // between top equator and bottom equator, which forms the cylinder body.
    let total_rings = 2 * rows_per_hemi;
    for r in 0..(total_rings - 1) {
        for s in 0..segments {
            let a = r * row + s;
            let b = a + row;
            indices.extend_from_slice(&[a, a + 1, b + 1, a, b + 1, b]);
        }
    }

    Mesh { positions, normals, uvs, indices, ..Default::default() }
}

/// Torus lying flat in the XZ plane. `major_radius` is the distance from the
/// torus centre to the centre of the tube; `minor_radius` is the tube radius.
pub fn torus_mesh(
    major_radius: f32,
    minor_radius: f32,
    major_segments: u32,
    minor_segments: u32,
) -> Mesh {
    let major_segments = major_segments.max(3);
    let minor_segments = minor_segments.max(3);
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut uvs = Vec::new();
    let mut indices = Vec::new();

    let v_row = minor_segments + 1;
    for i in 0..=major_segments {
        let u = i as f32 / major_segments as f32;
        let phi = u * TAU;
        let (sp, cp) = (phi.sin(), phi.cos());
        for j in 0..=minor_segments {
            let v = j as f32 / minor_segments as f32;
            let theta = v * TAU;
            let (st, ct) = (theta.sin(), theta.cos());
            // Outward normal: radial in the tube's cross-section plane.
            let nx = cp * ct;
            let ny = st;
            let nz = sp * ct;
            let rx = major_radius + minor_radius * ct;
            positions.push([cp * rx, ny * minor_radius, sp * rx]);
            normals.push([nx, ny, nz]);
            uvs.push([u, v]);
        }
    }
    // Wind (a, b, c, a, c, d) with b along +v, d along +u, so triangle normals
    // match the outward tube normal (opposite of du × dv).
    for i in 0..major_segments {
        for j in 0..minor_segments {
            let a = i * v_row + j;
            let b = a + 1;
            let d = a + v_row;
            let c = d + 1;
            indices.extend_from_slice(&[a, b, c, a, c, d]);
        }
    }

    Mesh { positions, normals, uvs, indices, ..Default::default() }
}

/// Symmetric isoceles triangular prism. `size = [width_x, height_y, depth_z]`.
/// Base sits on -Y at y = -h/2, apex is a ridge along +Z at y = +h/2.
pub fn prism_mesh(size: [f32; 3]) -> Mesh {
    let hx = size[0] * 0.5;
    let hy = size[1] * 0.5;
    let hz = size[2] * 0.5;
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    let slant_len = ((2.0 * hy).powi(2) + hx.powi(2)).sqrt().max(1e-6);
    let n_left = [-(2.0 * hy) / slant_len, hx / slant_len, 0.0];
    let n_right = [(2.0 * hy) / slant_len, hx / slant_len, 0.0];

    // Each face: push its verts with per-face normals and UVs, then fan-triangulate.
    let push_face = |verts: &[[f32; 3]], n: [f32; 3], face_uvs: &[[f32; 2]],
                         positions: &mut Vec<[f32; 3]>,
                         normals: &mut Vec<[f32; 3]>,
                         uvs: &mut Vec<[f32; 2]>,
                         indices: &mut Vec<u32>| {
        let base = positions.len() as u32;
        for (v, uv) in verts.iter().zip(face_uvs.iter()) {
            positions.push(*v);
            normals.push(n);
            uvs.push(*uv);
        }
        for i in 1..(verts.len() as u32 - 1) {
            indices.extend_from_slice(&[base, base + i, base + i + 1]);
        }
    };

    // Back triangle (normal -Z). Triangle apex at (0.5, 1.0).
    push_face(
        &[[-hx, -hy, -hz], [0.0, hy, -hz], [hx, -hy, -hz]],
        [0.0, 0.0, -1.0],
        &[[0.0, 0.0], [0.5, 1.0], [1.0, 0.0]],
        &mut positions, &mut normals, &mut uvs, &mut indices,
    );
    // Front triangle (normal +Z).
    push_face(
        &[[-hx, -hy, hz], [hx, -hy, hz], [0.0, hy, hz]],
        [0.0, 0.0, 1.0],
        &[[0.0, 0.0], [1.0, 0.0], [0.5, 1.0]],
        &mut positions, &mut normals, &mut uvs, &mut indices,
    );
    // Bottom quad (normal -Y).
    push_face(
        &[[-hx, -hy, -hz], [hx, -hy, -hz], [hx, -hy, hz], [-hx, -hy, hz]],
        [0.0, -1.0, 0.0],
        &[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        &mut positions, &mut normals, &mut uvs, &mut indices,
    );
    // Left slant quad.
    push_face(
        &[[-hx, -hy, -hz], [-hx, -hy, hz], [0.0, hy, hz], [0.0, hy, -hz]],
        n_left,
        &[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        &mut positions, &mut normals, &mut uvs, &mut indices,
    );
    // Right slant quad.
    push_face(
        &[[hx, -hy, -hz], [0.0, hy, -hz], [0.0, hy, hz], [hx, -hy, hz]],
        n_right,
        &[[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]],
        &mut positions, &mut normals, &mut uvs, &mut indices,
    );

    Mesh { positions, normals, uvs, indices, ..Default::default() }
}

/// Regular N-sided pyramid: base polygon on -Y circumscribed by `radius`,
/// apex directly above the centre at +height/2.
pub fn pyramid_mesh(radius: f32, height: f32, sides: u32) -> Mesh {
    let sides = sides.max(3);
    let hy = height * 0.5;
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    let mut base_verts: Vec<[f32; 3]> = Vec::with_capacity(sides as usize);
    for i in 0..sides {
        let a = (i as f32 / sides as f32) * TAU;
        base_verts.push([a.cos() * radius, -hy, a.sin() * radius]);
    }

    // Base face (normal -Y) with disc UV projection.
    let base_start = positions.len() as u32;
    for (i, v) in base_verts.iter().enumerate() {
        positions.push(*v);
        normals.push([0.0, -1.0, 0.0]);
        let a = (i as f32 / sides as f32) * TAU;
        uvs.push([a.cos() * 0.5 + 0.5, a.sin() * 0.5 + 0.5]);
    }
    for i in 1..(sides - 1) {
        indices.extend_from_slice(&[base_start, base_start + i, base_start + i + 1]);
    }

    // Side faces: each triangular face gets its own [0,1] square, with the apex
    // at U=0.5 and the two base corners at U=0 and U=1.
    let apex = [0.0, hy, 0.0];
    for i in 0..sides {
        let j = (i + 1) % sides;
        let v0 = base_verts[i as usize];
        let v1 = base_verts[j as usize];
        let e1 = [apex[0] - v0[0], apex[1] - v0[1], apex[2] - v0[2]];
        let e2 = [v1[0] - v0[0], v1[1] - v0[1], v1[2] - v0[2]];
        let n = [
            e1[1] * e2[2] - e1[2] * e2[1],
            e1[2] * e2[0] - e1[0] * e2[2],
            e1[0] * e2[1] - e1[1] * e2[0],
        ];
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-6);
        let n = [n[0] / len, n[1] / len, n[2] / len];

        let base_idx = positions.len() as u32;
        positions.push(v0); normals.push(n); uvs.push([0.0, 0.0]);
        positions.push(apex); normals.push(n); uvs.push([0.5, 1.0]);
        positions.push(v1); normals.push(n); uvs.push([1.0, 0.0]);
        indices.extend_from_slice(&[base_idx, base_idx + 1, base_idx + 2]);
    }

    Mesh { positions, normals, uvs, indices, ..Default::default() }
}

/// Flat disc lying on the XZ plane, facing +Y. One-sided; CSG on a disc alone
/// is ill-defined but it is handy as a terminator cap or decal.
pub fn disc_mesh(radius: f32, segments: u32) -> Mesh {
    let segments = segments.max(3);
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut uvs = Vec::new();
    let mut indices = Vec::new();
    let n = [0.0, 1.0, 0.0];

    let center = positions.len() as u32;
    positions.push([0.0, 0.0, 0.0]);
    normals.push(n);
    uvs.push([0.5, 0.5]);
    for i in 0..=segments {
        let a = (i as f32 / segments as f32) * TAU;
        let (sa, ca) = (a.sin(), a.cos());
        positions.push([ca * radius, 0.0, sa * radius]);
        normals.push(n);
        uvs.push([ca * 0.5 + 0.5, sa * 0.5 + 0.5]);
    }
    // Winding matches cylinder's +Y cap: centre → next_ring → this_ring.
    for i in 0..segments {
        indices.extend_from_slice(&[center, center + 2 + i, center + 1 + i]);
    }

    Mesh { positions, normals, uvs, indices, ..Default::default() }
}

/// Geodesic sphere built by subdividing an icosahedron. Produces more uniform
/// triangle area than `sphere_mesh` and avoids UV sphere pole pinching.
pub fn icosphere_mesh(radius: f32, subdivisions: u32) -> Mesh {
    let phi = (1.0 + 5.0_f32.sqrt()) * 0.5;
    let inv = 1.0 / (1.0 + phi * phi).sqrt();
    let mut verts: Vec<[f32; 3]> = [
        [-1.0,  phi, 0.0], [ 1.0,  phi, 0.0], [-1.0, -phi, 0.0], [ 1.0, -phi, 0.0],
        [ 0.0, -1.0,  phi], [ 0.0,  1.0,  phi], [ 0.0, -1.0, -phi], [ 0.0,  1.0, -phi],
        [ phi, 0.0, -1.0], [ phi, 0.0,  1.0], [-phi, 0.0, -1.0], [-phi, 0.0,  1.0],
    ]
    .iter()
    .map(|v| [v[0] * inv, v[1] * inv, v[2] * inv])
    .collect();

    // 20 canonical icosahedron faces, wound CCW viewed from outside.
    let mut faces: Vec<[u32; 3]> = vec![
        [0, 11, 5], [0, 5, 1], [0, 1, 7], [0, 7, 10], [0, 10, 11],
        [1, 5, 9], [5, 11, 4], [11, 10, 2], [10, 7, 6], [7, 1, 8],
        [3, 9, 4], [3, 4, 2], [3, 2, 6], [3, 6, 8], [3, 8, 9],
        [4, 9, 5], [2, 4, 11], [6, 2, 10], [8, 6, 7], [9, 8, 1],
    ];

    for _ in 0..subdivisions {
        let mut cache: HashMap<(u32, u32), u32> = HashMap::new();
        let mut next = Vec::with_capacity(faces.len() * 4);
        for f in &faces {
            let ab = icosphere_midpoint(&mut verts, &mut cache, f[0], f[1]);
            let bc = icosphere_midpoint(&mut verts, &mut cache, f[1], f[2]);
            let ca = icosphere_midpoint(&mut verts, &mut cache, f[2], f[0]);
            next.push([f[0], ab, ca]);
            next.push([f[1], bc, ab]);
            next.push([f[2], ca, bc]);
            next.push([ab, bc, ca]);
        }
        faces = next;
    }

    let positions: Vec<[f32; 3]> =
        verts.iter().map(|v| [v[0] * radius, v[1] * radius, v[2] * radius]).collect();
    let normals = verts.clone();
    // Equirectangular UVs derived from per-vertex unit normals. Has a known
    // seam at theta=0 and pole pinching at y=±1 — standard for a UV sphere
    // and acceptable for a first-pass texture pipeline.
    let uvs: Vec<[f32; 2]> = verts
        .iter()
        .map(|v| {
            let u = v[2].atan2(v[0]) / TAU + 0.5;
            let vv = v[1].clamp(-1.0, 1.0).acos() / PI;
            [u, vv]
        })
        .collect();
    let indices: Vec<u32> = faces.into_iter().flat_map(|f| f.into_iter()).collect();

    Mesh { positions, normals, uvs, indices, ..Default::default() }
}

fn icosphere_midpoint(
    verts: &mut Vec<[f32; 3]>,
    cache: &mut HashMap<(u32, u32), u32>,
    a: u32,
    b: u32,
) -> u32 {
    let key = if a < b { (a, b) } else { (b, a) };
    if let Some(&i) = cache.get(&key) {
        return i;
    }
    let va = verts[a as usize];
    let vb = verts[b as usize];
    let m = [(va[0] + vb[0]) * 0.5, (va[1] + vb[1]) * 0.5, (va[2] + vb[2]) * 0.5];
    let len = (m[0] * m[0] + m[1] * m[1] + m[2] * m[2]).sqrt().max(1e-12);
    let idx = verts.len() as u32;
    verts.push([m[0] / len, m[1] / len, m[2] / len]);
    cache.insert(key, idx);
    idx
}

/// Box of `size` with corners rounded to `radius`. Built as the Minkowski sum
/// of an interior "core" box with a sphere: six flat face rectangles, twelve
/// quarter-cylinder edge strips, and eight spherical-octant corner patches.
/// Seams along the interior-box silhouette are vertex-duplicated, but normals
/// at the seam match on both sides so shading is smooth.
pub fn rounded_box_mesh(size: [f32; 3], radius: f32, segments: u32) -> Mesh {
    let sx = size[0].max(0.0);
    let sy = size[1].max(0.0);
    let sz = size[2].max(0.0);
    let r = radius.min(sx * 0.5).min(sy * 0.5).min(sz * 0.5).max(0.0);
    if r < 1e-6 {
        return box_mesh([sx, sy, sz]);
    }
    let hx = sx * 0.5;
    let hy = sy * 0.5;
    let hz = sz * 0.5;
    let cx = hx - r;
    let cy = hy - r;
    let cz = hz - r;
    let seg = segments.max(1);

    let mut mesh = Mesh::default();

    // Six flat face rectangles (shrunken from full size so they don't bleed
    // past the rounded edges).
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

    // Twelve quarter-cylinder edges, one per (axis, perp1_sign, perp2_sign).
    let edges: [(usize, f32, f32); 12] = [
        (0,  1.0,  1.0), (0, -1.0,  1.0), (0, -1.0, -1.0), (0,  1.0, -1.0),
        (1,  1.0,  1.0), (1, -1.0,  1.0), (1, -1.0, -1.0), (1,  1.0, -1.0),
        (2,  1.0,  1.0), (2, -1.0,  1.0), (2, -1.0, -1.0), (2,  1.0, -1.0),
    ];
    for (axis, s1, s2) in edges {
        let (along_half, cp1, cp2) = match axis {
            0 => (cx, cy, cz),
            1 => (cy, cx, cz),
            2 => (cz, cx, cy),
            _ => unreachable!(),
        };
        let rows = (seg + 1) as usize;
        let cols = 2usize;
        let mut patch_pos: Vec<[f32; 3]> = Vec::with_capacity(rows * cols);
        let mut patch_n: Vec<[f32; 3]> = Vec::with_capacity(rows * cols);
        for ri in 0..=seg {
            let t = (ri as f32 / seg as f32) * (PI * 0.5);
            let (sin_t, cos_t) = t.sin_cos();
            let np1 = s1 * cos_t;
            let np2 = s2 * sin_t;
            for c in 0..cols {
                let along = if c == 0 { -along_half } else { along_half };
                let (pos, n) = match axis {
                    0 => (
                        [along, s1 * cp1 + r * np1, s2 * cp2 + r * np2],
                        [0.0, np1, np2],
                    ),
                    1 => (
                        [s1 * cp1 + r * np1, along, s2 * cp2 + r * np2],
                        [np1, 0.0, np2],
                    ),
                    2 => (
                        [s1 * cp1 + r * np1, s2 * cp2 + r * np2, along],
                        [np1, np2, 0.0],
                    ),
                    _ => unreachable!(),
                };
                patch_pos.push(pos);
                patch_n.push(n);
            }
        }
        push_patch(&mut mesh, &patch_pos, &patch_n, rows, cols);
    }

    // Eight sphere-octant corner patches.
    let corners: [[f32; 3]; 8] = [
        [ 1.0,  1.0,  1.0], [-1.0,  1.0,  1.0], [-1.0, -1.0,  1.0], [ 1.0, -1.0,  1.0],
        [ 1.0,  1.0, -1.0], [-1.0,  1.0, -1.0], [-1.0, -1.0, -1.0], [ 1.0, -1.0, -1.0],
    ];
    for s in corners {
        let sx_s = s[0]; let sy_s = s[1]; let sz_s = s[2];
        let rows = (seg + 1) as usize;
        let cols = (seg + 1) as usize;
        let mut patch_pos: Vec<[f32; 3]> = Vec::with_capacity(rows * cols);
        let mut patch_n: Vec<[f32; 3]> = Vec::with_capacity(rows * cols);
        for ri in 0..=seg {
            let phi = (ri as f32 / seg as f32) * (PI * 0.5);
            let (sin_phi, cos_phi) = phi.sin_cos();
            for ci in 0..=seg {
                let theta = (ci as f32 / seg as f32) * (PI * 0.5);
                let (sin_th, cos_th) = theta.sin_cos();
                let nx = sx_s * sin_phi * cos_th;
                let ny = sy_s * cos_phi;
                let nz = sz_s * sin_phi * sin_th;
                patch_pos.push([sx_s * cx + r * nx, sy_s * cy + r * ny, sz_s * cz + r * nz]);
                patch_n.push([nx, ny, nz]);
            }
        }
        push_patch(&mut mesh, &patch_pos, &patch_n, rows, cols);
    }

    // Triplanar UV projection: each vertex picks the dominant normal axis and
    // projects its position onto the remaining two. Box-like surfaces get
    // predictable per-face unwraps; rounded edges and corners blend smoothly
    // because neighbouring verts pick the same axis until the normal crosses
    // 45°.
    mesh.uvs = triplanar_uvs_for_box(&mesh.positions, &mesh.normals, [sx, sy, sz]);
    mesh
}

/// Compute triplanar UVs mapped into [0, 1]² based on the bounding extent of
/// each axis. For a vertex whose normal is dominantly +X or -X, project onto
/// (Z, Y) normalized by (`size_z`, `size_y`); likewise for Y and Z dominance.
fn triplanar_uvs_for_box(positions: &[[f32; 3]], normals: &[[f32; 3]], size: [f32; 3]) -> Vec<[f32; 2]> {
    let [sx, sy, sz] = size;
    let ix = if sx > 1e-6 { 1.0 / sx } else { 1.0 };
    let iy = if sy > 1e-6 { 1.0 / sy } else { 1.0 };
    let iz = if sz > 1e-6 { 1.0 / sz } else { 1.0 };
    positions
        .iter()
        .zip(normals.iter())
        .map(|(p, n)| {
            let (ax, ay, az) = (n[0].abs(), n[1].abs(), n[2].abs());
            if ax >= ay && ax >= az {
                [(p[2] * iz) + 0.5, (p[1] * iy) + 0.5]
            } else if ay >= az {
                [(p[0] * ix) + 0.5, (p[2] * iz) + 0.5]
            } else {
                [(p[0] * ix) + 0.5, (p[1] * iy) + 0.5]
            }
        })
        .collect()
}

/// Right triangular prism ("doorstop" wedge). `size = [x, y, z]`: bottom
/// rectangle sits on -Y (full x×z), back wall is at -Z (tall, reaching to +hy).
/// The top edge is a single line at y=+hy, z=-hz, so the slope rises from the
/// front-bottom edge (at y=-hy, z=+hz) up to the back-top edge. Slope face
/// normal has +Y and +Z components.
pub fn wedge_mesh(size: [f32; 3]) -> Mesh {
    let hx = size[0] * 0.5;
    let hy = size[1] * 0.5;
    let hz = size[2] * 0.5;
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    let push_face = |verts: &[[f32; 3]], n: [f32; 3], face_uvs: &[[f32; 2]],
                     positions: &mut Vec<[f32; 3]>,
                     normals: &mut Vec<[f32; 3]>,
                     uvs: &mut Vec<[f32; 2]>,
                     indices: &mut Vec<u32>| {
        let base = positions.len() as u32;
        for (v, uv) in verts.iter().zip(face_uvs.iter()) {
            positions.push(*v);
            normals.push(n);
            uvs.push(*uv);
        }
        for i in 1..(verts.len() as u32 - 1) {
            indices.extend_from_slice(&[base, base + i, base + i + 1]);
        }
    };

    // Back wall (rectangle, normal -Z).
    push_face(
        &[[-hx, -hy, -hz], [-hx, hy, -hz], [hx, hy, -hz], [hx, -hy, -hz]],
        [0.0, 0.0, -1.0],
        &[[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]],
        &mut positions, &mut normals, &mut uvs, &mut indices,
    );
    // Bottom (rectangle, normal -Y).
    push_face(
        &[[-hx, -hy, -hz], [hx, -hy, -hz], [hx, -hy, hz], [-hx, -hy, hz]],
        [0.0, -1.0, 0.0],
        &[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        &mut positions, &mut normals, &mut uvs, &mut indices,
    );
    // Left triangle (normal -X).
    push_face(
        &[[-hx, -hy, -hz], [-hx, -hy, hz], [-hx, hy, -hz]],
        [-1.0, 0.0, 0.0],
        &[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
        &mut positions, &mut normals, &mut uvs, &mut indices,
    );
    // Right triangle (normal +X).
    push_face(
        &[[hx, -hy, -hz], [hx, hy, -hz], [hx, -hy, hz]],
        [1.0, 0.0, 0.0],
        &[[0.0, 0.0], [0.0, 1.0], [1.0, 0.0]],
        &mut positions, &mut normals, &mut uvs, &mut indices,
    );
    // Slope (the hypotenuse face). Normal has +Y and +Z components.
    let slant_len = ((2.0 * hy).powi(2) + (2.0 * hz).powi(2)).sqrt().max(1e-6);
    let n_slope = [0.0, (2.0 * hz) / slant_len, (2.0 * hy) / slant_len];
    push_face(
        &[[-hx, hy, -hz], [-hx, -hy, hz], [hx, -hy, hz], [hx, hy, -hz]],
        n_slope,
        &[[0.0, 1.0], [0.0, 0.0], [1.0, 0.0], [1.0, 1.0]],
        &mut positions, &mut normals, &mut uvs, &mut indices,
    );

    Mesh { positions, normals, uvs, indices, ..Default::default() }
}

/// Tapered box (rectangular frustum). Bottom rectangle `bottom = [x, z]` sits
/// on y=-height/2; top rectangle `top = [x, z]` sits on y=+height/2. Both
/// rectangles are centered on the Y axis. Either end may be larger.
pub fn frustum_mesh(bottom: [f32; 2], top: [f32; 2], height: f32) -> Mesh {
    let hy = height * 0.5;
    let (bx, bz) = (bottom[0] * 0.5, bottom[1] * 0.5);
    let (tx, tz) = (top[0] * 0.5, top[1] * 0.5);

    // Eight corners.
    let b_bl = [-bx, -hy, -bz];
    let b_br = [ bx, -hy, -bz];
    let b_fr = [ bx, -hy,  bz];
    let b_fl = [-bx, -hy,  bz];
    let t_bl = [-tx,  hy, -tz];
    let t_br = [ tx,  hy, -tz];
    let t_fr = [ tx,  hy,  tz];
    let t_fl = [-tx,  hy,  tz];

    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    let push_quad = |quad: [[f32; 3]; 4], n: [f32; 3],
                     positions: &mut Vec<[f32; 3]>,
                     normals: &mut Vec<[f32; 3]>,
                     uvs: &mut Vec<[f32; 2]>,
                     indices: &mut Vec<u32>| {
        let base = positions.len() as u32;
        const UVS: [[f32; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        for (i, v) in quad.iter().enumerate() {
            positions.push(*v);
            normals.push(n);
            uvs.push(UVS[i]);
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    };

    fn normalize(n: [f32; 3]) -> [f32; 3] {
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-6);
        [n[0] / len, n[1] / len, n[2] / len]
    }

    // Top cap (+Y).
    push_quad([t_fl, t_fr, t_br, t_bl], [0.0, 1.0, 0.0],
              &mut positions, &mut normals, &mut uvs, &mut indices);
    // Bottom cap (-Y), wound opposite.
    push_quad([b_bl, b_br, b_fr, b_fl], [0.0, -1.0, 0.0],
              &mut positions, &mut normals, &mut uvs, &mut indices);

    // Right (+X) slant: face spans from base x=bx to top x=tx over height in y.
    // Outward normal has +X from height, and -(tx-bx) in y (if top narrows).
    let n_right = normalize([height, -(tx - bx), 0.0]);
    push_quad([t_br, t_fr, b_fr, b_br], n_right,
              &mut positions, &mut normals, &mut uvs, &mut indices);
    // Left (-X) slant.
    let n_left = normalize([-height, -(tx - bx), 0.0]);
    push_quad([t_fl, t_bl, b_bl, b_fl], n_left,
              &mut positions, &mut normals, &mut uvs, &mut indices);
    // Front (+Z) slant.
    let n_front = normalize([0.0, -(tz - bz), height]);
    push_quad([t_fr, t_fl, b_fl, b_fr], n_front,
              &mut positions, &mut normals, &mut uvs, &mut indices);
    // Back (-Z) slant.
    let n_back = normalize([0.0, -(tz - bz), -height]);
    push_quad([t_bl, t_br, b_br, b_bl], n_back,
              &mut positions, &mut normals, &mut uvs, &mut indices);

    Mesh { positions, normals, uvs, indices, ..Default::default() }
}

/// Hollow cylinder (pipe) aligned to Y, centered at origin. `outer` is the
/// outside radius, `inner` is the bore radius (must satisfy `inner < outer`);
/// `height` runs along Y. The mesh has four surfaces: outer wall (normals
/// radially outward), inner wall (normals radially inward), and two annular
/// end caps.
pub fn tube_mesh(outer: f32, inner: f32, height: f32, segments: u32) -> Mesh {
    let segments = segments.max(3);
    let outer = outer.max(0.0);
    let inner = inner.max(0.0).min(outer);
    let hy = height * 0.5;
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut uvs = Vec::new();
    let mut indices = Vec::new();

    // Outer wall (normals point +radial). U wraps, V goes bottom→top.
    let o_start = positions.len() as u32;
    for i in 0..=segments {
        let t = i as f32 / segments as f32;
        let a = t * TAU;
        let (sa, ca) = (a.sin(), a.cos());
        let n = [ca, 0.0, sa];
        positions.push([ca * outer, -hy, sa * outer]);
        normals.push(n);
        uvs.push([t, 0.0]);
        positions.push([ca * outer,  hy, sa * outer]);
        normals.push(n);
        uvs.push([t, 1.0]);
    }
    for i in 0..segments {
        let base = o_start + i * 2;
        indices.extend_from_slice(&[base, base + 1, base + 3, base, base + 3, base + 2]);
    }

    // Inner wall (normals point -radial). Same U wrap; V flipped so textures
    // don't mirror between the two walls.
    let i_start = positions.len() as u32;
    for i in 0..=segments {
        let t = i as f32 / segments as f32;
        let a = t * TAU;
        let (sa, ca) = (a.sin(), a.cos());
        let n = [-ca, 0.0, -sa];
        positions.push([ca * inner, -hy, sa * inner]);
        normals.push(n);
        uvs.push([t, 0.0]);
        positions.push([ca * inner,  hy, sa * inner]);
        normals.push(n);
        uvs.push([t, 1.0]);
    }
    // Wind inner wall opposite of outer so triangles face the bore centre.
    for i in 0..segments {
        let base = i_start + i * 2;
        indices.extend_from_slice(&[base, base + 3, base + 1, base, base + 2, base + 3]);
    }

    // Top annular cap (+Y). Disc projection; inner rim lands on a shrunken circle.
    let outer_s = if outer > 1e-6 { 0.5 / outer } else { 0.0 };
    let t_start = positions.len() as u32;
    for i in 0..=segments {
        let a = (i as f32 / segments as f32) * TAU;
        let (sa, ca) = (a.sin(), a.cos());
        positions.push([ca * outer, hy, sa * outer]);
        normals.push([0.0, 1.0, 0.0]);
        uvs.push([ca * outer * outer_s + 0.5, sa * outer * outer_s + 0.5]);
        positions.push([ca * inner, hy, sa * inner]);
        normals.push([0.0, 1.0, 0.0]);
        uvs.push([ca * inner * outer_s + 0.5, sa * inner * outer_s + 0.5]);
    }
    for i in 0..segments {
        let o0 = t_start + i * 2;
        let i0 = o0 + 1;
        let o1 = o0 + 2;
        let i1 = o0 + 3;
        // CCW viewed from +Y: outer_i → inner_i → inner_{i+1} → outer_{i+1}.
        indices.extend_from_slice(&[o0, i0, i1, o0, i1, o1]);
    }

    // Bottom annular cap (-Y), wound opposite.
    let b_start = positions.len() as u32;
    for i in 0..=segments {
        let a = (i as f32 / segments as f32) * TAU;
        let (sa, ca) = (a.sin(), a.cos());
        positions.push([ca * outer, -hy, sa * outer]);
        normals.push([0.0, -1.0, 0.0]);
        uvs.push([ca * outer * outer_s + 0.5, sa * outer * outer_s + 0.5]);
        positions.push([ca * inner, -hy, sa * inner]);
        normals.push([0.0, -1.0, 0.0]);
        uvs.push([ca * inner * outer_s + 0.5, sa * inner * outer_s + 0.5]);
    }
    for i in 0..segments {
        let o0 = b_start + i * 2;
        let i0 = o0 + 1;
        let o1 = o0 + 2;
        let i1 = o0 + 3;
        indices.extend_from_slice(&[o0, o1, i1, o0, i1, i0]);
    }

    Mesh { positions, normals, uvs, indices, ..Default::default() }
}

/// Half-sphere with flat base on the XZ plane at y=0 and dome rising to y=+radius.
/// Origin sits at the centre of the flat base (not the sphere centre) so the
/// primitive stacks naturally — a `bottom` connector at y=0 meets any surface.
pub fn hemisphere_mesh(radius: f32, rings: u32, segments: u32) -> Mesh {
    let rings = rings.max(2);
    let segments = segments.max(3);
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut uvs = Vec::new();
    let mut indices = Vec::new();

    // Dome: phi in [0, PI/2]. ring=0 at apex (+Y), ring=rings at equator (y=0).
    for ring in 0..=rings {
        let v = ring as f32 / rings as f32;
        let phi = v * (PI * 0.5);
        let y = phi.cos();
        let r = phi.sin();
        for seg in 0..=segments {
            let u = seg as f32 / segments as f32;
            let theta = u * TAU;
            let x = r * theta.cos();
            let z = r * theta.sin();
            positions.push([x * radius, y * radius, z * radius]);
            normals.push([x, y, z]);
            uvs.push([u, v]);
        }
    }
    let row = segments + 1;
    for ring in 0..rings {
        for seg in 0..segments {
            let a = ring * row + seg;
            let b = a + row;
            indices.extend_from_slice(&[a, a + 1, b + 1, a, b + 1, b]);
        }
    }

    // Flat base cap at y=0, normal -Y.
    let center = positions.len() as u32;
    positions.push([0.0, 0.0, 0.0]);
    normals.push([0.0, -1.0, 0.0]);
    uvs.push([0.5, 0.5]);
    for i in 0..=segments {
        let a = (i as f32 / segments as f32) * TAU;
        let (sa, ca) = (a.sin(), a.cos());
        positions.push([ca * radius, 0.0, sa * radius]);
        normals.push([0.0, -1.0, 0.0]);
        uvs.push([ca * 0.5 + 0.5, sa * 0.5 + 0.5]);
    }
    for i in 0..segments {
        // CCW from -Y (looking +Y): centre → ring_i → ring_{i+1}.
        indices.extend_from_slice(&[center, center + 1 + i, center + 2 + i]);
    }

    Mesh { positions, normals, uvs, indices, ..Default::default() }
}

/// Half of a cylinder (D-shape extrusion). Axis along Y, `height` runs along Y.
/// The cut plane is the YZ plane: the kept half is the +X side, so the flat
/// rectangular face sits on x=0 with outward normal -X. Curved surface spans
/// theta in [-PI/2, PI/2] (i.e. from -Z to +Z around +X).
pub fn half_cylinder_mesh(radius: f32, height: f32, segments: u32) -> Mesh {
    let segments = segments.max(2);
    let hy = height * 0.5;
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut uvs = Vec::new();
    let mut indices = Vec::new();

    // Curved wall, sweeping theta in [-PI/2, +PI/2] around +X.
    let side_start = positions.len() as u32;
    for i in 0..=segments {
        let t = i as f32 / segments as f32;
        let a = -PI * 0.5 + t * PI;
        let (sa, ca) = (a.sin(), a.cos());
        let n = [ca, 0.0, sa];
        positions.push([ca * radius, -hy, sa * radius]);
        normals.push(n);
        uvs.push([t, 0.0]);
        positions.push([ca * radius,  hy, sa * radius]);
        normals.push(n);
        uvs.push([t, 1.0]);
    }
    for i in 0..segments {
        let base = side_start + i * 2;
        indices.extend_from_slice(&[base, base + 1, base + 3, base, base + 3, base + 2]);
    }

    // Flat face (the rectangle at x=0, spanning Y ±hy and Z ±radius). Normal -X.
    let flat_base = positions.len() as u32;
    let flat_quad = [
        [0.0, -hy,  radius], [0.0,  hy,  radius],
        [0.0,  hy, -radius], [0.0, -hy, -radius],
    ];
    const FLAT_UVS: [[f32; 2]; 4] = [[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]];
    for (i, v) in flat_quad.into_iter().enumerate() {
        positions.push(v);
        normals.push([-1.0, 0.0, 0.0]);
        uvs.push(FLAT_UVS[i]);
    }
    indices.extend_from_slice(&[
        flat_base, flat_base + 1, flat_base + 2,
        flat_base, flat_base + 2, flat_base + 3,
    ]);

    // Top semicircle cap (+Y). Maps D-shape into the upper half of [0,1]².
    let top_center = positions.len() as u32;
    positions.push([0.0, hy, 0.0]);
    normals.push([0.0, 1.0, 0.0]);
    uvs.push([0.5, 0.5]);
    for i in 0..=segments {
        let t = i as f32 / segments as f32;
        let a = -PI * 0.5 + t * PI;
        let (sa, ca) = (a.sin(), a.cos());
        positions.push([ca * radius, hy, sa * radius]);
        normals.push([0.0, 1.0, 0.0]);
        uvs.push([sa * 0.5 + 0.5, ca * 0.5 + 0.5]);
    }
    // Sweep goes -Z→+X→+Z (theta -PI/2 to +PI/2). Viewed from +Y, that's CW
    // in (x, z); CCW from +Y requires the reverse order.
    for i in 0..segments {
        indices.extend_from_slice(&[top_center, top_center + 2 + i, top_center + 1 + i]);
    }

    // Bottom semicircle cap (-Y).
    let bot_center = positions.len() as u32;
    positions.push([0.0, -hy, 0.0]);
    normals.push([0.0, -1.0, 0.0]);
    uvs.push([0.5, 0.5]);
    for i in 0..=segments {
        let t = i as f32 / segments as f32;
        let a = -PI * 0.5 + t * PI;
        let (sa, ca) = (a.sin(), a.cos());
        positions.push([ca * radius, -hy, sa * radius]);
        normals.push([0.0, -1.0, 0.0]);
        uvs.push([sa * 0.5 + 0.5, ca * 0.5 + 0.5]);
    }
    for i in 0..segments {
        indices.extend_from_slice(&[bot_center, bot_center + 1 + i, bot_center + 2 + i]);
    }

    Mesh { positions, normals, uvs, indices, ..Default::default() }
}

/// Partial torus sweeping `arc` radians around +Y, starting from phi=0 (which
/// places the first cross-section on +X). Major/minor behave as `torus_mesh`;
/// `arc` is clamped to (0, 2π]. When `arc < 2π` the tube is closed at each end
/// with a disc cap so the mesh is watertight.
pub fn torus_arc_mesh(
    major_radius: f32,
    minor_radius: f32,
    arc: f32,
    major_segments: u32,
    minor_segments: u32,
) -> Mesh {
    let major_segments = major_segments.max(2);
    let minor_segments = minor_segments.max(3);
    let arc = arc.clamp(1e-4, TAU);
    let closed = (TAU - arc).abs() < 1e-4;
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut uvs = Vec::new();
    let mut indices = Vec::new();

    let v_row = minor_segments + 1;
    for i in 0..=major_segments {
        let u = i as f32 / major_segments as f32;
        let phi = u * arc;
        let (sp, cp) = (phi.sin(), phi.cos());
        for j in 0..=minor_segments {
            let v = j as f32 / minor_segments as f32;
            let theta = v * TAU;
            let (st, ct) = (theta.sin(), theta.cos());
            let nx = cp * ct;
            let ny = st;
            let nz = sp * ct;
            let rx = major_radius + minor_radius * ct;
            positions.push([cp * rx, ny * minor_radius, sp * rx]);
            normals.push([nx, ny, nz]);
            uvs.push([u, v]);
        }
    }
    for i in 0..major_segments {
        for j in 0..minor_segments {
            let a = i * v_row + j;
            let b = a + 1;
            let d = a + v_row;
            let c = d + 1;
            indices.extend_from_slice(&[a, b, c, a, c, d]);
        }
    }

    if !closed {
        // End cap at phi=0 (lies on +X axis; face normal -tangent = -Z). Disc UV projection.
        let start_center = positions.len() as u32;
        positions.push([major_radius, 0.0, 0.0]);
        normals.push([0.0, 0.0, -1.0]);
        uvs.push([0.5, 0.5]);
        for j in 0..=minor_segments {
            let theta = (j as f32 / minor_segments as f32) * TAU;
            let (st, ct) = (theta.sin(), theta.cos());
            positions.push([major_radius + minor_radius * ct, minor_radius * st, 0.0]);
            normals.push([0.0, 0.0, -1.0]);
            uvs.push([ct * 0.5 + 0.5, st * 0.5 + 0.5]);
        }
        for j in 0..minor_segments {
            // CCW from -Z side (outward cap): centre → ring_{j+1} → ring_j.
            indices.extend_from_slice(&[start_center, start_center + 2 + j, start_center + 1 + j]);
        }

        // End cap at phi=arc.
        let (sp, cp) = (arc.sin(), arc.cos());
        let end_center = positions.len() as u32;
        positions.push([cp * major_radius, 0.0, sp * major_radius]);
        // Cap normal = +tangent direction at phi=arc, which is (-sin phi, 0, cos phi).
        normals.push([-sp, 0.0, cp]);
        uvs.push([0.5, 0.5]);
        for j in 0..=minor_segments {
            let theta = (j as f32 / minor_segments as f32) * TAU;
            let (st, ct) = (theta.sin(), theta.cos());
            let rx = major_radius + minor_radius * ct;
            positions.push([cp * rx, minor_radius * st, sp * rx]);
            normals.push([-sp, 0.0, cp]);
            uvs.push([ct * 0.5 + 0.5, st * 0.5 + 0.5]);
        }
        for j in 0..minor_segments {
            indices.extend_from_slice(&[end_center, end_center + 1 + j, end_center + 2 + j]);
        }
    }

    Mesh { positions, normals, uvs, indices, ..Default::default() }
}

/// Axis-aligned ellipsoid with independent radii along X/Y/Z. `size = [x,y,z]`
/// is the bounding diameter on each axis; radii = size * 0.5. Normals are
/// computed from the implicit surface gradient so shading is correct even when
/// the axes differ.
pub fn ellipsoid_mesh(size: [f32; 3], rings: u32, segments: u32) -> Mesh {
    let rx = size[0] * 0.5;
    let ry = size[1] * 0.5;
    let rz = size[2] * 0.5;
    let rings = rings.max(2);
    let segments = segments.max(3);
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut uvs = Vec::new();
    let mut indices = Vec::new();

    // Inverse-squared radii for implicit-surface gradient normals:
    //   f(x,y,z) = (x/rx)^2 + (y/ry)^2 + (z/rz)^2 − 1
    //   grad f   = (2x/rx^2, 2y/ry^2, 2z/rz^2)
    let inv_rx2 = 1.0 / (rx * rx).max(1e-12);
    let inv_ry2 = 1.0 / (ry * ry).max(1e-12);
    let inv_rz2 = 1.0 / (rz * rz).max(1e-12);

    for ring in 0..=rings {
        let v = ring as f32 / rings as f32;
        let phi = v * PI;
        let y_u = phi.cos();
        let r_u = phi.sin();
        for seg in 0..=segments {
            let u = seg as f32 / segments as f32;
            let theta = u * TAU;
            let x_u = r_u * theta.cos();
            let z_u = r_u * theta.sin();
            let px = x_u * rx;
            let py = y_u * ry;
            let pz = z_u * rz;
            let mut nx = px * inv_rx2;
            let mut ny = py * inv_ry2;
            let mut nz = pz * inv_rz2;
            let nl = (nx * nx + ny * ny + nz * nz).sqrt().max(1e-12);
            nx /= nl;
            ny /= nl;
            nz /= nl;
            positions.push([px, py, pz]);
            normals.push([nx, ny, nz]);
            uvs.push([u, v]);
        }
    }

    let row = segments + 1;
    for ring in 0..rings {
        for seg in 0..segments {
            let a = ring * row + seg;
            let b = a + row;
            indices.extend_from_slice(&[a, a + 1, b + 1, a, b + 1, b]);
        }
    }

    Mesh { positions, normals, uvs, indices, ..Default::default() }
}

/// Add a row×col patch to `mesh`, triangulating into quads. The winding of
/// all triangles is flipped together if the first triangle's geometric normal
/// disagrees with the stored vertex normal — this lets callers emit patches in
/// whichever parameter order is convenient and get consistent outward-facing
/// triangles regardless.
fn push_patch(
    mesh: &mut Mesh,
    patch_pos: &[[f32; 3]],
    patch_n: &[[f32; 3]],
    rows: usize,
    cols: usize,
) {
    let base = mesh.positions.len() as u32;
    mesh.positions.extend_from_slice(patch_pos);
    mesh.normals.extend_from_slice(patch_n);

    if rows < 2 || cols < 2 {
        return;
    }
    let mut idx: Vec<u32> = Vec::with_capacity((rows - 1) * (cols - 1) * 6);
    for r in 0..rows - 1 {
        for c in 0..cols - 1 {
            let a = base + (r * cols + c) as u32;
            let b = base + (r * cols + c + 1) as u32;
            let d = base + ((r + 1) * cols + c) as u32;
            let e = base + ((r + 1) * cols + c + 1) as u32;
            idx.extend_from_slice(&[a, b, e, a, e, d]);
        }
    }

    // First non-degenerate triangle determines winding against its vertex normal.
    let mut flip = false;
    for tri in idx.chunks(3) {
        let p0 = mesh.positions[tri[0] as usize];
        let p1 = mesh.positions[tri[1] as usize];
        let p2 = mesh.positions[tri[2] as usize];
        let n0 = mesh.normals[tri[0] as usize];
        let e1 = [p1[0] - p0[0], p1[1] - p0[1], p1[2] - p0[2]];
        let e2 = [p2[0] - p0[0], p2[1] - p0[1], p2[2] - p0[2]];
        let tn = [
            e1[1] * e2[2] - e1[2] * e2[1],
            e1[2] * e2[0] - e1[0] * e2[2],
            e1[0] * e2[1] - e1[1] * e2[0],
        ];
        let len2 = tn[0] * tn[0] + tn[1] * tn[1] + tn[2] * tn[2];
        if len2 < 1e-20 {
            continue;
        }
        let dot = tn[0] * n0[0] + tn[1] * n0[1] + tn[2] * n0[2];
        flip = dot < 0.0;
        break;
    }
    if flip {
        for i in (0..idx.len()).step_by(3) {
            idx.swap(i + 1, i + 2);
        }
    }
    mesh.indices.extend(idx);
}

/// Signed power: `sign(x) * |x|^p`. Used in superellipsoid parameterization so
/// the surface stays continuous across sign changes of cos/sin.
#[inline]
fn spow(x: f32, p: f32) -> f32 {
    x.signum() * x.abs().powf(p)
}

/// Barr superellipsoid with axis-aligned radii `size*0.5` and "boxiness"
/// parameters `ew` (cross-section roundness, XZ plane) and `ns` (vertical
/// profile, Y axis). Convention: `ew = ns = 1` is a sphere; values > 1 push
/// the shape toward a box (edges flatten, corners sharpen); values in (0, 1)
/// pinch it toward a diamond / octahedron. `rings` is the η resolution,
/// `segments` is the ω resolution. Normals are derived from the implicit
/// gradient so shading stays correct for non-spherical exponents.
pub fn superellipsoid_mesh(size: [f32; 3], ew: f32, ns: f32, rings: u32, segments: u32) -> Mesh {
    let rx = size[0] * 0.5;
    let ry = size[1] * 0.5;
    let rz = size[2] * 0.5;
    let rings = rings.max(4);
    let segments = segments.max(3);
    // Map the user-facing "boxiness" (1 = sphere, larger = boxier) to the
    // classical Barr exponents ε ∈ (0, 2]. ε = 1 is a sphere, ε → 0 is a box,
    // ε > 1 is pinched — exactly the inverse of our user parameter.
    let eps_ns = 1.0 / ns.max(0.05);
    let eps_ew = 1.0 / ew.max(0.05);
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut uvs = Vec::new();
    let mut indices = Vec::new();

    let inv_rx = 1.0 / rx.max(1e-12);
    let inv_ry = 1.0 / ry.max(1e-12);
    let inv_rz = 1.0 / rz.max(1e-12);

    // η ∈ [-π/2, π/2] (latitude), ω ∈ [-π, π] (longitude).
    for ring in 0..=rings {
        let v = ring as f32 / rings as f32;
        let eta = -PI * 0.5 + v * PI;
        let cos_eta = eta.cos();
        let sin_eta = eta.sin();
        let c_eta = spow(cos_eta, eps_ns);
        let s_eta = spow(sin_eta, eps_ns);
        for seg in 0..=segments {
            let u = seg as f32 / segments as f32;
            let omega = -PI + u * TAU;
            let cos_w = omega.cos();
            let sin_w = omega.sin();
            let c_w = spow(cos_w, eps_ew);
            let s_w = spow(sin_w, eps_ew);

            let px = rx * c_eta * c_w;
            let py = ry * s_eta;
            let pz = rz * c_eta * s_w;

            // Implicit-gradient normal, using 2 - ε for each axis.
            let nx = inv_rx * spow(cos_eta, 2.0 - eps_ns) * spow(cos_w, 2.0 - eps_ew);
            let ny = inv_ry * spow(sin_eta, 2.0 - eps_ns);
            let nz = inv_rz * spow(cos_eta, 2.0 - eps_ns) * spow(sin_w, 2.0 - eps_ew);
            let nl = (nx * nx + ny * ny + nz * nz).sqrt();
            let (nx, ny, nz) = if nl > 1e-8 {
                (nx / nl, ny / nl, nz / nl)
            } else {
                (0.0, sin_eta.signum(), 0.0)
            };

            positions.push([px, py, pz]);
            normals.push([nx, ny, nz]);
            uvs.push([u, v]);
        }
    }

    let row = segments + 1;
    for ring in 0..rings {
        for seg in 0..segments {
            let a = ring * row + seg;
            let b = a + row;
            indices.extend_from_slice(&[a, a + 1, b + 1, a, b + 1, b]);
        }
    }

    Mesh { positions, normals, uvs, indices, ..Default::default() }
}

/// Bent-plane patch, useful for petals, leaves, fish fins, roof tiles. The
/// unbent plane lies flat in XZ facing +Y. `size = [x, z]`. `bend_u` is the
/// total bend angle (radians) along the X axis — the left and right edges
/// curl toward +Y as the angle grows. `bend_v` does the same along Z.
/// `segments_u` / `segments_v` subdivide the patch so the bend looks smooth.
/// The mesh is single-sided (facing +Y when unbent); wrap in `mirror` or pair
/// with a flipped copy if you need a double-sided leaf.
pub fn curved_plane_mesh(
    size: [f32; 2],
    bend_u: f32,
    bend_v: f32,
    segments_u: u32,
    segments_v: u32,
) -> Mesh {
    let sx = size[0];
    let sz = size[1];
    let su = segments_u.max(1);
    let sv = segments_v.max(1);
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    // Arc-length parameterization: when bend is non-zero, the radius R = s / θ
    // makes the total arc length equal to the declared dimension, so an
    // unbent size=[1,1] patch has the same extent as a bent one. Positive
    // bend lifts both edges toward +Y.
    let pos_for = |u: f32, v: f32| -> [f32; 3] {
        let (x, y_u) = arc_offset(u, sx, bend_u);
        let (z, y_v) = arc_offset(v, sz, bend_v);
        [x, y_u + y_v, z]
    };

    for iv in 0..=sv {
        let tv = iv as f32 / sv as f32;
        let centered_v = tv - 0.5;
        for iu in 0..=su {
            let tu = iu as f32 / su as f32;
            let centered_u = tu - 0.5;
            positions.push(pos_for(centered_u, centered_v));
            uvs.push([tu, tv]);
        }
    }
    let cols = su + 1;
    for iv in 0..sv {
        for iu in 0..su {
            let a = iv * cols + iu;
            let b = a + 1;
            let d = a + cols;
            let c = d + 1;
            indices.extend_from_slice(&[a, d, c, a, c, b]);
        }
    }

    // Face-normal averaging yields correct smooth normals regardless of bend.
    let mesh = Mesh {
        positions,
        normals: vec![[0.0, 1.0, 0.0]; ((su + 1) * (sv + 1)) as usize],
        uvs,
        indices,
        ..Default::default()
    };
    recompute_normals(&mesh)
}

/// Offset from the flat parameter `t ∈ [-0.5, 0.5]` to `(axis, y)` coordinates
/// after a bend of `angle` radians across the full extent `s`. Handles the
/// `angle ≈ 0` case by collapsing to a flat line.
fn arc_offset(t: f32, s: f32, angle: f32) -> (f32, f32) {
    if angle.abs() < 1e-5 {
        return (t * s, 0.0);
    }
    let r = s / angle;
    let phi = t * angle;
    (r * phi.sin(), r * (1.0 - phi.cos()))
}

/// Lathe / revolve: spin a 2D profile `(radius, y)` around +Y.
/// `profile` is a list of points in cross-section space, authored from bottom
/// to top. `segments` is the rotational resolution. When `cap_ends` is true
/// and the first/last profile point has `radius > 0`, disc caps are added so
/// the mesh is watertight. Profile points with `radius = 0` are treated as
/// poles (a single shared vertex per cross-section, avoiding a triangle fan
/// with degenerate tips).
pub fn lathe_mesh(profile: &[[f32; 2]], segments: u32, cap_ends: bool) -> Mesh {
    let segments = segments.max(3);
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let normals: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    if profile.len() < 2 {
        return Mesh { positions, normals, uvs, indices, ..Default::default() };
    }

    // V runs bottom→top along the profile; U wraps around. This keeps textures
    // continuous across lathed surfaces regardless of profile density.
    let row_count = profile.len() as f32 - 1.0;
    let ring_start: Vec<u32> = profile
        .iter()
        .enumerate()
        .map(|(idx, p)| {
            let start = positions.len() as u32;
            let v = idx as f32 / row_count;
            let r = p[0].max(0.0);
            let y = p[1];
            if r < 1e-6 {
                positions.push([0.0, y, 0.0]);
                uvs.push([0.0, v]);
                for s in 1..=segments {
                    positions.push([0.0, y, 0.0]);
                    uvs.push([s as f32 / segments as f32, v]);
                }
            } else {
                for s in 0..=segments {
                    let t = s as f32 / segments as f32;
                    let a = t * TAU;
                    positions.push([a.cos() * r, y, a.sin() * r]);
                    uvs.push([t, v]);
                }
            }
            start
        })
        .collect();

    for i in 0..profile.len() - 1 {
        let ra = ring_start[i];
        let rb = ring_start[i + 1];
        for s in 0..segments {
            let a = ra + s;
            let b = a + 1;
            let d = rb + s;
            let c = d + 1;
            // CCW when viewed from outside — winding matches cylinder_mesh.
            indices.extend_from_slice(&[a, d, c, a, c, b]);
        }
    }

    if cap_ends {
        if profile[0][0] > 1e-6 {
            // Bottom cap (normal -Y).
            let center = positions.len() as u32;
            positions.push([0.0, profile[0][1], 0.0]);
            uvs.push([0.5, 0.5]);
            for s in 0..=segments {
                let a = (s as f32 / segments as f32) * TAU;
                let (sa, ca) = (a.sin(), a.cos());
                positions.push([ca * profile[0][0], profile[0][1], sa * profile[0][0]]);
                uvs.push([ca * 0.5 + 0.5, sa * 0.5 + 0.5]);
            }
            for s in 0..segments {
                // CCW from -Y.
                indices.extend_from_slice(&[center, center + 1 + s, center + 2 + s]);
            }
        }
        if let Some(last) = profile.last() {
            if last[0] > 1e-6 {
                // Top cap (normal +Y).
                let center = positions.len() as u32;
                positions.push([0.0, last[1], 0.0]);
                uvs.push([0.5, 0.5]);
                for s in 0..=segments {
                    let a = (s as f32 / segments as f32) * TAU;
                    let (sa, ca) = (a.sin(), a.cos());
                    positions.push([ca * last[0], last[1], sa * last[0]]);
                    uvs.push([ca * 0.5 + 0.5, sa * 0.5 + 0.5]);
                }
                for s in 0..segments {
                    // CCW from +Y.
                    indices.extend_from_slice(&[center, center + 2 + s, center + 1 + s]);
                }
            }
        }
    }

    let verts = positions.len();
    let mesh = Mesh {
        positions,
        normals: vec![[0.0, 1.0, 0.0]; verts],
        uvs,
        indices,
        ..Default::default()
    };
    recompute_normals(&mesh)
}

/// Catmull–Rom spline through `points`, sampled `samples_per_segment` times per
/// input interval. Used by `spline_tube_mesh` to build a smooth centerline.
fn sample_catmull_rom(points: &[[f32; 3]], samples_per_segment: u32) -> Vec<Vec3> {
    let n = points.len();
    if n < 2 {
        return points.iter().map(|p| Vec3::from_array(*p)).collect();
    }
    let samples_per_segment = samples_per_segment.max(1);
    let mut out = Vec::with_capacity(n * samples_per_segment as usize + 1);

    // Duplicate endpoints so the spline passes through the first and last points.
    let get = |i: isize| -> Vec3 {
        let j = i.clamp(0, n as isize - 1) as usize;
        Vec3::from_array(points[j])
    };

    for seg in 0..n - 1 {
        let p0 = get(seg as isize - 1);
        let p1 = get(seg as isize);
        let p2 = get(seg as isize + 1);
        let p3 = get(seg as isize + 2);
        let steps = if seg == n - 2 { samples_per_segment + 1 } else { samples_per_segment };
        for s in 0..steps {
            let t = s as f32 / samples_per_segment as f32;
            let t2 = t * t;
            let t3 = t2 * t;
            // Centripetal-ish Catmull–Rom (uniform tension 0.5).
            let a = -0.5 * t3 + t2 - 0.5 * t;
            let b = 1.5 * t3 - 2.5 * t2 + 1.0;
            let c = -1.5 * t3 + 2.0 * t2 + 0.5 * t;
            let d = 0.5 * t3 - 0.5 * t2;
            out.push(p0 * a + p1 * b + p2 * c + p3 * d);
        }
    }
    out
}

/// Tube swept along a Catmull–Rom curve through `points`, with a circular
/// cross-section whose radius is sampled from `radii` (one per control point;
/// pass `&[r]` for a constant-radius tube). `radial_segments` is the ring
/// resolution; `samples_per_segment` subdivides each segment between control
/// points. End caps are added when `cap_ends` is true.
///
/// The cross-section frame is propagated via parallel-transport from the first
/// sample, so the tube doesn't suddenly flip orientation when the curve bends
/// — critical for banana/tentacle/stem geometry where a Frenet frame with an
/// inflection point would visibly snap.
pub fn spline_tube_mesh(
    points: &[[f32; 3]],
    radii: &[f32],
    radial_segments: u32,
    samples_per_segment: u32,
    cap_ends: bool,
) -> Mesh {
    let radial_segments = radial_segments.max(3);
    if points.len() < 2 {
        return Mesh::default();
    }

    let samples = sample_catmull_rom(points, samples_per_segment);
    if samples.len() < 2 {
        return Mesh::default();
    }

    // Per-sample radius: linear interp of radii over the input intervals,
    // matched to the sampling density. Scalar radii broadcast.
    let radius_at = |sample_idx: usize| -> f32 {
        if radii.len() == 1 {
            return radii[0];
        }
        if radii.is_empty() {
            return 0.1;
        }
        // Sample indices map to a fractional position in input space.
        let total = samples.len() as f32 - 1.0;
        let pos = (sample_idx as f32 / total) * (points.len() as f32 - 1.0);
        let i = pos.floor().min(points.len() as f32 - 2.0).max(0.0) as usize;
        let t = (pos - i as f32).clamp(0.0, 1.0);
        let a = radii[i.min(radii.len() - 1)];
        let b = radii[(i + 1).min(radii.len() - 1)];
        a * (1.0 - t) + b * t
    };

    // Parallel-transport frame (tangent, normal, binormal).
    let tangent_at = |i: usize| -> Vec3 {
        let a = if i == 0 { samples[0] } else { samples[i - 1] };
        let b = if i + 1 == samples.len() { samples[samples.len() - 1] } else { samples[i + 1] };
        (b - a).normalize_or(Vec3::Z)
    };

    let t0 = tangent_at(0);
    // Pick an initial normal orthogonal to the first tangent — prefer +Y
    // unless the tangent is nearly vertical.
    let up = if t0.y.abs() > 0.9 { Vec3::X } else { Vec3::Y };
    let mut n_prev = up.cross(t0).cross(t0).normalize_or(Vec3::X * -1.0);
    if n_prev.length_squared() < 1e-8 {
        n_prev = Vec3::X;
    }
    let mut t_prev = t0;

    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    let total_samples = (samples.len() as f32 - 1.0).max(1.0);
    for (i, center) in samples.iter().enumerate() {
        let v = i as f32 / total_samples;
        let t_cur = tangent_at(i);
        // Rotate previous normal by the minimal rotation from t_prev to t_cur.
        let dot = t_prev.dot(t_cur).clamp(-1.0, 1.0);
        let n_cur = if dot > 0.9999 {
            n_prev
        } else {
            let axis = t_prev.cross(t_cur).normalize_or_zero();
            if axis.length_squared() < 1e-8 {
                n_prev
            } else {
                let angle = dot.acos();
                rotate_around_axis(n_prev, axis, angle).normalize_or(n_prev)
            }
        };
        let b_cur = t_cur.cross(n_cur).normalize_or(Vec3::Y);
        let n_cur = b_cur.cross(t_cur).normalize_or(n_cur); // re-orthogonalize

        let r = radius_at(i);
        for s in 0..=radial_segments {
            let u = s as f32 / radial_segments as f32;
            let a = u * TAU;
            let (sa, ca) = (a.sin(), a.cos());
            let offset = n_cur * ca + b_cur * sa;
            let p = *center + offset * r;
            positions.push([p.x, p.y, p.z]);
            normals.push([offset.x, offset.y, offset.z]);
            uvs.push([u, v]);
        }
        n_prev = n_cur;
        t_prev = t_cur;
    }

    let row = radial_segments + 1;
    for i in 0..samples.len() as u32 - 1 {
        for s in 0..radial_segments {
            let a = i * row + s;
            let b = a + 1;
            let d = a + row;
            let c = d + 1;
            indices.extend_from_slice(&[a, b, c, a, c, d]);
        }
    }

    if cap_ends {
        // Start cap: normal = -t0. Fan around the first ring. Disc UV projection
        // in the local frame.
        let start_center = positions.len() as u32;
        let c0 = samples[0];
        positions.push([c0.x, c0.y, c0.z]);
        let n_start = -t0;
        normals.push([n_start.x, n_start.y, n_start.z]);
        uvs.push([0.5, 0.5]);
        for s in 0..=radial_segments {
            let src = s as u32;
            let p = positions[src as usize];
            positions.push(p);
            normals.push([n_start.x, n_start.y, n_start.z]);
            let a = (s as f32 / radial_segments as f32) * TAU;
            uvs.push([a.cos() * 0.5 + 0.5, a.sin() * 0.5 + 0.5]);
        }
        for s in 0..radial_segments {
            // CCW viewed along -t0 = ccw from outside.
            indices.extend_from_slice(&[start_center, start_center + 2 + s, start_center + 1 + s]);
        }

        // End cap: normal = +t_end. Fan around the last ring.
        let end_first = (samples.len() as u32 - 1) * row;
        let c_end = samples[samples.len() - 1];
        let t_end = tangent_at(samples.len() - 1);
        let end_center = positions.len() as u32;
        positions.push([c_end.x, c_end.y, c_end.z]);
        normals.push([t_end.x, t_end.y, t_end.z]);
        uvs.push([0.5, 0.5]);
        for s in 0..=radial_segments {
            let p = positions[(end_first + s) as usize];
            positions.push(p);
            normals.push([t_end.x, t_end.y, t_end.z]);
            let a = (s as f32 / radial_segments as f32) * TAU;
            uvs.push([a.cos() * 0.5 + 0.5, a.sin() * 0.5 + 0.5]);
        }
        for s in 0..radial_segments {
            indices.extend_from_slice(&[end_center, end_center + 1 + s, end_center + 2 + s]);
        }
    }

    Mesh { positions, normals, uvs, indices, ..Default::default() }
}

#[inline]
fn rotate_around_axis(v: Vec3, axis: Vec3, angle: f32) -> Vec3 {
    let (s, c) = angle.sin_cos();
    v * c + axis.cross(v) * s + axis * axis.dot(v) * (1.0 - c)
}
