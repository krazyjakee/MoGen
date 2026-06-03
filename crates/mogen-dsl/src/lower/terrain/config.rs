//! AST → typed `TerrainCfg` reader.
//!
//! A terrain patch is a deterministic function of `seed=` plus the declared
//! attrs, exactly like `cave`/`building`/`branch`. The reader pulls every attr
//! off the `terrain` node, applies a default, and clamps defensively (the
//! validator has already rejected the egregious cases, but a value that slipped
//! past shouldn't panic at lowering time).
//!
//! The headline knobs split into three groups: the noise *source* + its shape
//! parameters (which fill the height field), the *retouch* passes that refine
//! that field (smooth / terrace / sea level — mirroring DTL's `Retouch` layer),
//! and the *chunking* + collider/POI controls that drive emission.

use crate::ast::Node;
use crate::lower::cfg;

/// The height-field source algorithm. All but `voronoi` are cheap transforms of
/// the same fractional-Brownian-motion value noise; `voronoi` adds a worley
/// distance field. They share one sampler and lower without extra dependencies.
/// (Diamond-square remains a planned follow-up — it needs a recursive whole-grid
/// fill that does not fit the per-sample model used here.)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SourceKind {
    /// Plain fBm — rolling hills and dunes.
    Fbm,
    /// Ridged multifractal (`1 - |fBm|`) — sharp mountain ridges and canyons.
    Ridged,
    /// Billow (`|fBm|`) — lumpy, cloud-like rounded mounds.
    Billow,
    /// fBm shaped by a radial falloff so the patch rises to a central landmass
    /// and sinks below `sea_level` toward the edges — an island in open water.
    Island,
    /// Worley (cellular) F1 distance — crater rims, dry cracked basins, and
    /// rocky cell-edge ridges from scattered feature points.
    Voronoi,
}

impl SourceKind {
    pub fn parse(s: &str) -> Option<SourceKind> {
        Some(match s {
            "fbm" => SourceKind::Fbm,
            "ridged" => SourceKind::Ridged,
            "billow" => SourceKind::Billow,
            "island" => SourceKind::Island,
            "voronoi" => SourceKind::Voronoi,
            _ => return None,
        })
    }
}

/// Which terrain surfaces get a trimesh collider for the game engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ColliderMode {
    /// No colliders on any terrain geometry.
    None,
    /// Every terrain chunk gets a trimesh collider.
    All,
}

impl ColliderMode {
    pub fn parse(s: &str) -> Option<ColliderMode> {
        Some(match s {
            "none" => ColliderMode::None,
            "all" => ColliderMode::All,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug)]
#[allow(dead_code)] // `mat_style` is forwarded to texture generation only.
pub(super) struct TerrainCfg {
    pub seed: u32,
    pub mat_style: String,
    /// Patch dimensions `[width(x), amplitude(y), depth(z)]` in metres. The
    /// patch is centred on the origin in XZ; the surface rises from `y=0` up to
    /// `amplitude` at the highest sample.
    pub size: [f32; 3],
    pub source: SourceKind,
    /// fBm octave count (layered noise frequencies).
    pub octaves: u32,
    /// Base spatial frequency (cycles per metre) of the noise.
    pub frequency: f32,
    /// Per-octave amplitude falloff (typically 0.5).
    pub persistence: f32,
    /// Target grid divisions along an axis before chunk splitting. Rounded up to
    /// a multiple of `chunks` so chunk boundaries land on shared grid lines
    /// (crack-free seams).
    pub resolution: u32,
    /// Chunks per axis: the patch is split into a `chunks × chunks` grid of
    /// independently cullable mesh nodes, each carrying its LOD variants.
    pub chunks: u32,
    /// Number of baked render-LOD variants emitted per chunk (`1`–`4`). Level 0
    /// is the full-resolution mesh; each higher level halves the sampling stride
    /// (quarter the triangles) and takes over at a greater camera distance.
    /// `1` emits a single full-detail mesh per chunk (no distance swapping).
    pub lod_levels: u32,
    /// Box-blur smoothing iterations applied to the field (DTL `Average`).
    pub smooth: u32,
    /// Terrace step count — quantises height into bands for a stepped/plateau
    /// look. `0` disables.
    pub terrace: u32,
    /// Normalised water level in `[0, 1)`. `0` disables water entirely; any
    /// positive value leaves the height field untouched (land keeps its true
    /// shape underwater) and emits a flat water plane + shoreline POIs on top.
    pub sea_level: f32,
    pub colliders: ColliderMode,
    /// Number of `peak` POI markers (local maxima) to emit.
    pub peaks: u32,
    /// Number of `flat_spot` POI markers (low-gradient buildable spots) to emit.
    pub flat_spots: u32,
    /// Number of `shoreline` POI markers to emit (only when `sea_level > 0`).
    pub shore_points: u32,
    /// Mesh-quality scale `(0, 1]`; compounds with the file-global `lod_scale`.
    /// Trims the grid resolution without changing the field shape or POIs.
    pub lod_scale: f32,
    /// Debug-only: give every POI marker a small bright sphere so the
    /// otherwise geometry-free markers show up in a preview.
    pub debug_show_poi: bool,
}

pub(super) fn read_cfg(node: &Node) -> TerrainCfg {
    let seed = cfg::seed(node);
    let mat_style = node.attr_string("mat_style").unwrap_or("").to_string();

    let size = node
        .attr_vec3("size")
        .map(|v| [v.x.max(1.0), v.y.max(0.0), v.z.max(1.0)])
        .unwrap_or([40.0, 6.0, 40.0]);

    let source = node
        .attr_string("source")
        .and_then(SourceKind::parse)
        .unwrap_or(SourceKind::Fbm);

    let octaves = cfg::int_clamped(node, "octaves", 4, 1, 8);
    let frequency = cfg::scalar(node, "frequency", 0.06, 1e-4);
    let persistence = cfg::scalar_clamped(node, "persistence", 0.5, 0.0, 1.0);
    let resolution = cfg::int_clamped(node, "resolution", 128, 4, 1024);
    let chunks = cfg::int_clamped(node, "chunks", 4, 1, 16);
    let lod_levels = cfg::int_clamped(node, "lod_levels", 3, 1, 4);

    let smooth = cfg::int_clamped(node, "smooth", 0, 0, 32);
    let terrace = cfg::int_clamped(node, "terrace", 0, 0, 64);
    let sea_level = cfg::scalar_clamped(node, "sea_level", 0.0, 0.0, 0.95);

    let colliders = node
        .attr_string("colliders")
        .and_then(ColliderMode::parse)
        .unwrap_or(ColliderMode::All);

    let peaks = cfg::count(node, "peaks", 0.0, 0.0);
    let flat_spots = cfg::count(node, "flat_spots", 0.0, 0.0);
    let shore_points = cfg::count(node, "shore_points", 0.0, 0.0);

    let lod_scale = (node.attr_number("lod_scale").unwrap_or(1.0)
        * crate::lower::lod::current_lod_scale())
    .clamp(0.1, 1.0);

    let debug_show_poi = cfg::flag(node, "debug_show_poi", false);

    TerrainCfg {
        seed,
        mat_style,
        size,
        source,
        octaves,
        frequency,
        persistence,
        resolution,
        chunks,
        lod_levels,
        smooth,
        terrace,
        sea_level,
        colliders,
        peaks,
        flat_spots,
        shore_points,
        lod_scale,
        debug_show_poi,
    }
}
