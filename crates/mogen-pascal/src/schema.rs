//! Just enough of pascalorg/editor's node schema to read one.
//!
//! # Everything is optional
//!
//! Their format has **no version field**, and the shapes have moved: their
//! current `roof` node is a container for `roof-segment` children, but the demo
//! scene shipped with the editor has `roof` nodes carrying `length` /
//! `leftWidth` / `rightWidth` — fields the present schema does not mention at
//! all. Likewise openings exist both as first-class `door` / `window` nodes and
//! as `item` nodes with an asset category.
//!
//! So this deserialises defensively: every field is `Option`, unknown fields
//! are ignored, and unknown node types are *reported* rather than rejected.
//! Plugins can register namespaced kinds (`trees:tree`), so the set of types is
//! open by design and refusing to load a file because of one is wrong.
//!
//! # Defaults
//!
//! Absent fields fall back to the values their code uses, taken from source:
//! wall thickness `0.1` (`DEFAULT_WALL_THICKNESS`), level height `2.5`
//! (`DEFAULT_LEVEL_HEIGHT`), slab elevation and thickness `0.05` each. These
//! matter more than they look: the demo scene omits every one of them.

use std::collections::HashMap;

use serde::Deserialize;

/// `DEFAULT_WALL_THICKNESS` in `packages/core/src/systems/wall/wall-footprint.ts`.
pub const DEFAULT_WALL_THICKNESS: f32 = 0.1;

/// `DEFAULT_LEVEL_HEIGHT` in `packages/core/src/services/level-height.ts`.
/// A level's `height` is optional and carries no schema default — their own
/// comment says it is "absent only on unmigrated legacy data" — so anything
/// reading one has to supply this.
pub const DEFAULT_LEVEL_HEIGHT: f32 = 2.5;

/// Slab `elevation` and `thickness` defaults, from their slab schema.
pub const DEFAULT_SLAB_ELEVATION: f32 = 0.05;
pub const DEFAULT_SLAB_THICKNESS: f32 = 0.05;

/// A whole exported scene. Their exporter writes exactly these two keys.
#[derive(Debug, Deserialize)]
pub struct Scene {
    pub nodes: HashMap<String, Node>,
    #[serde(default, rename = "rootNodeIds")]
    pub root_node_ids: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Node {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub name: Option<String>,
    #[serde(rename = "parentId")]
    pub parent_id: Option<String>,
    pub visible: Option<bool>,
    pub children: Vec<String>,
    pub metadata: Option<serde_json::Value>,

    // -- level --
    /// Storey ordinal. Named `level` in their data, which is why it is not
    /// `index`.
    pub level: Option<i32>,
    pub camera: Option<serde_json::Value>,

    // -- wall --
    pub start: Option<[f32; 2]>,
    pub end: Option<[f32; 2]>,
    pub thickness: Option<f32>,
    #[serde(rename = "curveOffset")]
    pub curve_offset: Option<f32>,

    // -- slab / ceiling / zone --
    /// Bare ring, or a `{type:"polygon", points:[…]}` wrapper — see
    /// [`de_polygon`].
    #[serde(default, deserialize_with = "de_polygon")]
    pub polygon: Option<Vec<[f32; 2]>>,
    pub holes: Option<Vec<Vec<[f32; 2]>>>,
    pub elevation: Option<f32>,

    // -- shared: openings, items, roofs --
    /// `[x, y, z]` for most nodes; **wall-local** for a wall's children, where
    /// `x` runs from the wall's start and `y` is the opening's *centre*.
    pub position: Option<Vec<f32>>,
    /// A single number (radians about +Y) on roofs, a triple on items.
    pub rotation: Option<serde_json::Value>,
    pub width: Option<f32>,
    /// Doubles as a wall's height, an opening's height, and a legacy roof's
    /// rise. Which one depends entirely on `kind`.
    pub height: Option<f32>,
    pub side: Option<String>,
    pub asset: Option<Asset>,
    pub interactive: Option<serde_json::Value>,

    // -- roof-segment (current schema) --
    #[serde(rename = "roofType")]
    pub roof_type: Option<String>,
    pub pitch: Option<f32>,
    pub depth: Option<f32>,
    pub overhang: Option<f32>,
    #[serde(rename = "wallHeight")]
    pub wall_height: Option<f32>,
    #[serde(rename = "gambrelLowerWidthRatio")]
    pub gambrel_lower_width: Option<f32>,
    #[serde(rename = "gambrelLowerHeightRatio")]
    pub gambrel_lower_height: Option<f32>,
    #[serde(rename = "mansardSteepWidthRatio")]
    pub mansard_steep_width: Option<f32>,
    #[serde(rename = "mansardSteepHeightRatio")]
    pub mansard_steep_height: Option<f32>,
    #[serde(rename = "dutchHipWidthRatio")]
    pub dutch_hip_width: Option<f32>,
    #[serde(rename = "dutchHipHeightRatio")]
    pub dutch_hip_height: Option<f32>,
    #[serde(rename = "deckThickness")]
    pub deck_thickness: Option<f32>,

    // -- roof (legacy shape, as found in their own demo scene) --
    pub length: Option<f32>,
    #[serde(rename = "leftWidth")]
    pub left_width: Option<f32>,
    #[serde(rename = "rightWidth")]
    pub right_width: Option<f32>,

    // -- materials --
    #[serde(rename = "materialPreset")]
    pub material_preset: Option<String>,
    pub material: Option<serde_json::Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Asset {
    pub id: Option<String>,
    pub category: Option<String>,
    pub name: Option<String>,
    pub dimensions: Option<Vec<f32>>,
}

/// A polygon ring, in either of the two shapes their data uses.
///
/// Their *file export* writes a bare `[[x, z], …]`, and that is what slab and
/// ceiling nodes carry. But `site` nodes — and only `site` nodes, in the scenes
/// seen so far — carry the editor's internal wrapper,
/// `{"type": "polygon", "points": [[x, z], …]}`. The difference is invisible
/// until you load a scene straight out of the running app rather than out of
/// its exporter, at which point one node in the file refuses to parse and takes
/// the whole scene with it.
///
/// Both are accepted, and anything else is `None` rather than an error, for the
/// same reason every other field here is optional: a shape we have not seen
/// should cost us one polygon, not the file.
fn de_polygon<'de, D>(d: D) -> Result<Option<Vec<[f32; 2]>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Shape {
        Bare(Vec<[f32; 2]>),
        Wrapped {
            points: Vec<[f32; 2]>,
        },
        Other(serde::de::IgnoredAny),
    }

