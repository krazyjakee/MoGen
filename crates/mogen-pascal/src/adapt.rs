//! Their node graph → [`ArchModel`].
//!
//! **This file only maps fields.** Every piece of geometry maths lives in
//! `mogen_dsl::lower::arch`. That split is what makes the same solver reusable
//! by the `building` generator later; bury a mitre calculation here and it has
//! to be written twice.
//!
//! # Traversal order is the determinism trap
//!
//! Their scene is a `Record<id, Node>` — a hash map. Iterating it directly
//! would give a different element order on every run, and since IR ids are
//! assigned by push order, that means different names in the emitted file for
//! byte-identical input. So the walk starts at `rootNodeIds` and descends
//! through each node's `children` array, which is ordered. The map is only ever
//! used for lookup by id, never iterated.
//!
//! # Two of everything
//!
//! Their format carries no version, and its shapes have changed without one:
//!
//! - Openings are `door` / `window` nodes **and** `item` nodes with an asset
//!   category of `"door"` / `"window"`. Their own shipped demo uses the latter.
//! - Roofs are a `roof` container of `roof-segment` children **and** a legacy
//!   `roof` carrying `length` / `leftWidth` / `rightWidth` directly — fields the
//!   current schema does not define at all. Their demo uses the latter.
//!
//! Both are handled. Anything else is reported, never fatal.

use mogen_dsl::lower::arch::{
    ArchModel, Ceiling, CeilingId, Level, LevelId, MatRef, Marker, ModelSource, Opening,
    OpeningKind, Polygon, RoofId, RoofParams, RoofSegment, RoofType, Slab, SlabId, Wall, WallId,
};

use crate::schema::{
    Node, Scene, DEFAULT_LEVEL_HEIGHT, DEFAULT_SLAB_ELEVATION, DEFAULT_SLAB_THICKNESS,
    DEFAULT_WALL_THICKNESS,
};

/// What the import could not use, so the caller can say so rather than
/// pretending the file was fully understood.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Report {
    /// Node kind → how many were skipped. A `Vec` rather than a map so the
    /// order is the order first seen, and so it prints reproducibly.
    pub skipped: Vec<(String, usize)>,
    pub walls: usize,
    pub slabs: usize,
    pub ceilings: usize,
    pub roofs: usize,
    pub markers: usize,
    pub notes: Vec<String>,
}

impl Report {
    fn skip(&mut self, kind: &str) {
        match self.skipped.iter_mut().find(|(k, _)| k == kind) {
            Some((_, n)) => *n += 1,
            None => self.skipped.push((kind.to_string(), 1)),
        }
    }

    /// The summary that goes into the generated file's header.
    pub fn summary(&self) -> Vec<String> {
        let mut out = vec![format!(
            "{} walls · {} slabs · {} ceilings · {} roof segments · {} markers",
            self.walls, self.slabs, self.ceilings, self.roofs, self.markers
        )];
        if !self.skipped.is_empty() {
            let total: usize = self.skipped.iter().map(|(_, n)| n).sum();
            let detail: Vec<String> =
                self.skipped.iter().map(|(k, n)| format!("{k} ({n})")).collect();
            out.push(format!("Skipped {total} nodes: {}", detail.join(", ")));
        }
        out.extend(self.notes.iter().cloned());
        out
    }
}

