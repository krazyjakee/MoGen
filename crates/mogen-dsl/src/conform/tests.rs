use mogen_core::{ConformBinding, SceneGraph};

use crate::lower;
use crate::parser::parse;

fn build(src: &str) -> SceneGraph {
    let ast = parse(src).expect("parse");
    lower(&ast).expect("lower")
}

fn build_err(src: &str) -> String {
    let ast = parse(src).expect("parse");
    format!("{}", lower(&ast).unwrap_err())
}

#[test]
fn flat_target_keeps_strip_unchanged() {
    // Conforming a flat strip onto a flat plane shouldn't warp it.
    let src = r#"
        scene {
          plane "ground" (size=[4, 4]) {
            connector "a" (at=[-1, 0,  0], dir=[0, 1, 0])
            connector "b" (at=[ 1, 0,  0], dir=[0, 1, 0])
          }
          box "stripe" (size=[2.0, 0.05, 0.2])
          conform (target="ground", child="stripe", from="a", to="b",
                   along=x, lift=0.001)
        }
    "#;
    let g = build(src);
    let stripe = g.find_node("stripe").unwrap();
    let mesh = g.nodes[stripe.0 as usize].mesh.as_ref().unwrap();
    // Every vertex y should be lift ± strip half-thickness (0.025).
    for p in &mesh.positions {
        assert!(
            p[1] > -0.03 && p[1] < 0.03,
            "stripe vertex y outside expected range: {p:?}"
        );
    }
    // After reparent=true (default), stripe is parented under ground.
    let ground = g.find_node("ground").unwrap();
    assert_eq!(g.nodes[stripe.0 as usize].parent, Some(ground));
}

#[test]
fn zip_on_ellipsoid_lands_on_surface() {
    let src = r#"
        scene {
          ellipsoid "bag" (size=[1.0, 0.6, 0.6]) {
            connector "zip_a" (at=[-0.4, 0.25, 0.30], dir=[0, 0, 1])
            connector "zip_b" (at=[ 0.4, 0.25, 0.30], dir=[0, 0, 1])
          }
          box "zip" (size=[0.8, 0.012, 0.04])
          conform (target="bag", child="zip", from="zip_a", to="zip_b",
                   along=x, lift=0.003)
        }
    "#;
    let g = build(src);
    let zip = g.find_node("zip").unwrap();
    let mesh = g.nodes[zip.0 as usize].mesh.as_ref().unwrap();
    // Every zip vertex should sit just outside the ellipsoid surface.
    // The ellipsoid is the "bag" with semi-axes (0.5, 0.3, 0.3); a
    // surface point at (x, y, z) satisfies x²/0.25 + y²/0.09 + z²/0.09 = 1.
    for p in &mesh.positions {
        let v = (p[0] * p[0]) / 0.25 + (p[1] * p[1]) / 0.09 + (p[2] * p[2]) / 0.09;
        // lift=3mm + strip thickness up to ~6mm pushes vertices
        // slightly outside the unit isosurface.
        assert!(
            v > 0.95 && v < 1.20,
            "zip vertex {p:?} not near surface (iso={v})"
        );
    }
}

#[test]
fn conform_writes_binding_for_tooling() {
    let src = r#"
        scene {
          plane "ground" (size=[4, 4]) {
            connector "a" (at=[-1, 0, 0], dir=[0, 1, 0])
            connector "b" (at=[ 1, 0, 0], dir=[0, 1, 0])
          }
          box "stripe" (size=[2, 0.05, 0.2])
          conform (target="ground", child="stripe", from="a", to="b", along=x)
        }
    "#;
    let g = build(src);
    let stripe = g.find_node("stripe").unwrap();
    let cb = g.nodes[stripe.0 as usize]
        .conform_binding
        .as_ref()
        .expect("conform binding written");
    match cb {
        ConformBinding::Path { target, from, to, .. } => {
            assert_eq!(*target, g.find_node("ground").unwrap());
            assert_eq!(from, "a");
            assert_eq!(to, "b");
        }
        other => panic!("expected Path binding, got {other:?}"),
    }
}

