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
fn unknown_phys_reference_is_an_error() {
    let ast = crate::parser::parse(
        "scene {\n  box \"crate\" (size=[1,1,1], phys=\"missing\")\n}\n",
    )
    .expect("parse");
    let err = crate::lower::lower(&ast).expect_err("unknown phys should fail");
    assert!(err.to_string().contains("unknown physics material"));
}
