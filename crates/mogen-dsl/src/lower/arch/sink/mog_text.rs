//! [`ResolvedGeometry`] → `.mog` source text.
//!
//! The phase-1 sink. An import produces a *file the user can then edit*, not an
//! opaque mesh, which is the whole reason to import into this project rather
//! than just render the JSON.
//!
//! # Mapping
//!
//! | Shape | DSL |
//! |---|---|
//! | `Prism`, no holes or one | `extrude (points=…, hole=…, height=…)` |
//! | `Prism`, several holes | `difference { extrude … extrude … }` |
//! | `Hull` | `hull (points=[[x,y,z], …])` |
//!
//! Two details of `extrude` drive most of the code here. Its mesh is **centred
//! on y = 0**, spanning `[-h/2, +h/2]`, so a solid spanning `[base, top]` is
//! emitted at `pos.y = (base + top) / 2` — not at `base`. And it takes exactly
//! **one** `hole`, so a slab with two stairwells has to become a `difference`.
//!
//! Ring winding is normalised on the way out rather than assumed. The solver
//! orders the rings it builds, but a hole arrives however its producer supplied
//! it — and a clockwise `points=` gives earcut an inverted cap, which reaches
//! Manifold as a non-manifold operand and *panics* rather than erroring.
//!
//! # Materials
//!
//! A `mat=` naming a material the file never declares is a hard lowering
//! error, so a single unresolved material would take down an entire import.
//! References to undeclared materials are therefore dropped and noted in the
//! header: grey geometry beats no geometry.
//!
//! # Numbers
//!
//! Everything is rounded to 0.1 mm and printed with trailing zeros stripped.
//! That is not cosmetic: it keeps a re-import diffable against the previous
//! one, and it cannot break the shared-vertex property the mitre solver
//! establishes, because two walls at a corner compute *bit-identical* floats
//! and so round to identical text.

use std::fmt::Write as _;

use super::super::ir::{MatRef, Marker};
use super::super::plan;
use super::super::resolved::{MaterialDecl, Placement, ResolvedGeometry, Shape, Solid, P3};

/// How far a `difference` cutter over-runs the solid it cuts, top and bottom.
///
/// Coplanar faces are the classic way to make a boolean produce degenerate
/// slivers, so the cutter is deliberately taller than the thing it passes
/// through.
const CUT_OVERRUN: f32 = 0.01;

/// Render a whole file: header comments, material declarations, then the scene.
///
/// Scoped to `arch` rather than the crate: everything it takes and returns is
/// arch's own vocabulary, so a public façade belongs in `arch/mod.rs` alongside
/// `solve` when the CLI needs one — not here.
pub(in crate::lower::arch) fn write_mog(
    scene_name: &str,
    header: &[String],
    materials: &[MaterialDecl],
    g: &ResolvedGeometry,
) -> String {
    let mut s = String::new();
    let declared: Vec<&str> = materials.iter().map(|m| m.name.as_str()).collect();

    // A `mat=` naming a material the file never declares is a hard lowering
    // error, so one material a producer failed to resolve would take down the
    // entire import. Drop the reference instead and say so — the geometry is
    // still worth having, and grey is a better outcome than nothing.
    let mut missing: Vec<String> = g
        .solids
        .iter()
        .filter_map(|s| s.material.as_ref())
        .filter(|MatRef(n)| !declared.contains(&n.as_str()))
        .map(|MatRef(n)| n.clone())
        .collect();
    missing.sort();
    missing.dedup();

    let notes: Vec<String> = g
        .warnings
        .iter()
        .cloned()
        .chain(
            missing
                .iter()
                .map(|n| format!("material {n:?} was never declared; nodes left unpainted")),
        )
        .collect();

    for line in header {
        let _ = writeln!(s, "// {line}");
    }
    // Notes go into the file rather than to stderr: they survive, they diff,
    // and whoever opens the result later sees what was dropped from it.
    if !notes.is_empty() {
        let _ = writeln!(s, "//");
        let _ = writeln!(s, "// {} item(s) needed attention:", notes.len());
        for w in &notes {
            let _ = writeln!(s, "//   {w}");
        }
    }
    if !s.is_empty() {
        s.push('\n');
    }

    for m in materials {
        let _ = writeln!(s, "{}", material_line(m));
    }
    if !materials.is_empty() {
        s.push('\n');
    }

    let _ = writeln!(s, "scene {} {{", quote(scene_name));

    // Grouped by level, in order, so the file reads like a building rather
    // than like a dump.
    let mut levels: Vec<_> = g.solids.iter().map(|s| s.level).collect();
    levels.sort_unstable();
    levels.dedup();

    for level in levels {
        let _ = writeln!(s, "  group {} {{", quote(&format!("level_{}", level.0)));
        for solid in g.solids.iter().filter(|s| s.level == level) {
            for line in solid_lines(solid, &declared) {
                let _ = writeln!(s, "    {line}");
            }
        }
        let _ = writeln!(s, "  }}");
    }

    for m in &g.markers {
        let _ = writeln!(s, "  {}", marker_line(m));
    }

    let _ = writeln!(s, "}}");
    s
}

