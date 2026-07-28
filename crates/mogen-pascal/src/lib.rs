//! Read a [pascalorg/editor](https://github.com/pascalorg/editor) scene and
//! write it as `.mog` source.
//!
//! Their editor is a web architectural tool (MIT) whose data model is richer
//! than the `building` generator's: a wall is a centreline with a thickness and
//! an optional arc, a slab is a polygon with holes, a roof is a shape and a
//! pitch. This crate maps that vocabulary onto
//! [`mogen_dsl::lower::arch`]'s IR — and does nothing else. Every piece of
//! geometry maths lives on the other side of that boundary, so the same solver
//! serves the generator when it is retargeted.
//!
//! ```no_run
//! let json = std::fs::read_to_string("scene.json")?;
//! let out = mogen_pascal::import(&json, "house")?;
//! std::fs::write("house.mog", out.source)?;
//! eprintln!("{}", out.report.summary().join("\n"));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

pub mod adapt;
pub mod schema;

pub use adapt::Report;

/// The result of an import: the file to write, and what could not be used.
#[derive(Debug)]
pub struct Import {
    pub source: String,
    pub report: Report,
}

/// Why a scene could not be read, and — where we can work it out — *which node*
/// is responsible.
///
/// Serde reports a byte offset. Their scenes are written minified, so that
/// offset is "line 1 column 5666", which on a 190 KB single-line file is not a
/// location, it is a number. Worse, the failure is almost always one node in
/// several hundred using a shape we have not seen, and the offset gives no clue
/// which one or which field.
///
/// So on failure the whole file is re-read as untyped JSON and each node is
/// deserialised on its own. Any that fail are named. Then, for each of those,
/// one field at a time is removed to find the single field whose absence makes
/// the node parse — which turns "invalid type: map, expected a sequence" into
/// "node `site_0` (type `site`), field `polygon`". All of this runs only on the
/// error path, so it costs nothing when the import works.
#[derive(Debug)]
pub struct ImportError {
    pub source: serde_json::Error,
    /// One entry per node that could not be read, already formatted. Empty if
    /// the failure was not localisable — malformed JSON, or a problem outside
    /// the `nodes` map.
    pub nodes: Vec<String>,
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.nodes.is_empty() {
            return write!(f, "{}", self.source);
        }
        write!(f, "{}", self.nodes.join("; "))
    }
}

// Deliberately no `source()` override. The underlying serde error is kept on
// the struct for anyone who wants it, but reporters that walk the chain would
// otherwise append "invalid type: map, expected a sequence at line 1 column
// 5199" to a message that already said which node and field — restating the
// problem in the one form that was no use to begin with.
impl std::error::Error for ImportError {}

/// Point at the node, and the field, that serde only gave a byte offset for.
fn blame(json: &str) -> Vec<String> {
    let Ok(root) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let Some(nodes) = root.get("nodes").and_then(|n| n.as_object()) else {
        return Vec::new();
    };

    let mut out: Vec<String> = nodes
        .iter()
        .filter_map(|(id, node)| {
            let err = serde_json::from_value::<schema::Node>(node.clone()).err()?;
            let kind = node.get("type").and_then(|t| t.as_str()).unwrap_or("?");

            // Which single field, if removing it is enough to make this parse?
            let culprit = node.as_object().and_then(|obj| {
                obj.keys().find(|key| {
                    let mut trimmed = obj.clone();
                    trimmed.remove(*key);
                    serde_json::from_value::<schema::Node>(serde_json::Value::Object(trimmed))
                        .is_ok()
                })
            });

            Some(match culprit {
                Some(field) => format!("node {id:?} (type {kind:?}), field {field:?}: {err}"),
                None => format!("node {id:?} (type {kind:?}): {err}"),
            })
        })
        .collect();
    // `nodes` is a map, so iteration order is serde_json's, not ours.
    out.sort();
    out
}

