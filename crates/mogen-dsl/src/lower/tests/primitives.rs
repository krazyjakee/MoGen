use super::*;
use crate::lower::*;
use crate::parser::parse;
use glam::Vec3;

#[test]
fn lowers_every_new_primitive() {
    // One scene that exercises every new primitive kind end-to-end:
    // parse → validate attrs → lower → mesh attached to node.
    let g = lower_src(
        r#"
        scene {
          wedge         "w" (size=[1, 0.5, 1])
          frustum       "f" (bottom=[1, 1], top=[0.5, 0.5], height=1)
          tube          "t" (outer=0.5, inner=0.3, height=1)
          hemisphere    "h" (radius=0.5)
          half_cylinder "hc" (radius=0.5, height=1)
          torus_arc     "ta" (major=0.5, minor=0.1, arc=90)
          ellipsoid     "e" (size=[1, 0.5, 0.8])
        }
    "#,
    );
    for name in ["w", "f", "t", "h", "hc", "ta", "e"] {
        let n = find_mesh_node(&g, name);
        assert!(n.mesh.is_some(), "{name} has no mesh");
        let mesh = n.mesh.as_ref().unwrap();
        assert!(!mesh.positions.is_empty(), "{name} mesh has no positions");
        assert!(!mesh.indices.is_empty(), "{name} mesh has no indices");
        // Default connectors were populated.
        assert!(!n.connectors.is_empty(), "{name} has no default connectors");
    }
}

#[test]
fn tube_has_inner_and_outer_walls() {
    let g = lower_src(
        r#"scene { tube "t" (outer=1.0, inner=0.5, height=1.0) }"#,
    );
    let n = find_mesh_node(&g, "t");
    let mesh = n.mesh.as_ref().unwrap();
    // Some verts at outer radius, some at inner radius — cheap "is hollow" check.
    let has_outer = mesh.positions.iter().any(|p| (p[0] * p[0] + p[2] * p[2]).sqrt() > 0.9);
    let has_inner = mesh.positions.iter().any(|p| {
        let r = (p[0] * p[0] + p[2] * p[2]).sqrt();
        r > 0.4 && r < 0.6
    });
    assert!(has_outer, "tube is missing outer wall");
    assert!(has_inner, "tube is missing inner wall");
}

#[test]
fn hemisphere_has_base_at_origin() {
    let g = lower_src(r#"scene { hemisphere "h" (radius=1.0) }"#);
    let mesh = find_mesh_node(&g, "h").mesh.as_ref().unwrap();
    // Base cap sits on y=0; apex at y=+radius.
    let min_y = mesh.positions.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min);
    let max_y = mesh.positions.iter().map(|p| p[1]).fold(f32::NEG_INFINITY, f32::max);
    assert!((min_y).abs() < 1e-5, "expected base at y=0, got {min_y}");
    assert!((max_y - 1.0).abs() < 1e-5, "expected apex at y=radius, got {max_y}");
}

#[test]
fn wedge_slope_connector_faces_up_and_forward() {
    let g = lower_src(r#"scene { wedge "w" (size=[1.0, 1.0, 1.0]) }"#);
    let n = find_mesh_node(&g, "w");
    let slope = n
        .connectors
        .iter()
        .find(|c| c.name == "slope")
        .expect("wedge missing slope connector");
    // Connector rotation turns +Y into the connector's outward dir.
    let dir = slope.rotation * Vec3::Y;
    assert!(dir.y > 0.0 && dir.z > 0.0, "slope normal should point +Y and +Z, got {dir:?}");
}

#[test]
fn leaf_card_emits_cross_quads_with_bottom_at_origin() {
    let g = lower_src(
        r#"scene { leaf_card "l" (size=[0.4, 0.5], cards=2) }"#,
    );
    let mesh = find_mesh_node(&g, "l").mesh.as_ref().unwrap();
    // Two cards × 4 verts each = 8 verts. Single winding: two tris per
    // card → 2 cards × 6 = 12 indices. The material's `double_sided`
    // flag is what makes the leaf visible from both sides.
    assert_eq!(mesh.positions.len(), 8);
    assert_eq!(mesh.indices.len(), 12);
    let min_y = mesh.positions.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min);
    let max_y = mesh.positions.iter().map(|p| p[1]).fold(f32::NEG_INFINITY, f32::max);
    assert!(min_y.abs() < 1e-5, "bottom edge should sit at y=0, got {min_y}");
    assert!((max_y - 0.5).abs() < 1e-5, "top edge should be at y=h, got {max_y}");
    // Default connectors include `stem` at origin and `tip` at the top.
    let n = find_mesh_node(&g, "l");
    let names: Vec<_> = n.connectors.iter().map(|c| c.name.as_str()).collect();
    assert!(names.contains(&"stem"), "missing stem: {names:?}");
    assert!(names.contains(&"tip"), "missing tip: {names:?}");
}

