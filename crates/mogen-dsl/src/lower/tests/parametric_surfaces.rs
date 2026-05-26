use super::*;
use crate::lower::*;
use crate::parser::parse;

#[test]
fn coil_winds_through_full_revolution_via_dsl() {
    // End-to-end check: parse → validate → lower → mesh. A 1-turn coil
    // must reach all four cardinal sweeps in XZ; if `coil_mesh` ever
    // started silently early-returning (turns clamp, samples too low),
    // this assertion would catch it before the geometry test does.
    let g = lower_src(
        r#"scene { coil "spring" (radius=0.5, height=1.0, turns=1, profile_radius=0.05, samples=24) }"#,
    );
    let n = find_mesh_node(&g, "spring");
    let mesh = n.mesh.as_ref().expect("coil produced no mesh");
    let max_x = mesh.positions.iter().map(|p| p[0]).fold(f32::NEG_INFINITY, f32::max);
    let min_x = mesh.positions.iter().map(|p| p[0]).fold(f32::INFINITY, f32::min);
    let max_z = mesh.positions.iter().map(|p| p[2]).fold(f32::NEG_INFINITY, f32::max);
    let min_z = mesh.positions.iter().map(|p| p[2]).fold(f32::INFINITY, f32::min);
    assert!(max_x > 0.4 && min_x < -0.4, "coil missing X sweep: [{min_x}, {max_x}]");
    assert!(max_z > 0.4 && min_z < -0.4, "coil missing Z sweep: [{min_z}, {max_z}]");
    // Helix climbs from 0 to height — small slack for the parallel-transport
    // tilt at the endpoints.
    let max_y = mesh.positions.iter().map(|p| p[1]).fold(f32::NEG_INFINITY, f32::max);
    let min_y = mesh.positions.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min);
    assert!(min_y.abs() < 0.1 && (max_y - 1.0).abs() < 0.1, "coil Y range wrong: [{min_y}, {max_y}]");
}

#[test]
fn coil_handedness_string_validates() {
    // Both spellings should parse and lower cleanly.
    let _ = lower_src(
        r#"scene { coil "lh" (radius=0.3, height=0.6, turns=2, handedness="left") }"#,
    );
    let _ = lower_src(
        r#"scene { coil "rh" (radius=0.3, height=0.6, turns=2, handedness="right") }"#,
    );
}

#[test]
fn coil_unknown_handedness_errors_at_lower() {
    let ast = parse(
        r#"scene { coil "x" (radius=0.3, height=0.6, turns=2, handedness="diagonal") }"#,
    ).expect("parse");
    let err = lower(&ast).expect_err("expected lowering error for bad handedness");
    assert!(format!("{err:#}").contains("handedness"), "wrong error: {err:#}");
}

#[test]
fn heightfield_lowers_with_displacement() {
    // The defining property: a heightfield with non-zero amplitude must
    // produce a Y-spread mesh — proves the noise sampling actually fires
    // through the lowering path.
    let g = lower_src(
        r#"scene { heightfield "ground" (size=[4, 4], segments_u=16, segments_v=16,
            amplitude=0.5, octaves=3, frequency=0.6, seed=7) }"#,
    );
    let mesh = find_mesh_node(&g, "ground").mesh.as_ref().unwrap();
    let max_y = mesh.positions.iter().map(|p| p[1]).fold(f32::NEG_INFINITY, f32::max);
    let min_y = mesh.positions.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min);
    assert!(max_y - min_y > 0.1, "heightfield Y spread too small: [{min_y}, {max_y}]");
    // 17×17 vertex grid → 289 verts.
    assert_eq!(mesh.positions.len(), 17 * 17);
}

#[test]
fn heightfield_zero_amplitude_is_flat() {
    let g = lower_src(
        r#"scene { heightfield "flat" (size=[2, 2], segments_u=4, segments_v=4, amplitude=0) }"#,
    );
    let mesh = find_mesh_node(&g, "flat").mesh.as_ref().unwrap();
    for p in &mesh.positions {
        assert!(p[1].abs() < 1e-5, "expected flat patch, got y={}", p[1]);
    }
}

