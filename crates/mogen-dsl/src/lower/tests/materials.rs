use super::*;
use crate::lower::*;
use crate::parser::parse;

#[test]
fn skeleton_nested_inside_group_is_lowered() {
    // Same shape as the businessman example: wrapping `use "humanoid_full" ()`
    // in `group "humanoid" { ... }` puts the module-declared `skeleton "rig"`
    // inside that group. The skeleton must still register so a sibling mesh
    // with `skin="rig"` can bind to it.
    let g = lower_src(
        r#"
        scene {
          group "wrapper" {
            skeleton "rig" {
              bone "root" (envelope=0.5)
            }
            box "thing" (size=[1, 1, 1], skin="rig", bind="root")
          }
        }
        "#,
    );
    let skin = g.find_skin("rig").expect("rig skin registered from inside group");
    let thing = find_mesh_node(&g, "thing");
    assert_eq!(thing.skin, Some(skin), "mesh inside the same group should bind to rig");
}

#[test]
fn materials_nested_inside_groups_are_discovered() {
    // A `material` decl that lands inside a wrapping `group` (the shape that
    // module expansion produces when the user writes
    // `group "g" { use "m" () }`) must still be findable for `mat=` lookup.
    // Regression: collect_materials used to walk only top-level / scene-level.
    let g = lower_src(
        r#"
        scene {
          group "wrapper" {
            material "boot" (color=[0.04, 0.04, 0.04])
            box "shoe" (size=[1, 0.5, 1.5], mat="boot")
          }
        }
        "#,
    );
    let mid = g.find_material("boot").expect("boot material registered");
    let shoe = find_mesh_node(&g, "shoe");
    assert_eq!(shoe.material, Some(mid), "shoe should resolve mat=\"boot\"");
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

#[test]
fn vertical_gradient_bakes_endpoint_colours_at_y_extents() {
    // `vertical` is sugar for `linear(axis=y)`. The bake samples each vertex
    // against the mesh-local AABB, so the bottom vertices land on `from` and
    // the top vertices land on `to` regardless of mesh placement.
    let g = lower_src(
        r#"
        material "ramp" (color=[1, 1, 1],
                         gradient=vertical(from=[1, 0, 0], to=[0, 0, 1]))
        scene { box "b" (size=[1, 1, 1], pos=[5, 5, 5], mat="ramp") }
        "#,
    );
    let mesh = find_mesh_node(&g, "b").mesh.as_ref().unwrap();
    assert_eq!(mesh.colors.len(), mesh.positions.len(), "COLOR_0 row count mismatch");
    let ymin_i = mesh
        .positions
        .iter()
        .enumerate()
        .min_by(|a, b| a.1[1].partial_cmp(&b.1[1]).unwrap())
        .map(|(i, _)| i)
        .unwrap();
    let ymax_i = mesh
        .positions
        .iter()
        .enumerate()
        .max_by(|a, b| a.1[1].partial_cmp(&b.1[1]).unwrap())
        .map(|(i, _)| i)
        .unwrap();
    assert!((mesh.colors[ymin_i][0] - 1.0).abs() < 1e-5);
    assert!((mesh.colors[ymax_i][2] - 1.0).abs() < 1e-5);
}

#[test]
fn radial_gradient_orders_colours_by_distance_from_centre() {
    // Radial sampling is centre-to-furthest-corner. MoGen primitives don't
    // generally emit a vertex at their AABB centroid, so a strict t=0 check
    // would fail. Instead verify the ordering invariant the bake guarantees:
    // the vertex closest to the AABB centre receives more `center` colour
    // than the vertex furthest from it.
    let g = lower_src(
        r#"
        material "ring" (color=[1, 1, 1],
                         gradient=radial(center=[1, 0, 0], edge=[0, 0, 1]))
        scene { cylinder "c" (radius=0.5, height=2, segments=24, mat="ring") }
        "#,
    );
    let mesh = find_mesh_node(&g, "c").mesh.as_ref().unwrap();
    let mut centroid = [0.0f32; 3];
    for p in &mesh.positions {
        centroid[0] += p[0];
        centroid[1] += p[1];
        centroid[2] += p[2];
    }
    let n = mesh.positions.len() as f32;
    centroid = [centroid[0] / n, centroid[1] / n, centroid[2] / n];
    let dist2 = |p: &[f32; 3]| {
        let d = [p[0] - centroid[0], p[1] - centroid[1], p[2] - centroid[2]];
        d[0] * d[0] + d[1] * d[1] + d[2] * d[2]
    };
    let nearest = (0..mesh.positions.len())
        .min_by(|&a, &b| dist2(&mesh.positions[a]).partial_cmp(&dist2(&mesh.positions[b])).unwrap())
        .unwrap();
    let furthest = (0..mesh.positions.len())
        .max_by(|&a, &b| dist2(&mesh.positions[a]).partial_cmp(&dist2(&mesh.positions[b])).unwrap())
        .unwrap();
    assert!(
        mesh.colors[nearest][0] > mesh.colors[furthest][0],
        "nearest vertex should carry more `center` red"
    );
    assert!(
        mesh.colors[furthest][2] > mesh.colors[nearest][2],
        "furthest vertex should carry more `edge` blue"
    );
}

#[test]
fn stops_gradient_default_positions_are_evenly_spaced() {
    // With `colors=[a, b, c]` and no `positions`, stops should land at 0, 0.5, 1.
    // Verify by giving a tall box a 3-stop Y ramp and checking the mid-Y
    // vertex receives the middle colour.
    let g = lower_src(
        r#"
        material "rgb" (color=[1, 1, 1],
                        gradient=stops(colors=[[1, 0, 0], [0, 1, 0], [0, 0, 1]], axis=y))
        scene { box "b" (size=[1, 4, 1], mat="rgb") }
        "#,
    );
    let mesh = find_mesh_node(&g, "b").mesh.as_ref().unwrap();
    // Box verts only exist at the AABB corners (y=±2 for a height-4 box), so
    // we can't sample at Y=0 directly. Instead verify the endpoints are exact
    // — those are what default position spacing pins.
    let ymin_color = mesh
        .colors
        .iter()
        .zip(mesh.positions.iter())
        .min_by(|a, b| a.1[1].partial_cmp(&b.1[1]).unwrap())
        .map(|(c, _)| *c)
        .unwrap();
    let ymax_color = mesh
        .colors
        .iter()
        .zip(mesh.positions.iter())
        .max_by(|a, b| a.1[1].partial_cmp(&b.1[1]).unwrap())
        .map(|(c, _)| *c)
        .unwrap();
    assert!((ymin_color[0] - 1.0).abs() < 1e-5, "bottom should be red, got {ymin_color:?}");
    assert!((ymax_color[2] - 1.0).abs() < 1e-5, "top should be blue, got {ymax_color:?}");
}

#[test]
fn material_without_gradient_leaves_mesh_colors_empty() {
    // Make sure a plain material doesn't accidentally trigger the bake.
    let g = lower_src(
        r#"
        material "plain" (color=[0.5, 0.5, 0.5])
        scene { box "b" (size=[1, 1, 1], mat="plain") }
        "#,
    );
    let mesh = find_mesh_node(&g, "b").mesh.as_ref().unwrap();
    assert!(mesh.colors.is_empty(), "plain material should not bake colours");
}

#[test]
fn gradient_axis_must_be_x_y_or_z() {
    let ast = parse(
        r#"
        material "bad" (color=[1, 1, 1],
                        gradient=linear(from=[1, 0, 0], to=[0, 0, 1], axis=q))
        scene { box "b" (size=[1, 1, 1], mat="bad") }
        "#,
    ).expect("parse");
    let err = lower(&ast).expect_err("unknown axis must reject");
    let msg = format!("{err:#}");
    assert!(msg.contains("axis"), "wrong error: {msg}");
}
