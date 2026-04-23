use std::f32::consts::{PI, TAU};

use mogen_core::{Mesh, UvMode};

use super::common::{disc_center_uv, disc_rim_uv};

/// Axis-aligned cylinder along Y, centered at origin.
pub fn cylinder_mesh(radius: f32, height: f32, segments: u32, mode: UvMode) -> Mesh {
    let segments = segments.max(3);
    let hy = height * 0.5;
    let circumference = TAU * radius;
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut uvs = Vec::new();
    let mut indices = Vec::new();

    // Side wall — duplicated ring verts so side normals are radial. Fit mode
    // wraps U over [0, 1] and V over [0, 1]; tile mode uses arc length around
    // (so circumference maps to `circumference` UV units) and world-space
    // height. Duplicated seam vert at i=segments closes the wrap cleanly.
    let side_start = positions.len() as u32;
    for i in 0..=segments {
        let t = i as f32 / segments as f32;
        let a = t * TAU;
        let (sa, ca) = (a.sin(), a.cos());
        let n = [ca, 0.0, sa];
        let (u, v_top) = match mode {
            UvMode::Fit => (t, 1.0),
            UvMode::Tile => (t * circumference, height),
        };
        positions.push([ca * radius, -hy, sa * radius]);
        normals.push(n);
        uvs.push([u, 0.0]);
        positions.push([ca * radius,  hy, sa * radius]);
        normals.push(n);
        uvs.push([u, v_top]);
    }
    for i in 0..segments {
        let base = side_start + i * 2;
        // Wound CCW when viewed from outside so face normals point radially
        // outward — required for backface culling and boolean CSG.
        indices.extend_from_slice(&[base, base + 1, base + 3, base, base + 3, base + 2]);
    }

    // Top cap (fan around center, CCW from +Y). Fit-mode disc maps into
    // [0,1]²; tile-mode preserves the world-space (X, Z) extent so disc texel
    // density matches the side wall.
    let top_center = positions.len() as u32;
    positions.push([0.0, hy, 0.0]);
    normals.push([0.0, 1.0, 0.0]);
    uvs.push(disc_center_uv(mode));
    for i in 0..=segments {
        let a = (i as f32 / segments as f32) * TAU;
        let (sa, ca) = (a.sin(), a.cos());
        positions.push([ca * radius, hy, sa * radius]);
        normals.push([0.0, 1.0, 0.0]);
        uvs.push(disc_rim_uv(ca * radius, sa * radius, radius, mode));
    }
    for i in 0..segments {
        indices.extend_from_slice(&[top_center, top_center + 2 + i, top_center + 1 + i]);
    }

    // Bottom cap (CCW from -Y).
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

/// Hollow cylinder (pipe) aligned to Y, centered at origin. `outer` is the
/// outside radius, `inner` is the bore radius (must satisfy `inner < outer`);
/// `height` runs along Y. The mesh has four surfaces: outer wall (normals
/// radially outward), inner wall (normals radially inward), and two annular
/// end caps.
pub fn tube_mesh(outer: f32, inner: f32, height: f32, segments: u32, mode: UvMode) -> Mesh {
    let segments = segments.max(3);
    let outer = outer.max(0.0);
    let inner = inner.max(0.0).min(outer);
    let hy = height * 0.5;
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut uvs = Vec::new();
    let mut indices = Vec::new();

    let outer_circ = TAU * outer;
    let inner_circ = TAU * inner;
    // Outer wall (normals point +radial). U wraps, V goes bottom→top.
    let o_start = positions.len() as u32;
    for i in 0..=segments {
        let t = i as f32 / segments as f32;
        let a = t * TAU;
        let (sa, ca) = (a.sin(), a.cos());
        let n = [ca, 0.0, sa];
        let (u, v_top) = match mode {
            UvMode::Fit => (t, 1.0),
            UvMode::Tile => (t * outer_circ, height),
        };
        positions.push([ca * outer, -hy, sa * outer]);
        normals.push(n);
        uvs.push([u, 0.0]);
        positions.push([ca * outer,  hy, sa * outer]);
        normals.push(n);
        uvs.push([u, v_top]);
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
        let (u, v_top) = match mode {
            UvMode::Fit => (t, 1.0),
            UvMode::Tile => (t * inner_circ, height),
        };
        positions.push([ca * inner, -hy, sa * inner]);
        normals.push(n);
        uvs.push([u, 0.0]);
        positions.push([ca * inner,  hy, sa * inner]);
        normals.push(n);
        uvs.push([u, v_top]);
    }
    // Wind inner wall opposite of outer so triangles face the bore centre.
    for i in 0..segments {
        let base = i_start + i * 2;
        indices.extend_from_slice(&[base, base + 3, base + 1, base, base + 2, base + 3]);
    }

    // Top annular cap (+Y). Both rims share the same disc projection so the
    // texture stays continuous across the annulus.
    let t_start = positions.len() as u32;
    for i in 0..=segments {
        let a = (i as f32 / segments as f32) * TAU;
        let (sa, ca) = (a.sin(), a.cos());
        positions.push([ca * outer, hy, sa * outer]);
        normals.push([0.0, 1.0, 0.0]);
        uvs.push(disc_rim_uv(ca * outer, sa * outer, outer, mode));
        positions.push([ca * inner, hy, sa * inner]);
        normals.push([0.0, 1.0, 0.0]);
        uvs.push(disc_rim_uv(ca * inner, sa * inner, outer, mode));
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
        uvs.push(disc_rim_uv(ca * outer, sa * outer, outer, mode));
        positions.push([ca * inner, -hy, sa * inner]);
        normals.push([0.0, -1.0, 0.0]);
        uvs.push(disc_rim_uv(ca * inner, sa * inner, outer, mode));
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

/// Half of a cylinder (D-shape extrusion). Axis along Y, `height` runs along Y.
/// The cut plane is the YZ plane: the kept half is the +X side, so the flat
/// rectangular face sits on x=0 with outward normal -X. Curved surface spans
/// theta in [-PI/2, PI/2] (i.e. from -Z to +Z around +X).
pub fn half_cylinder_mesh(radius: f32, height: f32, segments: u32, mode: UvMode) -> Mesh {
    let segments = segments.max(2);
    let hy = height * 0.5;
    let half_circ = PI * radius;
    let flat_width = 2.0 * radius;
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
        let (u, v_top) = match mode {
            UvMode::Fit => (t, 1.0),
            UvMode::Tile => (t * half_circ, height),
        };
        positions.push([ca * radius, -hy, sa * radius]);
        normals.push(n);
        uvs.push([u, 0.0]);
        positions.push([ca * radius,  hy, sa * radius]);
        normals.push(n);
        uvs.push([u, v_top]);
    }
    for i in 0..segments {
        let base = side_start + i * 2;
        indices.extend_from_slice(&[base, base + 1, base + 3, base, base + 3, base + 2]);
    }

    // Flat face (the rectangle at x=0, spanning Y ±hy and Z ±radius). Normal -X.
    // U maps to Z (full diameter), V maps to Y (full height).
    let flat_base = positions.len() as u32;
    let flat_quad = [
        [0.0, -hy,  radius], [0.0,  hy,  radius],
        [0.0,  hy, -radius], [0.0, -hy, -radius],
    ];
    let flat_uvs: [[f32; 2]; 4] = match mode {
        UvMode::Fit => [[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]],
        UvMode::Tile => [
            [0.0, 0.0], [0.0, height], [flat_width, height], [flat_width, 0.0],
        ],
    };
    for (i, v) in flat_quad.into_iter().enumerate() {
        positions.push(v);
        normals.push([-1.0, 0.0, 0.0]);
        uvs.push(flat_uvs[i]);
    }
    indices.extend_from_slice(&[
        flat_base, flat_base + 1, flat_base + 2,
        flat_base, flat_base + 2, flat_base + 3,
    ]);

    // Top semicircle cap (+Y). Disc projection in world units (tile) or the
    // legacy [0,1]² half-disc mapping (fit).
    let top_center = positions.len() as u32;
    positions.push([0.0, hy, 0.0]);
    normals.push([0.0, 1.0, 0.0]);
    uvs.push(match mode {
        UvMode::Fit => [0.5, 0.5],
        UvMode::Tile => [0.0, 0.0],
    });
    for i in 0..=segments {
        let t = i as f32 / segments as f32;
        let a = -PI * 0.5 + t * PI;
        let (sa, ca) = (a.sin(), a.cos());
        positions.push([ca * radius, hy, sa * radius]);
        normals.push([0.0, 1.0, 0.0]);
        // Legacy fit mapping packs the D into the upper half of [0,1]² with
        // (sa, ca) → (U, V); tile uses true world coords.
        uvs.push(match mode {
            UvMode::Fit => [sa * 0.5 + 0.5, ca * 0.5 + 0.5],
            UvMode::Tile => [sa * radius, ca * radius],
        });
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
    uvs.push(match mode {
        UvMode::Fit => [0.5, 0.5],
        UvMode::Tile => [0.0, 0.0],
    });
    for i in 0..=segments {
        let t = i as f32 / segments as f32;
        let a = -PI * 0.5 + t * PI;
        let (sa, ca) = (a.sin(), a.cos());
        positions.push([ca * radius, -hy, sa * radius]);
        normals.push([0.0, -1.0, 0.0]);
        uvs.push(match mode {
            UvMode::Fit => [sa * 0.5 + 0.5, ca * 0.5 + 0.5],
            UvMode::Tile => [sa * radius, ca * radius],
        });
    }
    for i in 0..segments {
        indices.extend_from_slice(&[bot_center, bot_center + 1 + i, bot_center + 2 + i]);
    }

    Mesh { positions, normals, uvs, indices, ..Default::default() }
}
