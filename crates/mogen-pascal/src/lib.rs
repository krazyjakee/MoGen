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
pub struct Import {
    pub source: String,
    pub report: Report,
}

/// Parse a scene and render it as `.mog` source.
///
/// Fails only when the JSON itself cannot be read. Anything the importer does
/// not understand — an unknown node kind, a wall with no endpoints, a roof in a
/// shape their schema no longer describes — is recorded in the report and
/// written into the file's header as a comment. A partially understood building
/// is worth having; an error message is not.
pub fn import(json: &str, scene_name: &str) -> Result<Import, serde_json::Error> {
    let scene: schema::Scene = serde_json::from_str(json)?;
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
}
