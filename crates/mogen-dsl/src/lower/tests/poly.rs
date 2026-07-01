use super::*;
use crate::parser::parse;

#[test]
fn poly_carries_explicit_uvs_through_lowering() {
    // One quad whose UVs sample an atlas sub-rectangle — exactly the case a
    // procedural Tile/Fit projection can't reproduce. The lowered mesh must
    // hand those UVs back verbatim.
    let g = lower_src(
        r#"scene {
            poly "face" (
                points=[[0, 0, 0], [1, 0, 0], [1, 1, 0], [0, 1, 0]],
                uvs=[[0.25, 0.5], [0.5, 0.5], [0.5, 0.75], [0.25, 0.75]],
                indices=[0, 1, 2, 0, 2, 3]
            )
        }"#,
    );
    let m = find_mesh_node(&g, "face").mesh.as_ref().unwrap();
    assert_eq!(m.positions.len(), 4);
    assert_eq!(m.uvs.len(), 4, "UV channel must be present");
    assert!((m.uvs[0][0] - 0.25).abs() < 1e-6 && (m.uvs[0][1] - 0.5).abs() < 1e-6);
    assert!((m.uvs[2][0] - 0.5).abs() < 1e-6 && (m.uvs[2][1] - 0.75).abs() < 1e-6);
    assert_eq!(m.indices, vec![0, 1, 2, 0, 2, 3]);
}

#[test]
fn poly_rejects_uv_count_mismatch() {
    // Three points, two UVs — must fail rather than silently drop the channel.
    let src = r#"scene {
        poly "bad" (
            points=[[0, 0, 0], [1, 0, 0], [1, 1, 0]],
            uvs=[[0, 0], [1, 0]],
            indices=[0, 1, 2]
        )
    }"#;
    let ast = parse(src).unwrap();
    assert!(lower(&ast).is_err(), "poly must reject a UV/point length mismatch");
}

#[test]
fn poly_rejects_indices_not_multiple_of_three() {
    let src = r#"scene {
        poly "bad" (
            points=[[0, 0, 0], [1, 0, 0], [1, 1, 0]],
            indices=[0, 1, 2, 0]
        )
    }"#;
    let ast = parse(src).unwrap();
    assert!(lower(&ast).is_err(), "poly indices must be a multiple of 3");
}
