//! Does an imported scene land where it should?
//!
//! The tests next to the code check that geometry is *well formed* — closed,
//! non-empty, deterministic. None of that catches the failure mode this
//! project keeps hitting: geometry that is perfectly valid and in the wrong
//! place. A mirrored plan is a plan. A curve bulging the wrong way is a curve.
//! A storey stacked from the wrong datum is a storey.
//!
//! So every case here is one where the answer can be worked out by hand, and
//! is asymmetric enough that getting a sign or an axis wrong changes it. The
//! standing risks, each with a test:
//!
//! - `[x, z]` read as `[x, y]`, so the plan lies on its side
//! - an opening's `along` measured from the wrong end of its wall
//! - a positive `curveOffset` bulging left instead of right
//! - storeys stacked floor-to-ceiling instead of floor-to-floor
//!
//! This does not replace comparing against their renderer, which is the only
//! way to catch a convention we have misread *consistently*. It does mean a
//! regression has to survive arithmetic that was done independently.

use mogen_core::SceneGraph;

/// Build a scene JSON from a level plus a list of node literals.
fn scene(level_extra: &str, nodes: &[&str]) -> String {
    format!(
        r#"{{"nodes":{{
            "l0":{{"id":"l0","type":"level","level":0,"parentId":null,
                   "children":[{}]{}}}
            {}{}
        }},"rootNodeIds":["l0"]}}"#,
        nodes
            .iter()
            .enumerate()
            .map(|(i, _)| format!("\"n{i}\""))
            .collect::<Vec<_>>()
            .join(","),
        level_extra,
        if nodes.is_empty() { "" } else { "," },
        nodes
            .iter()
            .enumerate()
            .map(|(i, n)| format!("\"n{i}\":{{\"id\":\"n{i}\",\"parentId\":\"l0\",{n}}}"))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn build(json: &str) -> SceneGraph {
    let out = mogen_pascal::import(json, "t").expect("imports");
    let ast = mogen_dsl::parse(&out.source)
        .unwrap_or_else(|e| panic!("{e}\n---\n{}", out.source));
    mogen_dsl::lower(&ast).unwrap_or_else(|e| panic!("{e}\n---\n{}", out.source))
}

/// Axis-aligned bounds over every mesh in the scene.
///
/// Walls and slabs are emitted in world plan coordinates with only a vertical
/// offset on the node, so a mesh's own x and z are already world x and z; only
/// y needs the node transform.
fn bounds(g: &SceneGraph) -> [[f32; 2]; 3] {
    let mut lo = [f32::INFINITY; 3];
    let mut hi = [f32::NEG_INFINITY; 3];
    for n in &g.nodes {
        let Some(mesh) = &n.mesh else { continue };
        let dy = n.transform.translation.y;
        for p in &mesh.positions {
            let world = [p[0], p[1] + dy, p[2]];
            for a in 0..3 {
                lo[a] = lo[a].min(world[a]);
                hi[a] = hi[a].max(world[a]);
            }
        }
    }
    [[lo[0], hi[0]], [lo[1], hi[1]], [lo[2], hi[2]]]
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.02
}

#[test]
fn a_plan_keeps_its_orientation() {
    // An L of two walls: 5m along +x, 3m along +z. Reading their `[x, z]` as
    // `[x, y]` would stand the whole plan on edge, and reading it as `[z, x]`
    // would give a 3m run and a 5m one — both perfectly valid buildings.
    let g = build(&scene(
        "",
        &[
            r#""type":"wall","start":[0,0],"end":[5,0],"thickness":0.2"#,
            r#""type":"wall","start":[0,0],"end":[0,3],"thickness":0.2"#,
        ],
    ));
    let [x, y, z] = bounds(&g);

    // Note the asymmetry, which is not a rounding artefact: the two walls meet
    // at the origin, so the mitre there pushes the outer corner out to −0.1 on
    // both axes. Their far ends are free, and a free end butts square on the
    // centreline's endpoint — so they stop at exactly 5 and 3.
    assert!(close(x[0], -0.1) && close(x[1], 5.0), "x span {x:?}");
    assert!(close(z[0], -0.1) && close(z[1], 3.0), "z span {z:?}");
    assert!(close(y[0], 0.0) && close(y[1], 2.5), "walls run up +Y: {y:?}");
}

