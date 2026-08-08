use super::*;
use crate::lower::geometry_identity::{reset_tessellation_misses, tessellation_misses};

fn mesh_bytes_equal(a: &mogen_core::Mesh, b: &mogen_core::Mesh) -> bool {
    a.positions == b.positions
        && a.normals == b.normals
        && a.uvs == b.uvs
        && a.indices == b.indices
        && a.joints == b.joints
        && a.weights == b.weights
}

#[test]
fn expanded_identical_primitives_share_identity_and_one_tessellation() {
    reset_tessellation_misses();
    let graph = lower_src(
        r#"
        module "ball" () { sphere "orb" (radius=0.5) }
        scene {
          use "ball" ()
          use "ball" ()
        }
        "#,
    );
    let balls: Vec<_> = graph.nodes.iter().filter(|n| n.name == "orb").collect();
    assert_eq!(balls.len(), 2);
    assert_eq!(balls[0].geometry_identity, balls[1].geometry_identity);
    assert!(balls[0].geometry_identity.is_some());
    assert!(mesh_bytes_equal(
        balls[0].mesh.as_ref().unwrap(),
        balls[1].mesh.as_ref().unwrap()
    ));
    assert_eq!(
        tessellation_misses(),
        1,
        "the analytic sphere kernel should run once for two expanded copies"
    );
}

#[test]
fn resolved_defaults_merge_while_geometry_inputs_split() {
    let graph = lower_src(
        r#"
        material "tile" (uv_mode="tile")
        material "fit" (uv_mode="fit")
        scene {
          sphere "implicit" (mat="tile")
          sphere "explicit" (radius=0.5, rings=16, segments=24, mat="tile", pos=[2,0,0])
          sphere "radius" (radius=0.6, mat="tile")
          sphere "deformed" (radius=0.5, bend_x=10, mat="tile")
          sphere "fit_uv" (radius=0.5, mat="fit")
        }
        "#,
    );
    let implicit = find_mesh_node(&graph, "implicit");
    let explicit = find_mesh_node(&graph, "explicit");
    assert_eq!(implicit.geometry_identity, explicit.geometry_identity);
    assert!(mesh_bytes_equal(
        implicit.mesh.as_ref().unwrap(),
        explicit.mesh.as_ref().unwrap()
    ));
    for name in ["radius", "deformed", "fit_uv"] {
        assert_ne!(
            implicit.geometry_identity,
            find_mesh_node(&graph, name).geometry_identity,
            "resolved geometry input on {name} must split the parameter key"
        );
    }

    let coarse = lower_src(r#"lod_scale (value=0.5) scene { sphere "s" () }"#);
    let full = lower_src(r#"scene { sphere "s" () }"#);
    assert_ne!(
        find_mesh_node(&coarse, "s").geometry_identity,
        find_mesh_node(&full, "s").geometry_identity,
        "resolved LOD counts are part of the identity"
    );
}

#[test]
fn parameter_and_mesh_equivalence_agree_on_a_primitive_corpus() {
    let graph = lower_src(
        r#"
        scene {
          sphere "sphere_default" ()
          sphere "sphere_explicit" (radius=0.5, rings=16, segments=24)
          sphere "sphere_other" (radius=0.7)
          box "box" (size=[1,1,1])
          slab "slab_alias" (size=1)
          plane "plane_default" ()
          plane "plane_explicit" (size=[1,1,1])
          cylinder "cylinder" (radius=0.5, height=1, segments=24)
        }
        "#,
    );
    let nodes: Vec<_> = graph.nodes.iter().filter(|n| n.mesh.is_some()).collect();
    assert!(nodes.len() >= 8, "corpus unexpectedly lost primitive nodes");
    for (i, a) in nodes.iter().enumerate() {
        let a_mesh = a.mesh.as_ref().unwrap();
        let a_key = a
            .geometry_identity
            .expect("analytic primitive must carry a key");
        for b in nodes.iter().skip(i + 1) {
            let same_bytes = mesh_bytes_equal(a_mesh, b.mesh.as_ref().unwrap());
            let same_params = a_key
                == b.geometry_identity
                    .expect("analytic primitive must carry a key");
            assert_eq!(
                same_params, same_bytes,
                "parameter/byte partition disagrees for `{}` and `{}`",
                a.name, b.name
            );
        }
    }
}

#[test]
fn opaque_and_post_mutated_geometry_falls_back_to_bytes() {
    let graph = lower_src(
        r#"
        scene {
          difference "cut" {
            box (size=1)
            sphere (radius=0.2)
          }
          poly "raw" (
            points=[[0,0,0],[1,0,0],[1,1,0],[0,1,0]],
            uvs=[[0,0],[1,0],[1,1],[0,1]],
            indices=[0,1,2,0,2,3]
          )
          mirror "pair" (axis=x) {
            box "mirrored" (size=[0.2,0.3,0.4], x=0.5)
          }
        }
        "#,
    );
    assert_eq!(find_mesh_node(&graph, "cut").geometry_identity, None);
    assert_eq!(find_mesh_node(&graph, "raw").geometry_identity, None);

    let mirrored: Vec<_> = graph
        .nodes
        .iter()
        .filter(|n| n.name == "mirrored")
        .collect();
    assert_eq!(mirrored.len(), 2);
    assert!(mirrored.iter().any(|n| n.geometry_identity.is_some()));
    assert!(mirrored.iter().any(|n| n.geometry_identity.is_none()));

    let fixture_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../mogen/tests/fixtures");
    let ast = crate::parse(r#"scene { mesh "imported" (src="wall_door.glb") }"#)
        .expect("parse imported mesh");
    let imported =
        crate::lower_with_source(&ast, Some(&fixture_dir)).expect("lower imported mesh fixture");
    assert_eq!(
        find_mesh_node(&imported, "imported").geometry_identity,
        None,
        "imported bytes never inherit an analytic parameter identity"
    );
}