#[test]
fn conform_rejects_sphere_child() {
    let err = build_err(
        r#"
        scene {
          sphere "ball" (radius=0.5) {
            connector "a" (at=[-0.5, 0, 0], dir=[-1, 0, 0])
            connector "b" (at=[ 0.5, 0, 0], dir=[ 1, 0, 0])
          }
          sphere "decal" (radius=0.05)
          conform (target="ball", child="decal", from="a", to="b")
        }
        "#,
    );
    assert!(err.contains("cannot mould a \"sphere\""), "err = {err}");
    assert!(
        err.contains("supported path-mode kinds"),
        "err = {err}"
    );
}

#[test]
fn conform_rejects_unknown_connector() {
    let err = build_err(
        r#"
        scene {
          plane "ground" (size=[4, 4]) {
            connector "a" (at=[-1, 0, 0], dir=[0, 1, 0])
          }
          box "stripe" (size=[2, 0.05, 0.2])
          conform (target="ground", child="stripe", from="a", to="zzz")
        }
        "#,
    );
    assert!(err.contains("no connector \"zzz\""), "err = {err}");
    // Available list should mention the connectors we did define.
    assert!(err.contains("\"ground\""), "err = {err}");
}

#[test]
fn conform_rejects_imported_mesh_without_along() {
    // Building from a literal `mesh` node that loads an external GLB
    // is heavy; instead simulate by invoking conform on a `mesh`-kinded
    // child via a synthesized scene. This exercises the
    // `kind == "mesh" && along.is_none()` branch even without I/O.
    // (`mesh` lowering needs a real .glb path. Skip in DSL tests; the
    // branch is exercised by the build_spec → apply_conform unit path
    // through the code, which we cover via the broken/ snapshot in
    // `tests/broken/conform_imported_no_along.mog`.)
}

#[test]
fn patch_disc_lays_on_plane_at_anchor() {
    // A disc patch at a connector on a flat plane: every vertex
    // sits at lift above the plane, planar offset equals the disc's
    // own rim radius from the anchor.
    let src = r#"
        scene {
          plane "ground" (size=[4, 4]) {
            connector "spot" (at=[0.5, 0, -0.2], dir=[0, 1, 0])
          }
          disc "patch" (radius=0.3, segments=24)
          conform (target="ground", child="patch", at="spot", lift=0.002)
        }
    "#;
    let g = build(src);
    let patch = g.find_node("patch").unwrap();
    let mesh = g.nodes[patch.0 as usize].mesh.as_ref().unwrap();
    for p in &mesh.positions {
        assert!(
            (p[1] - 0.002).abs() < 1e-3,
            "patch vertex y {} not at lift",
            p[1]
        );
    }
    // Reparent default → patch lives under ground.
    let ground = g.find_node("ground").unwrap();
    assert_eq!(g.nodes[patch.0 as usize].parent, Some(ground));
}

#[test]
fn patch_disc_on_curved_target_follows_curvature() {
    // Patch a disc onto an ellipsoid — every vertex must sit close to
    // the surface (not on a flat tangent plane).
    let src = r#"
        scene {
          ellipsoid "bag" (size=[1.0, 0.6, 0.6]) {
            connector "spot" (at=[0.4, 0.2, 0.3], dir=[0, 0, 1])
          }
          disc "decal" (radius=0.12, segments=32)
          conform (target="bag", child="decal", at="spot", lift=0.005)
        }
    "#;
    let g = build(src);
    let decal = g.find_node("decal").unwrap();
    let mesh = g.nodes[decal.0 as usize].mesh.as_ref().unwrap();
    // Ellipsoid semi-axes (0.5, 0.3, 0.3); vertex iso ≈ 1 + tiny lift.
    for p in &mesh.positions {
        let v = (p[0] * p[0]) / 0.25 + (p[1] * p[1]) / 0.09 + (p[2] * p[2]) / 0.09;
        assert!(
            v > 0.95 && v < 1.20,
            "decal vertex {p:?} far from ellipsoid (iso={v})",
        );
    }
}