#[test]
fn an_opening_is_positioned_from_its_walls_start() {
    // A 6m wall with a 0.9m door centred 1m from the start. The door leaves a
    // 0.55m pier on one side and a 4.55m run on the other, so a reading from
    // the far end would put the narrow pier at the wrong end -- and the wall
    // would still look like a wall with a door in it.
    let g = build(
        r#"{"nodes":{
            "l0":{"id":"l0","type":"level","level":0,"parentId":null,"children":["w"]},
            "w":{"id":"w","type":"wall","parentId":"l0","start":[0,0],"end":[6,0],
                 "thickness":0.2,"children":["d"]},
            "d":{"id":"d","type":"door","parentId":"w","position":[1.0,1.05,0],
                 "width":0.9,"height":2.1}
        },"rootNodeIds":["l0"]}"#,
    );

    // Piers are the full-height panels; the lintel above the door is not.
    let mut piers: Vec<[f32; 2]> = g
        .nodes
        .iter()
        .filter_map(|n| n.mesh.as_ref().map(|m| (n, m)))
        .filter(|(n, _)| n.name.starts_with("wall_"))
        .filter(|(_, m)| {
            let (lo, hi) = m.positions.iter().fold((f32::MAX, f32::MIN), |a, p| {
                (a.0.min(p[1]), a.1.max(p[1]))
            });
            hi - lo > 2.4
        })
        .map(|(_, m)| {
            m.positions.iter().fold([f32::MAX, f32::MIN], |a, p| {
                [a[0].min(p[0]), a[1].max(p[0])]
            })
        })
        .collect();
    piers.sort_by(|a, b| a[0].total_cmp(&b[0]));

    assert_eq!(piers.len(), 2, "two full-height piers, got {piers:?}");
    assert!(close(piers[0][1], 0.55), "near pier ends at the door: {piers:?}");
    assert!(close(piers[1][0], 1.45), "far pier starts after it: {piers:?}");
    assert!(
        piers[1][1] - piers[1][0] > piers[0][1] - piers[0][0],
        "the long run is on the far side: {piers:?}"
    );
}

#[test]
fn a_positive_curve_offset_bulges_right_of_start_to_end() {
    // The sign that mirrors a building silently. A wall running +x with a
    // positive sagitta must bow toward -z; the mirrored reading is an equally
    // valid arc, just the wrong one.
    let g = build(&scene(
        "",
        &[r#""type":"wall","start":[0,0],"end":[4,0],"thickness":0.2,"curveOffset":0.8"#],
    ));
    let [_, _, z] = bounds(&g);
    assert!(z[0] < -0.7, "the arc should reach -z, got {z:?}");
    assert!(z[1] < 0.2, "and should not cross to +z, got {z:?}");
}

#[test]
fn storeys_stack_floor_to_floor() {
    // Two 2.7m storeys put the upper floor plane at 2.7 and the top at 5.4.
    // Treating `height` as floor-to-ceiling would stack them at 2.75 and
    // leave a slab-thickness gap at the division -- open to the sky, and
    // invisible on any single wall.
    let json = r#"{"nodes":{
        "b":{"id":"b","type":"building","parentId":null,"children":["l0","l1"]},
        "l0":{"id":"l0","type":"level","level":0,"height":2.7,"parentId":"b","children":["w0"]},
        "l1":{"id":"l1","type":"level","level":1,"height":2.7,"parentId":"b","children":["w1"]},
        "w0":{"id":"w0","type":"wall","parentId":"l0","start":[0,0],"end":[4,0],"thickness":0.2},
        "w1":{"id":"w1","type":"wall","parentId":"l1","start":[0,0],"end":[4,0],"thickness":0.2}
    },"rootNodeIds":["b"]}"#;

    let g = build(json);
    let [_, y, _] = bounds(&g);
    assert!(close(y[0], 0.0) && close(y[1], 5.4), "two storeys span 0..5.4, got {y:?}");

    // And they meet: the lower wall's top is the upper wall's base, with no
    // gap and no overlap.
    let mut tops: Vec<f32> = g
        .nodes
        .iter()
        .filter_map(|n| n.mesh.as_ref().map(|m| (n.transform.translation.y, m)))
        .map(|(dy, m)| m.positions.iter().fold(f32::MIN, |a, p| a.max(p[1] + dy)))
        .collect();
    tops.sort_by(f32::total_cmp);
    assert!(close(tops[0], 2.7), "the lower wall tops out at the storey plane: {tops:?}");
}