    Ok(match Option::<Shape>::deserialize(d)? {
        Some(Shape::Bare(p)) | Some(Shape::Wrapped { points: p }) => Some(p),
        _ => None,
    })
}

impl Node {
    /// Whether the editor would draw this node. Absent means visible.
    pub fn is_visible(&self) -> bool {
        self.visible.unwrap_or(true)
    }

    /// Y rotation in radians, whether stored as a bare number (roofs) or an
    /// Euler triple (items).
    pub fn rotation_y(&self) -> f32 {
        match &self.rotation {
            Some(serde_json::Value::Number(n)) => n.as_f64().unwrap_or(0.0) as f32,
            Some(serde_json::Value::Array(a)) => {
                a.get(1).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32
            }
            _ => 0.0,
        }
    }

    pub fn pos(&self, i: usize) -> f32 {
        self.position.as_ref().and_then(|p| p.get(i)).copied().unwrap_or(0.0)
    }

    /// The category an `item` stands for — `"window"`, `"door"`, `"sofa"` — or
    /// the empty string.
    pub fn category(&self) -> &str {
        self.asset
            .as_ref()
            .and_then(|a| a.category.as_deref())
            .unwrap_or("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polygon_reads_both_shapes() {
        // Slabs out of their exporter carry a bare ring...
        let slab: Node = serde_json::from_str(
            r#"{"id":"s","type":"slab","polygon":[[0,0],[4,0],[4,3]]}"#,
        )
        .unwrap();
        assert_eq!(slab.polygon.unwrap(), vec![[0.0, 0.0], [4.0, 0.0], [4.0, 3.0]]);

        // ...but a site out of the running app wraps it, and used to take the
        // whole scene down with it.
        let site: Node = serde_json::from_str(
            r#"{"id":"t","type":"site","polygon":
                {"type":"polygon","points":[[0,0],[4,0],[4,3]]}}"#,
        )
        .unwrap();
        assert_eq!(site.polygon.unwrap(), vec![[0.0, 0.0], [4.0, 0.0], [4.0, 3.0]]);

        // A third shape costs one polygon, not the file.
        let odd: Node =
            serde_json::from_str(r#"{"id":"u","type":"slab","polygon":"circle"}"#).unwrap();
        assert!(odd.polygon.is_none());
    }

    #[test]
    fn a_node_with_unknown_fields_still_deserialises() {
        // Their format has no version field and gains fields freely, so an
        // unknown one must never stop a file loading.
        let n: Node = serde_json::from_str(
            r#"{"id":"wall_1","type":"wall","start":[0,0],"end":[4,0],
                "someFutureField":{"nested":true}}"#,
        )
        .expect("deserialises");
        assert_eq!(n.kind, "wall");
        assert_eq!(n.start, Some([0.0, 0.0]));
    }

    #[test]
    fn an_almost_empty_node_deserialises() {
        let n: Node = serde_json::from_str(r#"{"id":"x","type":"guide"}"#).expect("deserialises");
        assert!(n.is_visible(), "absent visible means visible");
        assert!(n.children.is_empty());
        assert_eq!(n.rotation_y(), 0.0);
    }

    #[test]
    fn rotation_reads_both_shapes() {
        // Roofs store a bare radian value; items store an Euler triple.
        let roof: Node =
            serde_json::from_str(r#"{"id":"r","type":"roof","rotation":1.5}"#).unwrap();
        assert_eq!(roof.rotation_y(), 1.5);

        let item: Node =
            serde_json::from_str(r#"{"id":"i","type":"item","rotation":[0,0.75,0]}"#).unwrap();
        assert_eq!(item.rotation_y(), 0.75);
    }

    #[test]
    fn the_shipped_demo_scene_parses() {
        let raw = include_str!("../tests/fixtures/demo_1.json");
        let scene: Scene = serde_json::from_str(raw).expect("their own demo must parse");
        assert_eq!(scene.root_node_ids.len(), 1);
        assert_eq!(scene.nodes.len(), 65);
        assert!(scene.nodes.values().any(|n| n.kind == "wall"));
    }
}
