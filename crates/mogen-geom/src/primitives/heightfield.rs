//! Tessellated XZ-grid plane displaced along +Y by deterministic value-noise.
//!
//! `heightfield` is the right primitive when you want terrain-like geometry —
//! dunes, scaled rooftops, bumpy table-top dioramas, organic stone slabs — at
//! a controlled triangle budget. A `plane` deformed by `noise=` displaces 4
//! corner vertices and reads as a single tilted quad; `heightfield` builds
//! the dense grid for you and samples a shared fBm value-noise field at
//! every grid vertex.
//!
//! The noise field reuses `cell_noise` from `crate::deform` so heightfields
//! and `noise=` deformations share the same hash mixer and stay deterministic
//! across runs for a given seed.

use mogen_core::{Mesh, UvMode};

use crate::cleanup::recompute_normals;

/// Build a heightfield mesh: an XZ grid of size `[w, d]` displaced along +Y
/// by fractional-Brownian-motion noise.
///
/// `segments_u` / `segments_v` are the X/Z grid divisions (vertex count is
/// `(segments_u+1) * (segments_v+1)`). `amplitude` is the peak Y
/// displacement; `0` produces a flat plane (handy as a sanity check).
/// `octaves` (1..=8 clamped) layers progressively higher-frequency noise on
/// top, each octave doubling the spatial frequency and scaling its amplitude
/// by `persistence` (typically 0.5). `frequency` is cycles per unit along
/// the base octave.
///
/// Origin sits at the centre of the patch (XZ both centred on 0, base Y at
/// `0` before displacement) so the primitive composes with `pos=`.
pub fn heightfield_mesh(
    size: [f32; 2],
    segments_u: u32,
    segments_v: u32,
    amplitude: f32,
    octaves: u32,
    frequency: f32,
    persistence: f32,
    seed: u32,
    mode: UvMode,
) -> Mesh {
    // Lower bound prevents zero-area meshes; upper bound prevents
    // accidental OOM from a misplaced segments=10000. 4096² = ~16M cells
    // is already an extreme grid.
    let su = segments_u.clamp(1, 4096);
    let sv = segments_v.clamp(1, 4096);
    let w = size[0].max(1e-4);
    let d = size[1].max(1e-4);
    let hx = w * 0.5;
    let hz = d * 0.5;

    let nx = su as usize + 1;
    let nz = sv as usize + 1;
    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(nx * nz);
    let mut uvs: Vec<[f32; 2]> = Vec::with_capacity(nx * nz);

    let octaves = octaves.clamp(1, 8);

    for j in 0..nz {
        let v = j as f32 / sv as f32; // 0..1 along Z
        for i in 0..nx {
            let u = i as f32 / su as f32; // 0..1 along X
            let x = -hx + u * w;
            let z = -hz + v * d;
            let y = if amplitude.abs() < 1e-6 {
                0.0
            } else {
                amplitude * fbm(x, z, frequency, octaves, persistence, seed)
            };
            positions.push([x, y, z]);
            uvs.push(match mode {
                UvMode::Fit => [u, v],
                // Tile mode emits world-space UVs (1 unit = 1 tile, then
                // scaled by the material's `uv_scale`). Same convention as
                // the base `plane_mesh`.
                UvMode::Tile => [x, z],
            });
        }
    }

    let mut indices: Vec<u32> = Vec::with_capacity(su as usize * sv as usize * 6);
    for j in 0..sv as usize {
        for i in 0..su as usize {
            let a = (j * nx + i) as u32;
            let b = a + 1;
            let c = a + nx as u32;
            let d_idx = c + 1;
            // CCW from +Y; matches `plane_mesh` winding.
            indices.push(a);
            indices.push(c);
            indices.push(d_idx);
            indices.push(a);
            indices.push(d_idx);
            indices.push(b);
        }
    }

    let mesh = Mesh {
        positions,
        normals: Vec::new(),
        uvs,
        indices,
        ..Default::default()
    };
    // Recompute normals from displaced geometry — gives accurate shading on
    // the bumps. A flat patch (amplitude=0) gets +Y normals just the same.
    recompute_normals(&mesh)
}

/// Two-axis fractional-Brownian-motion sampler. Octave 0 is a coarse
/// bilinear-interpolated value noise; each higher octave doubles the
/// spatial frequency and scales its contribution by `persistence`.
///
/// Returns a value in roughly `[-1, 1]` (no normalisation pass — for the
/// typical `octaves=3, persistence=0.5` case the sum sits in `[-1.75, 1.75]`,
/// which is fine when the caller multiplies by an explicit `amplitude`).
fn fbm(x: f32, z: f32, frequency: f32, octaves: u32, persistence: f32, seed: u32) -> f32 {
    let mut sum = 0.0_f32;
    let mut amp = 1.0_f32;
    let mut freq = frequency.max(1e-6);
    for _ in 0..octaves {
        sum += amp * value_noise_2d(x * freq, z * freq, seed);
        amp *= persistence;
        freq *= 2.0;
    }
    sum
}

