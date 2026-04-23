use std::f32::consts::{PI, TAU};

use mogen_core::{Mesh, UvMode};

use super::common::{disc_center_uv, disc_rim_uv};

/// UV sphere centered at origin.
pub fn sphere_mesh(radius: f32, rings: u32, segments: u32, mode: UvMode) -> Mesh {
    let rings = rings.max(2);
    let segments = segments.max(3);
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut uvs = Vec::new();
    let mut indices = Vec::new();

    // Tile: U = great-circle arc length at the equator (`u * 2πr`),
    // V = pole-to-pole arc length (`v * πr`). Same texel size as a cylinder
    // wrap of the same radius.
    let (u_scale, v_scale) = match mode {
        UvMode::Fit => (1.0, 1.0),
        UvMode::Tile => (TAU * radius, PI * radius),
    };
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
            uvs.push([u * u_scale, v * v_scale]);
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
pub fn capsule_mesh(radius: f32, height: f32, rings: u32, segments: u32, mode: UvMode) -> Mesh {
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
    // Tile: U scales to circumference, V to total arc length so texel density
    // matches a cylinder of the same radius.
    let (u_scale, v_scale) = match mode {
        UvMode::Fit => (1.0, 1.0),
        UvMode::Tile => (TAU * radius, total_arc),
    };

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
            uvs.push([u * u_scale, frac * v_top_equator * v_scale]);
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
            uvs.push([u * u_scale, (v_bottom_equator + frac * (1.0 - v_bottom_equator)) * v_scale]);
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

/// Half-sphere with flat base on the XZ plane at y=0 and dome rising to y=+radius.
/// Origin sits at the centre of the flat base (not the sphere centre) so the
/// primitive stacks naturally — a `bottom` connector at y=0 meets any surface.
pub fn hemisphere_mesh(radius: f32, rings: u32, segments: u32, mode: UvMode) -> Mesh {
    let rings = rings.max(2);
    let segments = segments.max(3);
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut uvs = Vec::new();
    let mut indices = Vec::new();

    let (u_scale, v_scale) = match mode {
        UvMode::Fit => (1.0, 1.0),
        // Equatorial circumference for U, quarter-meridian arc length for V.
        UvMode::Tile => (TAU * radius, PI * 0.5 * radius),
    };

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
            uvs.push([u * u_scale, v * v_scale]);
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
    uvs.push(disc_center_uv(mode));
    for i in 0..=segments {
        let a = (i as f32 / segments as f32) * TAU;
        let (sa, ca) = (a.sin(), a.cos());
        positions.push([ca * radius, 0.0, sa * radius]);
        normals.push([0.0, -1.0, 0.0]);
        uvs.push(disc_rim_uv(ca * radius, sa * radius, radius, mode));
    }
    for i in 0..segments {
        // CCW from -Y (looking +Y): centre → ring_i → ring_{i+1}.
        indices.extend_from_slice(&[center, center + 1 + i, center + 2 + i]);
    }

    Mesh { positions, normals, uvs, indices, ..Default::default() }
}

/// Axis-aligned ellipsoid with independent radii along X/Y/Z. `size = [x,y,z]`
/// is the bounding diameter on each axis; radii = size * 0.5. Normals are
/// computed from the implicit surface gradient so shading is correct even when
/// the axes differ.
pub fn ellipsoid_mesh(size: [f32; 3], rings: u32, segments: u32, mode: UvMode) -> Mesh {
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

    // Tile mode uses a sphere-equivalent mean radius — the ellipsoid's true
    // arc lengths vary per latitude, so this is a uniform-density compromise
    // that still scales sensibly with overall size.
    let r_mean = (rx + ry + rz) / 3.0;
    let (u_scale, v_scale) = match mode {
        UvMode::Fit => (1.0, 1.0),
        UvMode::Tile => (TAU * r_mean, PI * r_mean),
    };

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
            uvs.push([u * u_scale, v * v_scale]);
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

/// Barr superellipsoid with axis-aligned radii `size*0.5` and "boxiness"
/// parameters `ew` (cross-section roundness, XZ plane) and `ns` (vertical
/// profile, Y axis). Convention: `ew = ns = 1` is a sphere; values > 1 push
/// the shape toward a box (edges flatten, corners sharpen); values in (0, 1)
/// pinch it toward a diamond / octahedron. `rings` is the η resolution,
/// `segments` is the ω resolution. Normals are derived from the implicit
/// gradient so shading stays correct for non-spherical exponents.
pub fn superellipsoid_mesh(
    size: [f32; 3],
    ew: f32,
    ns: f32,
    rings: u32,
    segments: u32,
    mode: UvMode,
) -> Mesh {
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

    // Mean radius approximation for tile mode (same compromise as ellipsoid).
    let r_mean = (rx + ry + rz) / 3.0;
    let (u_scale, v_scale) = match mode {
        UvMode::Fit => (1.0, 1.0),
        UvMode::Tile => (TAU * r_mean, PI * r_mean),
    };

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
            uvs.push([u * u_scale, v * v_scale]);
        }
    }

    let row = segments + 1;
    for ring in 0..rings {
        for seg in 0..segments {
            let a = ring * row + seg;
            let b = a + row;
            // Ring 0 is south (eta=-π/2), ring=rings is north, so b is the
            // ring *above* a — winding is the mirror of sphere_mesh/ellipsoid_mesh.
            indices.extend_from_slice(&[a, b + 1, a + 1, a, b, b + 1]);
        }
    }

    Mesh { positions, normals, uvs, indices, ..Default::default() }
}

/// Signed power: `sign(x) * |x|^p`. Used in superellipsoid parameterization so
/// the surface stays continuous across sign changes of cos/sin.
#[inline]
fn spow(x: f32, p: f32) -> f32 {
    x.signum() * x.abs().powf(p)
}
