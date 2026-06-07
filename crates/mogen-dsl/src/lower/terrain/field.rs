//! The `HeightField` intermediate and the source + retouch pipeline.
//!
//! This is the heart of the terrain generator and the piece the DTL study
//! pointed at: instead of fusing noise straight into a mesh, a terrain is built
//! as a dense grid of normalised heights that **sources** fill and **retouch**
//! passes refine, before `emit.rs` ever touches geometry. Keeping the field
//! separate is what lets `source`, `smooth` and `terrace` compose as
//! independent, testable stages. `sea_level` is deliberately *not* a field
//! pass: the land keeps its real shape underwater and the sea is emitted as a
//! flat plane on top (see `emit.rs`), so basins read as real submerged terrain
//! rather than flat water beds.
//!
//! Heights are stored normalised in `[0, 1]`; `emit.rs` scales them by the
//! patch `amplitude`. The grid is `n × n` samples (`n = segments + 1`) so chunk
//! boundaries can fall exactly on shared grid lines and meet crack-free.

use super::config::{SourceKind, TerrainCfg};

/// A row-major grid of normalised heights in `[0, 1]`.
pub(super) struct HeightField {
    /// Samples per axis (`segments + 1`).
    pub n: usize,
    /// Length-`n*n` height values, row-major (`h[j * n + i]`, i over X, j over Z).
    pub h: Vec<f32>,
    /// Length-`n*n` road intensity in `[0, 1]`, row-major like `h`. `0` away from
    /// any road, ramping to `1` on the flattened corridor centre (see
    /// `carve::carve_roads`). `emit.rs` reads it to tint COLOR_0 toward the road
    /// surface colour. All-zero when no `road` children are declared.
    pub road_mask: Vec<f32>,
}

impl HeightField {
    #[inline]
    pub fn at(&self, i: usize, j: usize) -> f32 {
        self.h[j * self.n + i]
    }
}

/// Build the height field for `cfg`: pick a grid resolution, fill it from the
/// chosen source, then run the retouch passes in order. The returned grid has
/// `segments` divisible by `cfg.chunks` so the emitter can split it cleanly.
pub(super) fn build(cfg: &TerrainCfg) -> HeightField {
    // Trim resolution by lod_scale, then round the per-axis segment count up to
    // a whole multiple of `chunks` so every chunk owns the same integer number
    // of cells and adjacent chunks share their boundary grid line exactly.
    let chunks = cfg.chunks.max(1);
    let target = ((cfg.resolution as f32 * cfg.lod_scale).round() as u32).max(chunks);
    // Each chunk must split evenly at every LOD stride (2^level), so its segment
    // count has to be a multiple of the coarsest stride `2^(lod_levels-1)`.
    // Round the per-chunk cell count up to that multiple; this also keeps it ≥ 1.
    let coarsest_stride = 1u32 << (cfg.lod_levels.clamp(1, 4) - 1);
    let seg_per_chunk = target.div_ceil(chunks).max(1).div_ceil(coarsest_stride) * coarsest_stride;
    let segments = (seg_per_chunk * chunks) as usize;
    let n = segments + 1;

    let w = cfg.size[0];
    let d = cfg.size[2];
    let half_w = w * 0.5;
    let half_d = d * 0.5;

    let mut h = vec![0.0f32; n * n];
    for j in 0..n {
        let tz = j as f32 / segments as f32; // 0..1 along Z
        let z = -half_d + tz * d;
        for i in 0..n {
            let tx = i as f32 / segments as f32; // 0..1 along X
            let x = -half_w + tx * w;
            h[j * n + i] = sample_source(cfg, x, z);
        }
    }

    let mut field = HeightField {
        n,
        h,
        road_mask: vec![0.0; n * n],
    };

    for _ in 0..cfg.smooth {
        smooth_once(&mut field);
    }
    if cfg.terrace > 1 {
        terrace(&mut field, cfg.terrace);
    }
    // `sea_level` intentionally does not touch the field: the water plane is
    // emitted separately so land keeps its true shape below the waterline.

    field
}

/// Sample the configured source at world `(x, z)`, returned normalised to
/// roughly `[0, 1]`.
fn sample_source(cfg: &TerrainCfg, x: f32, z: f32) -> f32 {
    let raw = fbm(
        x,
        z,
        cfg.frequency,
        cfg.octaves,
        cfg.persistence,
        cfg.seed,
    ); // ~[-1, 1]
    let v = match cfg.source {
        SourceKind::Fbm => 0.5 + 0.5 * raw,
        // Ridged multifractal: peaks where the noise crosses zero.
        SourceKind::Ridged => 1.0 - raw.abs(),
        // Billow: rounded mounds.
        SourceKind::Billow => raw.abs(),
        // Island: fBm hills shaped by a radial falloff so the patch sinks
        // toward the edges, leaving a central landmass surrounded by water.
        SourceKind::Island => (0.5 + 0.5 * raw) * radial_falloff(cfg, x, z),
        // Voronoi: worley F1 distance — feature-point cells with raised rims.
        SourceKind::Voronoi => worley_f1(x, z, cfg.frequency, cfg.seed),
    };
    v.clamp(0.0, 1.0)
}