/// Convert a parsed scene into a model the solver can take.
pub fn to_model(scene: &Scene) -> (ArchModel, Report) {
    let mut model = ArchModel::new(ModelSource::PascalEditor);
    let mut report = Report::default();

    // Depth-first from the roots, following `children`. Never iterate
    // `scene.nodes` — see the module note.
    let mut levels: Vec<(&Node, i32)> = Vec::new();
    let mut stack: Vec<&str> = scene.root_node_ids.iter().rev().map(String::as_str).collect();
    let mut order: Vec<&Node> = Vec::new();

    while let Some(id) = stack.pop() {
        let Some(node) = scene.nodes.get(id) else {
            report.notes.push(format!("dangling child reference {id:?}"));
            continue;
        };
        order.push(node);
        for child in node.children.iter().rev() {
            stack.push(child);
        }
    }

    for node in &order {
        if node.kind == "level" {
            levels.push((node, node.level.unwrap_or(0)));
        }
    }
    levels.sort_by_key(|(_, ord)| *ord);
    for (node, ord) in &levels {
        model.levels.push(Level {
            id: LevelId(*ord),
            name: node.name.clone(),
            height: node.height.unwrap_or(DEFAULT_LEVEL_HEIGHT),
        });
    }
    if model.levels.is_empty() {
        // A file with geometry but no level still has to land somewhere.
        model.levels.push(Level {
            id: LevelId(0),
            name: None,
            height: DEFAULT_LEVEL_HEIGHT,
        });
        report.notes.push("no level nodes found; everything placed on level 0".into());
    }

    for node in &order {
        if !node.is_visible() {
            report.skip(&format!("{} (hidden)", node.kind));
            continue;
        }
        let level = level_of(scene, node, &levels);

        match node.kind.as_str() {
            // Containers: carried by the traversal, nothing to emit.
            "building" | "level" | "roof" if node.length.is_none() => {}

            "wall" => add_wall(scene, node, level, &mut model, &mut report),
            "slab" => add_slab(node, level, &mut model, &mut report),
            "ceiling" => add_ceiling(node, level, &mut model, &mut report),
            "roof-segment" => add_segment(node, level, &mut model, &mut report),
            // A legacy roof carries its shape directly instead of in children.
            "roof" => add_legacy_roof(node, level, &mut model, &mut report),

            // Openings are attached to their wall, not emitted standalone.
            "door" | "window" => {}
            "item" => add_item(node, level, &mut model, &mut report),

            other => report.skip(other),
        }
    }

    (model, report)
}

/// Which storey a node belongs to: walk up `parentId` until a level turns up.
fn level_of(scene: &Scene, node: &Node, levels: &[(&Node, i32)]) -> LevelId {
    let mut cur = Some(node);
    // Bounded so a cyclic `parentId` cannot hang the import.
    for _ in 0..64 {
        let Some(n) = cur else { break };
        if n.kind == "level" {
            return LevelId(n.level.unwrap_or(0));
        }
        cur = n.parent_id.as_deref().and_then(|p| scene.nodes.get(p));
    }
    LevelId(levels.first().map(|(_, o)| *o).unwrap_or(0))
}

fn add_wall(scene: &Scene, node: &Node, level: LevelId, m: &mut ArchModel, r: &mut Report) {
    let (Some(start), Some(end)) = (node.start, node.end) else {
        r.skip("wall (no endpoints)");
        return;
    };

    let openings: Vec<Opening> = node
        .children
        .iter()
        .filter_map(|id| scene.nodes.get(id))
        .filter(|c| c.is_visible())
        .filter_map(as_opening)
        .collect();

    m.push_wall(Wall {
        id: WallId(0),
        level,
        start,
        end,
        thickness: node.thickness.unwrap_or(DEFAULT_WALL_THICKNESS),
        height: node.height,
        curve_offset: node.curve_offset,
        openings,
        material: node.material_preset.clone().map(MatRef),
    });
    r.walls += 1;
}

/// A wall's child, if it is a hole in that wall.
///
/// Their `position[1]` is the opening's **centre** height; the IR wants the
/// sill. Their cutout code does the same subtraction (`bottom = position[1] -
/// height / 2`), which is where this is verified rather than guessed.
fn as_opening(node: &Node) -> Option<Opening> {
    let kind = match (node.kind.as_str(), node.category()) {
        ("door", _) | (_, "door") => OpeningKind::Door,
        ("window", _) | (_, "window") => OpeningKind::Window,
        _ => return None,
    };

    // Fall back to the asset's own dimensions, which is where an `item`-shaped
    // opening keeps its size.
    let dims = node.asset.as_ref().and_then(|a| a.dimensions.as_ref());
    let width = node.width.or_else(|| dims.and_then(|d| d.first().copied()))?;
    let height = node.height.or_else(|| dims.and_then(|d| d.get(1).copied()))?;
    if width <= 0.0 || height <= 0.0 {
        return None;
    }

    Some(Opening {
        kind,
        along: node.pos(0),
        sill: (node.pos(1) - 0.5 * height).max(0.0),
        width,
        height,
    })
}