fn material_line(m: &MaterialDecl) -> String {
    let mut attrs = Vec::new();
    if let Some(c) = m.color {
        attrs.push(format!("color=[{}, {}, {}]", num(c[0]), num(c[1]), num(c[2])));
    }
    if let Some(v) = m.metallic {
        attrs.push(format!("metallic={}", num(v)));
    }
    if let Some(v) = m.roughness {
        attrs.push(format!("roughness={}", num(v)));
    }
    if let Some(t) = &m.texture {
        attrs.push(format!("texture={}", quote(t)));
    }
    format!("material {} ({})", quote(&m.name), attrs.join(", "))
}

/// One solid, as one or more lines of DSL.
fn solid_lines(solid: &Solid, declared: &[&str]) -> Vec<String> {
    match &solid.shape {
        Shape::Hull { points } => vec![format!(
            "hull {} (points={}{})",
            quote(&solid.name),
            points3(points),
            common_attrs(solid, solid.placement, None, declared)
        )],

        Shape::Prism { poly, base, top } => {
            let height = top - base;
            let lift = 0.5 * (base + top);

            match poly.holes.len() {
                // `extrude` takes at most one hole, so these are the cases it
                // can express directly.
                0 => vec![format!(
                    "extrude {} (points={}, height={}{})",
                    quote(&solid.name),
                    points2(&poly.outer),
                    num(height),
                    common_attrs(solid, solid.placement, Some(lift), declared)
                )],
                1 => vec![format!(
                    "extrude {} (points={}, hole={}, height={}{})",
                    quote(&solid.name),
                    points2(&poly.outer),
                    points2(&as_hole(&poly.holes[0])),
                    num(height),
                    common_attrs(solid, solid.placement, Some(lift), declared)
                )],
                // More than one hole has to become a boolean.
                _ => {
                    let mut out = vec![format!(
                        "difference {} ({}) {{",
                        quote(&solid.name),
                        common_attrs(solid, solid.placement, Some(lift), declared)
                            .trim_start_matches(", ")
                            .to_string()
                    )];
                    out.push(format!(
                        "  extrude (points={}, height={})",
                        points2(&as_outline(&poly.outer)),
                        num(height)
                    ));
                    for (i, hole) in poly.holes.iter().enumerate() {
                        out.push(format!(
                            "  extrude {} (points={}, height={})",
                            quote(&format!("cut{i}")),
                            points2(&as_outline(hole)),
                            num(height + 2.0 * CUT_OVERRUN)
                        ));
                    }
                    out.push("}".into());
                    out
                }
            }
        }
    }
}