/// Smoothstep in `[edge0, edge1]`, clamped outside.
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    if (edge1 - edge0).abs() < 1e-6 {
        return if x < edge0 { 0.0 } else { 1.0 };
    }
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Radial falloff for `island`: `1` at the patch centre, smoothly dropping to
/// `0` past `~85%` of the way to the nearest edge. Distance is normalised by the
/// patch half-extents so non-square patches still fall off evenly.
fn radial_falloff(cfg: &TerrainCfg, x: f32, z: f32) -> f32 {
    let half_w = (cfg.size[0] * 0.5).max(1e-3);
    let half_d = (cfg.size[2] * 0.5).max(1e-3);
    let nx = x / half_w;
    let nz = z / half_d;
    let d = (nx * nx + nz * nz).sqrt();
    1.0 - smoothstep(0.5, 0.85, d)
}

/// Worley (cellular) F1: distance to the nearest scattered feature point,
/// normalised to roughly `[0, 1]`. One jittered feature point per integer cell
/// (cell size `1/frequency` in world units); searches the 3×3 cell neighbourhood
/// so the true nearest point is never missed. Higher near cell edges → raised
/// rims and basins between points.
fn worley_f1(x: f32, z: f32, frequency: f32, seed: u32) -> f32 {
    let freq = frequency.max(1e-6);
    let px = x * freq;
    let pz = z * freq;
    let cx = px.floor() as i32;
    let cz = pz.floor() as i32;
    let mut best = f32::MAX;
    for dz in -1i32..=1 {
        for dx in -1i32..=1 {
            let gx = cx + dx;
            let gz = cz + dz;
            // Jitter the feature point inside its cell using two decorrelated
            // hashes (remap hash2's [-1,1] to [0,1]).
            let fx = gx as f32 + (hash2(gx, gz, seed) * 0.5 + 0.5);
            let fz = gz as f32 + (hash2(gx, gz, seed ^ 0x68BC_21EB) * 0.5 + 0.5);
            let ddx = fx - px;
            let ddz = fz - pz;
            let dist2 = ddx * ddx + ddz * ddz;
            if dist2 < best {
                best = dist2;
            }
        }
    }
    // F1 in cell units is at most ~1.4 (corner-to-corner); scale to fill [0,1].
    (best.sqrt() * 1.2).clamp(0.0, 1.0)
}

/// One 3×3 box-blur pass (DTL `Average`). Edge samples clamp to the border.
fn smooth_once(field: &mut HeightField) {
    let n = field.n;
    let mut out = vec![0.0f32; n * n];
    for j in 0..n {
        for i in 0..n {
            let mut sum = 0.0;
            let mut cnt = 0.0;
            for dj in -1i32..=1 {
                for di in -1i32..=1 {
                    let ii = (i as i32 + di).clamp(0, n as i32 - 1) as usize;
                    let jj = (j as i32 + dj).clamp(0, n as i32 - 1) as usize;
                    sum += field.at(ii, jj);
                    cnt += 1.0;
                }
            }
            out[j * n + i] = sum / cnt;
        }
    }
    field.h = out;
}

/// Quantise heights into `steps` bands for a stepped / plateau look.
fn terrace(field: &mut HeightField, steps: u32) {
    let steps = steps as f32;
    for v in field.h.iter_mut() {
        *v = (*v * steps).round() / steps;
    }
}

// --- value-noise fBm -------------------------------------------------------
//
// A compact, self-contained bilinear value-noise fBm so the terrain generator
// owns its field source outright. Mirrors the smoothstep-interpolated value
// noise used by the `heightfield` primitive; deterministic for a given seed.

fn fbm(x: f32, z: f32, frequency: f32, octaves: u32, persistence: f32, seed: u32) -> f32 {
    let mut sum = 0.0f32;
    let mut amp = 1.0f32;
    let mut freq = frequency.max(1e-6);
    let mut norm = 0.0f32;
    for _ in 0..octaves {
        sum += amp * value_noise_2d(x * freq, z * freq, seed);
        norm += amp;
        amp *= persistence;
        freq *= 2.0;
    }
    // Normalise by the summed octave amplitudes so the result stays in [-1, 1]
    // regardless of octave count / persistence (keeps `sea_level` and the
    // source transforms predictable).
    if norm > 0.0 {
        sum / norm
    } else {
        0.0
    }
}

fn value_noise_2d(x: f32, z: f32, seed: u32) -> f32 {
    let xi = x.floor();
    let zi = z.floor();
    let fx = x - xi;
    let fz = z - zi;
    let xi = xi as i32;
    let zi = zi as i32;
    let v00 = hash2(xi, zi, seed);
    let v10 = hash2(xi + 1, zi, seed);
    let v01 = hash2(xi, zi + 1, seed);
    let v11 = hash2(xi + 1, zi + 1, seed);
    let sx = fx * fx * (3.0 - 2.0 * fx);
    let sz = fz * fz * (3.0 - 2.0 * fz);
    let a = v00 * (1.0 - sx) + v10 * sx;
    let b = v01 * (1.0 - sx) + v11 * sx;
    a * (1.0 - sz) + b * sz
}

