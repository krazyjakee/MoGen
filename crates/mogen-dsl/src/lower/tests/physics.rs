//! Lowering tests for the `physics` block, `phys=` binding, and the auto-weigh
//! pass that turns a substance + real geometry into a computed weight.

use super::*;

#[test]
fn phys_binds_substance_properties() {
    let g = lower_src(
        r#"
        physics "oak" (weight=700kg/m3, friction=0.6, bounce=0.2)
        scene {
          box "crate" (size=[1, 1, 1], phys="oak")
        }
    "#,
    );
    let body = find_mesh_node(&g, "crate").physics.as_ref().expect("physics body");
    assert_eq!(body.material, "oak");
    assert!((body.weight_per_m3 - 700.0).abs() < 1e-3);
    assert!((body.friction - 0.6).abs() < 1e-4);
    assert!((body.bounce - 0.2).abs() < 1e-4);
}

#[test]
fn weight_auto_computed_from_volume() {
    // 1 m³ of oak (700 kg/m³) weighs 700 kg — no weight authored anywhere.
    let g = lower_src(
        r#"
        physics "oak" (weight=700kg/m3)
        scene {
          box "crate" (size=[1, 1, 1], phys="oak")
        }
    "#,
    );
    let body = find_mesh_node(&g, "crate").physics.as_ref().unwrap();
    let mass = body.mass.expect("mass computed");
    assert!((mass - 700.0).abs() < 1.0, "expected ~700 kg, got {mass}");
}

#[test]
fn scale_folds_into_computed_weight() {
    // A 1 m³ box scaled ×2 on every axis is 8 m³ → 8× the weight.
    let g = lower_src(
        r#"
        physics "oak" (weight=700kg/m3)
        scene {
          box "crate" (size=[1, 1, 1], scale=2, phys="oak")
        }
    "#,
    );
    let mass = find_mesh_node(&g, "crate").physics.as_ref().unwrap().mass.unwrap();
    assert!((mass - 5600.0).abs() < 10.0, "expected ~5600 kg, got {mass}");
}

#[test]
fn explicit_weight_overrides_computation() {
    // A flat `weight=5kg` on the node wins over the density-derived value.
    let g = lower_src(
        r#"
        physics "oak" (weight=700kg/m3)
        scene {
          box "prop" (size=[1, 1, 1], phys="oak", weight=5kg)
        }
    "#,
    );
    let mass = find_mesh_node(&g, "prop").physics.as_ref().unwrap().mass.unwrap();
    assert!((mass - 5.0).abs() < 1e-3, "override ignored, got {mass}");
}

#[test]
fn center_of_gravity_is_local_centroid() {
    // A centred unit box has its centre of gravity at the local origin.
    let g = lower_src(
        r#"
        physics "oak" (weight=700kg/m3)
        scene {
          box "crate" (size=[1, 1, 1], phys="oak")
        }
    "#,
    );
    let cog = find_mesh_node(&g, "crate").physics.as_ref().unwrap().center_of_gravity.unwrap();
    assert!(cog.iter().all(|c| c.abs() < 1e-4), "expected origin, got {cog:?}");
}

#[test]
fn phys_inherits_from_ancestor_group() {
    // `phys=` on a group flows to a child that declares none, exactly like mat=.
    let g = lower_src(
        r#"
        physics "oak" (weight=700kg/m3, friction=0.6, bounce=0.2)
        scene {
          group "frame" (phys="oak") {
            box "beam" (size=[1, 1, 1])
          }
        }
    "#,
    );
    let body = find_mesh_node(&g, "beam").physics.as_ref().expect("inherited body");
    assert_eq!(body.material, "oak");
    // The child weighs itself from the inherited substance.
    assert!((body.mass.unwrap() - 700.0).abs() < 1.0);
}

#[test]
fn explicit_phys_overrides_inherited() {
    let g = lower_src(
        r#"
        physics "oak"   (weight=700kg/m3)
        physics "steel" (weight=7850kg/m3)
        scene {
          group "frame" (phys="oak") {
            box "plate" (size=[1, 1, 1], phys="steel")
          }
        }
    "#,
    );
    let body = find_mesh_node(&g, "plate").physics.as_ref().unwrap();
    assert_eq!(body.material, "steel", "explicit phys must win over inherited");
    assert!((body.mass.unwrap() - 7850.0).abs() < 5.0);
}

#[test]
fn group_reports_compound_mass_and_weighted_cog() {
    // Two equal-mass unit cubes of oak, 2 m apart on X. The group's combined
    // weight is the sum, and its centre of gravity sits exactly between them.
    let g = lower_src(
        r#"
        physics "oak" (weight=700kg/m3)
        scene {
          group "pair" (phys="oak") {
            box "a" (pos=[-1, 0, 0], size=[1, 1, 1])
            box "b" (pos=[ 1, 0, 0], size=[1, 1, 1])
          }
        }
    "#,
    );
    let pair = find_mesh_node(&g, "pair").physics.as_ref().expect("compound body");
    assert!((pair.mass.unwrap() - 1400.0).abs() < 2.0, "combined mass, got {}", pair.mass.unwrap());
    let cog = pair.center_of_gravity.unwrap();
    assert!(cog.iter().all(|c| c.abs() < 1e-3), "COG should be midway, got {cog:?}");
}

#[test]
fn compound_cog_shifts_toward_the_heavier_part() {
    // A dense steel cube and a light foam cube, symmetric in space: the centre
    // of gravity must pull hard toward the steel side.
    let g = lower_src(
        r#"
        physics "steel" (weight=8000kg/m3)
        physics "foam"  (weight=100kg/m3)
        scene {
          group "asym" {
            box "heavy" (pos=[-1, 0, 0], size=[1,1,1], phys="steel")
            box "light" (pos=[ 1, 0, 0], size=[1,1,1], phys="foam")
          }
        }
    "#,
    );
    // The `asym` group has no phys= itself, so it carries no compound body; put
    // the aggregation on a wrapper that does.
    let g2 = lower_src(
        r#"
        physics "steel" (weight=8000kg/m3)
        physics "foam"  (weight=100kg/m3)
        physics "mixed" (weight=1kg/m3)
        scene {
          group "asym" (phys="mixed") {
            box "heavy" (pos=[-1, 0, 0], size=[1,1,1], phys="steel")
            box "light" (pos=[ 1, 0, 0], size=[1,1,1], phys="foam")
          }
        }
    "#,
    );
    // Sanity: without phys= on the group there is no compound body.
    assert!(find_mesh_node(&g, "asym").physics.is_none());
    // With one, the COG is well onto the steel (−X) side.
    let cog = find_mesh_node(&g2, "asym").physics.as_ref().unwrap().center_of_gravity.unwrap();
    assert!(cog[0] < -0.9, "COG should sit near the steel cube, got {cog:?}");
}

#[test]
fn unknown_phys_reference_is_an_error() {
    let ast = crate::parser::parse(
        "scene {\n  box \"crate\" (size=[1,1,1], phys=\"missing\")\n}\n",
    )
    .expect("parse");
    let err = crate::lower::lower(&ast).expect_err("unknown phys should fail");
    assert!(err.to_string().contains("unknown physics material"));
}