#[test]
fn a_slab_lifts_the_walls_standing_on_it() {
    // The slab's top is at 0.05, so the walls start there and still reach the
    // storey plane -- shorter, not taller.
    let g = build(&scene(
        "",
        &[
            r#""type":"slab","polygon":[[0,0],[6,0],[6,4],[0,4]],"elevation":0.05,"thickness":0.05"#,
            r#""type":"wall","start":[0,0],"end":[6,0],"thickness":0.2"#,
        ],
    ));

    let wall = g
        .nodes
        .iter()
        .find(|n| n.name.starts_with("wall_"))
        .expect("a wall");
    let mesh = wall.mesh.as_ref().expect("geometry");
    let dy = wall.transform.translation.y;
    let (lo, hi) = mesh.positions.iter().fold((f32::MAX, f32::MIN), |a, p| {
        (a.0.min(p[1] + dy), a.1.max(p[1] + dy))
    });
    assert!(close(lo, 0.05), "the wall stands on the slab, got {lo}");
    assert!(close(hi, 2.5), "and still reaches the storey plane, got {hi}");
}

#[test]
fn a_mitred_corner_joins_without_a_seam() {
    // Two walls meeting at a right angle share their corner points exactly.
    // Overlapping boxes would put four vertices near the corner where two
    // belong, and leave a hairline that only shows up under a light.
    let g = build(&scene(
        "",
        &[
            r#""type":"wall","start":[0,0],"end":[4,0],"thickness":0.2"#,
            r#""type":"wall","start":[4,0],"end":[4,3],"thickness":0.2"#,
        ],
    ));

    // The outer elbow of the L. Both walls must own this exact point.
    let corner = [4.1_f32, -0.1_f32];
    let owners = g
        .nodes
        .iter()
        .filter(|n| n.name.starts_with("wall_"))
        .filter(|n| {
            n.mesh.as_ref().is_some_and(|m| {
                m.positions
                    .iter()
                    .any(|p| (p[0] - corner[0]).abs() < 1e-3 && (p[2] - corner[1]).abs() < 1e-3)
            })
        })
        .count();
    assert_eq!(owners, 2, "both walls should reach {corner:?}");
}

#[test]
fn a_roof_segment_rides_its_containers_transform() {
    // A `roof` emits nothing itself, which makes it easy to carry the traversal
    // through it and drop the transform on the way -- leaving the roof at the
    // world origin rather than over the house. Their own demo scene keeps every
    // roof container at the origin, so this only shows up on a real project.
    //
    // Container at [10, 3, 5], segment at local origin, so the answer is the
    // container's own position: asymmetric on all three axes, and off by
    // something different if any one is dropped.
    let json = r#"{"nodes":{
        "l0":{"id":"l0","type":"level","level":0,"parentId":null,"children":["r"]},
        "r":{"id":"r","type":"roof","parentId":"l0","position":[10,3,5],
             "rotation":0,"children":["s"]},
        "s":{"id":"s","type":"roof-segment","parentId":"r","position":[0,0,0],
             "roofType":"gable","width":4,"depth":6,"pitch":45,"wallHeight":0}
    },"rootNodeIds":["l0"]}"#;

    // Not `bounds`: that one drops x/z translation because a wall carries its
    // plan position inside the mesh, but a roof is a `hull` whose position sits
    // on the node -- which is the very thing under test.
    let g = build(json);
    let (mut lo, mut hi) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
    for n in g.nodes.iter().filter(|n| n.name.starts_with("roof")) {
        let Some(mesh) = &n.mesh else { continue };
        let t = n.transform.translation;
        for p in &mesh.positions {
            for (a, v) in [p[0] + t.x, p[1] + t.y, p[2] + t.z].iter().enumerate() {
                lo[a] = lo[a].min(*v);
                hi[a] = hi[a].max(*v);
            }
        }
    }
    assert!(lo[0].is_finite(), "no roof mesh emitted");
    let [x, y, z] = [[lo[0], hi[0]], [lo[1], hi[1]], [lo[2], hi[2]]];
    let mid = |[lo, hi]: [f32; 2]| 0.5 * (lo + hi);
    assert!((mid(x) - 10.0).abs() < 1e-3, "x centred at {}, want 10", mid(x));
    assert!((mid(z) - 5.0).abs() < 1e-3, "z centred at {}, want 5", mid(z));
    // The eaves sit on the container's Y; the ridge rises 2 m above them
    // (half of a 4 m span at 45°).
    assert!((y[0] - 3.0).abs() < 1e-3, "eaves at {}, want 3", y[0]);
    assert!((y[1] - 5.0).abs() < 1e-3, "ridge at {}, want 5", y[1]);
}
