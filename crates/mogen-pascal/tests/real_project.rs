//! The importer against a whole building rather than a two-wall fixture.
//!
//! `examples/buildings/gatehouse.pascal.json` is ours, written by
//! `tests/fixtures/make_gatehouse.py` — a T-plan house over two storeys with a
//! cross-gabled roof. It exists because the other tests in this crate are
//! scenes small enough to reason about, which means they only ever contain
//! shapes we already thought of, and every bug the importer has had so far
//! lived in the gap between "a scene" and "a building":
//!
//! - a `site` node storing `polygon` as `{type, points}` rather than a bare
//!   ring, a shape that only exists in scenes taken from the running app;
//! - a `roof` container with a non-zero transform, which the editor's own demo
//!   scene never has, so dropping it looked correct until a roof floated off
//!   the walls;
//! - a wall bowed tighter than its own half-thickness, whose footprint doubles
//!   back along itself — accepted or rejected depending on how far the
//!   building sat from the origin.
//!
//! The generator is kept next to the fixture so the awkward parts are declared
//! rather than mysterious. Regenerate with:
//!
//! ```text
//! python3 crates/mogen-pascal/tests/fixtures/make_gatehouse.py
//! ```

use mogen_core::SceneGraph;

const HOUSE: &str = include_str!("../../../examples/buildings/gatehouse.pascal.json");

fn imported() -> (mogen_pascal::Import, SceneGraph) {
    let out = mogen_pascal::import(HOUSE, "gatehouse").expect("imports");
    let ast =
        mogen_dsl::parse(&out.source).unwrap_or_else(|e| panic!("import does not parse: {e}"));
    let graph = mogen_dsl::lower(&ast).unwrap_or_else(|e| panic!("import does not lower: {e}"));
    (out, graph)
}

#[test]
fn the_whole_house_arrives() {
    let (out, _) = imported();
    let r = &out.report;
    assert_eq!(r.walls, 26, "{r:?}");
    assert_eq!(r.slabs, 3, "{r:?}");
    assert_eq!(r.ceilings, 3, "{r:?}");
    assert_eq!(r.roofs, 2, "{r:?}");
    assert_eq!(r.markers, 12, "{r:?}");
}

