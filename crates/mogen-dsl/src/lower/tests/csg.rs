use super::*;

#[test]
fn final_boolean_normal_controls_cover_operations_and_transforms() {
    for op in ["union", "difference", "intersect"] {
        for segments in [12, 32] {
            let body=format!("{op} \"result\" (faceted=1) {{box(size=[2,2,2],ry=12) cylinder(radius=0.6,height=3,rz=90,segments={segments})}}");
            let graph = lower_src(&body);
            let mesh = find_mesh_node(&graph, "result").mesh.as_ref().unwrap();
            assert!(!mesh.indices.is_empty());
            for tri in mesh.indices.chunks_exact(3) {
                let p: [Vec3; 3] =
                    [0, 1, 2].map(|i| Vec3::from_array(mesh.positions[tri[i] as usize]));
                let normal = (p[1] - p[0]).cross(p[2] - p[0]).normalize_or_zero();
                for &i in tri {
                    assert!(normal.dot(Vec3::from_array(mesh.normals[i as usize])) > 0.999);
                }
            }
        }
    }
    let graph=lower_src("difference \"outer\" {union(subdivide=1){box(size=[2,2,2]) sphere(radius=1.3)} cylinder(radius=0.2,height=3)}");
    assert!(!find_mesh_node(&graph, "outer")
        .mesh
        .as_ref()
        .unwrap()
        .indices
        .is_empty());
}
#[test]
fn smooth_boolean_sphere_retains_curvature_across_seams() {
    for segments in [12, 32] {
        let source = format!(
            "union \"result\" (crease_angle=180) {{sphere(radius=1,segments={segments},rings=16)}}"
        );
        let graph = lower_src(&source);
        let mesh = find_mesh_node(&graph, "result").mesh.as_ref().unwrap();
        for n in &mesh.normals {
            assert!((Vec3::from_array(*n).length() - 1.0).abs() < 1e-5);
        }
        // UV spheres retain their pre-existing float-roundoff pole slivers;
        // don't treat their almost-zero-area winding as an analytic surface.
        // The longitudinal seam (including equatorial faces) is covered.
        for tri in mesh.indices.chunks_exact(3) {
            let points: [Vec3; 3] =
                [0, 1, 2].map(|i| Vec3::from_array(mesh.positions[tri[i] as usize]));
            if (points[1] - points[0])
                .cross(points[2] - points[0])
                .length()
                < 1e-6
            {
                continue;
            }
            for &id in tri {
                let p = Vec3::from_array(mesh.positions[id as usize]);
                let n = Vec3::from_array(mesh.normals[id as usize]);
                assert!(p.normalize().dot(n) > 0.97, "p={p:?} n={n:?}");
            }
        }
    }
}
#[test]
fn invalid_shading_options_are_source_aware() {
    for options in [
        "crease_angle=-1",
        "crease_angle=181",
        "crease_angle=40,faceted=1",
    ] {
        let ast = crate::parse(&format!("union \"body\" ({options}) {{box}}")).unwrap();
        let error = lower(&ast).unwrap_err().to_string();
        assert!(error.contains("body") && error.contains("bytes"), "{error}");
    }
}