#[test]
fn bezier_patch_corners_match_corner_control_points() {
    // Defining property of bicubic Bézier: P(0,0)=P00, P(1,0)=P30,
    // P(0,1)=P03, P(1,1)=P33. End-to-end test through the DSL — proves
    // we wired the `points=` flat list into the right row-major layout.
    let src = r#"scene {
        bezier_patch "p" (
            points = [
                [-1, 0, -1], [-0.3, 0, -1], [0.3, 0, -1], [1, 0, -1],
                [-1, 0, -0.3], [-0.3, 1, -0.3], [0.3, 1, -0.3], [1, 0, -0.3],
                [-1, 0,  0.3], [-0.3, 1,  0.3], [0.3, 1,  0.3], [1, 0,  0.3],
                [-1, 0,  1], [-0.3, 0,  1], [0.3, 0,  1], [1, 0,  1],
            ],
            segments_u = 4, segments_v = 4,
        )
    }"#;
    let g = lower_src(src);
    let mesh = find_mesh_node(&g, "p").mesh.as_ref().unwrap();
    // 5×5 vertex grid, row-major along u (5 vertices per row).
    let nu = 5usize;
    let nv = 5usize;
    assert_eq!(mesh.positions.len(), nu * nv);
    let p00 = mesh.positions[0];
    let p10 = mesh.positions[(nu - 1) * nv];
    let p01 = mesh.positions[nv - 1];
    let p11 = mesh.positions[(nu - 1) * nv + nv - 1];
    // Corners of the control net are at ±1 on X and Z, y=0.
    let close = |a: f32, b: f32| (a - b).abs() < 1e-4;
    assert!(close(p00[0], -1.0) && close(p00[2], -1.0));
    assert!(close(p10[0], -1.0) && close(p10[2],  1.0));
    assert!(close(p01[0],  1.0) && close(p01[2], -1.0));
    assert!(close(p11[0],  1.0) && close(p11[2],  1.0));
    assert!(p00[1].abs() < 1e-4 && p11[1].abs() < 1e-4);
    // Centre vertex bulges up — interior control points are at y=1.
    let centre = mesh.positions[(nu / 2) * nv + nv / 2];
    assert!(centre[1] > 0.4, "expected centre to bulge, got {}", centre[1]);
}

#[test]
fn bezier_patch_wrong_point_count_errors() {
    let ast = parse(
        r#"scene { bezier_patch "x" (points=[[0, 0, 0], [1, 0, 0], [0, 0, 1]]) }"#,
    ).expect("parse");
    let err = lower(&ast).expect_err("bezier_patch with 3 points must reject at lower");
    assert!(format!("{err:#}").contains("16"), "wrong error: {err:#}");
}

#[test]
fn metaball_three_centres_unite_into_one_mesh() {
    // Three overlapping spheres along X with a smooth blend should
    // produce a single connected mesh whose extent matches the union.
    let g = lower_src(
        r#"scene { metaball "blob" (
            points = [[-0.4, 0, 0], [0, 0, 0], [0.4, 0, 0]],
            radius = 0.4, blend = 0.1
        ) }"#,
    );
    let mesh = find_mesh_node(&g, "blob").mesh.as_ref().unwrap();
    assert!(!mesh.positions.is_empty(), "metaball produced no vertices");
    let max_x = mesh.positions.iter().map(|p| p[0]).fold(f32::NEG_INFINITY, f32::max);
    let min_x = mesh.positions.iter().map(|p| p[0]).fold(f32::INFINITY, f32::min);
    assert!(max_x > 0.7, "metaball missing +X extent: {max_x}");
    assert!(min_x < -0.7, "metaball missing -X extent: {min_x}");
}

#[test]
fn metaball_per_point_radii_apply() {
    let g = lower_src(
        r#"scene { metaball "asym" (
            points = [[-1, 0, 0], [1, 0, 0]],
            radii = [0.3, 0.7], blend = 0
        ) }"#,
    );
    let mesh = find_mesh_node(&g, "asym").mesh.as_ref().unwrap();
    let max_x = mesh.positions.iter().map(|p| p[0]).fold(f32::NEG_INFINITY, f32::max);
    let min_x = mesh.positions.iter().map(|p| p[0]).fold(f32::INFINITY, f32::min);
    assert!((max_x - 1.7).abs() < 0.05, "max_x off: {max_x}");
    assert!((min_x - (-1.3)).abs() < 0.05, "min_x off: {min_x}");
}

#[test]
fn metaball_no_radius_or_radii_errors() {
    let ast = parse(
        r#"scene { metaball "x" (points=[[0, 0, 0]]) }"#,
    ).expect("parse");
    let err = lower(&ast).expect_err("metaball without radius/radii must reject");
    assert!(format!("{err:#}").contains("radius"), "wrong error: {err:#}");
}
