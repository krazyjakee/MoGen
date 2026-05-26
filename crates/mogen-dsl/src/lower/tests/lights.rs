use super::*;
use crate::lower::*;
use glam::Vec3;

#[test]
fn light_directional_lowers_with_color_and_intensity() {
    let g = lower_src(
        r#"scene { light "sun" (kind=directional, color=[1, 0.95, 0.85], intensity=3) }"#,
    );
    let n = find_mesh_node(&g, "sun");
    assert!(n.mesh.is_none(), "light should not carry a mesh");
    let l = n.light.as_ref().expect("light field set");
    assert_eq!(l.kind, mogen_core::LightKind::Directional);
    assert_eq!(l.color, [1.0, 0.95, 0.85]);
    assert!((l.intensity - 3.0).abs() < 1e-6);
    assert!(l.range.is_none());
}

#[test]
fn light_point_carries_range() {
    let g = lower_src(
        r#"scene { light "lamp" (kind=point, pos=[0, 2, 0], intensity=10, range=8) }"#,
    );
    let n = find_mesh_node(&g, "lamp");
    let l = n.light.as_ref().unwrap();
    assert_eq!(l.kind, mogen_core::LightKind::Point);
    assert_eq!(l.range, Some(8.0));
    assert!((n.transform.translation - Vec3::new(0.0, 2.0, 0.0)).abs().max_element() < 1e-5);
}

#[test]
fn light_spot_converts_cone_degrees_to_radians() {
    let g = lower_src(
        r#"scene { light "spot" (kind=spot, intensity=20, range=10, inner_cone=20, outer_cone=35) }"#,
    );
    let l = find_mesh_node(&g, "spot").light.as_ref().unwrap();
    assert_eq!(l.kind, mogen_core::LightKind::Spot);
    assert!((l.inner_cone_rad - 20f32.to_radians()).abs() < 1e-5);
    assert!((l.outer_cone_rad - 35f32.to_radians()).abs() < 1e-5);
}

#[test]
fn light_dir_synthesizes_rotation_from_neg_z() {
    // dir=[0,-1,0] should rotate the node so its local -Z points down.
    let g = lower_src(
        r#"scene { light "sun" (kind=directional, dir=[0, -1, 0]) }"#,
    );
    let n = find_mesh_node(&g, "sun");
    let forward = n.transform.rotation * Vec3::NEG_Z;
    assert!(
        (forward - Vec3::new(0.0, -1.0, 0.0)).length() < 1e-5,
        "expected -Z to map to (0,-1,0), got {forward:?}"
    );
}
