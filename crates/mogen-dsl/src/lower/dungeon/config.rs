//! AST → typed `DungeonCfg` reader.
//!
//! A dungeon is a deterministic function of `seed=` plus the declared attrs,
//! exactly like `cave`/`terrain`. The reader pulls every attr off the `dungeon`
//! node, applies a default, and clamps defensively (the validator has already
//! rejected the egregious cases, but a value that slipped past shouldn't panic
//! at lowering time).
//!
//! Where `cave` carves an organic void, a dungeon is a tile grid: rectangular
//! rooms placed on a `cell`-metre lattice, joined by axis-aligned corridors,
//! stacked into `levels` floors that staircases connect. The headline knobs are
//! the footprint (`size` / `cell`), the room population (`rooms` + size range +
//! `spacing`), the corridor shape (`corridor_width` + `loops`), and the
//! vertical structure (`levels` + `stairs`).

use crate::ast::Node;
use crate::lower::cfg;

/// Which generated surfaces get a trimesh collider for the game engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ColliderMode {
    /// No colliders on any dungeon geometry.
    None,
    /// Every solid surface (decks, walls, steps) gets a trimesh collider.
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
pub(super) struct DungeonCfg {
    pub seed: u32,
    pub mat_style: String,
    /// Footprint + clearance `[width(x), room_height(y), depth(z)]` in metres.
    /// `width`/`depth` set the grid footprint (divided by `cell`); `room_height`
    /// is the floor-to-ceiling clearance of a single level.
    pub size: [f32; 3],
    /// Edge length of one grid cell in metres. Rooms and corridors are quantised
    /// to this lattice so walls always meet flush.
    pub cell: f32,
    /// Number of stacked floors. `1` is a single-storey dungeon; higher values
    /// stack independent room layouts linked by staircases.
    pub levels: u32,
    /// Target room count per level (placement is best-effort within the grid).
    pub rooms: u32,
    /// Room side length range in cells (inclusive).
    pub room_min: u32,
    pub room_max: u32,
    /// Minimum rock gap kept between rooms of the same level, in cells.
    pub spacing: u32,
    /// Corridor width in cells.
    pub corridor_width: u32,
    /// Extra corridor connections beyond the spanning tree, per level (loops).
    pub loops: u32,
    /// Staircases carved between each pair of adjacent levels.
    pub stairs: u32,
    /// Wall thickness in metres.
    pub wall_thickness: f32,
    /// Floor / ceiling deck thickness in metres.
    pub floor_thickness: f32,
    /// Emit ceiling decks above each level (the deck doubles as the next level's
    /// floor). When false, levels are open-topped.
    pub ceilings: bool,
    pub colliders: ColliderMode,
    /// Number of `prop_spot` POI markers scattered on room floors.
    pub prop_spots: u32,
    /// Mesh-quality scale `(0, 1]`; compounds with the file-global `lod_scale`.
    /// Dungeon geometry is already low-poly boxes, so this only trims a uniform
    /// detail factor; layout, counts and POIs are unchanged.
    pub lod_scale: f32,
    /// Debug-only: omit the topmost deck (the roof) so the rooms are visible
    /// from above in a preview. Mirrors `cave.debug_hide_shell`.
    pub debug_hide_roof: bool,
    /// Debug-only: when set, render only this level index (0 = ground), with no
    /// ceiling and only the staircases that touch it — lets callers peek inside
    /// one floor of a multi-level dungeon. Mirrors `building.debug_render_floor`.
    /// Out-of-range values fall back to rendering every level.
    pub debug_render_floor: Option<i32>,
    /// Debug-only: give every POI marker a small bright sphere so the otherwise
    /// geometry-free markers show up in a preview.
    pub debug_show_poi: bool,
}

pub(super) fn read_cfg(node: &Node) -> DungeonCfg {
    let seed = cfg::seed(node);
    let mat_style = node.attr_string("mat_style").unwrap_or("").to_string();

    let size = node
        .attr_vec3("size")
        .map(|v| [v.x.max(8.0), v.y.max(2.0), v.z.max(8.0)])
        .unwrap_or([48.0, 4.0, 48.0]);

    let cell = cfg::scalar(node, "cell", 4.0, 1.0);
    let levels = cfg::count(node, "levels", 1.0, 1.0);
    let rooms = cfg::count(node, "rooms", 6.0, 1.0);

    let mut room_min = cfg::int_clamped(node, "room_min", 2, 1, 64);
    let mut room_max = cfg::int_clamped(node, "room_max", 5, 1, 64);
    if room_min > room_max {
        std::mem::swap(&mut room_min, &mut room_max);
    }
    let spacing = cfg::int_clamped(node, "spacing", 1, 0, 16);
    let corridor_width = cfg::int_clamped(node, "corridor_width", 1, 1, 8);
    let loops = cfg::count(node, "loops", 1.0, 0.0);
    let stairs = cfg::count(node, "stairs", 1.0, 0.0);

    let wall_thickness = cfg::scalar(node, "wall_thickness", 0.4, 0.05);
    let floor_thickness = cfg::scalar(node, "floor_thickness", 0.4, 0.05);
    let ceilings = cfg::flag(node, "ceilings", true);

    let colliders = node
        .attr_string("colliders")
        .and_then(ColliderMode::parse)
        .unwrap_or(ColliderMode::All);

    let prop_spots = cfg::count(node, "prop_spots", 0.0, 0.0);

    let lod_scale = (node.attr_number("lod_scale").unwrap_or(1.0)
        * crate::lower::lod::current_lod_scale())
    .clamp(0.1, 1.0);

    let debug_hide_roof = cfg::flag(node, "debug_hide_roof", false);
    let debug_render_floor = node
        .attr_number("debug_render_floor")
        .map(|n| n.round() as i32);
    let debug_show_poi = cfg::flag(node, "debug_show_poi", false);

    DungeonCfg {
        seed,
        mat_style,
        size,
        cell,
        levels,
        rooms,
        room_min,
        room_max,
        spacing,
        corridor_width,
        loops,
        stairs,
        wall_thickness,
        floor_thickness,
        ceilings,
        colliders,
        prop_spots,
        lod_scale,
        debug_hide_roof,
        debug_render_floor,
        debug_show_poi,
    }
}
