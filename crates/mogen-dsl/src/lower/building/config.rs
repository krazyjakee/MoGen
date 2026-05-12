//! AST → typed `BuildingCfg` reader. Pulls every attr off the `building`
//! node and its `room_type` / `adjacency` children, applying T1 defaults.
//!
//! The validator has already rejected out-of-range numerics and the T1
//! tranche gates before we get here, so this module is allowed to assume
//! e.g. `floors_above == 1`. We re-clamp defensively anyway — a bad input
//! that slipped past validation shouldn't panic at lowering time.

use anyhow::{bail, Result};

use crate::ast::{Node, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Style {
    Grid,
    ApartmentBlock,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Roof {
    Flat,
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
#[allow(dead_code)] // `kind`, `min_area`, `max_area` honoured by T3+ scoring.
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
#[allow(dead_code)] // `mat_style`, multi-floor, circulation, skylight fields land in T2+.
pub(super) struct BuildingCfg {
    pub seed: u32,
    pub style: Style,
    pub mat_style: String,
    pub floor_area: f32,
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
    /// Debug-only: when true, suppress the top-storey ceiling slab (and its
    /// skylights) so a flat-roof building can be inspected from above.
    pub debug_hide_roof: bool,
    /// Debug-only: when set, render only this signed storey index, with no
    /// ceiling and no vertical circulation — lets callers peek inside one
    /// floor of a multi-storey building.
    pub debug_render_floor: Option<i32>,
}

pub(super) fn read_cfg(node: &Node) -> Result<BuildingCfg> {
    let seed = node
        .attr_number("seed")
        .map(|n| (n as i64).max(1) as u32)
        .unwrap_or(1);
    let style = match attr_str(node, "style").as_deref() {
        Some("grid") | None => Style::Grid,
        Some("apartment-block") => Style::ApartmentBlock,
        Some(other) => bail!("unsupported building style \"{other}\" in Tranche 1"),
    };
    let roof = match attr_str(node, "roof").as_deref() {
        Some("flat") | None => Roof::Flat,
        Some(other) => bail!("unsupported building roof \"{other}\" in Tranche 1"),
    };
    let mat_style = attr_str(node, "mat_style").unwrap_or_default();

    let floor_area = node.attr_number("floor_area").unwrap_or(120.0).max(4.0);
    let rooms = node.attr_number("rooms").unwrap_or(4.0).max(1.0) as u32;
    let floors_above = node.attr_number("floors_above").unwrap_or(1.0).max(1.0) as u32;
    let floors_below = node.attr_number("floors_below").unwrap_or(0.0).max(0.0) as u32;
    let windows = node.attr_number("windows").unwrap_or(0.0).max(0.0) as u32;
    let skylights = node.attr_number("skylights").unwrap_or(0.0).max(0.0) as u32;

    let ceiling_height = node.attr_number("ceiling_height").unwrap_or(2.6).max(1.5);
    let door_w = node.attr_number("door_w").unwrap_or(0.9).max(0.4);
    let door_h = node.attr_number("door_h").unwrap_or(2.1).max(1.4);
    let window_w = node.attr_number("window_w").unwrap_or(1.2).max(0.3);
    let window_h = node.attr_number("window_h").unwrap_or(1.4).max(0.3);
    let wall_thickness = node.attr_number("wall_thickness").unwrap_or(0.12).max(0.04);
    let ceiling_thickness = node
        .attr_number("ceiling_thickness")
        .unwrap_or(0.2)
        .max(0.05);

    let entrances = node.attr_number("entrances").unwrap_or(1.0).max(1.0) as u32;
    let elevators = node.attr_number("elevators").unwrap_or(0.0).max(0.0) as u32;
    let staircases = node.attr_number("staircases").unwrap_or(0.0).max(0.0) as u32;

    let debug_hide_roof = node
        .attr_number("debug_hide_roof")
        .map(|n| n.abs() > 0.5)
        .unwrap_or(false);
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

    Ok(BuildingCfg {
        seed,
        style,
        mat_style,
        floor_area,
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
    /// declared. Used by the apartment-block layout to switch from plain
    /// BSP to a corridor-and-side-rooms layout, and by the door planner to
    /// root the spanning tree at the corridor so all rooms open onto it.
    pub fn corridor_type_index(&self) -> Option<usize> {
        self.room_types
            .iter()
            .position(|r| r.name.eq_ignore_ascii_case("corridor"))
    }
}
