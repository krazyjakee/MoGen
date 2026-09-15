use glam::{Mat4, Vec3};
use mogen_geom::sdf::{BlobChild, SdfOp, SdfPrim};

#[test]
fn surface_net_gradients_are_unit_normals_at_different_resolutions() {
    for resolution in [16, 40, 64] {
        let children = [
            BlobChild::new(
                SdfPrim::Sphere { radius: 0.3 },
                SdfOp::Add,
                Mat4::from_translation(Vec3::new(0.0, 0.3, 0.0)),
            ),
            BlobChild::new(
                SdfPrim::Sphere { radius: 0.2 },
                SdfOp::Add,
                Mat4::from_translation(Vec3::new(0.05, 0.6, 0.0)),
            ),
        ];
        let mesh = mogen_geom::blob_to_mesh(&children, 0.1, resolution);
        assert!(!mesh.indices.is_empty());
        let diagnostics = mogen_core::validate_renderable_mesh(&mesh);
        assert!(!mogen_core::has_errors(&diagnostics), "{diagnostics:?}");
        for (position, normal) in mesh.positions.iter().zip(&mesh.normals) {
            let p = Vec3::from_array(*position);
            let n = Vec3::from_array(*normal);
            assert!((n.length() - 1.0).abs() < 1e-5);
            if p.y < 0.2 {
                assert!(n.dot(p - Vec3::new(0.0, 0.3, 0.0)) > 0.0);
            }
        }
    }
}
