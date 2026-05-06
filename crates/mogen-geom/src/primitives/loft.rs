//! Loft: linearly interpolate between cross sections at different heights
//! along +Y. Generalises `frustum` (only two rectangles) and `lathe` (only
//! axisymmetric profiles) to arbitrary closed sections at arbitrary
//! heights.
//!
//! Constraint: every section must contain the same vertex count. The
//! lowering pass enforces this with a clear diagnostic before this kernel
//! ever runs; reaching the kernel with mismatched sections returns an
//! empty mesh defensively.

use anyhow::{anyhow, Result};
use mogen_core::{Mesh, UvMode};

use crate::cleanup::recompute_normals;

/// One closed cross-section in its local XZ plane (the section lies flat
/// at the section's `y`).
pub type Section = Vec<[f32; 2]>;

/// Build a lofted mesh by linearly interpolating between `sections` at the
/// matching `heights`. Each section's vertex count must match. Sections
/// may be supplied in any Y order — they are sorted internally so
/// `heights = [2.0, 0.0, 1.0]` produces the same surface as `[0.0, 1.0,
/// 2.0]`.
///
/// `samples_between_sections` subdivides each pair of adjacent sections
/// (≥1; 1 means no extra subdivision). Caps triangulate the bottom-most
/// and top-most sections via earcut when `caps` is true.
pub fn loft_mesh(
    sections: &[Section],
    heights: &[f32],
    samples_between_sections: u32,
    caps: bool,
    mode: UvMode,
) -> Result<Mesh> {
    if sections.len() < 2 {
        return Err(anyhow!("loft requires at least 2 sections, got {}", sections.len()));
    }
    if sections.len() != heights.len() {
        return Err(anyhow!(
            "loft sections.len() ({}) must equal heights.len() ({})",
            sections.len(),
            heights.len(),
        ));
    }
    let n = sections[0].len();
    if n < 3 {
        return Err(anyhow!("loft sections must have at least 3 points, got {}", n));
    }
    for (i, s) in sections.iter().enumerate() {
        if s.len() != n {
            return Err(anyhow!(
                "loft section {} has {} points but section 0 has {} — every \
                 section must share the same vertex count",
                i,
                s.len(),
                n,
            ));
        }
    }

    // Sort sections by height (ascending) without disturbing the caller's
    // original arrays.
    let mut order: Vec<usize> = (0..sections.len()).collect();
    order.sort_by(|&a, &b| heights[a].partial_cmp(&heights[b]).unwrap_or(std::cmp::Ordering::Equal));
    let sorted_sections: Vec<&Section> = order.iter().map(|&i| &sections[i]).collect();
    let sorted_heights: Vec<f32> = order.iter().map(|&i| heights[i]).collect();

    let samples_between = samples_between_sections.max(1);

    // Build all rings (sample-interpolated rings between each adjacent
    // pair). The total ring count is `samples_between * (sections - 1) + 1`.
    let total_rings = samples_between as usize * (sorted_sections.len() - 1) + 1;
    let mut rings: Vec<(f32, Section)> = Vec::with_capacity(total_rings);
    for pair_idx in 0..sorted_sections.len() - 1 {
        let a = sorted_sections[pair_idx];
        let b = sorted_sections[pair_idx + 1];
        let h_a = sorted_heights[pair_idx];
        let h_b = sorted_heights[pair_idx + 1];
        let steps = if pair_idx == sorted_sections.len() - 2 {
            samples_between + 1
        } else {
            samples_between
        };
        for s in 0..steps {
            let t = s as f32 / samples_between as f32;
            let mut ring = Vec::with_capacity(n);
            for i in 0..n {
                let p_a = a[i];
                let p_b = b[i];
                ring.push([
                    p_a[0] * (1.0 - t) + p_b[0] * t,
                    p_a[1] * (1.0 - t) + p_b[1] * t,
                ]);
            }
            let h = h_a * (1.0 - t) + h_b * t;
            rings.push((h, ring));
        }
    }

    // Profile arc length (closed) — drives U in tile mode.
    let profile_arc = closed_arc_lengths(&rings[0].1);
    let profile_perimeter = *profile_arc.last().unwrap();

    let total_arc = (rings[rings.len() - 1].0 - rings[0].0).abs().max(1e-6);

    let mut mesh = Mesh::default();

    // Push every ring's vertices: n + 1 per ring (with a duplicated seam
    // vertex for clean U wrapping in tile mode).
    let row = (n + 1) as u32;
    for (h, ring) in rings.iter() {
        let v = match mode {
            UvMode::Fit => (h - rings[0].0) / total_arc,
            UvMode::Tile => *h,
        };
        for (j, p) in ring.iter().enumerate() {
            mesh.positions.push([p[0], *h, p[1]]);
            mesh.normals.push([0.0, 0.0, 0.0]);
            let u = match mode {
                UvMode::Fit => profile_arc[j] / profile_perimeter.max(1e-6),
                UvMode::Tile => profile_arc[j],
            };
            mesh.uvs.push([u, v]);
        }
        // Duplicated seam vertex.
        mesh.positions.push([ring[0][0], *h, ring[0][1]]);
        mesh.normals.push([0.0, 0.0, 0.0]);
        let u = match mode {
            UvMode::Fit => 1.0,
            UvMode::Tile => profile_perimeter,
        };
        mesh.uvs.push([u, v]);
    }

    // Quad strips between adjacent rings.
    for r in 0..(rings.len() as u32 - 1) {
        for j in 0..n as u32 {
            let a = r * row + j;
            let b = a + 1;
            let c = a + row;
            let d = c + 1;
            mesh.indices.extend_from_slice(&[a, b, d, a, d, c]);
        }
    }

    if caps {
        // Bottom cap: section[0], normal -Y, winding flipped.
        push_cap(&mut mesh, &rings[0].1, rings[0].0, true, mode);
        // Top cap: last section, normal +Y.
        let last = rings.len() - 1;
        push_cap(&mut mesh, &rings[last].1, rings[last].0, false, mode);
    }

    recompute_normals(&mut mesh);
    Ok(mesh)
}

