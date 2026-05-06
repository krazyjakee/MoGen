use super::*;
use crate::parser::parse;
use glam::Vec3;

fn lower_src(src: &str) -> SceneGraph {
    let ast = parse(src).expect("parse");
    lower(&ast).expect("lower")
}

fn find_mesh_node<'a>(g: &'a SceneGraph, name: &str) -> &'a mogen_core::SceneNode {
    g.nodes
        .iter()
        .find(|n| n.name == name)
        .unwrap_or_else(|| panic!("no node named {name}"))
}

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
fn branch_expands_to_seg_and_leaf_nodes() {
    // depth=2, splits=2 → 1 + 2 + 4 = 7 segments and 4 leaves.
    let g = lower_src(
        r#"
        material "bark" (color=[0.4, 0.25, 0.15])
        material "leaf" (color=[0.3, 0.6, 0.2], alpha_mode="mask", double_sided=1)
        scene {
          branch "tree" (
            length=1.0, radius=0.1, depth=2, splits=2,
            length_falloff=0.7, radius_falloff=0.6,
            branch_angle=30, jitter=0.0,
            leaves=1, leaf_size=0.3, leaf_mat="leaf",
            mat="bark"
          )
        }
        "#,
    );
    let segs = g.nodes.iter().filter(|n| n.kind == "branch_seg").count();
    let leaves = g.nodes.iter().filter(|n| n.kind == "leaf_card").count();
    assert_eq!(segs, 7, "expected 7 branch segments at depth=2 splits=2, got {segs}");
    assert_eq!(leaves, 4, "expected 4 leaf cards at depth=0 tips, got {leaves}");
    // Bark inherited on segments; explicit leaf material on leaves.
    let bark = g.find_material("bark").expect("bark");
    let leaf = g.find_material("leaf").expect("leaf");
    for n in &g.nodes {
        if n.kind == "branch_seg" {
            assert_eq!(n.material, Some(bark), "segment {} should inherit bark", n.name);
        } else if n.kind == "leaf_card" {
            assert_eq!(n.material, Some(leaf), "leaf {} should bind leaf material", n.name);
        }
    }
}

#[test]
fn branch_is_deterministic_for_a_given_seed() {
    let a = lower_src(
        r#"scene { branch "t" (length=1, radius=0.1, depth=3, splits=3, jitter=0.5, seed=7) }"#,
    );
    let b = lower_src(
        r#"scene { branch "t" (length=1, radius=0.1, depth=3, splits=3, jitter=0.5, seed=7) }"#,
    );
    assert_eq!(a.nodes.len(), b.nodes.len());
    for (na, nb) in a.nodes.iter().zip(b.nodes.iter()) {
        assert_eq!(na.kind, nb.kind, "kind diverges for seeded branch");
        assert_eq!(na.transform.translation, nb.transform.translation);
    }
}

#[test]
fn branch_seed_changes_geometry() {
    let a = lower_src(
        r#"scene { branch "t" (length=1, radius=0.1, depth=3, splits=3, jitter=0.5, seed=1) }"#,
    );
    let b = lower_src(
        r#"scene { branch "t" (length=1, radius=0.1, depth=3, splits=3, jitter=0.5, seed=2) }"#,
    );
    // Same node count, but at least one transform should differ.
    assert_eq!(a.nodes.len(), b.nodes.len());
    let any_diff = a
        .nodes
        .iter()
        .zip(b.nodes.iter())
        .any(|(x, y)| x.transform.translation != y.transform.translation);
    assert!(any_diff, "different seeds should produce different forks");
}

#[test]
fn branch_no_leaves_when_disabled() {
    let g = lower_src(
        r#"scene { branch "t" (length=1, radius=0.1, depth=2, splits=2, leaves=0) }"#,
    );
    let leaves = g.nodes.iter().filter(|n| n.kind == "leaf_card").count();
    assert_eq!(leaves, 0, "leaves=0 should suppress leaf cards");
}

#[test]
fn branch_leaves_align_to_branch_tip() {
    // With branch_angle=0 and bend=0 every segment grows along +Y, so a
    // depth=1 split should keep its single leaf pointing up — the leaf
    // card's local +Y should resolve to world +Y after composition.
    let g = lower_src(
        r#"scene {
            branch "t" (
                length=1, radius=0.1, depth=1, splits=1,
                branch_angle=0, bend=0, tropism=0, jitter=0,
                leaves=1, leaf_size=0.2
            )
        }"#,
    );
    // Find the leaf node and walk its world transform.
    let world = g.world_transforms();
    let leaf_idx = g
        .nodes
        .iter()
        .position(|n| n.kind == "leaf_card")
        .expect("leaf card present");
    let m = world[leaf_idx];
    let up = m.transform_vector3(Vec3::Y).normalize();
    assert!(up.y > 0.95, "leaf +Y should align with world +Y, got {up:?}");
}