/// `role`, `mat` and placement, as a trailing attribute fragment.
///
/// `lift` is the extra Y a prism needs because `extrude` centres its mesh on
/// the origin. Hulls pass `None` — their points already carry their own
/// heights.
fn common_attrs(
    solid: &Solid,
    placement: Placement,
    lift: Option<f32>,
    declared: &[&str],
) -> String {
    let mut attrs = Vec::new();

    let t = placement.translation;
    let y = t[1] + lift.unwrap_or(0.0);
    if t[0] != 0.0 || y != 0.0 || t[2] != 0.0 {
        attrs.push(format!("pos=[{}, {}, {}]", num(t[0]), num(y), num(t[2])));
    }
    if placement.rotation != 0.0 {
        attrs.push(format!("ry={}", num(placement.rotation.to_degrees())));
    }
    attrs.push(format!("role={}", quote(solid.role.as_str())));
    if let Some(MatRef(name)) = &solid.material {
        // Silently skipping an undeclared name is the point: see the note in
        // `write_mog` about one bad material taking down the whole file.
        if declared.contains(&name.as_str()) {
            attrs.push(format!("mat={}", quote(name)));
        }
    }

    format!(", {}", attrs.join(", "))
}

/// A marker, as an empty `group` carrying its role and tags.
///
/// Not `poi`: that is the *scene-graph* kind `lower/poi.rs` stamps on nodes it
/// builds programmatically, and the DSL has no such surface node — emitting it
/// makes the file fail to lower. A childless `group` is exactly a transform
/// with metadata, and `role` / `tags` reach `node.extras` the same way.
fn marker_line(m: &Marker) -> String {
    let mut attrs = vec![format!(
        "pos=[{}, {}, {}]",
        num(m.position[0]),
        num(m.position[1]),
        num(m.position[2])
    )];
    if m.rotation != 0.0 {
        attrs.push(format!("ry={}", num(m.rotation.to_degrees())));
    }
    attrs.push(format!("role={}", quote(&m.role)));
    if !m.tags.is_empty() {
        attrs.push(format!("tags={}", quote(&m.tags.join(","))));
    }
    format!("group {} ({}) {{ }}", quote(&m.name), attrs.join(", "))
}

/// A hole ring wound for `extrude`'s `hole=`, which earcut wants running
/// against the outer ring.
fn as_hole(ring: &[[f32; 2]]) -> Vec<[f32; 2]> {
    let mut r = as_outline(ring);
    r.reverse();
    r
}

/// A ring wound as an outer outline: counter-clockwise, which is what
/// `extrude`'s `points=` requires.
///
/// The solver normalises the rings it builds, but a hole arrives however its
/// producer supplied it — and a clockwise `points=` gives earcut an inverted
/// cap, which reaches Manifold as a non-manifold operand and panics rather
/// than erroring.
fn as_outline(ring: &[[f32; 2]]) -> Vec<[f32; 2]> {
    let mut r = ring.to_vec();
    plan::normalise_ccw(&mut r);
    r
}

fn points2(ring: &[[f32; 2]]) -> String {
    let inner: Vec<String> =
        ring.iter().map(|p| format!("[{}, {}]", num(p[0]), num(p[1]))).collect();
    format!("[{}]", inner.join(", "))
}

fn points3(pts: &[P3]) -> String {
    let inner: Vec<String> = pts
        .iter()
        .map(|p| format!("[{}, {}, {}]", num(p[0]), num(p[1]), num(p[2])))
        .collect();
    format!("[{}]", inner.join(", "))
}

/// A quoted DSL string, with the two escapes the grammar understands.
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            // A raw newline would end the token and produce a parse error two
            // lines later, pointing at something unrelated.
            '\n' | '\r' | '\t' => out.push(' '),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A number at 0.1 mm, with no trailing zeros and no `-0`.
