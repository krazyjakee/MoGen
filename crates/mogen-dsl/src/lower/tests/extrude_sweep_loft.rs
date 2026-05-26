use super::*;
use crate::lower::*;
use crate::parser::parse;

#[test]
fn extrude_lowers_with_default_attrs() {
    let g = lower_src(r#"scene { extrude "block" () }"#);
    let m = find_mesh_node(&g, "block").mesh.as_ref().unwrap();
    assert!(!m.positions.is_empty(), "extrude default mesh missing positions");
    assert!(!m.indices.is_empty(), "extrude default mesh missing indices");
    // Default outline is a unit square 1×1 in XZ; default height 1.
    let (min, max) = mesh_aabb(&g, "block");
    assert!((max.y - min.y - 1.0).abs() < 1e-3, "Y span should be 1");
}

#[test]
fn extrude_with_explicit_polygon_shape() {
    // L-shaped polygon: not convex, exercises earcut.
    let g = lower_src(
        r#"scene {
            extrude "ell" (
                points=[[0, 0], [2, 0], [2, 1], [1, 1], [1, 2], [0, 2]],
                height=0.5
            )
        }"#,
    );
    let (_min, max) = mesh_aabb(&g, "ell");
    assert!((max.x - 2.0).abs() < 1e-3 && (max.z - 2.0).abs() < 1e-3,
        "L-shape should span 2 units on X and Z");
    // Concave triangulation must still emit non-trivial tri count — earcut
    // on a 6-vert poly emits 4 cap tris × 2 caps + 12 side rib tris.
    let m = find_mesh_node(&g, "ell").mesh.as_ref().unwrap();
    assert!(m.indices.len() / 3 > 8, "concave extrude should emit > 8 tris");
}

#[test]
fn sweep_lowers_along_path() {
    let g = lower_src(
        r#"scene {
            sweep "rail" (
                profile=[[-0.05, -0.02], [0.05, -0.02], [0.05, 0.02], [-0.05, 0.02]],
                path=[[-1, 0, 0], [0, 0, 0], [1, 0, 0]],
                samples=8
            )
        }"#,
    );
    let (min, max) = mesh_aabb(&g, "rail");
    assert!(min.x < -0.9 && max.x > 0.9, "swept rail should span ±1 along path");
    let m = find_mesh_node(&g, "rail").mesh.as_ref().unwrap();
    assert!(!m.indices.is_empty(), "sweep produced no triangles");
}

#[test]
fn loft_lowers_three_sections() {
    let g = lower_src(
        r#"scene {
            loft "hull" (
                points=[
                    [-0.5, -0.2], [0.5, -0.2], [0.5, 0.2], [-0.5, 0.2],
                    [-1.0, -0.4], [1.0, -0.4], [1.0, 0.4], [-1.0, 0.4],
                    [-0.6, -0.1], [0.6, -0.1], [0.6, 0.1], [-0.6, 0.1]
                ],
                heights=[0.0, 1.0, 2.0]
            )
        }"#,
    );
    let (min, max) = mesh_aabb(&g, "hull");
    assert!(min.y.abs() < 1e-3 && (max.y - 2.0).abs() < 1e-3,
        "Y span should be [0, 2] (got [{}, {}])", min.y, max.y);
    // Middle section reaches ±1 on X.
    assert!(max.x > 0.95, "mid section X extent should reach ~1 (got {})", max.x);
}

#[test]
fn loft_rejects_inconsistent_section_lengths() {
    // points contains 5 vertices, heights demands 2 sections — 5 % 2 ≠ 0.
    let src = r#"scene {
        loft "bad" (
            points=[[0, 0], [1, 0], [1, 1], [0, 1], [0.5, 0.5]],
            heights=[0.0, 1.0]
        )
    }"#;
    let ast = parse(src).unwrap();
    assert!(lower(&ast).is_err(), "loft must reject mismatched section lengths");
}

#[test]
fn loft_rejects_single_section() {
    // heights with fewer than 2 entries leaves nothing to interpolate
    // between — must error rather than emit a degenerate mesh.
    let src = r#"scene {
        loft "bad" (
            points=[[0, 0], [1, 0], [1, 1], [0, 1]],
            heights=[0.0]
        )
    }"#;
    let ast = parse(src).unwrap();
    assert!(lower(&ast).is_err(), "loft must reject heights.len() < 2");
}

#[test]
fn loft_rejects_section_with_fewer_than_three_vertices() {
    // 2 sections × 2 verts per section → per_section=2 < 3 (a loft section
    // needs to be a closed polygon with at least 3 vertices).
    let src = r#"scene {
        loft "bad" (
            points=[[0, 0], [1, 0], [0, 1], [1, 1]],
            heights=[0.0, 1.0]
        )
    }"#;
    let ast = parse(src).unwrap();
    assert!(lower(&ast).is_err(), "loft must reject per-section vertex count < 3");
}