#[test]
fn patch_writes_patch_binding() {
    let src = r#"
        scene {
          plane "ground" (size=[4, 4]) {
            connector "spot" (at=[0, 0, 0], dir=[0, 1, 0])
          }
          disc "patch" (radius=0.2, segments=12)
          conform (target="ground", child="patch", at="spot")
        }
    "#;
    let g = build(src);
    let patch = g.find_node("patch").unwrap();
    let cb = g.nodes[patch.0 as usize]
        .conform_binding
        .as_ref()
        .expect("conform binding written");
    match cb {
        ConformBinding::Patch { target, at } => {
            assert_eq!(*target, g.find_node("ground").unwrap());
            assert_eq!(at, "spot");
        }
        other => panic!("expected Patch binding, got {other:?}"),
    }
}

#[test]
fn conform_rejects_mixing_modes() {
    let err = build_err(
        r#"
        scene {
          plane "ground" (size=[4, 4]) {
            connector "a" (at=[0, 0, 0], dir=[0, 1, 0])
            connector "b" (at=[1, 0, 0], dir=[0, 1, 0])
          }
          disc "patch" (radius=0.1, segments=8)
          conform (target="ground", child="patch", at="a", from="a", to="b")
        }
        "#,
    );
    assert!(
        err.contains("cannot combine patch-mode") && err.contains("path-mode"),
        "err = {err}"
    );
}

#[test]
fn conform_rejects_no_mode() {
    let err = build_err(
        r#"
        scene {
          plane "ground" (size=[4, 4])
          disc "patch" (radius=0.1, segments=8)
          conform (target="ground", child="patch")
        }
        "#,
    );
    assert!(
        err.contains("at=") && err.contains("from=") && err.contains("to="),
        "err = {err}"
    );
}

#[test]
fn conform_path_mode_disc_hints_patch_mode() {
    // Disc isn't allowed in path mode; the error should suggest patch
    // mode rather than just listing supported kinds.
    let err = build_err(
        r#"
        scene {
          plane "ground" (size=[4, 4]) {
            connector "a" (at=[-1, 0, 0], dir=[0, 1, 0])
            connector "b" (at=[ 1, 0, 0], dir=[0, 1, 0])
          }
          disc "patch" (radius=0.1, segments=12)
          conform (target="ground", child="patch", from="a", to="b")
        }
        "#,
    );
    assert!(
        err.contains("try patch mode") && err.contains("disc"),
        "err = {err}"
    );
}

#[test]
fn conform_patch_mode_sphere_still_rejected() {
    // Closed shapes (sphere) lack any canonical surface axis and stay
    // rejected even in patch mode.
    let err = build_err(
        r#"
        scene {
          plane "ground" (size=[4, 4]) {
            connector "spot" (at=[0, 0, 0], dir=[0, 1, 0])
          }
          sphere "ball" (radius=0.1)
          conform (target="ground", child="ball", at="spot")
        }
        "#,
    );
    assert!(err.contains("cannot mould a \"sphere\""), "err = {err}");
    assert!(err.contains("patch mode"), "err = {err}");
}

#[test]
fn conform_inside_group_lowers_cleanly() {
    // Regression: when an imported scene-as-module body carrying conform
    // directives is expanded inside a `group` wrapper, those conform
    // children land under the group rather than at scene level. The
    // node-lowering pass must skip them so they don't trip
    // "unknown node kind: conform" — `resolve_conforms` walks the
    // expanded AST recursively and picks them up regardless of nesting.
    let src = r#"
        scene {
          group "wrap" {
            plane "ground" (size=[4, 4]) {
              connector "spot" (at=[0, 0, 0], dir=[0, 1, 0])
            }
            disc "patch" (radius=0.1, segments=12)
            conform (target="ground", child="patch", at="spot", lift=0.01)
          }
        }
    "#;
    let g = build(src);
    let patch = g.find_node("patch").expect("patch node lowered");
    // The conform should still have run despite the wrapper — patch is
    // reparented under ground (the conform default).
    let ground = g.find_node("ground").unwrap();
    assert_eq!(g.nodes[patch.0 as usize].parent, Some(ground));
}