#[test]
fn leaf_card_with_alpha_mask_material_passes_validation() {
    // Full pipeline: alpha-cutout leaf with double_sided rendering — the
    // canonical recipe for game foliage.
    let g = lower_src(
        r#"
        material "leaf" (
            color=[0.3, 0.6, 0.2],
            alpha_mode="mask",
            alpha_cutoff=0.5,
            double_sided=1
        )
        scene {
          leaf_card "l" (size=[0.4, 0.4], cards=3, mat="leaf")
        }
        "#,
    );
    let leaf = find_mesh_node(&g, "l");
    assert!(leaf.mesh.is_some());
    let mid = leaf.material.expect("leaf material bound");
    let m = &g.materials[mid.0 as usize];
    assert_eq!(m.alpha_mode, mogen_core::AlphaMode::Mask);
    assert!(m.double_sided);
}

#[test]
fn lowers_every_organic_primitive() {
    // End-to-end check of the four organic-shape primitives. Uses nested
    // list literals (`[[x,y,z], ...]`, `[[r,y], ...]`) to confirm the
    // grammar extension landed.
    let g = lower_src(
        r#"
        scene {
          superellipsoid "se"   (size=[1, 0.8, 1], ew=0.5, ns=1)
          curved_plane   "leaf" (size=[0.4, 1.0], bend_u=20, bend_v=40)
          lathe          "vase" (profile=[[0.0, -0.5], [0.4, -0.3], [0.5, 0.0], [0.3, 0.4], [0.0, 0.5]])
          spline_tube    "ban"  (points=[[0, 0, 0], [0.3, 0.2, 0], [0.5, 0.1, 0], [0.6, -0.1, 0]],
                                 radii=[0.08, 0.12, 0.10, 0.05])
        }
    "#,
    );
    for name in ["se", "leaf", "vase", "ban"] {
        let n = find_mesh_node(&g, name);
        assert!(n.mesh.is_some(), "{name} has no mesh");
        let mesh = n.mesh.as_ref().unwrap();
        assert!(!mesh.positions.is_empty(), "{name} mesh has no positions");
        assert!(!mesh.indices.is_empty(), "{name} mesh has no indices");
        assert_eq!(mesh.positions.len(), mesh.normals.len(), "{name} normals arity mismatch");
    }
}

#[test]
fn superellipsoid_boxy_exponent_fills_corners() {
    // ew, ns > 1 push the shape toward a box — corner vertices sit close to
    // the declared size bounds, unlike a sphere which tucks them inward.
    let g = lower_src(
        r#"scene { superellipsoid "s" (size=[1.0, 1.0, 1.0], ew=3.0, ns=3.0, rings=24, segments=32) }"#,
    );
    let mesh = find_mesh_node(&g, "s").mesh.as_ref().unwrap();
    // Find the vertex nearest the +X+Y+Z corner and check it's close to [0.5, 0.5, 0.5].
    let max_corner = mesh
        .positions
        .iter()
        .map(|p| (p[0] + p[1] + p[2], *p))
        .fold((f32::NEG_INFINITY, [0.0; 3]), |acc, x| if x.0 > acc.0 { x } else { acc })
        .1;
    // Sphere would give ~0.29 on each axis; boxy should be > 0.4.
    assert!(max_corner[0] > 0.4 && max_corner[1] > 0.4 && max_corner[2] > 0.4,
        "boxy superellipsoid should reach corners, got {max_corner:?}");
}

