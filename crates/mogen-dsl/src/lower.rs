mod connector;
mod csg;
mod helpers;
mod layout;
mod material;
mod node;
mod primitive;

use anyhow::Result;

use mogen_core::SceneGraph;

use crate::anim_lower::{lower_clip, lower_joint, lower_template};
use crate::ast::Node;
use crate::attach::resolve_attaches;
use crate::module::{collect_modules, expand_modules};
use crate::skin_lower::{bind_meshes, lower_skeleton};

use material::collect_materials;
use node::lower_into;

const ANIM_KINDS: &[&str] = &["joint", "clip", "spin", "open_close", "wave", "flap", "idle"];

fn is_anim_decl(kind: &str) -> bool {
    ANIM_KINDS.contains(&kind)
}

pub fn lower(ast: &[Node]) -> Result<SceneGraph> {
    // Expand modules first: collect every `module` declaration, then substitute
    // `use` calls into concrete node trees. The result has no `module`/`use`
    // nodes and no `$param` references.
    let reg = collect_modules(ast)?;
    let expanded = expand_modules(ast, &reg)?;

    let mut graph = SceneGraph::new();

    // Pass 1: hoist every top-level and scene-level `material` declaration.
    collect_materials(&expanded, &mut graph)?;

    // Pass 2: build scene graph (skip anim declarations — they need nodes first).
    for n in &expanded {
        match n.kind.as_str() {
            "material" => {} // already handled
            k if is_anim_decl(k) => {} // pass 3
            "skeleton" => {
                lower_skeleton(n, None, &mut graph)?;
            }
            "scene" => {
                for c in &n.children {
                    if c.kind == "material" || c.kind == "attach" || is_anim_decl(&c.kind) {
                        continue;
                    }
                    if c.kind == "skeleton" {
                        lower_skeleton(c, None, &mut graph)?;
                        continue;
                    }
                    lower_into(c, None, &mut graph)?;
                }
            }
            "attach" => {} // pass 2.4
            _ => {
                lower_into(n, None, &mut graph)?;
            }
        }
    }

    // Pass 2.4: resolve `attach` specs. Runs before skin binding so bind-pose
    // world matrices reflect final part positions.
    resolve_attaches(&expanded, &mut graph)?;

    // Pass 2.5: bind mesh nodes carrying `skin="<name>"` to their skeleton.
    // Runs after every mesh exists and before animations so weights are
    // computed against bind-pose world transforms.
    bind_meshes(&expanded, &mut graph)?;

    // Pass 3: joints first (clips may reference joint names), then clips,
    // then procedural templates (which can target either joints or nodes).
    lower_animations(&expanded, &mut graph)?;
    Ok(graph)
}

fn lower_animations(ast: &[Node], graph: &mut SceneGraph) -> Result<()> {
    let iter = ast.iter().flat_map(|n| {
        if n.kind == "scene" {
            Box::new(n.children.iter()) as Box<dyn Iterator<Item = &Node>>
        } else {
            Box::new(std::iter::once(n))
        }
    });
    // Collect anim nodes by kind so ordering is deterministic regardless of
    // how the user wrote them in the file.
    let mut joints = Vec::new();
    let mut clips = Vec::new();
    let mut templates = Vec::new();
    for n in iter {
        match n.kind.as_str() {
            "joint" => joints.push(n),
            "clip" => clips.push(n),
            "spin" | "open_close" | "wave" | "flap" | "idle" => templates.push(n),
            _ => {}
        }
    }
    for n in joints {
        lower_joint(n, graph)?;
    }
    for n in clips {
        lower_clip(n, graph)?;
    }
    for n in templates {
        lower_template(n, graph)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
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
}
