use glam::Vec3;
use mogen_core::{Mesh, SceneGraph, Transform};
use mogen_geom::measure::{closest_triangle_points, measure_surfaces, MeasureOptions};

fn scene(a: Vec<[Vec3; 3]>, b: Vec<[Vec3; 3]>) -> SceneGraph {
    let mut scene = SceneGraph::new();
    for (name, tris) in [("first", a), ("second", b)] {
        let id = scene.add_root(name, "mesh", Transform::IDENTITY);
        let positions: Vec<_> = tris.into_iter().flatten().map(|p| p.to_array()).collect();
        scene.set_mesh(
            id,
            Mesh {
                indices: (0..positions.len() as u32).collect(),
                positions,
                ..Default::default()
            },
        );
    }
    scene
}
fn triangle(z: f32) -> [Vec3; 3] {
    [
        Vec3::new(0., 0., z),
        Vec3::new(1., 0., z),
        Vec3::new(0., 1., z),
    ]
}

#[test]
fn gap_touch_and_piercing_intersection_use_actual_surfaces() {
    let a = triangle(0.);
    let b = triangle(0.01);
    let (p, q) = closest_triangle_points(a, b);
    assert!((p.distance(q) - 0.01).abs() < 1e-7);
    let (p, q) = closest_triangle_points(a, a);
    assert_eq!(p, q);
    // Edge pierces the horizontal face away from all of its edges/vertices.
    let b = [
        Vec3::new(0.2, 0.2, -1.),
        Vec3::new(0.2, 0.2, 1.),
        Vec3::new(0.3, 0.2, 1.),
    ];
    let (p, q) = closest_triangle_points(a, b);
    assert_eq!(p, q);
}

#[test]
fn overlapping_bounds_are_not_contact_evidence() {
    let a = triangle(0.);
    let b = [
        Vec3::new(0.6, 0.6, 0.),
        Vec3::new(1.6, 0.6, 0.),
        Vec3::new(0.6, 1.6, 0.),
    ];
    let scene = scene(vec![a], vec![b]);
    let m = measure_surfaces(&scene, scene.roots[0], scene.roots[1], Default::default()).unwrap();
    assert!(m.exact);
    assert_eq!(m.status, "separated");
    assert!((m.distance - (0.02_f64).sqrt()).abs() < 1e-6);
    assert!(m.direction_first_to_second.is_some());
}

#[test]
fn search_and_triangle_caps_are_explicit() {
    let a: Vec<_> = (0..100).map(|i| triangle(i as f32)).collect();
    let b: Vec<_> = (0..100).map(|i| triangle(i as f32 + 0.01)).collect();
    let scene = scene(a, b);
    let m = measure_surfaces(
        &scene,
        scene.roots[0],
        scene.roots[1],
        MeasureOptions {
            max_work: 1,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(m.work <= 1);
    assert!(!m.exact);
    assert_eq!(m.status, "inconclusive_budget");
    assert!(m.lower_bound <= m.distance);
    assert!(measure_surfaces(
        &scene,
        scene.roots[0],
        scene.roots[1],
        MeasureOptions {
            max_triangles: 10,
            ..Default::default()
        }
    )
    .unwrap_err()
    .to_string()
    .contains("triangle limit"));
}

#[test]
fn world_parent_transform_changes_distance_and_tight_bounds() {
    let mut scene = scene(vec![triangle(0.)], vec![triangle(0.01)]);
    let a = scene.roots[0];
    let b = scene.roots[1];
    scene.nodes[b.0 as usize].transform.translation.z = 0.09;
    let m = measure_surfaces(&scene, a, b, Default::default()).unwrap();
    assert!((m.distance - 0.1).abs() < 1e-6);
    let bounds = mogen_core::world_part_measurements(&scene);
    assert!((bounds[b.0 as usize].as_ref().unwrap().bounds.min.z - 0.1).abs() < 1e-6);
}
