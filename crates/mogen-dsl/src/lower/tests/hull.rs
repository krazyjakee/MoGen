use super::*;
use crate::lower::*;
use crate::parser::parse;

#[test]
fn hull_lowers_a_closed_box_from_cube_corners() {
    let g = lower_src(
        r#"scene {
            hull "blk" (
                points=[
                    [-1, -1, -1], [1, -1, -1], [1, 1, -1], [-1, 1, -1],
                    [-1, -1,  1], [1, -1,  1], [1, 1,  1], [-1, 1,  1]
                ]
            )
        }"#,
    );
    let (min, max) = mesh_aabb(&g, "blk");
    assert!((min.x + 1.0).abs() < 1e-3 && (max.x - 1.0).abs() < 1e-3, "X span ±1");
    assert!((min.y + 1.0).abs() < 1e-3 && (max.y - 1.0).abs() < 1e-3, "Y span ±1");
    assert!((min.z + 1.0).abs() < 1e-3 && (max.z - 1.0).abs() < 1e-3, "Z span ±1");
    let m = find_mesh_node(&g, "blk").mesh.as_ref().unwrap();
    assert!(!m.indices.is_empty(), "hull produced no triangles");
}

#[test]
fn hull_ignores_interior_points() {
    // The origin sits inside the cube and must not change the hull's bounds.
    let g = lower_src(
        r#"scene {
            hull "blk" (
                points=[
                    [-1, -1, -1], [1, -1, -1], [1, 1, -1], [-1, 1, -1],
                    [-1, -1,  1], [1, -1,  1], [1, 1,  1], [-1, 1,  1],
                    [0, 0, 0]
                ]
            )
        }"#,
    );
    let (min, max) = mesh_aabb(&g, "blk");
    assert!((max.x - min.x - 2.0).abs() < 1e-3, "interior point must not grow the hull");
}

#[test]
fn hull_rejects_fewer_than_four_points() {
    // Three points can only span a plane — no closed volume to hull.
    let src = r#"scene {
        hull "bad" (
            points=[[0, 0, 0], [1, 0, 0], [0, 1, 0]]
        )
    }"#;
    let ast = parse(src).unwrap();
    assert!(lower(&ast).is_err(), "hull must reject fewer than 4 points");
}