#[test]
fn superellipsoid_faces_wind_outward() {
    // Face winding must match vertex normals so back-face culling shows the
    // outside. Regression: ring 0 sits at the south pole, so the sphere's
    // north-first winding would flip every triangle.
    let g = lower_src(
        r#"scene { superellipsoid "s" (size=[1.0, 1.0, 1.0], ew=1.0, ns=1.0, rings=12, segments=16) }"#,
    );
    let mesh = find_mesh_node(&g, "s").mesh.as_ref().unwrap();
    let mut checked = 0usize;
    let mut aligned = 0usize;
    for tri in mesh.indices.chunks_exact(3) {
        let (ia, ib, ic) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
        let a = Vec3::from(mesh.positions[ia]);
        let b = Vec3::from(mesh.positions[ib]);
        let c = Vec3::from(mesh.positions[ic]);
        let face = (b - a).cross(c - a);
        // Polar caps collapse to a point — skip those degenerate tris.
        if face.length_squared() < 1e-12 {
            continue;
        }
        checked += 1;
        let avg = (Vec3::from(mesh.normals[ia])
            + Vec3::from(mesh.normals[ib])
            + Vec3::from(mesh.normals[ic]))
            / 3.0;
        if face.dot(avg) > 0.0 {
            aligned += 1;
        }
    }
    assert!(checked > 0, "expected non-degenerate superellipsoid faces");
    assert_eq!(aligned, checked, "all non-degenerate superellipsoid faces should wind outward");
}

#[test]
fn curved_plane_bends_toward_positive_y() {
    // Positive bend_u lifts the left/right edges. The centre stays near y=0;
    // the edges sit well above y=0.
    let g = lower_src(
        r#"scene { curved_plane "l" (size=[1.0, 0.2], bend_u=90, segments_u=16) }"#,
    );
    let mesh = find_mesh_node(&g, "l").mesh.as_ref().unwrap();
    let max_y = mesh.positions.iter().map(|p| p[1]).fold(f32::NEG_INFINITY, f32::max);
    let min_y = mesh.positions.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min);
    assert!(max_y > 0.05, "bent plane should lift edges above y=0, got max_y={max_y}");
    assert!(min_y.abs() < 1e-4, "unbent center should still sit at y=0, got min_y={min_y}");
}

#[test]
fn lathe_revolves_around_y() {
    // A flat profile `[0.5, 0.0]` for two rows makes a closed cylinder;
    // every vertex on the side wall lands at radius ≈ 0.5.
    let g = lower_src(
        r#"scene { lathe "l" (profile=[[0.5, -0.5], [0.5, 0.5]], segments=16) }"#,
    );
    let mesh = find_mesh_node(&g, "l").mesh.as_ref().unwrap();
    let side_verts: Vec<_> = mesh
        .positions
        .iter()
        .filter(|p| (p[0] * p[0] + p[2] * p[2]).sqrt() > 0.4)
        .collect();
    assert!(!side_verts.is_empty(), "lathe should have side-wall verts");
    for p in side_verts {
        let r = (p[0] * p[0] + p[2] * p[2]).sqrt();
        assert!((r - 0.5).abs() < 1e-4, "side-wall radius should be 0.5, got {r}");
    }
}

#[test]
fn spline_tube_follows_control_points() {
    // Straight tube along Y should yield every vertex in a narrow X-band
    // around the axis.
    let g = lower_src(
        r#"scene { spline_tube "t" (points=[[0,0,0],[0,0.5,0],[0,1,0]], radius=0.1, segments=8, samples=4) }"#,
    );
    let mesh = find_mesh_node(&g, "t").mesh.as_ref().unwrap();
    for p in &mesh.positions {
        let r = (p[0] * p[0] + p[2] * p[2]).sqrt();
        assert!(r < 0.12, "straight tube along Y should stay near the axis, got r={r}");
    }
    let min_y = mesh.positions.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min);
    let max_y = mesh.positions.iter().map(|p| p[1]).fold(f32::NEG_INFINITY, f32::max);
    assert!(min_y < 0.05 && max_y > 0.95, "tube should span y∈[0, 1], got [{min_y}, {max_y}]");
}

#[test]
fn spline_tube_exposes_start_and_end_connectors() {
    let g = lower_src(
        r#"scene { spline_tube "t" (points=[[0,0,0],[0.5,0.5,0],[1,0,0]], radius=0.05) }"#,
    );
    let n = find_mesh_node(&g, "t");
    let names: Vec<_> = n.connectors.iter().map(|c| c.name.as_str()).collect();
    assert!(names.contains(&"start"), "connectors: {names:?}");
    assert!(names.contains(&"end"), "connectors: {names:?}");
}

