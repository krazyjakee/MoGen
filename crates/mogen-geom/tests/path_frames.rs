use glam::Vec3;
use mogen_core::UvMode;
use mogen_geom::{sweep_mesh_oriented, sweep_path_frames, transport_path_frames, SweepModulation};

#[test]
fn frames_are_right_handed_on_all_axes_and_slopes() {
    for tangent in [
        Vec3::X,
        -Vec3::X,
        Vec3::Y,
        -Vec3::Y,
        Vec3::Z,
        -Vec3::Z,
        Vec3::new(0.1, 1.0, 0.2).normalize(),
    ] {
        let up = if tangent.y.abs() > 0.95 {
            Vec3::Z
        } else {
            Vec3::Y
        };
        let frames = transport_path_frames(&[Vec3::ZERO, tangent], up, false).unwrap();
        for f in frames {
            assert!(((-f.binormal).cross(f.normal) - f.tangent).length() < 1e-5);
            assert!(f.normal.dot(up) > 0.0);
        }
    }
}

#[test]
fn sampling_density_does_not_flip_explicit_height() {
    let points = [[0.0, 0.0, 0.0], [0.01, 0.8, 0.0], [0.1, 1.5, 0.1]];
    let (_, coarse) = sweep_path_frames(&points, 4, [0.0, 0.0, 1.0], false).unwrap();
    let (_, fine) = sweep_path_frames(&points, 16, [0.0, 0.0, 1.0], false).unwrap();
    for (i, f) in coarse.iter().enumerate() {
        assert!(f.normal.dot(fine[i * 4].normal) > 0.99);
    }
}

#[test]
fn flat_armrest_has_horizontal_width_and_explicit_roll() {
    let profile = [[-0.1, -0.02], [0.1, -0.02], [0.1, 0.02], [-0.1, 0.02]];
    let path = [[0.0, 0.0, 0.0], [0.0, 0.0, 1.0]];
    for (roll, expected_x) in [(0.0, 0.1), (std::f32::consts::FRAC_PI_2, 0.02)] {
        let mesh = sweep_mesh_oriented(
            &profile,
            &path,
            4,
            0.0,
            &SweepModulation {
                roll: vec![roll],
                scale: vec![],
            },
            true,
            UvMode::default(),
            [0.0, 1.0, 0.0],
            false,
        )
        .unwrap();
        let max_x = mesh
            .positions
            .iter()
            .map(|p| p[0].abs())
            .fold(0.0_f32, f32::max);
        assert!((max_x - expected_x).abs() < 1e-5);
    }
}

#[test]
fn closed_loop_seam_has_matching_frames_positions_and_no_caps() {
    let path = [
        [1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0],
        [-1.0, 0.0, 0.0],
        [0.0, 0.0, -1.0],
    ];
    let (_, frames) = sweep_path_frames(&path, 8, [0.0, 1.0, 0.0], true).unwrap();
    assert_eq!(
        frames.first().unwrap().normal,
        frames.last().unwrap().normal
    );
    let profile = [[-0.03, -0.02], [0.03, -0.02], [0.03, 0.02], [-0.03, 0.02]];
    let mesh = sweep_mesh_oriented(
        &profile,
        &path,
        8,
        0.0,
        &Default::default(),
        true,
        UvMode::default(),
        [0.0, 1.0, 0.0],
        true,
    )
    .unwrap();
    assert_eq!(mesh.positions.len(), frames.len() * 5);
    for j in 0..5 {
        let last = (frames.len() - 1) * 5 + j;
        assert_eq!(mesh.positions[j], mesh.positions[last]);
        assert_eq!(mesh.normals[j], mesh.normals[last]);
    }
}

#[test]
fn ambiguous_hints_and_path_reversals_are_actionable() {
    for (points, up) in [
        (vec![Vec3::ZERO, Vec3::Y], Vec3::Y),
        (vec![Vec3::ZERO, Vec3::ZERO], Vec3::Y),
        (vec![Vec3::ZERO, Vec3::X, Vec3::ZERO], Vec3::Y),
    ] {
        let error = transport_path_frames(&points, up, false)
            .unwrap_err()
            .to_string();
        assert!(error.contains("E0130"));
    }
}
