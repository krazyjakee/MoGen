//! AST → typed `CaveCfg` reader. Pulls every attr off the `cave` node and its
//! `feature` children, applying defaults and clamping defensively (the
//! validator has already rejected the egregious cases, but a value that
//! slipped past shouldn't panic at lowering time).
//!
//! A cave is, like `building`, a deterministic function of `seed=` plus the
//! declared attrs. The top-level scalar knobs cover the headline numbers
//! (size, chamber count, decoration counts); optional `feature "<name>"`
//! children fine-tune the material / size range / count of one decoration
//! kind, exactly as `room_type` tunes one room class on a building.

use anyhow::{bail, Result};

use crate::ast::Node;

/// The decoration kinds a cave can be populated with. Each maps to a distinct
/// mesh builder in `decorate.rs`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DecoKind {
    /// Floor spike pointing up.
    Stalagmite,
    /// Ceiling spike pointing down.
    Stalactite,
    /// Floor-to-ceiling stone pillar (a fused stalagmite + stalactite).
    Column,
    /// Cluster of small boulders on the floor.
    RockPile,
    /// Small flat water surface on a chamber floor.
    Pool,
    /// Large flat water surface spanning much of a chamber floor.
    Lake,
}

impl DecoKind {
    pub fn parse(s: &str) -> Option<DecoKind> {
        Some(match s {
            "stalagmite" => DecoKind::Stalagmite,
            "stalactite" => DecoKind::Stalactite,
            "column" => DecoKind::Column,
            "rock_pile" => DecoKind::RockPile,
            "pool" => DecoKind::Pool,
            "lake" => DecoKind::Lake,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            DecoKind::Stalagmite => "stalagmite",
            DecoKind::Stalactite => "stalactite",
            DecoKind::Column => "column",
            DecoKind::RockPile => "rock_pile",
            DecoKind::Pool => "pool",
            DecoKind::Lake => "lake",
        }
    }

    /// Whether this decoration is a water surface (drives default material).
    pub fn is_water(self) -> bool {
        matches!(self, DecoKind::Pool | DecoKind::Lake)
    }
}

/// Which generated rock surfaces get a trimesh collider for the game engine.
/// Water is handled separately by `CaveCfg::water_collider` regardless of this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ColliderMode {
    /// No colliders on any cave geometry.
    None,
    /// Only the outer rock shell collides; decorations are walk-through.
    Shell,
    /// Shell plus every solid decoration (stalagmites, columns, rock piles…).
    All,
}

impl ColliderMode {
    pub fn parse(s: &str) -> Option<ColliderMode> {
        Some(match s {
            "none" => ColliderMode::None,
            "shell" => ColliderMode::Shell,
            "all" => ColliderMode::All,
            _ => return None,
        })
    }
}

/// A resolved decoration group: how many of `kind` to scatter, the size range
/// (metres — radius for spikes/piles, surface radius for water), and an
/// optional material override. Built by merging the top-level count knob for
/// each kind with any matching `feature` child.
#[derive(Clone, Debug)]
pub(super) struct DecoGroup {
    pub kind: DecoKind,
    pub count: u32,
    pub min_size: f32,
    pub max_size: f32,
    pub mat: Option<String>,
}