/// Parse a scene and render it as `.mog` source.
///
/// Fails only when the JSON itself cannot be read. Anything the importer does
/// not understand — an unknown node kind, a wall with no endpoints, a roof in a
/// shape their schema no longer describes — is recorded in the report and
/// written into the file's header as a comment. A partially understood building
/// is worth having; an error message is not.
pub fn import(json: &str, scene_name: &str) -> Result<Import, ImportError> {
    let scene: schema::Scene = serde_json::from_str(json)
        .map_err(|source| ImportError { nodes: blame(json), source })?;
    let (model, report) = adapt::to_model(&scene);

    let mut header = vec![format!("Imported from a pascalorg/editor scene as {scene_name:?}.")];
    header.extend(report.summary());

    let materials = adapt::material_decls(&model);
    let source = mogen_dsl::lower::arch::to_mog(scene_name, &header, &materials, &model);

    Ok(Import { source, report })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEMO: &str = include_str!("../tests/fixtures/demo_1.json");

    fn demo() -> Import {
        import(DEMO, "demo").expect("their own demo scene must import")
    }

    #[test]
    fn their_demo_scene_imports() {
        let out = demo();
        assert_eq!(out.report.walls, 6, "{:?}", out.report);
        assert_eq!(out.report.slabs, 1);
        assert!(out.report.roofs > 0, "{:?}", out.report);
        assert!(out.report.markers > 0, "{:?}", out.report);
    }

    #[test]
    fn the_emitted_source_parses_and_lowers() {
        // The only assertion that proves the import is worth anything. Every
        // count above could be right while the file refuses to open.
        let out = demo();
        let ast = mogen_dsl::parse(&out.source)
            .unwrap_or_else(|e| panic!("import does not parse: {e}\n---\n{}", out.source));
        let graph = mogen_dsl::lower(&ast)
            .unwrap_or_else(|e| panic!("import does not lower: {e}\n---\n{}", out.source));
        assert!(
            graph.nodes.iter().any(|n| n.mesh.is_some()),
            "imported scene has no geometry\n{}",
            out.source
        );
    }

    #[test]
    fn no_imported_node_lowers_to_an_empty_mesh() {
        let out = demo();
        let graph = mogen_dsl::lower(&mogen_dsl::parse(&out.source).expect("parse")).expect("lower");
        for node in &graph.nodes {
            if let Some(mesh) = &node.mesh {
                assert!(
                    !mesh.positions.is_empty(),
                    "node {:?} came out empty",
                    node.name
                );
            }
        }
    }

    #[test]
    fn importing_is_reproducible() {
        // Their scene is a hash map, so a traversal that iterated it would
        // reorder elements between runs and rename every node in the output.
        // The walk follows `rootNodeIds` and `children` precisely to avoid it.
        let a = demo();
        for _ in 0..8 {
            assert_eq!(a.source, demo().source, "import is not deterministic");
        }
    }

    #[test]
    fn every_material_used_is_also_declared() {
        // A `mat=` with no declaration is a hard lowering error, so this is
        // the difference between a file that opens and one that does not.
        let out = demo();
        for line in out.source.lines() {
            if let Some(rest) = line.split("mat=\"").nth(1) {
                let name = rest.split('"').next().unwrap_or_default();
                assert!(
                    out.source.contains(&format!("material \"{name}\"")),
                    "{name:?} is referenced but never declared"
                );
            }
        }
    }

    #[test]
    fn the_header_says_what_was_skipped() {
        let out = demo();
        assert!(out.source.starts_with("// Imported from"), "{}", first_lines(&out.source));
        assert!(
            out.source.contains("walls · ") && out.source.contains("markers"),
            "{}",
            first_lines(&out.source)
        );
    }

    #[test]
    fn malformed_json_is_the_only_hard_failure() {
        assert!(import("{ not json", "x").is_err());
        // A structurally valid but empty scene still yields a usable file.
        let out = import(r#"{"nodes":{},"rootNodeIds":[]}"#, "empty").expect("empty is fine");
        assert!(out.source.contains("scene \"empty\""), "{}", out.source);
    }

    #[test]
    fn a_node_that_cannot_be_read_is_named_along_with_its_field() {
        // What serde gives you on a minified 190 KB scene is "line 1 column
        // 5666", which is a number, not a location. The one thing worth knowing
        // -- which of several hundred nodes, and which field on it -- has to be
        // worked out separately.
        //
        // `bad` uses a shape the schema does not accept; `fine` is the same
        // node kind, valid, and must not be blamed alongside it.
        let json = r#"{"nodes":{
            "fine":{"id":"fine","type":"slab","polygon":[[0,0],[1,0],[1,1]]},
            "bad":{"id":"bad","type":"slab","holes":{"a":1}}
        },"rootNodeIds":[]}"#;

        let err = import(json, "x").expect_err("must not parse");
        let msg = err.to_string();
        assert!(msg.contains(r#"node "bad""#), "{msg}");
        assert!(msg.contains(r#"type "slab""#), "{msg}");
        assert!(msg.contains(r#"field "holes""#), "{msg}");
        assert!(!msg.contains(r#"node "fine""#), "blamed a valid node: {msg}");
    }

    #[test]
    fn an_unblamable_failure_still_reports_something() {
        // Malformed JSON cannot be localised at all -- there are no nodes to
        // walk -- so the message must fall back to serde's rather than go
        // empty.
        let err = import("{ not json", "x").expect_err("must not parse");
        assert!(err.nodes.is_empty());
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn an_unknown_node_kind_is_reported_not_fatal() {
        // Plugins register namespaced kinds, so the set is open by design.
        let json = r#"{"nodes":{
            "x":{"id":"x","type":"trees:tree","parentId":null},
            "l":{"id":"l","type":"level","level":0,"parentId":null,"children":["x"]}
        },"rootNodeIds":["l"]}"#;
        let out = import(json, "park").expect("imports");
        assert!(
            out.report.skipped.iter().any(|(k, _)| k == "trees:tree"),
            "{:?}",
            out.report
        );
    }

    #[test]
    fn a_dangling_child_reference_is_reported_not_fatal() {
        let json = r#"{"nodes":{
            "l":{"id":"l","type":"level","level":0,"parentId":null,"children":["ghost"]}
        },"rootNodeIds":["l"]}"#;
        let out = import(json, "x").expect("imports");
        assert!(
            out.report.notes.iter().any(|n| n.contains("dangling")),
            "{:?}",
            out.report
        );
    }

    #[test]
    fn hidden_nodes_are_skipped() {
        let json = r#"{"nodes":{
            "w":{"id":"w","type":"wall","parentId":"l","visible":false,"start":[0,0],"end":[4,0]},
            "l":{"id":"l","type":"level","level":0,"parentId":null,"children":["w"]}
        },"rootNodeIds":["l"]}"#;
        let out = import(json, "x").expect("imports");
        assert_eq!(out.report.walls, 0, "{:?}", out.report);
    }

    fn first_lines(s: &str) -> String {
        s.lines().take(6).collect::<Vec<_>>().join("\n")
    }

    // ---- A scene whose answer is known --------------------------------
    //
    // Their demo is a sandbox: six walls scattered across a plan, two roofs,
    // no coherent building. Useful for robustness, useless for correctness,
    // because nothing about it has a right answer to check against.
    //
    // `room.json` is the opposite — a closed 6×4 room, 0.2m walls, one 0.9×2.1
    // door meeting the floor. Every number below is derivable by hand, so this
    // is what actually pins the behaviour of the mitre solver, the opening
    // slicer and the sink working together.

    const ROOM: &str = include_str!("../tests/fixtures/room.json");

    fn room_graph() -> mogen_core::SceneGraph {
        let out = import(ROOM, "room").expect("imports");
        assert!(out.report.notes.is_empty(), "{:?}", out.report);
        let ast = mogen_dsl::parse(&out.source)
            .unwrap_or_else(|e| panic!("{e}\n---\n{}", out.source));
        mogen_dsl::lower(&ast).unwrap_or_else(|e| panic!("{e}\n---\n{}", out.source))
    }

    #[test]
    fn a_closed_room_imports_to_exactly_the_expected_parts() {
        let graph = room_graph();
        let mut names: Vec<&str> = graph
            .nodes
            .iter()
            .filter(|n| n.mesh.is_some())
            .map(|n| n.name.as_str())
            .collect();
        names.sort_unstable();

        // The doored wall splits into two piers and a lintel. There is no
        // sill, because the door's bottom edge *is* the floor — a zero-height
        // panel would be degenerate geometry. The four unprefixed names are
        // the stdlib `door_simple` filling the hole those panels left: three
        // frame strips and the leaf. A doorway with nothing in it imports as a
        // house with no doors.
        assert_eq!(
            names,
            vec![
                "frame_left",
                "frame_right",
                "frame_top",
                "slab",
                "slab_0",
                "wall_0_panel0",
                "wall_0_panel1",
                "wall_0_panel2",
                "wall_1",
                "wall_2",
                "wall_3",
            ]
        );
    }

    #[test]
    fn the_door_lands_in_its_own_doorway() {
        // The room's south wall runs along z = 0 with its door at the middle,
        // and the wall stands on a slab 0.05 up. A door placed off the wall's
        // *centreline* — from a pier's face, say — would sit half a thickness
        // out and read as a door stuck to the wall rather than in it.
        let out = import(ROOM, "room").expect("imports");
        let ast = mogen_dsl::parse(&out.source).expect("parses");
        let graph = mogen_dsl::lower(&ast).expect("lowers");

        let door = graph
            .nodes
            .iter()
            .find(|n| n.name == "wall_0_door0")
            .unwrap_or_else(|| panic!("no door node\n---\n{}", out.source));
        let p = door.transform.translation;
        assert!((p.x - 3.0).abs() < 1e-3, "{p:?}");
        assert!((p.y - 0.05).abs() < 1e-3, "sits on the slab, not the ground: {p:?}");
        assert!(p.z.abs() < 1e-3, "on the wall centreline: {p:?}");
    }

    #[test]
    fn the_rooms_walls_tile_their_ring_without_overlapping() {
        // Four mitred walls around a 6×4 centreline rectangle cover exactly
        // the annulus between the outer and inner rectangles. Too much volume
        // means the corners are double-counted; too little means there is a
        // gap at each one.
        let graph = room_graph();
        let (t, w, d) = (0.2_f32, 6.0_f32, 4.0_f32);
        let plan_area = (w + t) * (d + t) - (w - t) * (d - t);

        // Every wall stands on the slab, so each is the storey height less the
        // slab's elevation. If this ever comes out halfway between 2.65 and
        // 2.7, support has gone back to being decided per wall.
        let wall_h = 2.7 - 0.05;
        let expected = plan_area * wall_h - 0.9 * 2.1 * t;

        let volume: f32 = graph
            .nodes
            .iter()
            .filter(|n| n.name.starts_with("wall_"))
            .filter_map(|n| n.mesh.as_ref())
            .map(mesh_volume)
            .sum();
        assert!(
            (volume - expected).abs() < 0.02,
            "walls occupy {volume} m³, expected {expected} m³"
        );
    }

    /// Signed volume via the divergence theorem: a closed mesh gives the
    /// enclosed volume, and an unclosed one does not.
    fn mesh_volume(mesh: &mogen_core::Mesh) -> f32 {
        let p = &mesh.positions;
        mesh.indices
            .chunks_exact(3)
            .map(|t| {
                let (a, b, c) = (
                    p[t[0] as usize],
                    p[t[1] as usize],
                    p[t[2] as usize],
                );
                (a[0] * (b[1] * c[2] - c[1] * b[2]) - a[1] * (b[0] * c[2] - c[0] * b[2])
                    + a[2] * (b[0] * c[1] - c[0] * b[1]))
                    / 6.0
            })
            .sum::<f32>()
            .abs()
    }
}