fn add_slab(node: &Node, level: LevelId, m: &mut ArchModel, r: &mut Report) {
    let Some(outer) = node.polygon.clone() else {
        r.skip("slab (no polygon)");
        return;
    };
    if outer.len() < 3 {
        r.skip("slab (degenerate polygon)");
        return;
    }
    m.push_slab(Slab {
        id: SlabId(0),
        level,
        poly: Polygon { outer, holes: node.holes.clone().unwrap_or_default() },
        elevation: node.elevation.unwrap_or(DEFAULT_SLAB_ELEVATION),
        thickness: node.thickness.unwrap_or(DEFAULT_SLAB_THICKNESS),
        material: node.material_preset.clone().map(MatRef),
    });
    r.slabs += 1;
}

fn add_ceiling(node: &Node, level: LevelId, m: &mut ArchModel, r: &mut Report) {
    let Some(outer) = node.polygon.clone() else {
        r.skip("ceiling (no polygon)");
        return;
    };
    if outer.len() < 3 {
        r.skip("ceiling (degenerate polygon)");
        return;
    }
    m.push_ceiling(Ceiling {
        id: CeilingId(0),
        level,
        poly: Polygon { outer, holes: node.holes.clone().unwrap_or_default() },
        elevation: node.elevation,
        material: node.material_preset.clone().map(MatRef),
    });
    r.ceilings += 1;
}

fn add_segment(node: &Node, level: LevelId, m: &mut ArchModel, r: &mut Report) {
    let d = RoofParams::default();
    m.push_roof(RoofSegment {
        id: RoofId(0),
        level,
        centre: [node.pos(0), node.pos(2)],
        width: node.width.unwrap_or(0.0),
        depth: node.depth.unwrap_or(0.0),
        rotation: node.rotation_y(),
        pitch_deg: node.pitch.unwrap_or(40.0),
        roof_type: roof_type(node.roof_type.as_deref()),
        overhang: node.overhang.unwrap_or(0.0),
        wall_height: node.wall_height.unwrap_or(0.0),
        params: RoofParams {
            gambrel_lower_width: node.gambrel_lower_width.unwrap_or(d.gambrel_lower_width),
            gambrel_lower_height: node.gambrel_lower_height.unwrap_or(d.gambrel_lower_height),
            mansard_steep_width: node.mansard_steep_width.unwrap_or(d.mansard_steep_width),
            mansard_steep_height: node.mansard_steep_height.unwrap_or(d.mansard_steep_height),
            dutch_hip_width: node.dutch_hip_width.unwrap_or(d.dutch_hip_width),
            dutch_hip_height: node.dutch_hip_height.unwrap_or(d.dutch_hip_height),
            deck_thickness: node.deck_thickness.unwrap_or(d.deck_thickness),
        },
        material: node.material_preset.clone().map(MatRef),
    });
    r.roofs += 1;
}

