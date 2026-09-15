use glam::Vec3;

#[test]
fn cushion_resize_and_roundness_keep_the_welt_normal_offset() {
    let source = include_str!("../../../examples/furniture/guided_cushion.mog");
    for params in [
        "w=0.7,h=0.15,d=0.5,boxiness=1",
        "w=1.2,h=0.3,d=0.8,boxiness=4",
    ] {
        let source = source.replace("use \"cushion\" ()", &format!("use \"cushion\" ({params})"));
        let scene = mogen_dsl::lower(&mogen_dsl::parse(&source).unwrap()).unwrap();
        let guide = &scene.guides[0];
        let welt = scene.nodes.iter().find(|n| n.name == "welt").unwrap();
        assert_eq!(welt.parent, Some(guide.target));
        let mesh = welt.mesh.as_ref().unwrap();
        assert_eq!(mesh.positions.len(), guide.points.len() * 5);
        for (i, (point, normal)) in guide.points.iter().zip(&guide.normals).enumerate() {
            let center = mesh.positions[i * 5..i * 5 + 4]
                .iter()
                .map(|p| Vec3::from_array(*p))
                .sum::<Vec3>()
                / 4.0;
            let expected = Vec3::from_array(*point) + Vec3::from_array(*normal) * 0.002;
            assert!(center.distance(expected) < 1e-5);
        }
        for j in 0..5 {
            assert_eq!(
                mesh.positions[j],
                mesh.positions[(guide.points.len() - 1) * 5 + j]
            );
        }
        assert_ne!(welt.material, scene.get(guide.target).material);
    }
}

#[test]
fn target_tessellation_does_not_change_guide_identity_or_samples() {
    let source = include_str!("../../../examples/furniture/guided_cushion.mog");
    let low = mogen_dsl::lower(&mogen_dsl::parse(source).unwrap()).unwrap();
    let high = mogen_dsl::lower(
        &mogen_dsl::parse(&source.replace("rings=32,segments=64", "rings=48,segments=96")).unwrap(),
    )
    .unwrap();
    assert_eq!(low.guides[0].points, high.guides[0].points);
    assert_eq!(low.guides[0].name, high.guides[0].name);
}

#[test]
fn curved_frame_trim_compiles_and_invalid_guides_fail() {
    let source = include_str!("../../../examples/features/guided_frame_trim.mog");
    let scene = mogen_dsl::lower(&mogen_dsl::parse(source).unwrap()).unwrap();
    assert!(!scene.guides[0].closed);
    for bad in [
        source.replace("edge=2", "edge=99"),
        source.replace("target=\"frame\"", "target=\"missing\""),
    ] {
        assert!(mogen_dsl::lower(&mogen_dsl::parse(&bad).unwrap())
            .unwrap_err()
            .to_string()
            .contains("E0160"));
    }
}

#[test]
fn boxy_cushion_guide_bounds_geometric_error_within_its_sample_budget() {
    let source = include_str!("../../../examples/furniture/guided_cushion.mog");
    let scene = mogen_dsl::lower(&mogen_dsl::parse(source).unwrap()).unwrap();
    let guide = &scene.guides[0];
    assert!(guide.points.len() <= 4097);
    let points: Vec<_> = guide.points.iter().map(|p| Vec3::from_array(*p)).collect();
    for i in 0..4096 {
        let angle = i as f64 * std::f64::consts::TAU / 4096.0;
        let (s, c) = angle.sin_cos();
        let p = Vec3::new(
            (0.4 * c.signum() * c.abs().powf(1.0 / 3.0)) as f32,
            0.0,
            (0.325 * s.signum() * s.abs().powf(1.0 / 3.0)) as f32,
        );
        let distance = points
            .windows(2)
            .map(|w| {
                let q = mogen_geom::measure::closest_segment_points(p, p, w[0], w[1]).1;
                p.distance(q)
            })
            .fold(f32::INFINITY, f32::min);
        assert!(distance <= guide.tolerance, "sample {i}: {distance}");
    }
}