#[test]
fn branch_segments_are_marked_non_editable() {
    let g = lower_src(
        r#"scene { branch "t" (length=1, radius=0.1, depth=1, splits=2) }"#,
    );
    for n in &g.nodes {
        if matches!(n.kind.as_str(), "branch_seg" | "leaf_card") {
            assert!(!n.editable, "{} should be non-editable", n.name);
        }
    }
    // Wrapper itself stays editable so the user can tweak `branch` attrs.
    let wrapper = g
        .nodes
        .iter()
        .find(|n| n.kind == "branch")
        .expect("wrapper present");
    assert!(wrapper.editable, "branch wrapper should remain editable");
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
fn csg_inherits_first_operand_material_when_unset() {
    let g = lower_src(
        r#"
        material "brick" (color=[0.7, 0.3, 0.2])
        material "soot"  (color=[0.1, 0.1, 0.1])
        scene {
          difference "dome" {
            hemisphere "outer" (radius=0.6, mat="brick")
            hemisphere "inner" (radius=0.5, mat="soot")
          }
        }
        "#,
    );
    let dome = find_mesh_node(&g, "dome");
    let brick = g.find_material("brick").expect("brick material");
    assert_eq!(dome.material, Some(brick),
        "CSG should inherit first operand's material when own mat is absent");
}

#[test]
fn csg_own_material_wins_over_operand() {
    let g = lower_src(
        r#"
        material "brick" (color=[0.7, 0.3, 0.2])
        material "stone" (color=[0.5, 0.5, 0.5])
        scene {
          difference "dome" (mat="stone") {
            hemisphere "outer" (radius=0.6, mat="brick")
            hemisphere "inner" (radius=0.5)
          }
        }
        "#,
    );
    let dome = find_mesh_node(&g, "dome");
    let stone = g.find_material("stone").expect("stone material");
    assert_eq!(dome.material, Some(stone),
        "explicit mat on CSG node must win over first-operand inheritance");
}

#[test]
fn material_inherits_from_parent_to_unmarked_children() {
    // Regression: setting `mat` on a `solid` (or any group) must apply to
    // children that don't override it. Without this the merge pass groups
    // them under None and the merged leaf renders untextured.
    let g = lower_src(
        r#"
        material "wood" (color=[0.45, 0.28, 0.15])
        scene {
          solid "crate" (mat="wood") {
            box "floor" (size=1)
            group "walls" {
              box "left"  (pos=[-0.5, 0, 0], size=[0.1, 1, 1])
              box "right" (pos=[ 0.5, 0, 0], size=[0.1, 1, 1])
            }
          }
        }
        "#,
    );
    let wood = g.find_material("wood").expect("wood material");
    for name in ["crate", "floor", "walls", "left", "right"] {
        assert_eq!(
            find_mesh_node(&g, name).material,
            Some(wood),
            "{name} should inherit wood from parent solid"
        );
    }
}

#[test]
fn inherited_material_drives_primitive_uv_mode() {
    // Regression: uv_mode is baked into the mesh at primitive generation.
    // When a child inherits its material from a parent, the primitive must
    // see the inherited material's uv_mode — otherwise size=2 boxes render
    // with tile-mode UVs [0, 2] when the author asked for fit-mode [0, 1].
    let g = lower_src(
        r#"
        material "sign" (color=[1, 1, 1], uv_mode="fit")
        scene {
          group "billboard" (mat="sign") {
            box "face" (size=2)
          }
        }
        "#,
    );
    let mesh = find_mesh_node(&g, "face").mesh.as_ref().unwrap();
    let max_uv = mesh.uvs.iter().flat_map(|u| u.iter().copied()).fold(f32::NEG_INFINITY, f32::max);
    assert!(
        max_uv <= 1.0 + 1e-5,
        "expected fit-mode UVs capped at 1.0, got max={max_uv} — uv_mode didn't inherit"
    );
}

#[test]
fn child_material_overrides_inherited_parent_material() {
    let g = lower_src(
        r#"
        material "wood"  (color=[0.45, 0.28, 0.15])
        material "metal" (color=[0.7, 0.7, 0.7])
        scene {
          group "g" (mat="wood") {
            box "a" (size=1)
            box "b" (size=1, mat="metal")
          }
        }
        "#,
    );
    let wood = g.find_material("wood").expect("wood material");
    let metal = g.find_material("metal").expect("metal material");
    assert_eq!(find_mesh_node(&g, "a").material, Some(wood));
    assert_eq!(find_mesh_node(&g, "b").material, Some(metal));
}

#[test]
fn solid_lowers_as_tagged_group() {
    let g = lower_src(
        r#"
        material "stone" (color=[0.8, 0.8, 0.8])
        scene {
          solid "shell" (mat="stone") {
            box "a" (size=1)
            box "b" (pos=[0.5, 0, 0], size=1)
          }
        }
        "#,
    );
    let shell = find_mesh_node(&g, "shell");
    assert!(shell.mesh.is_none(), "solid itself has no mesh");
    assert!(shell.tags.iter().any(|t| t == "solid"));
    assert!(!shell.tags.iter().any(|t| t == "cleanup=coplanar"));
    assert_eq!(shell.children.len(), 2, "children are preserved");
}

#[test]
fn solid_records_cleanup_coplanar_tag() {
    let g = lower_src(
        r#"
        material "stone" (color=[0.8, 0.8, 0.8])
        scene {
          solid "shell" (mat="stone", cleanup="coplanar") {
            box "a" (size=1)
          }
        }
        "#,
    );
    let shell = find_mesh_node(&g, "shell");
    assert!(shell.tags.iter().any(|t| t == "solid"));
    assert!(shell.tags.iter().any(|t| t == "cleanup=coplanar"));
}

fn mesh_aabb(g: &SceneGraph, name: &str) -> (Vec3, Vec3) {
    let mesh = find_mesh_node(g, name).mesh.as_ref().unwrap();
    let min = mesh.positions.iter().fold(Vec3::splat(f32::INFINITY), |a, p| {
        a.min(Vec3::from_array(*p))
    });
    let max = mesh.positions.iter().fold(Vec3::splat(f32::NEG_INFINITY), |a, p| {
        a.max(Vec3::from_array(*p))
    });
    (min, max)
}

#[test]
fn scalar_size_expands_to_cube() {
    let g = lower_src(r#"scene { box "b" (size=2) }"#);
    let (min, max) = mesh_aabb(&g, "b");
    assert!((min - Vec3::splat(-1.0)).abs().max_element() < 1e-5);
    assert!((max - Vec3::splat(1.0)).abs().max_element() < 1e-5);
}

#[test]
fn whd_shortcuts_populate_size() {
    let g = lower_src(r#"scene { box "b" (w=2, h=4, d=6) }"#);
    let (min, max) = mesh_aabb(&g, "b");
    assert!((max.x - min.x - 2.0).abs() < 1e-5);
    assert!((max.y - min.y - 4.0).abs() < 1e-5);
    assert!((max.z - min.z - 6.0).abs() < 1e-5);
}

#[test]
fn whd_overrides_individual_size_components() {
    let g = lower_src(r#"scene { box "b" (size=[1, 1, 1], h=3) }"#);
    let (min, max) = mesh_aabb(&g, "b");
    assert!((max.x - min.x - 1.0).abs() < 1e-5);
    assert!((max.y - min.y - 3.0).abs() < 1e-5);
    assert!((max.z - min.z - 1.0).abs() < 1e-5);
}

#[test]
fn xyz_shortcuts_set_translation() {
    let g = lower_src(r#"scene { box "b" (y=1.5, size=1) }"#);
    let t = find_mesh_node(&g, "b").transform.translation;
    assert!((t - Vec3::new(0.0, 1.5, 0.0)).abs().max_element() < 1e-5);
}

#[test]
fn rxyz_shortcuts_set_rotation() {
    let g = lower_src(r#"scene { box "b" (ry=90, size=1) }"#);
    let q = find_mesh_node(&g, "b").transform.rotation;
    // 90° around Y rotates +X to -Z.
    let v = q * Vec3::X;
    assert!((v - Vec3::new(0.0, 0.0, -1.0)).abs().max_element() < 1e-4,
        "got {v:?}");
}

#[test]
fn anchor_bottom_places_mesh_above_origin() {
    let g = lower_src(r#"scene { box "b" (size=2, anchor=bottom) }"#);
    let (min, max) = mesh_aabb(&g, "b");
    assert!(min.y.abs() < 1e-5, "expected bottom on y=0, got {min:?}");
    assert!((max.y - 2.0).abs() < 1e-5);
}

#[test]
fn anchor_corner_combines_axes() {
    let g = lower_src(r#"scene { box "b" (size=2, anchor=bottom_left_front) }"#);
    let (min, _) = mesh_aabb(&g, "b");
    // All three mins should sit at 0.
    assert!(min.x.abs() < 1e-5 && min.y.abs() < 1e-5 && min.z.abs() < 1e-5,
        "expected all-mins at 0, got {min:?}");
}

#[test]
fn anchor_shifts_default_connectors() {
    // Anchor=bottom puts the box's bottom face on y=0; the `bottom`
    // default connector must follow — otherwise attach math breaks.
    let g = lower_src(r#"scene { box "b" (size=2, anchor=bottom) }"#);
    let n = find_mesh_node(&g, "b");
    let bottom = n.connectors.iter().find(|c| c.name == "bottom")
        .expect("missing bottom connector");
    assert!(bottom.pos.y.abs() < 1e-5,
        "bottom connector should be at y=0, got {:?}", bottom.pos);
    let top = n.connectors.iter().find(|c| c.name == "top")
        .expect("missing top connector");
    assert!((top.pos.y - 2.0).abs() < 1e-5,
        "top connector should be at y=2, got {:?}", top.pos);
}

#[test]
fn slab_defaults_to_bottom_anchor() {
    let g = lower_src(r#"scene { slab "floor" (size=[2, 0.2, 2]) }"#);
    let (min, max) = mesh_aabb(&g, "floor");
    assert!(min.y.abs() < 1e-5, "slab should sit on y=0");
    assert!((max.y - 0.2).abs() < 1e-5);
}

#[test]
fn panel_defaults_to_back_anchor() {
    let g = lower_src(r#"scene { panel "p" (size=[2, 2, 0.1]) }"#);
    let (min, max) = mesh_aabb(&g, "p");
    // Back face is the +Z face. Anchor=back means the +Z face lands at z=0.
    assert!(max.z.abs() < 1e-5, "panel back face should be at z=0, got max.z={}", max.z);
    assert!((min.z + 0.1).abs() < 1e-5);
}

#[test]
fn from_to_derives_size_and_pos() {
    let g = lower_src(r#"scene { box "b" (from=[-1, 0, -1], to=[1, 2, 1]) }"#);
    let t = find_mesh_node(&g, "b").transform.translation;
    assert!((t - Vec3::new(0.0, 1.0, 0.0)).abs().max_element() < 1e-5);
    let (min, max) = mesh_aabb(&g, "b");
    assert!((max - min - Vec3::new(2.0, 2.0, 2.0)).abs().max_element() < 1e-5);
}

#[test]
fn stack_y_packs_children_bottom_up() {
    let g = lower_src(
        r#"
        scene {
          stack "tower" (axis=y) {
            box "a" (size=[1, 1, 1])
            box "b" (size=[1, 2, 1])
            box "c" (size=[1, 0.5, 1])
          }
        }
        "#,
    );
    let ay = find_mesh_node(&g, "a").transform.translation.y;
    let by = find_mesh_node(&g, "b").transform.translation.y;
    let cy = find_mesh_node(&g, "c").transform.translation.y;
    // Each box's *center* sits at cumulative_base + half_height.
    // a: 0 + 0.5 = 0.5; b: 1 + 1 = 2.0; c: 3 + 0.25 = 3.25.
    assert!((ay - 0.5).abs() < 1e-4, "got a.y={ay}");
    assert!((by - 2.0).abs() < 1e-4, "got b.y={by}");
    assert!((cy - 3.25).abs() < 1e-4, "got c.y={cy}");
}

#[test]
fn stack_gap_inserts_space_between_children() {
    let g = lower_src(
        r#"
        scene {
          stack "s" (axis=y, gap=0.5) {
            box "a" (size=[1, 1, 1])
            box "b" (size=[1, 1, 1])
          }
        }
        "#,
    );
    let ay = find_mesh_node(&g, "a").transform.translation.y;
    let by = find_mesh_node(&g, "b").transform.translation.y;
    // a center at 0.5; gap of 0.5 → b center at 1 + 0.5 + 0.5 = 2.0.
    assert!((by - ay - 1.5).abs() < 1e-4, "gap not applied: a={ay} b={by}");
}

#[test]
fn grid_replicates_children() {
    let g = lower_src(
        r#"
        scene {
          grid "tiles" (count=[3, 1, 2], step=[1, 0, 1]) {
            box "t" (size=[0.9, 0.1, 0.9])
          }
        }
        "#,
    );
    // Expect 3*1*2 = 6 instance wrappers, each with a nested box.
    let t_count = g.nodes.iter().filter(|n| n.name == "t").count();
    assert_eq!(t_count, 6, "grid should produce 6 tiles, got {t_count}");
}

#[test]
fn grid_attach_applies_uniformly_per_instance() {
    // Regression: attach inside a grid body must not leak to the global
    // resolve_attaches pass — otherwise the first instance gets attach
    // applied twice (once globally, once per-instance) and ends up at
    // 2× the offset of the others.
    let g = lower_src(
        r#"
        scene {
          grid "row" (count=[4, 1, 1], step=[0.5, 0, 0]) {
            sphere "body" (radius=0.1)
            cylinder "cap" (radius=0.05, height=0.02)
            attach (parent="body", child="cap", socket="top", plug="bottom")
          }
        }
        "#,
    );
    let cap_ys: Vec<f32> = g
        .nodes
        .iter()
        .filter(|n| n.name == "cap")
        .map(|n| n.transform.translation.y)
        .collect();
    assert_eq!(cap_ys.len(), 4, "expected 4 cap instances, got {}", cap_ys.len());
    let first = cap_ys[0];
    for (i, y) in cap_ys.iter().enumerate() {
        assert!(
            (y - first).abs() < 1e-5,
            "cap[{i}] y={y} differs from cap[0] y={first} — attach applied unevenly across grid instances"
        );
    }
}

#[test]
fn relative_placement_above_snaps_flush() {
    let g = lower_src(
        r#"
        scene {
          group "world" {
            box "base" (size=[2, 1, 2])
            box "hat"  (size=[1, 1, 1], above="base")
          }
        }
        "#,
    );
    // base center y=0, top y=0.5. hat bottom flush → center at y=1.0.
    let hat_y = find_mesh_node(&g, "hat").transform.translation.y;
    assert!((hat_y - 1.0).abs() < 1e-4, "hat should be at y=1.0, got {hat_y}");
}

#[test]
fn relative_placement_honors_gap() {
    let g = lower_src(
        r#"
        scene {
          group "world" {
            box "base" (size=[2, 1, 2])
            box "hat"  (size=[1, 1, 1], above="base", gap=0.25)
          }
        }
        "#,
    );
    let hat_y = find_mesh_node(&g, "hat").transform.translation.y;
    assert!((hat_y - 1.25).abs() < 1e-4, "hat should be at y=1.25, got {hat_y}");
}

#[test]
fn explicit_pos_axis_wins_over_relative_placement() {
    // Explicit `pos` along the placement axis must survive — without this,
    // `behind` silently overwrites `pos.z`, which gave the imported
    // bed-with-headboard a different headboard position than the
    // standalone bed (top-level siblings skipped relative placement
    // entirely, so `pos` happened to win there).
    let g = lower_src(
        r#"
        scene {
          group "world" {
            box "base" (size=[2, 1, 2])
            box "back" (size=[2, 1, 0.1], behind="base", pos=[0, 0, 0.75])
          }
        }
        "#,
    );
    let z = find_mesh_node(&g, "back").transform.translation.z;
    assert!((z - 0.75).abs() < 1e-4, "explicit pos.z should win, got {z}");
}

#[test]
fn relative_placement_still_fires_when_pos_axis_is_zero() {
    // Pos on a perpendicular axis must not block the snap.
    let g = lower_src(
        r#"
        scene {
          group "world" {
            box "base" (size=[2, 1, 2])
            box "hat"  (size=[1, 1, 1], above="base", pos=[0.25, 0, 0])
          }
        }
        "#,
    );
    let t = find_mesh_node(&g, "hat").transform.translation;
    // Snap on Y still fires: hat at y=1.0 (base top + hat half-height).
    assert!((t.y - 1.0).abs() < 1e-4, "snap on Y should still fire, got {t:?}");
    // Pos.x preserved.
    assert!((t.x - 0.25).abs() < 1e-4, "pos.x should be preserved, got {t:?}");
}

#[test]
fn lod_scale_halves_default_segment_count() {
    let baseline = lower_src(r#"scene { sphere "s" (radius=0.5) }"#);
    let scaled = lower_src(r#"lod_scale (value=0.5) scene { sphere "s" (radius=0.5) }"#);
    let base_verts = find_mesh_node(&baseline, "s").mesh.as_ref().unwrap().positions.len();
    let scaled_verts = find_mesh_node(&scaled, "s").mesh.as_ref().unwrap().positions.len();
    assert!(
        scaled_verts < base_verts,
        "lod_scale=0.5 should reduce sphere vert count (base={base_verts}, scaled={scaled_verts})"
    );
}

#[test]
fn lod_scale_doubles_default_segment_count() {
    let baseline = lower_src(r#"scene { cylinder "c" (radius=0.5, height=1) }"#);
    let scaled = lower_src(
        r#"lod_scale (value=2) scene { cylinder "c" (radius=0.5, height=1) }"#,
    );
    let base_verts = find_mesh_node(&baseline, "c").mesh.as_ref().unwrap().positions.len();
    let scaled_verts = find_mesh_node(&scaled, "c").mesh.as_ref().unwrap().positions.len();
    assert!(
        scaled_verts > base_verts,
        "lod_scale=2 should increase cylinder vert count (base={base_verts}, scaled={scaled_verts})"
    );
}

#[test]
fn lod_scale_does_not_override_explicit_segments() {
    // Explicit per-primitive value wins over the global multiplier.
    let baseline = lower_src(r#"scene { sphere "s" (radius=0.5, rings=16, segments=24) }"#);
    let scaled = lower_src(
        r#"lod_scale (value=0.25) scene { sphere "s" (radius=0.5, rings=16, segments=24) }"#,
    );
    let base_verts = find_mesh_node(&baseline, "s").mesh.as_ref().unwrap().positions.len();
    let scaled_verts = find_mesh_node(&scaled, "s").mesh.as_ref().unwrap().positions.len();
    assert_eq!(
        base_verts, scaled_verts,
        "explicit segments=24/rings=16 must ignore lod_scale"
    );
}

#[test]
fn lod_scale_steps_icosphere_subdivisions() {
    // Icosphere triangle count is 20 * 4^subdivisions. Default subdivisions=2
    // → 320 tris. lod_scale=2 → subdivisions=3 → 1280 tris. lod_scale=0.5 →
    // subdivisions=1 → 80 tris.
    let base = lower_src(r#"scene { icosphere "i" (radius=0.5) }"#);
    let up = lower_src(r#"lod_scale (value=2) scene { icosphere "i" (radius=0.5) }"#);
    let down = lower_src(r#"lod_scale (value=0.5) scene { icosphere "i" (radius=0.5) }"#);
    let tris = |g: &SceneGraph| find_mesh_node(g, "i").mesh.as_ref().unwrap().indices.len() / 3;
    assert_eq!(tris(&base), 320);
    assert_eq!(tris(&up), 1280);
    assert_eq!(tris(&down), 80);
}

#[test]
fn lod_scale_default_keeps_existing_vertex_counts() {
    // No `lod_scale` directive should leave every default mesh untouched —
    // a regression here would silently change every existing .mog's output.
    let g = lower_src(r#"scene { sphere "s" (radius=0.5) cylinder "c" (radius=0.5, height=1) }"#);
    let s_verts = find_mesh_node(&g, "s").mesh.as_ref().unwrap().positions.len();
    let c_verts = find_mesh_node(&g, "c").mesh.as_ref().unwrap().positions.len();
    // Sphere default rings=16, segments=24 → 17 * 25 = 425 verts (one extra
    // ring + one extra segment for the seam); cylinder default segments=24
    // → 2 * (24 + 1) side verts + 2 * (24 + 1) cap-fan verts + 2 cap centres
    // = 102 verts. These exact counts depend on the mesh builder — the
    // test asserts them so a future LOD-scale change doesn't drift defaults.
    assert_eq!(s_verts, 425);
    assert_eq!(c_verts, 102);
}

#[test]
fn per_node_lod_doubles_segment_count_on_marked_subtree() {
    // `lod=2.0` on a single primitive doubles its default segment count
    // (matches the behaviour of `lod_scale (value=2)` but scoped to that
    // subtree only — see lod.rs::LodMultiplierGuard).
    let baseline = lower_src(r#"scene { cylinder "c" (radius=0.5, height=1) }"#);
    let scaled = lower_src(r#"scene { cylinder "c" (radius=0.5, height=1, lod=2) }"#);
    let base_verts = find_mesh_node(&baseline, "c").mesh.as_ref().unwrap().positions.len();
    let scaled_verts = find_mesh_node(&scaled, "c").mesh.as_ref().unwrap().positions.len();
    assert!(
        scaled_verts > base_verts,
        "per-node lod=2 should increase cylinder vert count (base={base_verts}, scaled={scaled_verts})"
    );
}

#[test]
fn per_node_lod_does_not_leak_to_siblings() {
    // The multiplier guard is RAII-scoped to the marked subtree. A `lod=2`
    // group must not boost a sibling that lives outside it.
    let g = lower_src(
        r#"scene {
            group "hi" (lod=2) { sphere "s" (radius=0.5) }
            sphere "lo" (radius=0.5)
        }"#,
    );
    let hi = find_mesh_node(&g, "s").mesh.as_ref().unwrap().positions.len();
    let lo = find_mesh_node(&g, "lo").mesh.as_ref().unwrap().positions.len();
    let baseline = lower_src(r#"scene { sphere "b" (radius=0.5) }"#);
    let base = find_mesh_node(&baseline, "b").mesh.as_ref().unwrap().positions.len();
    assert!(hi > base, "lod=2 group should boost child sphere (hi={hi}, base={base})");
    assert_eq!(lo, base, "sibling outside the lod=2 group must use baseline detail");
}

#[test]
fn per_node_lod_compounds_with_global_lod_scale() {
    // `lod=2.0` on top of `lod_scale (value=0.5)` cancels out — effective
    // multiplier is 1.0, so the marked subtree returns to the default
    // vertex count even though the file's global setting is 0.5.
    let baseline = lower_src(r#"scene { sphere "s" (radius=0.5) }"#);
    let compound = lower_src(
        r#"lod_scale (value=0.5) scene { sphere "s" (radius=0.5, lod=2) }"#,
    );
    let base_verts = find_mesh_node(&baseline, "s").mesh.as_ref().unwrap().positions.len();
    let compound_verts = find_mesh_node(&compound, "s").mesh.as_ref().unwrap().positions.len();
    assert_eq!(
        base_verts, compound_verts,
        "lod=2 should cancel a global lod_scale=0.5 (base={base_verts}, compound={compound_verts})"
    );
}

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
    let ast = crate::parser::parse(src).unwrap();
    assert!(lower(&ast).is_err(), "loft must reject mismatched section lengths");
}
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
fn deform_noise_changes_mesh_positions() {
    // Same primitive built once plain and once with noise+seed should differ.
    let plain = lower_src(r#"scene { sphere "s" (radius=0.5) }"#);
    let noisy = lower_src(r#"scene { sphere "s" (radius=0.5, noise=0.4, seed=7) }"#);
    let p = find_mesh_node(&plain, "s").mesh.as_ref().unwrap();
    let n = find_mesh_node(&noisy, "s").mesh.as_ref().unwrap();
    // Auto-bumped tessellation: noisy mesh should have more verts than plain.
    assert!(
        n.positions.len() > p.positions.len(),
        "expected noise to bump tessellation: plain={}, noisy={}",
        p.positions.len(),
        n.positions.len()
    );
    // Per-vertex positions necessarily differ from the plain unit sphere.
    let plain_radii: f32 = p.positions.iter()
        .map(|q| (q[0].powi(2) + q[1].powi(2) + q[2].powi(2)).sqrt())
        .sum::<f32>() / p.positions.len() as f32;
    let noisy_radii: f32 = n.positions.iter()
        .map(|q| (q[0].powi(2) + q[1].powi(2) + q[2].powi(2)).sqrt())
        .sum::<f32>() / n.positions.len() as f32;
    // Plain sphere radii average ~0.5; noise perturbs them but the mean stays
    // near 0.5 (zero-mean random shift). What we want is that the per-vertex
    // STD is non-trivial.
    let noisy_std: f32 = (n.positions.iter()
        .map(|q| {
            let r = (q[0].powi(2) + q[1].powi(2) + q[2].powi(2)).sqrt();
            (r - noisy_radii).powi(2)
        })
        .sum::<f32>() / n.positions.len() as f32).sqrt();
    let plain_std: f32 = (p.positions.iter()
        .map(|q| {
            let r = (q[0].powi(2) + q[1].powi(2) + q[2].powi(2)).sqrt();
            (r - plain_radii).powi(2)
        })
        .sum::<f32>() / p.positions.len() as f32).sqrt();
    assert!(
        noisy_std > plain_std + 0.001,
        "expected noise to widen radius distribution: plain_std={plain_std}, noisy_std={noisy_std}"
    );
}

#[test]
fn deform_seed_determinism() {
    // Same source compiled twice should produce byte-identical positions.
    let a = lower_src(r#"scene { box "b" (size=[1,1,1], noise=0.3, seed=42) }"#);
    let b = lower_src(r#"scene { box "b" (size=[1,1,1], noise=0.3, seed=42) }"#);
    let pa = find_mesh_node(&a, "b").mesh.as_ref().unwrap();
    let pb = find_mesh_node(&b, "b").mesh.as_ref().unwrap();
    assert_eq!(pa.positions, pb.positions);
}

#[test]
fn deform_taper_shrinks_top_of_cylinder() {
    let g = lower_src(
        r#"scene { cylinder "c" (radius=0.5, height=1.0, taper=0.5) }"#,
    );
    let mesh = find_mesh_node(&g, "c").mesh.as_ref().unwrap();
    let mut top_max = 0.0_f32;
    let mut bot_max = 0.0_f32;
    for p in &mesh.positions {
        let r = (p[0] * p[0] + p[2] * p[2]).sqrt();
        if p[1] > 0.4 { top_max = top_max.max(r); }
        if p[1] < -0.4 { bot_max = bot_max.max(r); }
    }
    assert!((top_max - 0.25).abs() < 1e-3, "top radius should be ~0.25, got {top_max}");
    assert!((bot_max - 0.5).abs() < 1e-3, "bottom radius should be ~0.5, got {bot_max}");
}

#[test]
fn mirror_bakes_reflection_into_subtree_so_chain_stays_positive_det() {
    // Regression: a `mirror (axis=x)` used to leave its second instance with
    // a `scale=(-1,1,1)` on the wrapper, which gave it a negative-determinant
    // world transform. Renderers that don't reverse front-face winding for
    // negative-det chains (and most glTF importers in practice) drew the
    // mirrored copy backface-culled — the `gaming_chair.mog` armpad bug.
    let g = lower_src(
        r#"
        scene {
          mirror "pair" (axis=x) {
            box "leaf" (pos=[0.5, 0.0, 0.0], size=[0.2, 0.2, 0.2])
          }
        }
        "#,
    );

    // Every node's local scale must be positive after the bake.
    for n in &g.nodes {
        let s = n.transform.scale;
        assert!(
            s.x * s.y * s.z > 0.0,
            "node `{}` has non-positive-det scale {:?} after mirror bake",
            n.name,
            s
        );
    }

    // Find the original (`pair_0`) and mirrored (`pair_1`) leaves and confirm
    // the second one has its translation flipped on x and its mesh winding
    // reversed relative to the first.
    let pair_0_leaf = g
        .nodes
        .iter()
        .find(|n| n.name == "leaf"
            && n.parent.is_some()
            && g.nodes[n.parent.unwrap().0 as usize].name == "pair_0")
        .expect("pair_0/leaf");
    let pair_1_leaf = g
        .nodes
        .iter()
        .find(|n| n.name == "leaf"
            && n.parent.is_some()
            && g.nodes[n.parent.unwrap().0 as usize].name == "pair_1")
        .expect("pair_1/leaf");

    assert!((pair_0_leaf.transform.translation.x - 0.5).abs() < 1e-5);
    assert!((pair_1_leaf.transform.translation.x + 0.5).abs() < 1e-5);

    let m0 = pair_0_leaf.mesh.as_ref().unwrap();
    let m1 = pair_1_leaf.mesh.as_ref().unwrap();
    assert_eq!(m0.indices.len(), m1.indices.len());
    // Per-triangle winding flipped: m1 swaps indices 1 and 2 of every tri
    // (and x-flips positions/normals).
    for (a, b) in m0.indices.chunks_exact(3).zip(m1.indices.chunks_exact(3)) {
        assert_eq!(a[0], b[0]);
        assert_eq!(a[1], b[2]);
        assert_eq!(a[2], b[1]);
    }
    for (p0, p1) in m0.positions.iter().zip(m1.positions.iter()) {
        assert!((p0[0] + p1[0]).abs() < 1e-5, "x should be negated");
        assert!((p0[1] - p1[1]).abs() < 1e-5);
        assert!((p0[2] - p1[2]).abs() < 1e-5);
    }
    for (n0, n1) in m0.normals.iter().zip(m1.normals.iter()) {
        assert!((n0[0] + n1[0]).abs() < 1e-5, "normal x should be negated");
    }
}


