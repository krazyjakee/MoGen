//! Shared point-of-interest emission for procedural generators.
//!
//! `building` (furniture markers) and `cave` (chamber / column / ladder /
//! mushroom markers) both drop transform-only marker nodes a game engine reads
//! from the glTF to place gameplay content the generator deliberately leaves
//! out. Each marker is a `kind="poi"` node carrying `role=<kind>` and a tag
//! list; the exporter stamps both into `node.extras`. Every generator routes
//! through this one harness so the POI contract (grouping, naming, tags,
//! optional debug spheres) is identical across systems and future generators
//! get it for free.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use mogen_core::{Material, NodeId, SceneGraph, Transform, UvMode};
use mogen_geom::icosphere_mesh;

/// Optional debug visualisation for a marker: a small emissive sphere so the
/// otherwise geometry-free POI shows up in a glTF preview. Only emitted when the
/// generator's `debug_show_poi` flag is set.
#[derive(Clone)]
pub(crate) struct PoiDebug {
    /// Stable material name (`cave_poi_<kind>`, `building_furniture_<cat>`, …).
    /// A user-declared material of the same name on the same origin wins.
    pub mat_name: String,
    /// Emissive (and base) colour for the marker sphere.
    pub color: [f32; 3],
    /// Sphere radius in metres.
    pub radius: f32,
}

/// One marker to emit. `name_key` is both the node-name prefix and the suffix
/// counter key, so two markers sharing a key become `bed_0`, `bed_1`, …
pub(crate) struct PoiMarker {
    pub name_key: String,
    pub role: String,
    pub tags: Vec<String>,
    pub transform: Transform,
    pub debug: Option<PoiDebug>,
}

/// Emit a group of POI markers under `parent`. Returns the group node id, or
/// `None` when there are no markers (no empty group is created). `debug_show`
/// gates the per-marker debug spheres; the markers themselves are always
/// emitted (they are the gameplay contract).
pub(crate) fn emit_poi_group(
    graph: &mut SceneGraph,
    parent: NodeId,
    origin: Option<&Path>,
    group_name: &str,
    group_tags: &[String],
    debug_show: bool,
    markers: Vec<PoiMarker>,
) -> Option<NodeId> {
    if markers.is_empty() {
        return None;
    }
    let origin_buf: Option<PathBuf> = origin.map(|p| p.to_path_buf());

    let group = graph.add_child(parent, group_name.to_string(), "group", Transform::IDENTITY);
    graph.nodes[group.0 as usize].origin = origin_buf.clone();
    graph.nodes[group.0 as usize].tags.extend(group_tags.iter().cloned());

    let mut counts: BTreeMap<String, u32> = BTreeMap::new();
    for m in markers {
        let idx = counts.entry(m.name_key.clone()).or_default();
        let id = graph.add_child(group, format!("{}_{}", m.name_key, idx), "poi", m.transform);
        *idx += 1;
        graph.nodes[id.0 as usize].origin = origin_buf.clone();
        graph.nodes[id.0 as usize].role = Some(m.role);
        graph.nodes[id.0 as usize].tags.extend(m.tags);

        if debug_show {
            if let Some(dbg) = m.debug {
                ensure_debug_material(graph, origin, &dbg);
                if let Some(mid) = graph.find_material_scoped(&dbg.mat_name, origin) {
                    graph.set_mesh(id, icosphere_mesh(dbg.radius, 1, UvMode::Tile));
                    graph.set_material(id, mid);
                }
            }
        }
    }
    Some(group)
}

/// Create the on-demand emissive debug material for a marker if it isn't
/// already declared on this origin (a user-declared material of the same name
/// wins, exactly like the generator's default materials).
fn ensure_debug_material(graph: &mut SceneGraph, origin: Option<&Path>, dbg: &PoiDebug) {
    if graph.find_material_scoped(&dbg.mat_name, origin).is_some() {
        return;
    }
    let [r, g, b] = dbg.color;
    let mut m = Material::new(&dbg.mat_name);
    m.base_color = [r, g, b, 1.0];
    m.emissive = [r, g, b];
    m.emissive_strength = 2.0;
    m.roughness = 0.5;
    m.origin = origin.map(|p| p.to_path_buf());
    graph.add_material(m);
}
