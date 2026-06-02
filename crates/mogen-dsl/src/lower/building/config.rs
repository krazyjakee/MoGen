//! AST → typed `BuildingCfg` reader. Pulls every attr off the `building`
//! node and its `room_type` / `adjacency` children, applying T1 defaults.
//!
//! The validator has already rejected out-of-range numerics and the T1
//! tranche gates before we get here, so this module is allowed to assume
//! e.g. `floors_above == 1`. We re-clamp defensively anyway — a bad input
//! that slipped past validation shouldn't panic at lowering time.

use anyhow::{bail, Result};

use crate::ast::{Node, Value};
use crate::lower::cfg;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Style {
    Grid,
    ApartmentBlock,
    HotelCorridor,
    OfficeCore,
    Radial,
    Organic,
    Maze,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Roof {
    Flat,
    /// Gable along the longer axis (the v1 axis-aligned implementation treats
    /// `pitched` as a synonym of `gabled`: sloped end-faces would require non-
    /// axis-aligned vertices we have nowhere to round-trip through).
    Gabled,
    Pitched,
    Hipped,
    Mansard,
    Shed,
}

impl Roof {
    pub fn is_flat(self) -> bool {
        matches!(self, Roof::Flat)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RoomKind {
    Public,
    Private,
    Service,
    Utility,
    Secure,
    StaffOnly,
}

#[derive(Clone, Debug)]
pub(super) struct RoomType {
    pub name: String,
    pub kind: RoomKind,
    pub density: f32,
    pub mat: Option<String>,
    pub min_area: Option<f32>,
    pub max_area: Option<f32>,
}

#[derive(Clone, Debug)]
pub(super) struct AdjacencyRule {
    pub name: String,
    pub adjacent_to: Vec<String>,
    pub away_from: Vec<String>,
}

#[derive(Clone, Debug)]
pub(super) struct WindowModules {
    pub small: String,
    pub medium: String,
    pub large: String,
}

#[derive(Clone, Debug)]
#[allow(dead_code)] // `mat_style` is forwarded to texture generation only; not used during lowering.
pub(super) struct BuildingCfg {
    pub seed: u32,
    pub style: Style,
    pub mat_style: String,
    pub floor_area: f32,
    /// Optional smaller footprint for below-ground storeys. `None` means
    /// basements reuse `floor_area`. When `Some`, every storey with
    /// `storey < 0` is solved against this footprint instead.
    pub cellar_area: Option<f32>,
    pub rooms: u32,
    pub floors_above: u32,
    pub floors_below: u32,
    pub windows: u32,
    pub skylights: u32,
    pub roof: Roof,
    pub ceiling_height: f32,
    pub door_w: f32,
    pub door_h: f32,
    pub window_w: f32,
    pub window_h: f32,
    pub wall_thickness: f32,
    pub ceiling_thickness: f32,
    pub entrances: u32,
    pub external_door: String,
    pub internal_door: String,
    pub windows_mod: WindowModules,
    pub skylight_mod: String,
    pub elevators: u32,
    pub staircases: u32,
    pub room_types: Vec<RoomType>,
    pub adjacencies: Vec<AdjacencyRule>,
    /// When true (the default), the furnishing pass drops transform-only POI
    /// markers into each room naming the props a game engine should place
    /// (bed, desk, stove, …). The markers carry no geometry — `building`
    /// still emits empty shells. `furnish=0` suppresses the pass entirely.
    pub furnish: bool,
    /// Debug-only: when true, give every furnishing POI marker a small
    /// emissive sphere so the otherwise geometry-free markers are visible in
    /// a glTF preview. The cave analogue of the same name.
    pub debug_show_poi: bool,
    /// Debug-only: when true, suppress the top-storey ceiling slab (and its
    /// skylights) so a flat-roof building can be inspected from above.
    pub debug_hide_roof: bool,
    /// Debug-only: when set, render only this signed storey index, with no
    /// ceiling and no vertical circulation — lets callers peek inside one
    /// floor of a multi-storey building.
    pub debug_render_floor: Option<i32>,
}

pub(super) fn read_cfg(node: &Node) -> Result<BuildingCfg> {
    let seed = cfg::seed(node);
    let style = match attr_str(node, "style").as_deref() {
        Some("grid") | None => Style::Grid,
        Some("apartment-block") => Style::ApartmentBlock,
        Some("hotel-corridor") => Style::HotelCorridor,
        Some("office-core") => Style::OfficeCore,
        Some("radial") => Style::Radial,
        Some("organic") => Style::Organic,
        Some("maze") => Style::Maze,
        Some(other) => bail!("unsupported building style \"{other}\""),
    };
    let roof = match attr_str(node, "roof").as_deref() {
        Some("flat") | None => Roof::Flat,
        Some("gabled") => Roof::Gabled,
        Some("pitched") => Roof::Pitched,
        Some("hipped") => Roof::Hipped,
        Some("mansard") => Roof::Mansard,
        Some("shed") => Roof::Shed,
        Some(other) => bail!("unsupported building roof \"{other}\""),
    };
    let mat_style = attr_str(node, "mat_style").unwrap_or_default();

    let floor_area = cfg::scalar(node, "floor_area", 120.0, 4.0);
    let cellar_area = node
        .attr_number("cellar_area")
        .filter(|v| *v > 0.0)
        .map(|v| v.max(4.0));
    let rooms = cfg::count(node, "rooms", 4.0, 1.0);
    let floors_above = cfg::count(node, "floors_above", 1.0, 1.0);
    let floors_below = cfg::count(node, "floors_below", 0.0, 0.0);
    let windows = cfg::count(node, "windows", 0.0, 0.0);
    let skylights = cfg::count(node, "skylights", 0.0, 0.0);

    let ceiling_height = cfg::scalar(node, "ceiling_height", 2.6, 1.5);
    let door_w = cfg::scalar(node, "door_w", 0.9, 0.4);
    let door_h = cfg::scalar(node, "door_h", 2.1, 1.4);
    let window_w = cfg::scalar(node, "window_w", 1.2, 0.3);
    let window_h = cfg::scalar(node, "window_h", 1.4, 0.3);
    let wall_thickness = cfg::scalar(node, "wall_thickness", 0.12, 0.04);
    let ceiling_thickness = cfg::scalar(node, "ceiling_thickness", 0.2, 0.05);

    let entrances = cfg::count(node, "entrances", 1.0, 1.0);
    let elevators = cfg::count(node, "elevators", 0.0, 0.0);
    let staircases = cfg::count(node, "staircases", 0.0, 0.0);

    // Furnishing is on by default: an unfurnished building is the rarer ask,
    // and the markers are geometry-free so they never bloat the export.
    let furnish = cfg::flag(node, "furnish", true);
    let debug_show_poi = cfg::flag(node, "debug_show_poi", false);
    let debug_hide_roof = cfg::flag(node, "debug_hide_roof", false);
    let debug_render_floor = node
        .attr_number("debug_render_floor")
        .map(|n| n.round() as i32);

    let external_door = attr_str(node, "external_door").unwrap_or_else(|| "door_simple".into());
    let internal_door = attr_str(node, "internal_door").unwrap_or_else(|| "door_simple".into());
    let windows_mod = WindowModules {
        small: attr_str(node, "window_small").unwrap_or_else(|| "window_simple".into()),
        medium: attr_str(node, "window_medium").unwrap_or_else(|| "window_simple".into()),
        large: attr_str(node, "window_large").unwrap_or_else(|| "window_simple".into()),
    };
    let skylight_mod = attr_str(node, "skylight").unwrap_or_else(|| "skylight_simple".into());

    let mut room_types: Vec<RoomType> = Vec::new();
    let mut adjacencies: Vec<AdjacencyRule> = Vec::new();
    for c in &node.children {
        match c.kind.as_str() {
            "room_type" => room_types.push(read_room_type(c)?),
            "adjacency" => adjacencies.push(read_adjacency(c)?),
            other => bail!(
                "`building` body accepts only `room_type` and `adjacency`; got `{other}`"
            ),
        }
    }
    if room_types.is_empty() {
        bail!("`building` requires at least one `room_type` declaration");
    }

    // Hotel / office styles need a corridor cell. If the author didn't
    // declare one we add a synthetic public corridor with density=0 so
    // it's never sampled as a regular room but is still pickable by
    // `corridor_type_index()` and lookups by name.
    if matches!(style, Style::HotelCorridor | Style::OfficeCore)
        && !room_types
            .iter()
            .any(|r| r.name.eq_ignore_ascii_case("corridor"))
    {
        room_types.push(RoomType {
            name: "corridor".into(),
            kind: RoomKind::Public,
            density: 0.0,
            mat: None,
            min_area: None,
            max_area: None,
        });
    }

    Ok(BuildingCfg {
        seed,
        style,
        mat_style,
        floor_area,
        cellar_area,
        rooms,
        floors_above,
        floors_below,
        windows,
        skylights,
        roof,
        ceiling_height,
        door_w,
        door_h,
        window_w,
        window_h,
        wall_thickness,
        ceiling_thickness,
        entrances,
        external_door,
        internal_door,
        windows_mod,
        skylight_mod,
        elevators,
        staircases,
        room_types,
        adjacencies,
        furnish,
        debug_show_poi,
        debug_hide_roof,
        debug_render_floor,
    })
}

fn read_room_type(c: &Node) -> Result<RoomType> {
    let name = c
        .name
        .clone()
        .ok_or_else(|| anyhow::anyhow!("`room_type` requires a quoted name"))?;
    let kind_str = attr_str(c, "kind")
        .ok_or_else(|| anyhow::anyhow!("room_type `{name}` requires `kind=`"))?;
    let kind = match kind_str.as_str() {
        "public" => RoomKind::Public,
        "private" => RoomKind::Private,
        "service" => RoomKind::Service,
        "utility" => RoomKind::Utility,
        "secure" => RoomKind::Secure,
        "staff_only" => RoomKind::StaffOnly,
        other => bail!("unknown room_type kind \"{other}\" on room_type \"{name}\""),
    };
    let density = c.attr_number("density").unwrap_or(1.0).clamp(0.0, 10.0);
    let mat = attr_str(c, "mat");
    let min_area = c.attr_number("min_area").filter(|v| *v > 0.0);
    let max_area = c.attr_number("max_area").filter(|v| *v > 0.0);
    Ok(RoomType {
        name,
        kind,
        density,
        mat,
        min_area,
        max_area,
    })
}

fn read_adjacency(c: &Node) -> Result<AdjacencyRule> {
    let name = c
        .name
        .clone()
        .ok_or_else(|| anyhow::anyhow!("`adjacency` requires a quoted name"))?;
    let adjacent_to = read_string_list(c, "adjacent_to");
    let away_from = read_string_list(c, "away_from");
    Ok(AdjacencyRule {
        name,
        adjacent_to,
        away_from,
    })
}

fn read_string_list(c: &Node, key: &str) -> Vec<String> {
    match c.attr(key) {
        Some(Value::ListString(items)) => items.clone(),
        // Single bare string is shorthand for a single-element list. Keeps
        // `adjacent_to="living"` working alongside `adjacent_to=["living"]`.
        Some(Value::String(s)) | Some(Value::Ident(s)) => vec![s.clone()],
        _ => Vec::new(),
    }
}

fn attr_str(node: &Node, key: &str) -> Option<String> {
    match node.attr(key)? {
        Value::String(s) | Value::Ident(s) => Some(s.clone()),
        _ => None,
    }
}

impl BuildingCfg {
    /// Per-room-type sampling weight. Density 0 → never sampled.
    pub fn density_weights(&self) -> Vec<f32> {
        self.room_types.iter().map(|r| r.density.max(0.0)).collect()
    }

    /// Index of the room_type whose name is `"corridor"`, if one is
    /// declared. Used by the hotel/office layouts to identify which type
    /// to emit as the corridor cell, and by the door planner to root the
    /// spanning tree at the corridor so all rooms open onto it.
    pub fn corridor_type_index(&self) -> Option<usize> {
        self.room_types
            .iter()
            .position(|r| r.name.eq_ignore_ascii_case("corridor"))
    }
}
