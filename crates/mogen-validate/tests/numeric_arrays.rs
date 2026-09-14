use mogen_core::has_errors;

fn compile(source: &str) -> mogen_core::SceneGraph {
    let ast = mogen_dsl::parse(source).unwrap();
    let diagnostics = mogen_validate::validate_ast(&ast);
    assert!(!has_errors(&diagnostics), "{diagnostics:?}");
    mogen_dsl::lower(&ast).unwrap()
}

#[test]
fn scalar_arrays_have_consistent_arity_through_validation_and_lowering() {
    for len in [2, 3, 4, 6] {
        let points = (0..len)
            .map(|i| format!("[0,{i},0]"))
            .collect::<Vec<_>>()
            .join(",");
        let values = (0..len)
            .map(|i| format!("{}", 0.1 + i as f32 * 0.03))
            .collect::<Vec<_>>()
            .join(",");
        for (kind, key) in [
            ("spline_tube", "radii"),
            ("spline_ribbon", "widths"),
            ("sweep", "roll"),
            ("sweep", "scale_along"),
        ] {
            let point_key = if kind == "sweep" { "path" } else { "points" };
            let source =
                format!("scene {{ {kind} \"part\" ({point_key}=[{points}],{key}=[{values}]) }}");
            let scene = compile(&source);
            assert!(scene
                .nodes
                .iter()
                .any(|n| n.mesh.as_ref().is_some_and(|m| !m.indices.is_empty())));
        }
        let heights = (0..len)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let sections = vec!["[-1,-1],[1,-1],[1,1],[-1,1]"; len].join(",");
        compile(&format!(
            "scene {{ loft (points=[{sections}],heights=[{heights}]) }}"
        ));
    }
}

#[test]
fn three_radii_reach_the_authored_control_points() {
    let scene = compile("scene { spline_tube \"leg\" (points=[[0,0,0],[0,1,0],[0,2,0]],radii=[0.1,0.2,0.3],segments=12,samples=4) }");
    let mesh = scene.nodes.iter().find_map(|n| n.mesh.as_ref()).unwrap();
    for (height, radius) in [(0.0_f32, 0.1_f32), (1.0, 0.2), (2.0, 0.3)] {
        let measured = mesh
            .positions
            .iter()
            .filter(|p| (p[1] - height).abs() < 1e-5)
            .map(|p| p[0].hypot(p[2]))
            .fold(0.0_f32, f32::max);
        assert!(
            (measured - radius).abs() < 1e-5,
            "at {height}: {measured} vs {radius}"
        );
    }
}

#[test]
fn expression_arrays_resolve_module_parameters_before_range_checks() {
    compile("module \"leg\" (r=0.1) { spline_tube (points=[[0,0,0],[0,1,0],[0,2,0]],radii=[$r,2*$r,3*$r]) } scene { use \"leg\" (r=0.2) }");
    let bad = mogen_dsl::parse("module \"leg\" (r=0.1) { spline_tube (points=[[0,0,0],[0,1,0],[0,2,0]],radii=[$r,2*$r,3*$r]) } scene { use \"leg\" (r=-0.2) }").unwrap();
    assert!(mogen_dsl::lower(&bad)
        .unwrap_err()
        .to_string()
        .contains("E0113"));
}

#[test]
fn length_and_coordinate_errors_do_not_fall_back_to_default_geometry() {
    for (source, code) in [
        (
            "scene { spline_tube (points=[[0,0,0],[0,1,0],[0,2,0]],radii=[0.1,0.2]) }",
            "E0112",
        ),
        (
            "scene { sweep (profile=[[0,0,0],[1,0,0],[1,1,0]]) }",
            "E0114",
        ),
        (
            "scene { spline_tube (radii=[[0.1,0.2],[0.3,0.4]]) }",
            "E0112",
        ),
    ] {
        let ast = mogen_dsl::parse(source).unwrap();
        assert!(mogen_validate::validate_ast(&ast)
            .iter()
            .any(|d| d.code == code));
        assert!(mogen_dsl::lower(&ast).is_err());
    }
    compile("scene { box (pos=[1,2,3],rot=[0,0,0],size=[1,2,3]) }");
    compile("scene { poly (points=[[0,0,0],[1,0,0],[0,1,0]],indices=[0,1,2]) }");
}