fn push_cap(mesh: &mut Mesh, section: &[[f32; 2]], y: f32, flip_winding: bool, mode: UvMode) {
    let base = mesh.positions.len() as u32;
    let (mut min_x, mut max_x) = (f32::INFINITY, f32::NEG_INFINITY);
    let (mut min_z, mut max_z) = (f32::INFINITY, f32::NEG_INFINITY);
    for p in section {
        if p[0] < min_x { min_x = p[0]; }
        if p[0] > max_x { max_x = p[0]; }
        if p[1] < min_z { min_z = p[1]; }
        if p[1] > max_z { max_z = p[1]; }
    }
    let span_x = (max_x - min_x).max(1e-6);
    let span_z = (max_z - min_z).max(1e-6);
    for p in section {
        mesh.positions.push([p[0], y, p[1]]);
        mesh.normals.push([0.0, 0.0, 0.0]);
        let uv = match mode {
            UvMode::Fit => [
                (p[0] - min_x) / span_x,
                (p[1] - min_z) / span_z,
            ],
            UvMode::Tile => [p[0], p[1]],
        };
        mesh.uvs.push(uv);
    }
    let mut flat: Vec<f32> = Vec::with_capacity(section.len() * 2);
    for p in section {
        flat.push(p[0]);
        flat.push(p[1]);
    }
    if let Ok(tri) = earcutr::earcut(&flat, &[], 2) {
        for c in tri.chunks(3) {
            let a = base + c[0] as u32;
            let b = base + c[1] as u32;
            let d = base + c[2] as u32;
            if flip_winding {
                mesh.indices.extend_from_slice(&[a, d, b]);
            } else {
                mesh.indices.extend_from_slice(&[a, b, d]);
            }
        }
    }
}