#[derive(Clone, Debug)]
#[allow(dead_code)] // `mat_style` is forwarded to texture generation only.
pub(super) struct CaveCfg {
    pub seed: u32,
    pub mat_style: String,
    /// Outer rock-block dimensions [width(x), height(y), depth(z)] in metres.
    /// The block's base sits on y=0; chambers are carved inside it leaving a
    /// `margin` rock shell on every face.
    pub size: [f32; 3],
    pub chambers: u32,
    /// Vertical bands chambers are distributed across — "caves overlapping to
    /// make floors". `levels=1` is a single-storey cave; higher values stack
    /// chambers so connecting tunnels climb between floors.
    pub levels: u32,
    pub chamber_min: f32,
    pub chamber_max: f32,
    /// Minimum rock gap kept between chamber surfaces of the SAME layer (m).
    /// Keeps separated chambers distinct rooms rather than merging into a blob.
    pub spacing: f32,
    /// Probability [0, 1] that a chamber is placed deliberately overlapping a
    /// same-layer neighbour, merging into one larger irregular cavern. The rest
    /// stay `spacing`-separated, so a cave mixes merged caverns and distinct
    /// rooms. `0` = all rooms separate; `1` = everything clusters/overlaps.
    pub overlap: f32,
    /// Vertical squash applied to chambers (height = radius * flatten). < 1
    /// keeps chamber floors gentle so they read as walkable rooms.
    pub chamber_flatten: f32,
    /// Rock thickness kept between vertical layers (m). Each `level` is its own
    /// horizontal layer of chambers + passages, stacked on top of the next with
    /// this much solid rock between them and linked by `level_links` shafts.
    pub level_gap: f32,
    /// Vertical passages carved between each pair of adjacent layers. Clamped
    /// to ≥ 1 when `levels > 1` so every layer stays reachable.
    pub level_links: u32,
    pub passage_radius: f32,
    /// Extra connections beyond the spanning tree within a layer, for loops.
    pub loops: u32,
    /// Maximum walkable slope in degrees. Tunnels steeper than this are
    /// rebuilt as switchback ramps so no floor ever exceeds it.
    pub max_slope: f32,
    /// Wall-noise amount [0, 1] applied to the rock mesh for a natural finish.
    pub roughness: f32,
    /// Smooth-union radius blending chambers and tunnels into one cavity.
    pub blend: f32,
    /// Rock thickness kept around the void on every face of the block.
    pub margin: f32,
    /// Voxel-grid resolution (samples along the longest axis).
    pub resolution: u32,
    /// Openings punched out to a side face so the cave is enterable.
    pub entrances: u32,
    pub water_mat: Option<String>,
    /// Which rock surfaces get a trimesh collider (`all` | `shell` | `none`).
    /// A game importer reads `extras.collider` off these nodes for physics.
    pub colliders: ColliderMode,
    /// When true, pools and lakes also get a trimesh collider (so a player
    /// stands on the surface). Off by default so water is wadeable.
    pub water_collider: bool,
    pub decorations: Vec<DecoGroup>,
    /// Mesh-quality scale `(0, 1]`. `1.0` is full detail; lower values reduce
    /// triangle count — the rock voxel grid and every decoration's tessellation
    /// scale by this factor. Layout, counts and feature positions are unchanged,
    /// so a low-detail bake keeps the same chambers, passages and POIs as the
    /// hero bake; only the polygon budget drops.
    pub lod_scale: f32,
    /// Number of mushroom-spawn points of interest scattered on chamber floors.
    /// These are empty marker nodes (no geometry) the game populates with props.
    pub mushrooms: u32,
    /// Debug-only: slice the front (+Z) half of the rock shell away so the
    /// chambers, passages and floors are visible in cross-section. Mirrors
    /// `building`'s `debug_hide_roof`. Decorations in the removed half are
    /// culled so they don't float in the opened section.
    pub debug_hide_shell: bool,
    /// Debug-only: give every point-of-interest marker a small visible sphere
    /// (in a bright debug material) so the otherwise-empty markers show up in a
    /// glTF preview. Off by default — production bakes keep POIs geometry-free.
    pub debug_show_poi: bool,
}

/// Default size range per decoration kind (min, max) in metres.
fn default_size_range(kind: DecoKind) -> (f32, f32) {
    match kind {
        DecoKind::Stalagmite | DecoKind::Stalactite => (0.3, 1.2),
        // Column size is the pillar's base radius; kept slender so a column
        // reads as a pillar rather than a plug.
        DecoKind::Column => (0.35, 0.9),
        DecoKind::RockPile => (0.4, 1.0),
        DecoKind::Pool => (1.0, 2.5),
        DecoKind::Lake => (3.0, 6.0),
    }
}

pub(super) fn read_cfg(node: &Node) -> Result<CaveCfg> {
    let seed = node
        .attr_number("seed")
        .map(|n| (n as i64).max(1) as u32)
        .unwrap_or(1);
    let mat_style = node.attr_string("mat_style").unwrap_or("").to_string();

    let size = node
        .attr_vec3("size")
        .map(|v| [v.x.max(4.0), v.y.max(3.0), v.z.max(4.0)])
        .unwrap_or([24.0, 10.0, 24.0]);

    let chambers = node.attr_number("chambers").unwrap_or(6.0).max(1.0) as u32;
    let levels = node.attr_number("levels").unwrap_or(2.0).max(1.0) as u32;

    let mut chamber_min = node.attr_number("chamber_min").unwrap_or(2.5).max(0.5);
    let mut chamber_max = node.attr_number("chamber_max").unwrap_or(5.0).max(0.5);
    if chamber_min > chamber_max {
        std::mem::swap(&mut chamber_min, &mut chamber_max);
    }
    let spacing = node.attr_number("spacing").unwrap_or(2.0).max(0.0);
    let overlap = node.attr_number("overlap").unwrap_or(0.35).clamp(0.0, 1.0);
    let chamber_flatten = node.attr_number("chamber_flatten").unwrap_or(0.6).clamp(0.2, 1.0);
    let level_gap = node.attr_number("level_gap").unwrap_or(1.5).max(0.0);
    let level_links = node.attr_number("level_links").unwrap_or(1.0).max(0.0) as u32;

    let passage_radius = node.attr_number("passage_radius").unwrap_or(1.1).max(0.3);
    let loops = node.attr_number("loops").unwrap_or(1.0).max(0.0) as u32;
    let max_slope = node.attr_number("max_slope").unwrap_or(45.0).clamp(5.0, 89.0);
    let roughness = node.attr_number("roughness").unwrap_or(0.35).clamp(0.0, 1.0);
    let blend = node.attr_number("blend").unwrap_or(1.5).max(0.0);
    let margin = node.attr_number("margin").unwrap_or(2.0).max(0.5);
    let resolution = node
        .attr_number("resolution")
        .map(|n| (n as u32).clamp(32, 224))
        .unwrap_or(96);
    let entrances = node.attr_number("entrances").unwrap_or(1.0).max(0.0) as u32;

    let water_mat = node.attr_string("water_mat").map(|s| s.to_string());

    // Collider controls. `colliders` defaults to `all` (every solid rock
    // surface collides); a bad value falls back to `all` since the validator
    // has already flagged it. `water_collider` is an independent opt-in.
    let colliders = node
        .attr_string("colliders")
        .and_then(ColliderMode::parse)
        .unwrap_or(ColliderMode::All);
    let water_collider = node
        .attr_number("water_collider")
        .map(|n| n.abs() > 0.5)
        .unwrap_or(false);

    // Combine the cave's own `lod_scale=` attr with the file-global LOD scale
    // (the studio slider writes the top-level `lod_scale (value=…)` directive,
    // which lands in `current_lod_scale()` here). Without this the slider would
    // have no effect on cave geometry, since the cave reads only its own attr.
    let lod_scale = (node.attr_number("lod_scale").unwrap_or(1.0)
        * crate::lower::lod::current_lod_scale())
    .clamp(0.1, 1.0);
    let mushrooms = node.attr_number("mushrooms").unwrap_or(0.0).max(0.0) as u32;

    let debug_hide_shell = node
        .attr_number("debug_hide_shell")
        .map(|n| n.abs() > 0.5)
        .unwrap_or(false);
    let debug_show_poi = node
        .attr_number("debug_show_poi")
        .map(|n| n.abs() > 0.5)
        .unwrap_or(false);

    let decorations = read_decorations(node)?;

    Ok(CaveCfg {
        seed,
        mat_style,
        size,
        chambers,
        levels,
        chamber_min,
        chamber_max,
        spacing,
        overlap,
        chamber_flatten,
        level_gap,
        level_links,
        passage_radius,
        loops,
        max_slope,
        roughness,
        blend,
        margin,
        resolution,
        entrances,
        water_mat,
        colliders,
        water_collider,
        decorations,
        lod_scale,
        mushrooms,
        debug_hide_shell,
        debug_show_poi,
    })
}

