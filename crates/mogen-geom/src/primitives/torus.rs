use std::f32::consts::TAU;

use mogen_core::{Mesh, UvMode};

use super::common::{disc_center_uv, disc_rim_uv};

/// Torus lying flat in the XZ plane. `major_radius` is the distance from the
/// torus centre to the centre of the tube; `minor_radius` is the tube radius.
pub fn torus_mesh(
    major_radius: f32,
    minor_radius: f32,
    major_segments: u32,
    minor_segments: u32,
    mode: UvMode,
) -> Mesh {
    let major_segments = major_segments.max(3);
    let minor_segments = minor_segments.max(3);
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut uvs = Vec::new();
    let mut indices = Vec::new();

    let (u_scale, v_scale) = match mode {
        UvMode::Fit => (1.0, 1.0),
        // Major axis: tube-centreline circumference. Minor axis: cross-section
        // circumference. Texel density matches a cylinder of the same radius.
        UvMode::Tile => (TAU * major_radius, TAU * minor_radius),
    };

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
            uvs.push([u * u_scale, v * v_scale]);
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
    mode: UvMode,
) -> Mesh {
    let major_segments = major_segments.max(2);
    let minor_segments = minor_segments.max(3);
    let arc = arc.clamp(1e-4, TAU);
    let closed = (TAU - arc).abs() < 1e-4;
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut uvs = Vec::new();
    let mut indices = Vec::new();

    // Tile: U covers the actual swept arc length along the centreline,
    // V covers the cross-section circumference.
    let (u_scale, v_scale) = match mode {
        UvMode::Fit => (1.0, 1.0),
        UvMode::Tile => (arc * major_radius, TAU * minor_radius),
    };

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
            uvs.push([u * u_scale, v * v_scale]);
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
        uvs.push(disc_center_uv(mode));
        for j in 0..=minor_segments {
            let theta = (j as f32 / minor_segments as f32) * TAU;
            let (st, ct) = (theta.sin(), theta.cos());
            positions.push([major_radius + minor_radius * ct, minor_radius * st, 0.0]);
            normals.push([0.0, 0.0, -1.0]);
            uvs.push(disc_rim_uv(ct * minor_radius, st * minor_radius, minor_radius, mode));
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
        uvs.push(disc_center_uv(mode));
        for j in 0..=minor_segments {
            let theta = (j as f32 / minor_segments as f32) * TAU;
            let (st, ct) = (theta.sin(), theta.cos());
            let rx = major_radius + minor_radius * ct;
            positions.push([cp * rx, minor_radius * st, sp * rx]);
            normals.push([-sp, 0.0, cp]);
            uvs.push(disc_rim_uv(ct * minor_radius, st * minor_radius, minor_radius, mode));
        }
        for j in 0..minor_segments {
            indices.extend_from_slice(&[end_center, end_center + 1 + j, end_center + 2 + j]);
        }
    }

    Mesh { positions, normals, uvs, indices, ..Default::default() }
}
