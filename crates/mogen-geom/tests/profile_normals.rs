use glam::Vec3;
use mogen_core::{Mesh, UvMode};
use mogen_geom::{extrude_mesh, loft_mesh, sweep_mesh, SweepModulation};

fn assert_lighting_normals(mesh: Mesh) {
    assert!(!mesh.indices.is_empty());
    assert_eq!(mesh.normals.len(), mesh.positions.len());
    for normal in &mesh.normals {
        let n = Vec3::from_array(*normal);
        assert!(
            n.is_finite() && (n.length() - 1.0).abs() < 1e-5,
            "Surface must have a finite unit lighting normal, got {normal:?}"
        );
    }
    for tri in mesh.indices.chunks_exact(3) {
        let [a, b, c] = [tri[0] as usize, tri[1] as usize, tri[2] as usize];
        let p = |i| Vec3::from_array(mesh.positions[i]);
        let face = (p(b) - p(a)).cross(p(c) - p(a));
        if face.length_squared() > 1e-12 {
            for i in [a, b, c] {
                assert!(
                    face.dot(Vec3::from_array(mesh.normals[i])) > 0.0,
                    "Lighting normal must agree with the face winding"
                );
            }
        }
    }
}

fn profile() -> Vec<[f32; 2]> {
    vec![[-0.1, -0.1], [0.1, -0.1], [0.1, 0.1], [-0.1, 0.1]]
}

#[test]
fn swept_profile_has_usable_lighting_normals() {
    assert_lighting_normals(sweep_mesh(
        &profile(),
        &[[0.0, 0.0, 0.0], [0.0, 0.1, 0.5], [0.0, 0.0, 1.0]],
        8,
        0.0,
        &SweepModulation::default(),
        true,
        UvMode::default(),
    ));
}

#[test]
fn extruded_profile_has_usable_lighting_normals() {
    assert_lighting_normals(extrude_mesh(
        &profile(),
        &[],
        1.0,
        0.8,
        0.0,
        true,
        UvMode::default(),
    ));
}

#[test]
fn lofted_profile_has_usable_lighting_normals() {
    assert_lighting_normals(
        loft_mesh(
            &[profile(), profile()],
            &[0.0, 1.0],
            4,
            true,
            UvMode::default(),
        )
        .unwrap(),
    );
}
