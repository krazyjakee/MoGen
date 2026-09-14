use glam::Vec3;

fn check(source: &str) -> mogen_core::SceneGraph {
    let scene = mogen_dsl::lower(&mogen_dsl::parse(source).unwrap()).unwrap();
    let worlds = scene.world_transforms();
    for r in &scene.relationships {
        let child = worlds[r.child.0 as usize].transform_point3(Vec3::from_array(r.child_anchor));
        let target =
            worlds[r.target.0 as usize].transform_point3(Vec3::from_array(r.target_anchor));
        let normal = worlds[r.target.0 as usize]
            .inverse()
            .transpose()
            .transform_vector3(Vec3::from_array(r.target_normal))
            .normalize();
        let desired = target + normal * (r.clearance - r.insertion);
        let residual = if r.mode == "ground" {
            (child - desired).dot(normal).abs()
        } else {
            child.distance(desired)
        };
        assert!(residual <= r.tolerance, "{} residual {residual}", r.mode);
    }
    scene
}

#[test]
fn chair_parameter_sweeps_preserve_joint_anchors() {
    let source = include_str!("../../../examples/furniture/relational_chair.mog");
    for params in ["w=0.8,h=0.5,d=0.6,recline=0", "w=1.3,h=0.9,d=1,recline=20"] {
        let scene =
            check(&source.replace("use \"chair\" ()", &format!("use \"chair\" ({params})")));
        assert_eq!(scene.relationships.len(), 18);
    }
}

#[test]
fn module_instances_and_parent_transforms_keep_relationships_scoped() {
    let source="module \"part\" () { box \"target\" (pos=[0,1,0],size=[1,0.1,1]) spline_tube \"leg\" (points=[[0,0,0],[0,0.5,0]],radius=0.02) relate (child=\"leg\",target=\"target\",mode=\"endpoint\",socket=\"bottom\",insertion=0.01) } scene { group \"a\" (rot=[0,45,0],scale=[2,1,0.5]) { use \"part\" () } group \"b\" (pos=[3,0,0]) { use \"part\" () } }";
    let scene = check(source);
    assert_eq!(scene.relationships.len(), 2);
    assert_ne!(scene.relationships[0].target, scene.relationships[1].target);
}

#[test]
fn cycles_duplicate_writes_and_missing_targets_fail() {
    let geometry = "scene { box \"a\" (size=[1,1,1]) box \"b\" (size=[1,1,1]) }";
    for specs in [
        "relate (child=\"a\",target=\"b\") relate (child=\"b\",target=\"a\")",
        "relate (child=\"a\",target=\"b\") relate (child=\"a\",target=\"b\")",
        "relate (child=\"a\",target=\"missing\")",
        "relate (child=\"a\",target=\"b\",mode=\"endpoint\")",
    ] {
        let ast = mogen_dsl::parse(&format!("{geometry} {specs}")).unwrap();
        assert!(mogen_dsl::lower(&ast)
            .unwrap_err()
            .to_string()
            .contains("E0150"));
    }
    assert!(check(geometry).relationships.is_empty());
}