/// Bilinear-interpolated value noise on the unit grid. Reuses the same
/// integer-mixing hash that `crate::deform::cell_noise` uses (cheap, stable
/// across runs, not cryptographic).
fn value_noise_2d(x: f32, z: f32, seed: u32) -> f32 {
    let xi = x.floor();
    let zi = z.floor();
    let fx = x - xi;
    let fz = z - zi;
    let xi = xi as i32;
    let zi = zi as i32;
    let v00 = hash2(xi,     zi,     seed);
    let v10 = hash2(xi + 1, zi,     seed);
    let v01 = hash2(xi,     zi + 1, seed);
    let v11 = hash2(xi + 1, zi + 1, seed);
    // Smoothstep on `fx`/`fz` removes the "boxy" look of straight bilinear
    // interpolation at the cell boundaries.
    let sx = fx * fx * (3.0 - 2.0 * fx);
    let sz = fz * fz * (3.0 - 2.0 * fz);
    let a = v00 * (1.0 - sx) + v10 * sx;
    let b = v01 * (1.0 - sx) + v11 * sx;
    a * (1.0 - sz) + b * sz
}

/// Cheap integer-coords hash → `[-1, 1]` float. Mirrors the three-stage
/// xorshift mixer in `crate::deform::cell_noise(x, y, z, seed)` exactly,
/// applied with `y = z` and `z = 0`. The middle stage uses the `y`
/// constant `0xC2B2AE35` and the third stage uses the `z` constant
/// `0x27D4EB2F`, so a heightfield at `(x, z)` and a noise-deformed mesh
/// hashed at `(x, z, 0)` with the same seed produce bit-identical
/// values.
fn hash2(x: i32, z: i32, seed: u32) -> f32 {
    let mut h: u32 = seed.wrapping_add(0x9E3779B9);
    h = h.wrapping_add(x as u32).wrapping_mul(0x85EBCA6B);
    h ^= h >> 13;
    h = h.wrapping_add(z as u32).wrapping_mul(0xC2B2AE35);
    h ^= h >> 16;
    h = h.wrapping_add(0u32).wrapping_mul(0x27D4EB2F);
    h ^= h >> 15;
    let bits = (h >> 8) & 0x00FF_FFFF;
    (bits as f32 / (1u32 << 24) as f32) * 2.0 - 1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heightfield_flat_when_amplitude_zero() {
        let m = heightfield_mesh([2.0, 2.0], 8, 8, 0.0, 3, 1.0, 0.5, 1, UvMode::Fit);
        assert!(!m.positions.is_empty());
        for p in &m.positions {
            assert!(p[1].abs() < 1e-6, "expected flat patch, got y={}", p[1]);
        }
    }

    #[test]
    fn heightfield_displaces_when_amplitude_nonzero() {
        let m = heightfield_mesh([4.0, 4.0], 16, 16, 0.5, 3, 1.0, 0.5, 1, UvMode::Fit);
        let max_y = m.positions.iter().map(|p| p[1]).fold(f32::NEG_INFINITY, f32::max);
        let min_y = m.positions.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min);
        assert!(max_y - min_y > 0.1, "heightfield Y range too small: [{min_y}, {max_y}]");
    }

    #[test]
    fn heightfield_deterministic_for_same_seed() {
        let a = heightfield_mesh([4.0, 4.0], 16, 16, 0.5, 3, 1.0, 0.5, 7, UvMode::Fit);
        let b = heightfield_mesh([4.0, 4.0], 16, 16, 0.5, 3, 1.0, 0.5, 7, UvMode::Fit);
        assert_eq!(a.positions, b.positions);
    }

    #[test]
    fn heightfield_differs_across_seeds() {
        let a = heightfield_mesh([4.0, 4.0], 16, 16, 0.5, 3, 1.0, 0.5, 1, UvMode::Fit);
        let b = heightfield_mesh([4.0, 4.0], 16, 16, 0.5, 3, 1.0, 0.5, 2, UvMode::Fit);
        assert_ne!(a.positions, b.positions);
    }

    #[test]
    fn heightfield_centered_on_origin() {
        let m = heightfield_mesh([3.0, 5.0], 4, 4, 0.0, 3, 1.0, 0.5, 1, UvMode::Fit);
        let max_x = m.positions.iter().map(|p| p[0]).fold(f32::NEG_INFINITY, f32::max);
        let min_x = m.positions.iter().map(|p| p[0]).fold(f32::INFINITY, f32::min);
        let max_z = m.positions.iter().map(|p| p[2]).fold(f32::NEG_INFINITY, f32::max);
        let min_z = m.positions.iter().map(|p| p[2]).fold(f32::INFINITY, f32::min);
        assert!((max_x + min_x).abs() < 1e-5, "X not centered: [{min_x}, {max_x}]");
        assert!((max_z + min_z).abs() < 1e-5, "Z not centered: [{min_z}, {max_z}]");
        assert!((max_x - 1.5).abs() < 1e-5);
        assert!((max_z - 2.5).abs() < 1e-5);
    }
}