fn hash2(x: i32, z: i32, seed: u32) -> f32 {
    let mut h: u32 = seed.wrapping_add(0x9E37_79B9);
    h = h.wrapping_add(x as u32).wrapping_mul(0x85EB_CA6B);
    h ^= h >> 13;
    h = h.wrapping_add(z as u32).wrapping_mul(0xC2B2_AE35);
    h ^= h >> 16;
    h = h.wrapping_mul(0x27D4_EB2F);
    h ^= h >> 15;
    let bits = (h >> 8) & 0x00FF_FFFF;
    (bits as f32 / (1u32 << 24) as f32) * 2.0 - 1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> TerrainCfg {
        TerrainCfg {
            seed: 7,
            mat_style: String::new(),
            size: [40.0, 6.0, 40.0],
            source: SourceKind::Fbm,
            octaves: 4,
            frequency: 0.06,
            persistence: 0.5,
            resolution: 32,
            chunks: 4,
            lod_levels: 3,
            smooth: 0,
            terrace: 0,
            sea_level: 0.0,
            colliders: super::super::config::ColliderMode::All,
            peaks: 0,
            flat_spots: 0,
            shore_points: 0,
            lod_scale: 1.0,
            debug_show_poi: false,
        }
    }

    #[test]
    fn segments_divisible_by_chunks() {
        let f = build(&cfg());
        let segments = f.n - 1;
        assert_eq!(segments % 4, 0, "segments {segments} not divisible by chunks");
    }

    #[test]
    fn chunk_segments_divide_at_coarsest_lod() {
        // Each chunk's cell count must be a multiple of 2^(lod_levels-1) so
        // every LOD stride samples it cleanly (no fractional steps / cracks).
        for lod_levels in 1..=4u32 {
            let mut c = cfg();
            c.lod_levels = lod_levels;
            let f = build(&c);
            let spc = (f.n - 1) / c.chunks as usize;
            let stride = 1usize << (lod_levels - 1);
            assert_eq!(
                spc % stride,
                0,
                "spc {spc} not divisible by stride {stride} at lod_levels={lod_levels}"
            );
        }
    }

    #[test]
    fn heights_normalised() {
        let f = build(&cfg());
        for v in &f.h {
            assert!((0.0..=1.0).contains(v), "height {v} out of [0,1]");
        }
    }

    #[test]
    fn deterministic_for_seed() {
        let a = build(&cfg());
        let b = build(&cfg());
        assert_eq!(a.h, b.h);
    }

    #[test]
    fn smooth_reduces_variation() {
        let mut c = cfg();
        c.smooth = 0;
        let rough = build(&c);
        c.smooth = 8;
        let smooth = build(&c);
        let var = |f: &HeightField| {
            let mean = f.h.iter().sum::<f32>() / f.h.len() as f32;
            f.h.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / f.h.len() as f32
        };
        assert!(var(&smooth) < var(&rough), "smoothing did not reduce variance");
    }

    #[test]
    fn island_falls_off_to_water_at_edges() {
        // Centre samples should sit well above the edge/corner samples, which
        // the radial falloff drives toward zero.
        let mut c = cfg();
        c.source = SourceKind::Island;
        let f = build(&c);
        let n = f.n;
        let centre = f.at(n / 2, n / 2);
        let corner = f.at(0, 0);
        assert!(
            centre > corner + 0.1,
            "island centre {centre} not clearly above corner {corner}"
        );
        assert!(corner < 0.1, "island corner {corner} not near sea floor");
    }

    #[test]
    fn voronoi_stays_normalised_and_varies() {
        let mut c = cfg();
        c.source = SourceKind::Voronoi;
        let f = build(&c);
        let mut lo = f32::MAX;
        let mut hi = f32::MIN;
        for &v in &f.h {
            assert!((0.0..=1.0).contains(&v), "voronoi height {v} out of [0,1]");
            lo = lo.min(v);
            hi = hi.max(v);
        }
        assert!(hi - lo > 0.1, "voronoi field is too flat ({lo}..{hi})");
    }

    #[test]
    fn sea_level_does_not_alter_field() {
        // Sea level is just a water plane now, so the height field must be
        // identical with and without it — and land must genuinely dip below
        // the waterline somewhere instead of being floored flat.
        let f0 = build(&cfg());
        let mut c = cfg();
        c.sea_level = 0.4;
        let f1 = build(&c);
        assert_eq!(f0.h, f1.h, "sea_level reshaped the height field");
        assert!(
            f1.h.iter().any(|&v| v < 0.4),
            "no terrain dips below sea level"
        );
    }
}