/// Merge the top-level count knobs with any `feature` children into one
/// resolved list of decoration groups. A `feature` whose `kind=` matches a
/// top-level knob overrides that knob's count (when it declares `count=`),
/// material and size range; otherwise the knob's count is used with defaults.
fn read_decorations(node: &Node) -> Result<Vec<DecoGroup>> {
    // Top-level shorthand counts, keyed by the decoration kind they drive.
    let knobs = [
        (DecoKind::RockPile, "rock_piles"),
        (DecoKind::Pool, "pools"),
        (DecoKind::Lake, "lakes"),
        (DecoKind::Stalagmite, "stalagmites"),
        (DecoKind::Stalactite, "stalactites"),
        (DecoKind::Column, "columns"),
    ];

    // Parse the explicit `feature` children first so they can override knobs.
    let mut features: Vec<(DecoKind, FeatureOverride)> = Vec::new();
    for c in &node.children {
        match c.kind.as_str() {
            "feature" => {
                let kind_str = c
                    .attr_string("kind")
                    .ok_or_else(|| anyhow::anyhow!("`feature` requires `kind=`"))?;
                let kind = DecoKind::parse(kind_str).ok_or_else(|| {
                    anyhow::anyhow!("unknown feature kind \"{kind_str}\"")
                })?;
                features.push((kind, read_feature_override(c)));
            }
            other => bail!("`cave` body accepts only `feature` declarations; got `{other}`"),
        }
    }

    let mut groups: Vec<DecoGroup> = Vec::new();
    for (kind, attr) in knobs {
        let knob_count = node.attr_number(attr).unwrap_or(0.0).max(0.0) as u32;
        let feature = features.iter().find(|(k, _)| *k == kind).map(|(_, f)| f);
        let count = match feature.and_then(|f| f.count) {
            Some(c) => c,
            None => knob_count,
        };
        if count == 0 {
            continue;
        }
        let (dmin, dmax) = default_size_range(kind);
        let min_size = feature.and_then(|f| f.min_size).unwrap_or(dmin).max(0.05);
        let mut max_size = feature.and_then(|f| f.max_size).unwrap_or(dmax).max(0.05);
        if max_size < min_size {
            max_size = min_size;
        }
        let mat = feature.and_then(|f| f.mat.clone());
        groups.push(DecoGroup {
            kind,
            count,
            min_size,
            max_size,
            mat,
        });
    }
    Ok(groups)
}

#[derive(Clone, Debug, Default)]
struct FeatureOverride {
    count: Option<u32>,
    min_size: Option<f32>,
    max_size: Option<f32>,
    mat: Option<String>,
}

fn read_feature_override(c: &Node) -> FeatureOverride {
    FeatureOverride {
        count: c.attr_number("count").map(|n| n.max(0.0) as u32),
        min_size: c.attr_number("min_size").filter(|v| *v > 0.0),
        max_size: c.attr_number("max_size").filter(|v| *v > 0.0),
        mat: c.attr_string("mat").map(|s| s.to_string()),
    }
}
