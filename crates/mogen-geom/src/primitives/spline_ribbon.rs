use glam::Vec3;
use mogen_core::{Mesh, UvMode};

/// Catmull–Rom spline through `points`, sampled `samples_per_segment` times per
/// input interval. Mirrors the centerline sampling used by `spline_tube_mesh`
/// so a ribbon and a tube authored from the same control points trace the
/// same path.
fn sample_catmull_rom(points: &[[f32; 3]], samples_per_segment: u32) -> Vec<Vec3> {
    let n = points.len();
    if n < 2 {
        return points.iter().map(|p| Vec3::from_array(*p)).collect();
    }
    let samples_per_segment = samples_per_segment.max(1);
    let mut out = Vec::with_capacity(n * samples_per_segment as usize + 1);

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
            let a = -0.5 * t3 + t2 - 0.5 * t;
            let b = 1.5 * t3 - 2.5 * t2 + 1.0;
            let c = -1.5 * t3 + 2.0 * t2 + 0.5 * t;
            let d = 0.5 * t3 - 0.5 * t2;
            out.push(p0 * a + p1 * b + p2 * c + p3 * d);
        }
    }
    out
}

#[inline]
fn rotate_around_axis(v: Vec3, axis: Vec3, angle: f32) -> Vec3 {
    let (s, c) = angle.sin_cos();
    v * c + axis.cross(v) * s + axis * axis.dot(v) * (1.0 - c)
}

/// Flat strip swept along a Catmull–Rom curve through `points`, with width
/// sampled from `widths` (one per control point; pass `&[w]` for a constant
/// width). The strip lies across the curve's binormal at each sample, with the
/// front face's normal aligned to the curve's normal axis (the same axis used
/// for `spline_tube`'s parallel-transport frame). `twist` rotates the cross-
/// section linearly from 0 at the start to `twist` radians at the end.
///
/// The mesh is emitted **double-sided**: each quad is duplicated with reversed
/// winding and inverted normals, so the ribbon renders correctly regardless of
/// the assigned material's `double_sided` flag (or absence of any material at
/// all). A ribbon is by nature a 2D surface in 3D — back-face culling on a
/// single-sided emission would make half of every shot invisible.
pub fn spline_ribbon_mesh(
    points: &[[f32; 3]],
    widths: &[f32],
    samples_per_segment: u32,
    twist: f32,
    mode: UvMode,
) -> Mesh {
    if points.len() < 2 {
        return Mesh::default();
    }

    let samples = sample_catmull_rom(points, samples_per_segment);
    if samples.len() < 2 {
        return Mesh::default();
    }

    // Per-sample arc length along the centerline — used as V in tile mode so
    // the texture follows the actual swept length, not the sample-index ratio.
    let mut sample_arc: Vec<f32> = Vec::with_capacity(samples.len());
    sample_arc.push(0.0);
    for w in samples.windows(2) {
        let last = *sample_arc.last().unwrap();
        sample_arc.push(last + (w[1] - w[0]).length());
    }

    // Per-sample width: linear interp of `widths` over the input intervals.
    // Scalar widths broadcast.
    let width_at = |sample_idx: usize| -> f32 {
        if widths.len() == 1 {
            return widths[0];
        }
        if widths.is_empty() {
            return 0.2;
        }
        let total = samples.len() as f32 - 1.0;
        let pos = (sample_idx as f32 / total) * (points.len() as f32 - 1.0);
        let i = pos.floor().min(points.len() as f32 - 2.0).max(0.0) as usize;
        let t = (pos - i as f32).clamp(0.0, 1.0);
        let a = widths[i.min(widths.len() - 1)];
        let b = widths[(i + 1).min(widths.len() - 1)];
        a * (1.0 - t) + b * t
    };

    // Parallel-transport frame, identical strategy to `spline_tube_mesh`. Only
    // the cross-section differs: a 1D segment along the binormal instead of a
    // ring around (normal, binormal).
    let tangent_at = |i: usize| -> Vec3 {
        let a = if i == 0 { samples[0] } else { samples[i - 1] };
        let b = if i + 1 == samples.len() { samples[samples.len() - 1] } else { samples[i + 1] };
        (b - a).normalize_or(Vec3::Z)
    };

    let t0 = tangent_at(0);
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
    // Front-face vertices: two per sample (left edge, right edge along binormal).
    for (i, center) in samples.iter().enumerate() {
        let v = match mode {
            UvMode::Fit => i as f32 / total_samples,
            UvMode::Tile => sample_arc[i],
        };
        let t_cur = tangent_at(i);
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

        // Apply per-sample twist around the tangent so the strip can spiral
        // along the curve. Twist is linear in arc-fraction, matching how a
        // ribbon physically twists when a flat band is rolled.
        let twist_t = i as f32 / total_samples;
        let theta = twist * twist_t;
        let n_twist = rotate_around_axis(n_cur, t_cur, theta).normalize_or(n_cur);
        let b_twist = rotate_around_axis(b_cur, t_cur, theta).normalize_or(b_cur);

        let half = width_at(i) * 0.5;
        let p_left = *center - b_twist * half;
        let p_right = *center + b_twist * half;
        positions.push([p_left.x, p_left.y, p_left.z]);
        positions.push([p_right.x, p_right.y, p_right.z]);
        normals.push([n_twist.x, n_twist.y, n_twist.z]);
        normals.push([n_twist.x, n_twist.y, n_twist.z]);
        let u_scale = match mode {
            UvMode::Fit => 1.0,
            UvMode::Tile => width_at(i),
        };
        uvs.push([0.0, v]);
        uvs.push([u_scale, v]);

        n_prev = n_cur;
        t_prev = t_cur;
    }

    // Front-face triangles (CCW when viewed from +n_twist).
    for i in 0..samples.len() as u32 - 1 {
        let a = i * 2;
        let b = a + 1;
        let d = a + 2;
        let c = a + 3;
        indices.extend_from_slice(&[a, b, c, a, c, d]);
    }

    // Back-face: duplicate every front vertex with its normal flipped, and
    // emit the same quads with reversed winding. Doubling the vertex set
    // (rather than relying on a `double_sided` material flag) means a
    // `spline_ribbon` looks correct even with no material, the wrong material,
    // or under engines that ignore glTF's `doubleSided`.
    let back_base = positions.len() as u32;
    let front_count = positions.len();
    for i in 0..front_count {
        positions.push(positions[i]);
        let n = normals[i];
        normals.push([-n[0], -n[1], -n[2]]);
        // Mirror U so the back face's UVs aren't a mirror of the front
        // (otherwise text would read backwards through the strip).
        let uv = uvs[i];
        uvs.push([1.0 - uv[0], uv[1]]);
    }
    for i in 0..samples.len() as u32 - 1 {
        let a = back_base + i * 2;
        let b = a + 1;
        let d = a + 2;
        let c = a + 3;
        // Reversed winding so the back face's geometric normal points into
        // -n_twist, matching the flipped vertex normal.
        indices.extend_from_slice(&[a, c, b, a, d, c]);
    }

    Mesh { positions, normals, uvs, indices, ..Default::default() }
}