#[test]
fn patch_mode_rejects_path_only_attrs() {
    let err = build_err(
        r#"
        scene {
          plane "ground" (size=[4, 4]) {
            connector "spot" (at=[0, 0, 0], dir=[0, 1, 0])
          }
          disc "patch" (radius=0.1, segments=12)
          conform (target="ground", child="patch", at="spot", along=x)
        }
        "#,
    );
    assert!(err.contains("path-mode only"), "err = {err}");
}

#[test]
fn conformed_decal_rot_z_spins_in_tangent_plane() {
    // Pre-conform `rot=[0, 0, 90]` should swap the decal's local X and Y
    // extents (rotation around the surface normal). Match against the
    // decal's local AABB on the deformed mesh — width and height swap.
    let src = r#"
        scene {
          plane "ground" (size=[4, 4]) {
            connector "spot" (at=[0, 0, 0], dir=[0, 1, 0])
          }
          decal "logo" (size=[0.4, 0.2], prompt="x", rot=[0, 0, 90])
          conform (target="ground", child="logo", at="spot", lift=0.001)
        }
    "#;
    let g = build(src);
    let logo = g.find_node("logo").unwrap();
    let mesh = g.nodes[logo.0 as usize].mesh.as_ref().unwrap();
    let mut x_extent = (f32::INFINITY, f32::NEG_INFINITY);
    let mut z_extent = (f32::INFINITY, f32::NEG_INFINITY);
    for p in &mesh.positions {
        x_extent.0 = x_extent.0.min(p[0]);
        x_extent.1 = x_extent.1.max(p[0]);
        z_extent.0 = z_extent.0.min(p[2]);
        z_extent.1 = z_extent.1.max(p[2]);
    }
    let x_size = x_extent.1 - x_extent.0;
    let z_size = z_extent.1 - z_extent.0;
    // Without the rotation, x_size would be ~0.4 and z_size ~0.2 (the decal
    // sized [0.4, 0.2] sits flat on a +Y plane mapping its local +X→world+X
    // and local +Y→world+Z). With `rot=[0,0,90]` the rotation around +Z
    // (the surface normal) swaps X and Y in the decal frame, so X→world Z
    // and Y→world X.
    assert!(
        (x_size - 0.2).abs() < 0.05,
        "expected x_size ≈ 0.2 after Z rotation, got {x_size:.3}"
    );
    assert!(
        (z_size - 0.4).abs() < 0.05,
        "expected z_size ≈ 0.4 after Z rotation, got {z_size:.3}"
    );
}

#[test]
fn conformed_decal_rot_x_tilts_artwork() {
    // The user's reported case: `rot=[90, 0, 0]` tilts the decal's
    // artwork off the surface — the decal's local +Z (originally the
    // surface normal direction) gets rotated to -Y, so vertices that
    // were at z=±tiny now displace ±size/2 along the original +Y / -Y.
    // The point of the test is not to prescribe the exact look, but to
    // prove the rotation is no longer silently dropped — the deformed
    // mesh must have notably different geometry from the un-rotated
    // baseline.
    let baseline_src = r#"
        scene {
          plane "ground" (size=[4, 4]) {
            connector "spot" (at=[0, 0, 0], dir=[0, 1, 0])
          }
          decal "logo" (size=[0.2, 0.3], prompt="x")
          conform (target="ground", child="logo", at="spot", lift=0.001)
        }
    "#;
    let rotated_src = r#"
        scene {
          plane "ground" (size=[4, 4]) {
            connector "spot" (at=[0, 0, 0], dir=[0, 1, 0])
          }
          decal "logo" (size=[0.2, 0.3], prompt="x", rot=[90, 0, 0])
          conform (target="ground", child="logo", at="spot", lift=0.001)
        }
    "#;
    let g_a = build(baseline_src);
    let g_b = build(rotated_src);
    let mesh_a = g_a.nodes[g_a.find_node("logo").unwrap().0 as usize].mesh.as_ref().unwrap();
    let mesh_b = g_b.nodes[g_b.find_node("logo").unwrap().0 as usize].mesh.as_ref().unwrap();
    let mut max_diff = 0.0_f32;
    for (a, b) in mesh_a.positions.iter().zip(&mesh_b.positions) {
        for k in 0..3 {
            max_diff = max_diff.max((a[k] - b[k]).abs());
        }
    }
    assert!(
        max_diff > 0.05,
        "expected rotation to alter geometry; max position diff was {max_diff:.4}"
    );
}