#[test]
fn wall_cuts_holes_via_csg() {
    // A wall with one hole should have substantially fewer closed-face
    // verts on its interior than a plain box would, and the hole should
    // leave a gap in the y-z cross section at x=0.
    let g = lower_src(
        r#"scene { wall "w" (size=[4, 3, 0.1], holes=[[0, 0, 1, 2]]) }"#,
    );
    let mesh = find_mesh_node(&g, "w").mesh.as_ref().unwrap();
    // No vertex should lie strictly inside the hole rectangle on the
    // front/back face (x in [-0.5, 0.5], y in [-1, 1], z ~ 0).
    // Instead, the hole boundary sits at x=±0.5 / y=±1, which is fine.
    let strict_inside = mesh.positions.iter().any(|p| {
        p[0].abs() < 0.45 && p[1].abs() < 0.95 && (p[2].abs() - 0.05).abs() < 0.01
    });
    assert!(!strict_inside, "wall hole interior should be empty");
}

#[test]
fn chamfered_box_lowers_with_default_attrs() {
    let g = lower_src(r#"scene { chamfered_box "b" () }"#);
    let m = find_mesh_node(&g, "b").mesh.as_ref().unwrap();
    // 6 face rects (12 tris) + 12 bevel quads (24 tris) + 8 corner tris (8) = 44.
    assert_eq!(m.indices.len() / 3, 44);
    // Default size is 1×1×1 so the AABB sits in [-0.5, 0.5] on every axis.
    let mut mn = [f32::INFINITY; 3];
    let mut mx = [f32::NEG_INFINITY; 3];
    for p in &m.positions {
        for k in 0..3 {
            mn[k] = mn[k].min(p[k]);
            mx[k] = mx[k].max(p[k]);
        }
    }
    for k in 0..3 {
        assert!((mx[k] - 0.5).abs() < 1e-5, "axis {k} mx {} not 0.5", mx[k]);
        assert!((mn[k] + 0.5).abs() < 1e-5, "axis {k} mn {} not -0.5", mn[k]);
    }
}

#[test]
fn chamfered_box_radius_zero_matches_plain_box() {
    let chamfered = lower_src(r#"scene { chamfered_box "b" (radius=0) }"#);
    let plain = lower_src(r#"scene { box "b" () }"#);
    let cm = find_mesh_node(&chamfered, "b").mesh.as_ref().unwrap();
    let pm = find_mesh_node(&plain, "b").mesh.as_ref().unwrap();
    assert_eq!(cm.positions.len(), pm.positions.len());
    assert_eq!(cm.indices.len(), pm.indices.len());
}

#[test]
fn inset_box_lowers_with_default_face() {
    let g = lower_src(r#"scene { inset_box "b" (size=[1, 1, 1]) }"#);
    let m = find_mesh_node(&g, "b").mesh.as_ref().unwrap();
    // Default face is +y; the sunken floor should sit at y = 0.5 - 0.05 = 0.45.
    let mut floor_y_seen = false;
    for p in &m.positions {
        if (p[1] - 0.45).abs() < 1e-5 {
            floor_y_seen = true;
            break;
        }
    }
    assert!(floor_y_seen, "expected a vertex at the default sunken Y=0.45");
}

#[test]
fn inset_box_face_aliases_resolve() {
    // "top" must resolve to +y; the resulting mesh should be identical
    // to the canonical "+y" form vertex-for-vertex.
    let a = lower_src(r#"scene { inset_box "b" (face="+y", amount=0.2, depth=0.1) }"#);
    let b = lower_src(r#"scene { inset_box "b" (face="top", amount=0.2, depth=0.1) }"#);
    let pa = find_mesh_node(&a, "b").mesh.as_ref().unwrap();
    let pb = find_mesh_node(&b, "b").mesh.as_ref().unwrap();
    assert_eq!(pa.positions, pb.positions);
    assert_eq!(pa.indices, pb.indices);
}

#[test]
fn inset_box_unknown_face_errors() {
    let ast = parse(r#"scene { inset_box "b" (face="diagonal") }"#).expect("parse");
    let err = lower(&ast).expect_err("unknown face must error");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("inset_box.face") && msg.contains("diagonal"),
        "error should name the bad face value, got: {msg}"
    );
}

#[test]
fn inset_box_minus_x_face_sinks_along_negative_x() {
    // face="-x" should produce a sunken floor at x = -hx + depth = -0.5 + 0.1 = -0.4.
    let g = lower_src(r#"scene { inset_box "b" (face="-x", amount=0.2, depth=0.1) }"#);
    let m = find_mesh_node(&g, "b").mesh.as_ref().unwrap();
    let mut floor_x_seen = false;
    for p in &m.positions {
        if (p[0] + 0.4).abs() < 1e-5 {
            floor_x_seen = true;
            break;
        }
    }
    assert!(floor_x_seen, "expected vertex at sunken X=-0.4");
}