/// A legacy `roof`: `length` along the ridge, `leftWidth` + `rightWidth` either
/// side of it, and `height` as the rise.
///
/// Their current schema has none of these fields — but their own shipped demo
/// scene is full of them, so a file in the wild is at least as likely to use
/// this shape as the documented one.
///
/// The asymmetry is the lossy part: two different slope widths cannot be a
/// symmetric gable. The pitch is taken from the *wider* side, which keeps the
/// ridge height right and the broader roof plane correct, and the narrower side
/// comes out shallower than it should. Noted in the report rather than passed
/// off as exact.
fn add_legacy_roof(node: &Node, level: LevelId, m: &mut ArchModel, r: &mut Report) {
    let (Some(length), Some(rise)) = (node.length, node.height) else {
        r.skip("roof (no legacy shape)");
        return;
    };
    let left = node.left_width.unwrap_or(0.0);
    let right = node.right_width.unwrap_or(0.0);
    let depth = left + right;
    if length <= 0.0 || depth <= 0.0 {
        r.skip("roof (degenerate)");
        return;
    }

    let half_run = 0.5 * depth.min(length);
    let pitch_deg = if half_run > 0.0 {
        (rise / half_run).atan().to_degrees()
    } else {
        40.0
    };
    if (left - right).abs() > 1e-3 {
        r.notes.push(format!(
            "roof {:?} has unequal slopes ({left}m / {right}m); imported as a symmetric gable",
            node.name.as_deref().unwrap_or(&node.id)
        ));
    }

    m.push_roof(RoofSegment {
        id: RoofId(0),
        level,
        // Their legacy roof's position is the centre of the ridge, and the
        // ridge sits between the two slope widths rather than at the middle of
        // them, so the footprint centre shifts by half the difference.
        centre: [node.pos(0), node.pos(2) + 0.5 * (right - left)],
        width: length,
        depth,
        rotation: node.rotation_y(),
        pitch_deg,
        roof_type: RoofType::Gable,
        overhang: 0.0,
        wall_height: node.pos(1),
        params: RoofParams::default(),
        material: node.material_preset.clone().map(MatRef),
    });
    r.roofs += 1;
}

/// Furniture and fittings become transform-only markers.
///
/// Deliberately not geometry: their asset library is a pile of GLB models we
/// have no licence to vendor and no reason to duplicate. A marker carries the
/// category and name, so an engine can put its own model there — which is the
/// same contract the `building` generator's points of interest already use.
fn add_item(node: &Node, _level: LevelId, m: &mut ArchModel, r: &mut Report) {
    // A door or window is a hole in a wall, handled with that wall.
    if matches!(node.category(), "door" | "window") {
        return;
    }

    let mut tags = vec!["imported".to_string()];
    if let Some(a) = &node.asset {
        if let Some(id) = &a.id {
            tags.push(id.clone());
        }
    }

    m.markers.push(Marker {
        name: node.name.clone().unwrap_or_else(|| node.id.clone()),
        role: match node.category() {
            "" => "item".to_string(),
            c => c.to_string(),
        },
        position: [node.pos(0), node.pos(1), node.pos(2)],
        rotation: node.rotation_y(),
        tags,
    });
    r.markers += 1;
}

fn roof_type(s: Option<&str>) -> RoofType {
    match s.unwrap_or("gable") {
        "hip" => RoofType::Hip,
        "shed" => RoofType::Shed,
        "gambrel" => RoofType::Gambrel,
        "dutch" => RoofType::Dutch,
        "mansard" => RoofType::Mansard,
        "flat" => RoofType::Flat,
        _ => RoofType::Gable,
    }
}

/// Materials referenced by `materialPreset`, so the emitted file declares
/// every name it uses.
///
/// Their JSON export drops the material table entirely — it writes only
/// `{nodes, rootNodeIds}` — so a preset name arrives with nothing behind it.
/// Rather than guess at colours we cannot see, each one becomes a plain grey
/// declaration the user can edit. A named grey material is a starting point; a
/// dangling reference is a file that will not load.
pub fn material_decls(model: &ArchModel) -> Vec<mogen_dsl::lower::arch::MaterialDecl> {
    let mut names: Vec<String> = model
        .walls
        .iter()
        .map(|w| &w.material)
        .chain(model.slabs.iter().map(|s| &s.material))
        .chain(model.ceilings.iter().map(|c| &c.material))
        .chain(model.roofs.iter().map(|r| &r.material))
        .flatten()
        .map(|MatRef(n)| n.clone())
        .collect();
    names.sort();
    names.dedup();

    names
        .into_iter()
        .map(|name| mogen_dsl::lower::arch::MaterialDecl {
            name,
            color: Some([0.72, 0.72, 0.70]),
            roughness: Some(0.85),
            ..Default::default()
        })
        .collect()
}