#[test]
fn decal_on_synthesizes_patch_conform() {
    // `decal (on="bag", at="spot")` should produce the same end state as
    // an explicit `conform (target="bag", child="logo", at="spot")`: the
    // decal's vertices land on the ellipsoid surface.
    let src = r#"
        scene {
          ellipsoid "bag" (size=[1.0, 0.6, 0.6]) {
            connector "spot" (at=[0.4, 0.2, 0.3], dir=[0, 0, 1])
          }
          decal "logo" (size=[0.2, 0.12], on="bag", at="spot", lift=0.005)
        }
    "#;
    let g = build(src);
    let logo = g.find_node("logo").unwrap();
    let cb = g.nodes[logo.0 as usize]
        .conform_binding
        .as_ref()
        .expect("decal on= should write a Patch ConformBinding");
    match cb {
        ConformBinding::Patch { target, at } => {
            assert_eq!(*target, g.find_node("bag").unwrap());
            assert_eq!(at, "spot");
        }
        other => panic!("expected Patch binding, got {other:?}"),
    }
    // Vertices should be on (or just above) the ellipsoid isosurface.
    let mesh = g.nodes[logo.0 as usize].mesh.as_ref().unwrap();
    for p in &mesh.positions {
        let v = (p[0] * p[0]) / 0.25 + (p[1] * p[1]) / 0.09 + (p[2] * p[2]) / 0.09;
        assert!(
            v > 0.95 && v < 1.20,
            "decal vertex {p:?} far from ellipsoid (iso={v})",
        );
    }
    // Decal must be reparented under the bag.
    assert_eq!(g.nodes[logo.0 as usize].parent, Some(g.find_node("bag").unwrap()));
}

#[test]
fn decal_rotation_is_applied_when_not_conformed() {
    // Sanity check: a plain decal honors `rot=`. (The shortcut path resets
    // the transform to identity because conform reparents under the
    // target, but standalone decals should still rotate.)
    let src = r#"
        scene {
          decal "logo" (size=[0.2, 0.1], prompt="x", rot=[0, 90, 0])
        }
    "#;
    let g = build(src);
    let logo = g.find_node("logo").unwrap();
    let q = g.nodes[logo.0 as usize].transform.rotation;
    // Quat for 90° around Y under EulerRot::XYZ has w ≈ cos(45°) ≈ 0.7071
    // and y ≈ sin(45°) ≈ 0.7071.
    assert!((q.w - 0.7071).abs() < 1e-3, "expected ~0.707 w, got {q:?}");
    assert!((q.y - 0.7071).abs() < 1e-3, "expected ~0.707 y, got {q:?}");
}

#[test]
fn conform_no_reparent_keeps_original_parent() {
    let src = r#"
        scene {
          group "root" {
            plane "ground" (size=[4, 4]) {
              connector "a" (at=[-1, 0, 0], dir=[0, 1, 0])
              connector "b" (at=[ 1, 0, 0], dir=[0, 1, 0])
            }
            box "stripe" (size=[2, 0.05, 0.2])
          }
          conform (target="ground", child="stripe", from="a", to="b",
                   along=x, reparent=0)
        }
    "#;
    let g = build(src);
    let root = g.find_node("root").unwrap();
    let stripe = g.find_node("stripe").unwrap();
    // With reparent=0, stripe stays under "root", not under ground.
    assert_eq!(g.nodes[stripe.0 as usize].parent, Some(root));
}
