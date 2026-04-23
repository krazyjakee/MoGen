use std::f32::consts::TAU;

use mogen_core::{Mesh, UvMode};

use super::common::{disc_center_uv, disc_rim_uv};

/// Cone with apex at +height/2 and base at -height/2, aligned to Y.
pub fn cone_mesh(radius: f32, height: f32, segments: u32, mode: UvMode) -> Mesh {
    let segments = segments.max(3);
    let hy = height * 0.5;
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut uvs = Vec::new();
    let mut indices = Vec::new();

    let slant = (radius * radius + height * height).sqrt();
    let ny_side = radius / slant;
    let nh_side = height / slant;
    let circumference = TAU * radius;

    let side_start = positions.len() as u32;
    for i in 0..=segments {
        let t = i as f32 / segments as f32;
        let a = t * TAU;
        let (sa, ca) = (a.sin(), a.cos());
        let (u, v_apex) = match mode {
            UvMode::Fit => (t, 1.0),
            // Apex shares its base vertex's U (the side triangle becomes a
            // narrow wedge in UV space — the standard unrolled-cone mapping).
            UvMode::Tile => (t * circumference, slant),
        };
        // Unique apex per segment so side normals can differ around the cone.
        positions.push([ca * radius, -hy, sa * radius]);
        normals.push([ca * nh_side, ny_side, sa * nh_side]);
        uvs.push([u, 0.0]);
        positions.push([0.0, hy, 0.0]);
        normals.push([ca * nh_side, ny_side, sa * nh_side]);
        uvs.push([u, v_apex]);
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
    uvs.push(disc_center_uv(mode));
    for i in 0..=segments {
        let a = (i as f32 / segments as f32) * TAU;
        let (sa, ca) = (a.sin(), a.cos());
        positions.push([ca * radius, -hy, sa * radius]);
        normals.push([0.0, -1.0, 0.0]);
        uvs.push(disc_rim_uv(ca * radius, sa * radius, radius, mode));
    }
    for i in 0..segments {
        indices.extend_from_slice(&[bot_center, bot_center + 1 + i, bot_center + 2 + i]);
    }

    Mesh { positions, normals, uvs, indices, ..Default::default() }
}

/// Tapered box (rectangular frustum). Bottom rectangle `bottom = [x, z]` sits
/// on y=-height/2; top rectangle `top = [x, z]` sits on y=+height/2. Both
/// rectangles are centered on the Y axis. Either end may be larger.
pub fn frustum_mesh(bottom: [f32; 2], top: [f32; 2], height: f32, mode: UvMode) -> Mesh {
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

    let push_quad = |quad: [[f32; 3]; 4], n: [f32; 3], face_uvs: [[f32; 2]; 4],
                     positions: &mut Vec<[f32; 3]>,
                     normals: &mut Vec<[f32; 3]>,
                     uvs: &mut Vec<[f32; 2]>,
                     indices: &mut Vec<u32>| {
        let base = positions.len() as u32;
        for (i, v) in quad.iter().enumerate() {
            positions.push(*v);
            normals.push(n);
            uvs.push(face_uvs[i]);
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    };

    fn normalize(n: [f32; 3]) -> [f32; 3] {
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-6);
        [n[0] / len, n[1] / len, n[2] / len]
    }

    const FIT: [[f32; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
    // Per-axis slant lengths for the four side faces in tile mode. Each side
    // is a trapezoid with parallel base (bottom) and top edges of differing
    // widths; the slant is √(height² + (top-bottom)²) in the perpendicular
    // direction. We pin U to the base width for each side and use the slant
    // for V — top corners get U = (top width) so the trapezoid maps cleanly.
    let slant_x = (height * height + (tx - bx).powi(2)).sqrt();
    let slant_z = (height * height + (tz - bz).powi(2)).sqrt();

    // Top cap (+Y): U=X, V=Z, world units in tile mode.
    let top_uvs = match mode {
        UvMode::Fit => FIT,
        UvMode::Tile => [
            [-tx, tz], [tx, tz], [tx, -tz], [-tx, -tz],
        ],
    };
    push_quad([t_fl, t_fr, t_br, t_bl], [0.0, 1.0, 0.0], top_uvs,
              &mut positions, &mut normals, &mut uvs, &mut indices);
    // Bottom cap (-Y), wound opposite.
    let bot_uvs = match mode {
        UvMode::Fit => FIT,
        UvMode::Tile => [
            [-bx, -bz], [bx, -bz], [bx, bz], [-bx, bz],
        ],
    };
    push_quad([b_bl, b_br, b_fr, b_fl], [0.0, -1.0, 0.0], bot_uvs,
              &mut positions, &mut normals, &mut uvs, &mut indices);

    // Right (+X) slant: face spans from base x=bx to top x=tx over height in y.
    // Outward normal has +X from height, and -(tx-bx) in y (if top narrows).
    let n_right = normalize([height, -(tx - bx), 0.0]);
    // Side U axis: depth z. V axis: slant length from base to top.
    let right_uvs = match mode {
        UvMode::Fit => FIT,
        // Verts: t_br (-Z, top), t_fr (+Z, top), b_fr (+Z, bot), b_br (-Z, bot).
        UvMode::Tile => [
            [-tz, slant_x], [tz, slant_x], [bz, 0.0], [-bz, 0.0],
        ],
    };
    push_quad([t_br, t_fr, b_fr, b_br], n_right, right_uvs,
              &mut positions, &mut normals, &mut uvs, &mut indices);
    // Left (-X) slant.
    let n_left = normalize([-height, -(tx - bx), 0.0]);
    let left_uvs = match mode {
        UvMode::Fit => FIT,
        // Verts: t_fl (+Z, top), t_bl (-Z, top), b_bl (-Z, bot), b_fl (+Z, bot).
        UvMode::Tile => [
            [tz, slant_x], [-tz, slant_x], [-bz, 0.0], [bz, 0.0],
        ],
    };
    push_quad([t_fl, t_bl, b_bl, b_fl], n_left, left_uvs,
              &mut positions, &mut normals, &mut uvs, &mut indices);
    // Front (+Z) slant.
    let n_front = normalize([0.0, -(tz - bz), height]);
    let front_uvs = match mode {
        UvMode::Fit => FIT,
        // Verts: t_fr (+X, top), t_fl (-X, top), b_fl (-X, bot), b_fr (+X, bot).
        UvMode::Tile => [
            [tx, slant_z], [-tx, slant_z], [-bx, 0.0], [bx, 0.0],
        ],
    };
    push_quad([t_fr, t_fl, b_fl, b_fr], n_front, front_uvs,
              &mut positions, &mut normals, &mut uvs, &mut indices);
    // Back (-Z) slant.
    let n_back = normalize([0.0, -(tz - bz), -height]);
    let back_uvs = match mode {
        UvMode::Fit => FIT,
        // Verts: t_bl (-X, top), t_br (+X, top), b_br (+X, bot), b_bl (-X, bot).
        UvMode::Tile => [
            [-tx, slant_z], [tx, slant_z], [bx, 0.0], [-bx, 0.0],
        ],
    };
    push_quad([t_bl, t_br, b_br, b_bl], n_back, back_uvs,
              &mut positions, &mut normals, &mut uvs, &mut indices);

    Mesh { positions, normals, uvs, indices, ..Default::default() }
}