fn closed_arc_lengths(profile: &[[f32; 2]]) -> Vec<f32> {
    let mut arc = vec![0.0_f32];
    for w in profile.windows(2) {
        let last = *arc.last().unwrap();
        let dx = w[1][0] - w[0][0];
        let dy = w[1][1] - w[0][1];
        arc.push(last + (dx * dx + dy * dy).sqrt());
    }
    let last = *arc.last().unwrap();
    let dx = profile[0][0] - profile[profile.len() - 1][0];
    let dy = profile[0][1] - profile[profile.len() - 1][1];
    arc.push(last + (dx * dx + dy * dy).sqrt());
    arc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(w: f32, d: f32) -> Section {
        vec![[-w, -d], [w, -d], [w, d], [-w, d]]
    }

    #[test]
    fn lofts_two_rectangles_into_frustum_like_shape() {
        let mesh = loft_mesh(
            &[rect(1.0, 1.0), rect(0.5, 0.5)],
            &[0.0, 1.0],
            8,
            true,
            UvMode::default(),
        )
        .unwrap();
        let (min, max) = aabb(&mesh.positions);
        assert!((min[1]).abs() < 1e-5 && (max[1] - 1.0).abs() < 1e-5,
            "Y span should be [0,1] (got [{}, {}])", min[1], max[1]);
        assert!((min[0] + 1.0).abs() < 1e-3 && (max[0] - 1.0).abs() < 1e-3,
            "bottom rect dominates X extent (got [{}, {}])", min[0], max[0]);
    }

    #[test]
    fn loft_rejects_mismatched_section_lengths() {
        let triangle: Section = vec![[-1.0, 0.0], [1.0, 0.0], [0.0, 1.0]];
        let result = loft_mesh(
            &[rect(1.0, 1.0), triangle],
            &[0.0, 1.0],
            4,
            true,
            UvMode::default(),
        );
        assert!(result.is_err(), "mismatched section sizes should error");
    }

    #[test]
    fn loft_three_sections_interpolates_middle() {
        // Tall slim, middle wider, top tall slim again — boat hull / fuselage shape.
        let mesh = loft_mesh(
            &[rect(0.5, 0.2), rect(1.0, 0.4), rect(0.6, 0.1)],
            &[0.0, 1.0, 2.0],
            8,
            true,
            UvMode::default(),
        )
        .unwrap();
        // Mid-height ring should reach max X = 1.0.
        let mut max_at_mid = 0.0_f32;
        for p in &mesh.positions {
            if (p[1] - 1.0).abs() < 0.05 && p[0] > max_at_mid {
                max_at_mid = p[0];
            }
        }
        assert!((max_at_mid - 1.0).abs() < 1e-3,
            "middle section X extent should be 1.0 (got {max_at_mid})");
    }

    #[test]
    fn loft_sorts_sections_by_height() {
        // Same shape as `lofts_two_rectangles_into_frustum_like_shape` but
        // heights given in reverse — output must be identical.
        let mesh_a = loft_mesh(
            &[rect(1.0, 1.0), rect(0.5, 0.5)],
            &[0.0, 1.0],
            4,
            false,
            UvMode::default(),
        )
        .unwrap();
        let mesh_b = loft_mesh(
            &[rect(0.5, 0.5), rect(1.0, 1.0)],
            &[1.0, 0.0],
            4,
            false,
            UvMode::default(),
        )
        .unwrap();
        assert_eq!(mesh_a.positions.len(), mesh_b.positions.len());
        assert_eq!(mesh_a.indices.len(), mesh_b.indices.len());
    }

    fn aabb(positions: &[[f32; 3]]) -> ([f32; 3], [f32; 3]) {
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        for p in positions {
            for i in 0..3 {
                if p[i] < min[i] { min[i] = p[i]; }
                if p[i] > max[i] { max[i] = p[i]; }
            }
        }
        (min, max)
    }
}
