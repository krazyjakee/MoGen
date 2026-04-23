use std::f32::consts::TAU;

use glam::Vec3;
use mogen_core::{Mesh, UvMode};

use crate::cleanup::recompute_normals;

use super::common::{disc_center_uv, disc_rim_uv};

/// Lathe / revolve: spin a 2D profile `(radius, y)` around +Y.
/// `profile` is a list of points in cross-section space, authored from bottom
/// to top. `segments` is the rotational resolution. When `cap_ends` is true
/// and the first/last profile point has `radius > 0`, disc caps are added so
/// the mesh is watertight. Profile points with `radius = 0` are treated as
/// poles (a single shared vertex per cross-section, avoiding a triangle fan
/// with degenerate tips).
pub fn lathe_mesh(profile: &[[f32; 2]], segments: u32, cap_ends: bool, mode: UvMode) -> Mesh {
    let segments = segments.max(3);
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let normals: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    if profile.len() < 2 {
        return Mesh { positions, normals, uvs, indices, ..Default::default() };
    }

    // Tile mode: U wraps by arc length around a representative radius (max
    // profile radius — the widest cross-section sets the texel density);
    // V tracks the cumulative 2D arc length along the profile.
    let max_radius: f32 = profile
        .iter()
        .map(|p| p[0].max(0.0))
        .fold(0.0_f32, f32::max);
    let mut profile_v: Vec<f32> = Vec::with_capacity(profile.len());
    let mut acc = 0.0_f32;
    profile_v.push(0.0);
    for w in profile.windows(2) {
        let dr = w[1][0] - w[0][0];
        let dy = w[1][1] - w[0][1];
        acc += (dr * dr + dy * dy).sqrt();
        profile_v.push(acc);
    }
    // V runs bottom→top along the profile; U wraps around. This keeps textures
    // continuous across lathed surfaces regardless of profile density.
    let row_count = profile.len() as f32 - 1.0;
    let (u_scale, fit_v_norm) = match mode {
        UvMode::Fit => (1.0, true),
        UvMode::Tile => (TAU * max_radius, false),
    };
    let ring_start: Vec<u32> = profile
        .iter()
        .enumerate()
        .map(|(idx, p)| {
            let start = positions.len() as u32;
            let v = if fit_v_norm {
                idx as f32 / row_count
            } else {
                profile_v[idx]
            };
            let r = p[0].max(0.0);
            let y = p[1];
            if r < 1e-6 {
                positions.push([0.0, y, 0.0]);
                uvs.push([0.0, v]);
                for s in 1..=segments {
                    positions.push([0.0, y, 0.0]);
                    uvs.push([(s as f32 / segments as f32) * u_scale, v]);
                }
            } else {
                for s in 0..=segments {
                    let t = s as f32 / segments as f32;
                    let a = t * TAU;
                    positions.push([a.cos() * r, y, a.sin() * r]);
                    uvs.push([t * u_scale, v]);
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
            let r0 = profile[0][0];
            let center = positions.len() as u32;
            positions.push([0.0, profile[0][1], 0.0]);
            uvs.push(disc_center_uv(mode));
            for s in 0..=segments {
                let a = (s as f32 / segments as f32) * TAU;
                let (sa, ca) = (a.sin(), a.cos());
                positions.push([ca * r0, profile[0][1], sa * r0]);
                uvs.push(disc_rim_uv(ca * r0, sa * r0, r0, mode));
            }
            for s in 0..segments {
                // CCW from -Y.
                indices.extend_from_slice(&[center, center + 1 + s, center + 2 + s]);
            }
        }
        if let Some(last) = profile.last() {
            if last[0] > 1e-6 {
                // Top cap (normal +Y).
                let rl = last[0];
                let center = positions.len() as u32;
                positions.push([0.0, last[1], 0.0]);
                uvs.push(disc_center_uv(mode));
                for s in 0..=segments {
                    let a = (s as f32 / segments as f32) * TAU;
                    let (sa, ca) = (a.sin(), a.cos());
                    positions.push([ca * rl, last[1], sa * rl]);
                    uvs.push(disc_rim_uv(ca * rl, sa * rl, rl, mode));
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
    mode: UvMode,
) -> Mesh {
    let radial_segments = radial_segments.max(3);
    if points.len() < 2 {
        return Mesh::default();
    }

    let samples = sample_catmull_rom(points, samples_per_segment);
    if samples.len() < 2 {
        return Mesh::default();
    }

    // Per-sample arc length along the centreline — used as V in tile mode so
    // the texture follows the actual swept length, not the sample-index ratio.
    let mut sample_arc: Vec<f32> = Vec::with_capacity(samples.len());
    sample_arc.push(0.0);
    for w in samples.windows(2) {
        let last = *sample_arc.last().unwrap();
        sample_arc.push(last + (w[1] - w[0]).length());
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
        let v = match mode {
            UvMode::Fit => i as f32 / total_samples,
            UvMode::Tile => sample_arc[i],
        };
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
        let circ = TAU * r;
        for s in 0..=radial_segments {
            let u_param = s as f32 / radial_segments as f32;
            let a = u_param * TAU;
            let (sa, ca) = (a.sin(), a.cos());
            let offset = n_cur * ca + b_cur * sa;
            let p = *center + offset * r;
            positions.push([p.x, p.y, p.z]);
            normals.push([offset.x, offset.y, offset.z]);
            let u_out = match mode {
                UvMode::Fit => u_param,
                UvMode::Tile => u_param * circ,
            };
            uvs.push([u_out, v]);
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
        let r0 = radius_at(0);
        let r_end = radius_at(samples.len() - 1);
        // Start cap: normal = -t0. Fan around the first ring. Disc UV projection
        // in the local frame.
        let start_center = positions.len() as u32;
        let c0 = samples[0];
        positions.push([c0.x, c0.y, c0.z]);
        let n_start = -t0;
        normals.push([n_start.x, n_start.y, n_start.z]);
        uvs.push(disc_center_uv(mode));
        for s in 0..=radial_segments {
            let src = s as u32;
            let p = positions[src as usize];
            positions.push(p);
            normals.push([n_start.x, n_start.y, n_start.z]);
            let a = (s as f32 / radial_segments as f32) * TAU;
            uvs.push(disc_rim_uv(a.cos() * r0, a.sin() * r0, r0, mode));
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
        uvs.push(disc_center_uv(mode));
        for s in 0..=radial_segments {
            let p = positions[(end_first + s) as usize];
            positions.push(p);
            normals.push([t_end.x, t_end.y, t_end.z]);
            let a = (s as f32 / radial_segments as f32) * TAU;
            uvs.push(disc_rim_uv(a.cos() * r_end, a.sin() * r_end, r_end, mode));
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
