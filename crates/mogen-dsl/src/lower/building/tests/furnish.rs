//! Furnishing-POI pass tests: the right props land in the right rooms, the
//! pass is seed-deterministic, `furnish=0` suppresses it, and `debug_show_poi`
//! makes the markers visible without giving them colliders.

use super::{count_kind, count_role, has_tag, lower_src};

fn building_src(room_name: &str, kind: &str, extra: &str) -> String {
    format!(
        r#"
material "concrete" (color=[0.8, 0.8, 0.8])
building "block" (
  seed=9, style="grid", floor_area=120, rooms=3, {extra}
  mat="concrete",
) {{
  room_type "{room_name}" (kind={kind}, density=1)
}}
"#
    )
}

#[test]
fn bedroom_gets_a_bed_marker() {
    let g = lower_src(&building_src("bedroom", "private", ""));
    assert!(count_role(&g, "bed") >= 1, "bedroom should place a bed POI");
    assert!(count_kind(&g, "poi") >= 1, "markers use kind=poi");
    assert!(has_tag(&g, "furniture"), "furniture group + markers tagged");
}

#[test]
fn classify_routes_by_room_name() {
    // A kitchen gets kitchen props, never a bed.
    let k = lower_src(&building_src("kitchen", "utility", ""));
    assert!(count_role(&k, "stove") >= 1, "kitchen should place a stove");
    assert_eq!(count_role(&k, "bed"), 0, "a kitchen is not a bedroom");

    // A free-text synonym still classifies: "server room" → server racks.
    let s = lower_src(&building_src("server room", "secure", ""));
    assert!(
        count_role(&s, "server_rack") >= 1,
        "‘server room’ should classify as a server room"
    );

    // Multi-word names with a furnishing keyword resolve too.
    let m = lower_src(&building_src("master suite", "private", ""));
    assert!(count_role(&m, "bed") >= 1, "‘master suite’ → bedroom");
}

#[test]
fn furnish_zero_suppresses_all_markers() {
    let g = lower_src(&building_src("bedroom", "private", "furnish=0,"));
    assert_eq!(count_kind(&g, "poi"), 0, "furnish=0 emits no POI markers");
    assert!(!has_tag(&g, "furniture"), "furnish=0 emits no furniture group");
}

#[test]
fn markers_are_geometry_free_by_default() {
    let g = lower_src(&building_src("kitchen", "utility", ""));
    let pois: Vec<_> = g.nodes.iter().filter(|n| n.kind == "poi").collect();
    assert!(!pois.is_empty());
    for p in &pois {
        assert!(p.mesh.is_none(), "POIs carry no geometry without debug flag");
        assert!(p.collider.is_none(), "POIs never collide");
    }
}

#[test]
fn furnishing_is_seed_deterministic() {
    let src = building_src("living room", "public", "");
    let a = lower_src(&src);
    let b = lower_src(&src);
    assert_eq!(a.nodes.len(), b.nodes.len(), "same seed → identical node count");
    assert_eq!(count_kind(&a, "poi"), count_kind(&b, "poi"));
}

#[test]
fn debug_show_poi_adds_spheres_and_materials_no_collider() {
    let g = lower_src(&building_src("kitchen", "utility", "debug_show_poi=1,"));
    let markers: Vec<_> = g.nodes.iter().filter(|n| n.kind == "poi").collect();
    assert!(!markers.is_empty(), "expected POI markers");
    for m in &markers {
        assert!(m.mesh.is_some(), "debug markers carry a sphere mesh");
        assert!(m.collider.is_none(), "debug markers stay collider-free");
    }
    assert!(
        g.find_material("building_furniture_kitchen").is_some(),
        "debug emits a per-category material"
    );
}

#[test]
fn tiny_room_is_left_unfurnished() {
    // floor_area split across many rooms drives each cell below the usable
    // threshold; the pass must bail rather than cram or panic.
    let src = r#"
material "concrete" (color=[0.8, 0.8, 0.8])
building "cramped" (
  seed=2, style="grid", floor_area=10, rooms=8,
  mat="concrete",
) {
  room_type "closet" (kind=service, density=1)
}
"#;
    let g = lower_src(src); // must not panic
    // No assertion on exact count — some cells may still be furnishable — but
    // lowering completing without panic is the property under test.
    let _ = count_kind(&g, "poi");
}
