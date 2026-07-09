use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Mesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
    /// Per-vertex UVs (`TEXCOORD_0` in glTF). Must be empty or the same length
    /// as `positions`. Empty means "no UV channel" — the exporter omits
    /// `TEXCOORD_0` entirely, and any material with texture slots will render
    /// as a solid colour.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uvs: Vec<[f32; 2]>,
    /// Per-vertex joint indices into the owning node's `Skin::joints`. Up to 4
    /// influences. Must be empty or the same length as `positions`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub joints: Vec<[u16; 4]>,
    /// Per-vertex weights matching `joints`. Each row must sum to ~1.0.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub weights: Vec<[f32; 4]>,
    /// Per-vertex RGBA colours (`COLOR_0` in glTF). Populated by the
    /// gradient-bake pass when a mesh's material carries a gradient ramp.
    /// Multiplies `baseColorFactor` at render time per the glTF spec, so an
    /// authoring convention of `color=[1, 1, 1]` plus a gradient yields the
    /// raw ramp colours. Must be empty or the same length as `positions`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub colors: Vec<[f32; 4]>,
}

impl Mesh {
    pub fn new(positions: Vec<[f32; 3]>, normals: Vec<[f32; 3]>, indices: Vec<u32>) -> Self {
        Self {
            positions,
            normals,
            indices,
            uvs: Vec::new(),
            joints: Vec::new(),
            weights: Vec::new(),
            colors: Vec::new(),
        }
    }

    pub fn is_skinned(&self) -> bool {
        !self.joints.is_empty() && self.joints.len() == self.weights.len()
    }

    pub fn has_uvs(&self) -> bool {
        !self.uvs.is_empty() && self.uvs.len() == self.positions.len()
    }

    pub fn has_colors(&self) -> bool {
        !self.colors.is_empty() && self.colors.len() == self.positions.len()
    }

    /// Enclosed solid volume in local units (m³), via the divergence theorem:
    /// the signed volume is `⅙ Σ aᵢ·(bᵢ×cᵢ)` over every triangle `(a,b,c)`.
    /// Assumes a closed, consistently wound surface — which is exactly what
    /// mogen's watertight primitives + `clean_csg_output` produce. The result
    /// is returned as an absolute value so a flipped winding still yields a
    /// positive volume. An open or degenerate mesh yields a meaningless number;
    /// callers pair this with a mesh they know is closed.
    pub fn solid_volume(&self) -> f32 {
        let mut v6 = 0.0f64;
        for tri in self.indices.chunks_exact(3) {
            let a = self.positions[tri[0] as usize];
            let b = self.positions[tri[1] as usize];
            let c = self.positions[tri[2] as usize];
            v6 += scalar_triple(a, b, c) as f64;
        }
        (v6.abs() / 6.0) as f32
    }

    /// Volume centroid (centre of mass for a uniform-density solid), in local
    /// mesh space. Each triangle forms a tetrahedron with the origin whose
    /// centroid is `(a+b+c)/4`, weighted by its signed volume; the `⅙` factors
    /// cancel in the ratio. Returns `None` for a zero-volume (degenerate or
    /// open) mesh where the centroid is undefined.
    pub fn solid_centroid(&self) -> Option<[f32; 3]> {
        let mut vol = 0.0f64;
        let mut acc = [0.0f64; 3];
        for tri in self.indices.chunks_exact(3) {
            let a = self.positions[tri[0] as usize];
            let b = self.positions[tri[1] as usize];
            let c = self.positions[tri[2] as usize];
            let w = scalar_triple(a, b, c) as f64;
            vol += w;
            for k in 0..3 {
                acc[k] += w * (a[k] as f64 + b[k] as f64 + c[k] as f64) / 4.0;
            }
        }
        if vol.abs() < 1e-12 {
            return None;
        }
        Some([
            (acc[0] / vol) as f32,
            (acc[1] / vol) as f32,
            (acc[2] / vol) as f32,
        ])
    }
}

/// `a · (b × c)` — six times the signed volume of the tetrahedron `(0,a,b,c)`.
fn scalar_triple(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> f32 {
    let cx = b[1] * c[2] - b[2] * c[1];
    let cy = b[2] * c[0] - b[0] * c[2];
    let cz = b[0] * c[1] - b[1] * c[0];
    a[0] * cx + a[1] * cy + a[2] * cz
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unit cube spanning [-0.5, 0.5]³, wound outward (CCW seen from outside).
    fn unit_cube() -> Mesh {
        // 8 corners.
        let p = [
            [-0.5, -0.5, -0.5],
            [0.5, -0.5, -0.5],
            [0.5, 0.5, -0.5],
            [-0.5, 0.5, -0.5],
            [-0.5, -0.5, 0.5],
            [0.5, -0.5, 0.5],
            [0.5, 0.5, 0.5],
            [-0.5, 0.5, 0.5],
        ];
        // 12 triangles, outward-facing.
        let idx: Vec<u32> = vec![
            0, 2, 1, 0, 3, 2, // -Z
            4, 5, 6, 4, 6, 7, // +Z
            0, 1, 5, 0, 5, 4, // -Y
            3, 7, 6, 3, 6, 2, // +Y
            0, 4, 7, 0, 7, 3, // -X
            1, 2, 6, 1, 6, 5, // +X
        ];
        Mesh::new(p.to_vec(), vec![], idx)
    }

    #[test]
    fn unit_cube_volume_is_one() {
        let v = unit_cube().solid_volume();
        assert!((v - 1.0).abs() < 1e-5, "expected 1.0 m³, got {v}");
    }

    #[test]
    fn centered_cube_centroid_is_origin() {
        let c = unit_cube().solid_centroid().expect("closed mesh has a centroid");
        assert!(c[0].abs() < 1e-5 && c[1].abs() < 1e-5 && c[2].abs() < 1e-5, "got {c:?}");
    }

    #[test]
    fn translated_cube_centroid_follows() {
        let mut m = unit_cube();
        for p in &mut m.positions {
            p[1] += 2.0; // lift 2 m in +Y
        }
        let c = m.solid_centroid().expect("centroid");
        assert!((c[1] - 2.0).abs() < 1e-5, "centroid should track translation, got {c:?}");
        // Volume is translation-invariant.
        assert!((m.solid_volume() - 1.0).abs() < 1e-5);
    }

    #[test]
    fn open_mesh_has_no_centroid() {
        // A single triangle encloses no volume.
        let m = Mesh::new(
            vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            vec![],
            vec![0, 1, 2],
        );
        assert!(m.solid_centroid().is_none());
    }
}