#[test]
fn it_passes_the_same_validation_mogen_build_runs() {
    // The assertion that caught the roof-container bug, and the reason to run
    // the *validator* rather than just check it lowered. Lowering a roof into
    // the middle of the sky succeeds perfectly well; what fails is E1101,
    // "scene has 2 disconnected part clusters". A house whose roof does not
    // touch it is still a valid scene graph.
    let (_, graph) = imported();
    let errors: Vec<_> = mogen_validate::validate_graph(&graph)
        .into_iter()
        .filter(|d| d.severity == mogen_core::Severity::Error)
        .collect();
    assert!(
        errors.is_empty(),
        "imported house does not build:\n{}",
        errors.iter().map(|d| d.message.as_str()).collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn every_emitted_mesh_is_closed() {
    // The importer's contract: a shape it cannot make watertight is dropped and
    // reported, never emitted half-formed. So this holds over whatever
    // survived, and the test below holds over what did not.
    let (_, graph) = imported();
    let mut checked = 0;
    for node in &graph.nodes {
        let Some(mesh) = &node.mesh else { continue };
        assert!(
            mogen_geom::cleanup::is_closed_manifold(mesh),
            "{} is not closed",
            node.name
        );
        checked += 1;
    }
    assert!(checked > 100, "only {checked} meshes; expected the whole house");
}

#[test]
fn no_wall_pokes_out_through_the_roof() {
    // The fixture's roof container sits 1.2 m low, which is the sort of number
    // a real project has and a tidy one does not. Taken literally it drops the
    // main ridge below the storey it covers, and the top floor's walls come out
    // through the slopes — every one of them a perfectly valid closed solid, so
    // nothing downstream can tell it from a design. `arch::roof` lifts the eave
    // onto the walls instead, and says so in the header.
    let (out, graph) = imported();

    let top_of = |role: &str| {
        graph
            .nodes
            .iter()
            .filter(|n| n.role.as_deref() == Some(role))
            .filter_map(|n| n.mesh.as_ref().map(|m| (n, m)))
            .flat_map(|(n, m)| {
                let y = world_y(&graph, n);
                m.positions.iter().map(move |p| p[1] + y)
            })
            .fold(f32::NEG_INFINITY, f32::max)
    };
    let base_of_roof = graph
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("roof"))
        .filter_map(|n| n.mesh.as_ref().map(|m| (n, m)))
        .flat_map(|(n, m)| {
            let y = world_y(&graph, n);
            m.positions.iter().map(move |p| p[1] + y)
        })
        .fold(f32::INFINITY, f32::min);

    let walls = top_of("wall").max(top_of("wall_joint"));
    assert!(
        walls <= base_of_roof + 1e-3,
        "walls reach {walls}m but the roof starts at {base_of_roof}m"
    );
    assert!(
        out.source.contains("raised to meet them"),
        "the lift should be reported, not silent"
    );
}

/// A node's world Y. The importer only ever nests one group deep and never
/// scales or tilts, so summing translations is exact here.
fn world_y(graph: &SceneGraph, node: &mogen_core::SceneNode) -> f32 {
    let mut y = node.transform.translation.y;
    let mut parent = node.parent;
    while let Some(p) = parent {
        let n = &graph.nodes[p.0 as usize];
        y += n.transform.translation.y;
        parent = n.parent;
    }
    y
}

#[test]
fn every_doorway_and_window_has_something_in_it() {
    // Openings used to be cut and then forgotten, which reads as a house with
    // no windows rather than a house with glass in them. Exact counts, because
    // the fixture is a constant: one front door, eight interior ones, and a
    // window on every external wall over two storeys.
    let (_, graph) = imported();
    let count = |role: &str| {
        graph.nodes.iter().filter(|n| n.role.as_deref() == Some(role)).count()
    };
    assert_eq!(count("door"), 9);
    assert_eq!(count("window"), 26);

    // And they are filled, not just labelled: each wrapper holds the stdlib
    // module's parts.
    assert!(
        graph.nodes.iter().any(|n| n.name == "pane" && n.mesh.is_some()),
        "windows carry no glass"
    );
}

#[test]
fn the_deliberately_malformed_shapes_are_still_reported() {
    // Four shapes in the fixture are broken on purpose, one per way their
    // editor will hand us something we cannot build: a self-intersecting ring,
    // a hole hanging outside its slab, a zero-length wall, and a wall bowed
    // tighter than its own half-thickness. Their renderer tolerates all four.
    // We must refuse them, because emitting one means a hole in the mesh.
    //
    // If this goes red the interesting possibility is not that the fixture
    // improved -- it is a constant -- but that something stopped refusing
    // geometry it should refuse.
    let (out, _) = imported();
    let header: String = out
        .source
        .lines()
        .take_while(|l| l.starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    for expected in [
        "RingSelfIntersects",
        "HoleOutsideOuter",
        "zero length or thickness",
        "curves tighter",
    ] {
        assert!(
            header.contains(expected),
            "expected {expected:?} in the header; got:\n{header}"
        );
    }
}

#[test]
fn a_tight_curve_is_refused_wherever_the_building_stands() {
    // The bug this pins was invisible in every fixture we had: the wall above
    // is rejected, but an identical wall nearer the origin was rejected for the
    // *wrong reason* -- f32 noise tilting two collinear edges into a crossing.
    // Move the building and the noise changes, and the same wall silently
    // became a capless tube. So the assertion is not "this is rejected", it is
    // "this is rejected from wherever you stand".
    for (dx, dz) in [(0.0, 0.0), (-6.0, 2.4), (120.0, -88.0), (-410.5, 260.9)] {
        let json = format!(
            r#"{{"nodes":{{
                "l":{{"id":"l","type":"level","level":0,"parentId":null,"children":["w"]}},
                "w":{{"id":"w","type":"wall","parentId":"l",
                      "start":[{},{}],"end":[{},{}],
                      "thickness":1.2,"curveOffset":0.5}}
            }},"rootNodeIds":["l"]}}"#,
            dx,
            dz,
            dx + 1.0,
            dz
        );
        let out = mogen_pascal::import(&json, "t").expect("imports");
        assert!(
            out.source.contains("curves tighter"),
            "the same wall was accepted at ({dx}, {dz})"
        );
    }
}