fn num(v: f32) -> String {
    if !v.is_finite() {
        return "0".into();
    }
    let r = (v * 10_000.0).round() / 10_000.0;
    if r == 0.0 {
        // Catches -0.0 too, which would otherwise print as "-0" and make two
        // identical files differ.
        return "0".into();
    }
    let mut s = format!("{r:.4}");
    while s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lower::arch::ir::{LevelId, Polygon};
    use crate::lower::arch::resolved::Role;

    fn square(size: f32) -> Vec<[f32; 2]> {
        vec![[0.0, 0.0], [size, 0.0], [size, size], [0.0, size]]
    }

    fn solid(name: &str, shape: Shape) -> Solid {
        Solid {
            name: name.into(),
            role: Role::Wall,
            level: LevelId(0),
            shape,
            placement: Placement::IDENTITY,
            material: None,
        }
    }

    fn prism(base: f32, top: f32, holes: Vec<Vec<[f32; 2]>>) -> Shape {
        Shape::Prism { poly: Polygon { outer: square(4.0), holes }, base, top }
    }

    fn render(g: &ResolvedGeometry) -> String {
        write_mog("house", &[], &[], g)
    }

    fn geometry(solids: Vec<Solid>) -> ResolvedGeometry {
        ResolvedGeometry { solids, ..Default::default() }
    }

    #[test]
    fn numbers_are_compact_and_have_no_negative_zero() {
        assert_eq!(num(1.0), "1");
        assert_eq!(num(2.5), "2.5");
        assert_eq!(num(0.1234), "0.1234");
        assert_eq!(num(-0.0), "0", "-0 would make two identical files differ");
        assert_eq!(num(0.000_01), "0");
        assert_eq!(num(-3.25), "-3.25");
        assert_eq!(num(f32::NAN), "0");
    }

    #[test]
    fn a_prism_is_lifted_because_extrude_centres_its_mesh() {
        // extrude spans [-h/2, +h/2], so a solid from 0 to 2.7 sits at 1.35.
        // Emitting pos.y = base instead would sink every wall half its height
        // into the floor -- and each wall would still look like a wall.
        let g = geometry(vec![solid("w", prism(0.0, 2.7, vec![]))]);
        let out = render(&g);
        assert!(out.contains("height=2.7"), "{out}");
        assert!(out.contains("pos=[0, 1.35, 0]"), "{out}");
    }

    #[test]
    fn a_prism_starting_off_the_ground_is_lifted_from_its_own_midpoint() {
        let g = geometry(vec![solid("lintel", prism(2.1, 2.7, vec![]))]);
        assert!(render(&g).contains("pos=[0, 2.4, 0]"), "{}", render(&g));
    }

    #[test]
    fn one_hole_uses_extrudes_own_hole_attribute() {
        let hole = vec![[1.0, 1.0], [1.0, 3.0], [3.0, 3.0], [3.0, 1.0]];
        let g = geometry(vec![solid("slab", prism(0.0, 0.2, vec![hole.clone()]))]);
        let out = render(&g);
        assert!(out.contains("hole="), "{out}");
        assert!(!out.contains("difference"), "one hole needs no boolean: {out}");

        // Earcut wants the hole wound against the outer ring. Assert the
        // winding itself rather than a particular starting vertex — the ring
        // is normalised before it is reversed, so which point comes first is
        // an implementation detail and the handedness is the contract.
        assert!(plan::signed_area2(&as_outline(&hole)) > 0.0, "outlines run CCW");
        assert!(plan::signed_area2(&as_hole(&hole)) < 0.0, "holes run against them");
    }

    #[test]
    fn several_holes_become_a_difference() {
        // `extrude` takes exactly one hole, so a slab with two stairwells has
        // to be expressed as a boolean instead.
        let a = vec![[1.0, 1.0], [1.0, 2.0], [2.0, 2.0], [2.0, 1.0]];
        let b = vec![[3.0, 1.0], [3.0, 2.0], [3.5, 2.0], [3.5, 1.0]];
        let g = geometry(vec![solid("slab", prism(0.0, 0.2, vec![a, b]))]);
        let out = render(&g);
        assert!(out.contains("difference \"slab\""), "{out}");
        assert_eq!(out.matches("extrude").count(), 3, "one base plus two cutters: {out}");
        assert!(!out.contains("hole="), "{out}");
    }

    #[test]
    fn difference_cutters_overrun_the_solid_they_cut() {
        // Coplanar faces are how booleans produce degenerate slivers.
        let a = vec![[1.0, 1.0], [1.0, 2.0], [2.0, 2.0], [2.0, 1.0]];
        let b = vec![[3.0, 1.0], [3.0, 2.0], [3.5, 2.0], [3.5, 1.0]];
        let g = geometry(vec![solid("slab", prism(0.0, 0.2, vec![a, b]))]);
        let out = render(&g);
        assert!(out.contains("height=0.2"), "the base keeps its true height: {out}");
        assert!(out.contains("height=0.22"), "cutters over-run: {out}");
    }

    #[test]
    fn a_hull_writes_its_points_in_three_dimensions() {
        let g = geometry(vec![solid(
            "roof",
            Shape::Hull {
                points: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]],
            },
        )]);
        let out = render(&g);
        assert!(out.contains("hull \"roof\" (points=[[0, 0, 0], "), "{out}");
        assert!(!out.contains("height="), "a hull has no height attribute: {out}");
    }

    #[test]
    fn a_hulls_placement_becomes_pos_and_ry() {
        // Rotation must not be baked into the points, or nudging a roof's
        // angle rewrites every number in the file.
        let mut s = solid(
            "roof",
            Shape::Hull {
                points: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]],
            },
        );
        s.placement = Placement {
            translation: [3.0, 5.2, -2.0],
            rotation: std::f32::consts::FRAC_PI_2,
        };
        let out = render(&geometry(vec![s]));
        assert!(out.contains("pos=[3, 5.2, -2]"), "{out}");
        assert!(out.contains("ry=90"), "radians must be converted: {out}");
    }

    #[test]
    fn solids_are_grouped_by_level_in_order() {
        let mut upper = solid("w1", prism(2.7, 5.4, vec![]));
        upper.level = LevelId(1);
        let mut cellar = solid("w2", prism(-2.4, 0.0, vec![]));
        cellar.level = LevelId(-1);
        let out = render(&geometry(vec![upper, solid("w0", prism(0.0, 2.7, vec![])), cellar]));

        let order: Vec<_> = ["level_-1", "level_0", "level_1"]
            .iter()
            .map(|n| out.find(n).unwrap_or_else(|| panic!("{n} missing from {out}")))
            .collect();
        assert!(order[0] < order[1] && order[1] < order[2], "levels out of order: {out}");
    }

    #[test]
    fn materials_are_declared_before_the_scene() {
        let m = MaterialDecl {
            name: "brick".into(),
            color: Some([0.6, 0.3, 0.25]),
            roughness: Some(0.9),
            ..Default::default()
        };
        let mut s = solid("w", prism(0.0, 2.7, vec![]));
        s.material = Some(MatRef("brick".into()));
        let out = write_mog("house", &[], &[m], &geometry(vec![s]));

        let decl = out.find("material \"brick\"").expect("declaration");
        let scene = out.find("scene").expect("scene");
        assert!(decl < scene, "a material must be declared before it is used: {out}");
        assert!(out.contains("color=[0.6, 0.3, 0.25]"), "{out}");
        assert!(out.contains("mat=\"brick\""), "{out}");
        assert!(!out.contains("metallic="), "unset fields are omitted: {out}");
    }

    #[test]
    fn warnings_are_written_into_the_file() {
        // They survive, they diff, and whoever opens the result sees what was
        // dropped from it.
        let g = ResolvedGeometry {
            solids: vec![solid("w", prism(0.0, 2.7, vec![]))],
            warnings: vec!["wall 3: zero length or thickness, dropped".into()],
            ..Default::default()
        };
        let out = render(&g);
        assert!(out.contains("// 1 item(s) needed attention:"), "{out}");
        assert!(out.contains("//   wall 3: zero length"), "{out}");
    }

    #[test]
    fn header_lines_become_comments() {
        let g = geometry(vec![solid("w", prism(0.0, 2.7, vec![]))]);
        let out = write_mog("house", &["Imported from garden.json".into()], &[], &g);
        assert!(out.starts_with("// Imported from garden.json\n"), "{out}");
    }

    #[test]
    fn strings_with_quotes_and_newlines_are_escaped() {
        // A raw newline inside a string ends the token and reports a parse
        // error somewhere unrelated two lines later.
        assert_eq!(quote(r#"a"b"#), r#""a\"b""#);
        assert_eq!(quote(r"a\b"), r#""a\\b""#);
        assert_eq!(quote("a\nb"), r#""a b""#);
    }

    #[test]
    fn markers_are_emitted_as_poi_nodes() {
        let g = ResolvedGeometry {
            markers: vec![Marker {
                name: "sofa_1".into(),
                role: "furniture".into(),
                position: [1.0, 0.0, 2.0],
                rotation: 0.0,
                tags: vec!["seating".into(), "imported".into()],
            }],
            ..Default::default()
        };
        let out = render(&g);
        assert!(out.contains("group \"sofa_1\""), "{out}");
        assert!(out.contains("role=\"furniture\""), "{out}");
        assert!(out.contains("tags=\"seating,imported\""), "{out}");
    }

    #[test]
    fn rendering_is_reproducible() {
        let g = geometry(vec![
            solid("w0", prism(0.0, 2.7, vec![])),
            solid("w1", prism(0.0, 2.7, vec![])),
        ]);
        assert_eq!(render(&g), render(&g));
    }

    #[test]
    fn an_empty_model_still_produces_a_valid_scene() {
        let out = render(&ResolvedGeometry::default());
        assert!(out.contains("scene \"house\" {"), "{out}");
        assert!(out.trim_end().ends_with('}'), "{out}");
    }

    // ---- Round trips -------------------------------------------------
    //
    // Everything above asserts on the *text*, which proves nothing about
    // whether the text is valid DSL. These run the real parser and lowerer
    // over the output. A missing comma or a misnamed attribute shows up here
    // and nowhere else.

    fn round_trip(out: &str) -> mogen_core::SceneGraph {
        let ast = crate::parser::parse(out)
            .unwrap_or_else(|e| panic!("emitted source does not parse: {e}\n---\n{out}"));
        crate::lower(&ast)
            .unwrap_or_else(|e| panic!("emitted source does not lower: {e}\n---\n{out}"))
    }

    fn built_house() -> ResolvedGeometry {
        use crate::lower::arch::ir::{
            ArchModel, Ceiling, CeilingId, Level, ModelSource, Opening, OpeningKind, RoofId,
            RoofParams, RoofSegment, RoofType, Slab, SlabId, Wall, WallId,
        };
        use crate::lower::arch::resolve::solve;

        let mut m = ArchModel::new(ModelSource::PascalEditor);
        m.levels.push(Level { id: LevelId(0), name: None, height: 2.7 });

        let corners = [[0.0, 0.0], [6.0, 0.0], [6.0, 4.0], [0.0, 4.0]];
        for i in 0..4 {
            m.push_wall(Wall {
                id: WallId(0),
                level: LevelId(0),
                start: corners[i],
                end: corners[(i + 1) % 4],
                thickness: 0.2,
                height: None,
                curve_offset: None,
                openings: Vec::new(),
                material: Some(MatRef("brick".into())),
            });
        }
        m.walls[0].openings.push(Opening {
            kind: OpeningKind::Door,
            along: 3.0,
            sill: 0.0,
            width: 0.9,
            height: 2.1,
        });
        m.walls[1].openings.push(Opening {
            kind: OpeningKind::Window,
            along: 2.0,
            sill: 0.9,
            width: 1.2,
            height: 1.0,
        });

        let floor = Polygon {
            outer: vec![[0.0, 0.0], [6.0, 0.0], [6.0, 4.0], [0.0, 4.0]],
            holes: vec![],
        };
        m.push_slab(Slab {
            id: SlabId(0),
            level: LevelId(0),
            poly: floor.clone(),
            elevation: 0.05,
            thickness: 0.05,
            material: None,
        });
        m.push_ceiling(Ceiling {
            id: CeilingId(0),
            level: LevelId(0),
            poly: floor,
            elevation: None,
            material: None,
        });
        m.push_roof(RoofSegment {
            id: RoofId(0),
            level: LevelId(0),
            centre: [3.0, 2.0],
            width: 6.0,
            depth: 4.0,
            rotation: 0.0,
            pitch_deg: 35.0,
            roof_type: RoofType::Gable,
            overhang: 0.4,
            wall_height: 2.7,
            params: RoofParams::default(),
            material: None,
        });

        solve(&m)
    }

    #[test]
    fn a_whole_house_parses_and_lowers() {
        let g = built_house();
        assert!(g.warnings.is_empty(), "{:?}", g.warnings);

        let brick = MaterialDecl {
            name: "brick".into(),
            color: Some([0.6, 0.3, 0.25]),
            roughness: Some(0.9),
            ..Default::default()
        };
        let out = write_mog("house", &["Imported from a fixture".into()], &[brick], &g);
        let graph = round_trip(&out);

        // Every solid became a node, and every node has geometry.
        let meshes = graph.nodes.iter().filter(|n| n.mesh.is_some()).count();
        assert!(
            meshes >= g.solids.len(),
            "{} solids produced only {meshes} meshes\n{out}",
            g.solids.len()
        );
    }

    #[test]
    fn no_emitted_node_lowers_to_an_empty_mesh() {
        // The failure this whole layer exists to prevent. `extrude` returns a
        // capless mesh on a bad polygon and `hull` an empty one on coplanar
        // points -- neither reports anything, so the only way to catch it is
        // to look at what came out.
        let graph = round_trip(&write_mog("house", &[], &[], &built_house()));
        for node in &graph.nodes {
            if let Some(mesh) = &node.mesh {
                assert!(
                    !mesh.positions.is_empty() && !mesh.indices.is_empty(),
                    "node {:?} lowered to an empty mesh",
                    node.name
                );
            }
        }
    }

    #[test]
    fn a_multi_hole_slab_parses_and_lowers() {
        // The `difference` path, which no other round trip reaches.
        let slab = Solid {
            name: "slab".into(),
            role: Role::Slab,
            level: LevelId(0),
            shape: Shape::Prism {
                poly: Polygon {
                    outer: square(6.0),
                    holes: vec![
                        vec![[1.0, 1.0], [1.0, 2.0], [2.0, 2.0], [2.0, 1.0]],
                        vec![[4.0, 3.0], [4.0, 4.0], [5.0, 4.0], [5.0, 3.0]],
                    ],
                },
                base: 0.0,
                top: 0.2,
            },
            placement: Placement::IDENTITY,
            material: None,
        };
        let out = write_mog("floor", &[], &[], &geometry(vec![slab]));
        let graph = round_trip(&out);
        assert!(
            graph.nodes.iter().any(|n| n.mesh.is_some()),
            "the difference produced no geometry\n{out}"
        );
    }

    #[test]
    fn a_curved_wall_parses_and_lowers() {
        use crate::lower::arch::ir::{ArchModel, Level, ModelSource, Wall, WallId};
        use crate::lower::arch::resolve::solve;

        let mut m = ArchModel::new(ModelSource::PascalEditor);
        m.levels.push(Level { id: LevelId(0), name: None, height: 2.7 });
        m.push_wall(Wall {
            id: WallId(0),
            level: LevelId(0),
            start: [0.0, 0.0],
            end: [6.0, 0.0],
            thickness: 0.2,
            height: None,
            curve_offset: Some(1.0),
            openings: Vec::new(),
            material: None,
        });

        let out = write_mog("bay", &[], &[], &solve(&m));
        round_trip(&out);
    }
}
