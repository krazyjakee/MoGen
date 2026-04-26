    use super::flatten::{flatten, FLOATS_PER_VERTEX};
    use super::state::{
        apply_gizmo_drag, commit_gizmo_drag, snap_rotate_delta, snap_scale_factor,
        snap_translate_delta, GizmoDrag, PendingEdit, ViewerState, SCALE_SNAP_STEP,
    };
    use glam::{Mat4, Quat, Vec3};
    use mogen_core::{AlphaMode, Material, Mesh, NodeId, SceneGraph, Transform};
    use std::path::{Path, PathBuf};

    fn quad_mesh() -> Mesh {
        let mut m = Mesh::new(
            vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [1.0, 1.0, 0.0],
                [0.0, 1.0, 0.0],
            ],
            vec![[0.0, 0.0, 1.0]; 4],
            vec![0, 1, 2, 0, 2, 3],
        );
        m.uvs = vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        m
    }

    fn material_with_texture(name: &str, path: Option<&str>) -> Material {
        let mut m = Material::new(name);
        if let Some(p) = path {
            m.base_color_texture = Some(mogen_core::TextureRef::new(PathBuf::from(p)));
        }
        m
    }

    #[test]
    fn flatten_groups_nodes_by_material_id() {
        let mut scene = SceneGraph::new();
        let m_plain = scene.add_material(material_with_texture("plain", None));
        let m_a = scene.add_material(material_with_texture("a", Some("a.png")));
        let m_b = scene.add_material(material_with_texture("b", Some("b.png")));

        for (i, mat) in [m_plain, m_a, m_b, m_a].iter().enumerate() {
            let id = scene.add_root(format!("n{i}"), "primitive", Transform::IDENTITY);
            scene.set_mesh(id, quad_mesh());
            scene.set_material(id, *mat);
        }

        let mesh = flatten(&scene, None);
        assert_eq!(mesh.batches.len(), 3, "one batch per material id");

        let a_batch = mesh
            .batches
            .iter()
            .find(|b| b.base_color_texture.as_deref() == Some(Path::new("a.png")))
            .expect("a.png batch present");
        assert_eq!(a_batch.index_count, 12, "two m_a quads coalesce");

        let plain_batch = mesh
            .batches
            .iter()
            .find(|b| b.base_color_texture.is_none())
            .expect("plain batch present");
        assert_eq!(plain_batch.index_count, 6);

        let total: u32 = mesh.batches.iter().map(|b| b.index_count).sum();
        assert_eq!(total as usize, mesh.indices.len());

        assert_eq!(mesh.vertices.len(), 4 * 4 * FLOATS_PER_VERTEX);
    }

    #[test]
    fn flatten_skinned_mesh_emits_skin_batch_and_identity_palette_at_bind() {
        use mogen_core::Skin;

        let mut scene = SceneGraph::new();
        let bone = scene.add_root("bone", "bone", Transform::IDENTITY);
        let skin_id = scene.add_skin(Skin {
            name: "skel".into(),
            joints: vec![bone],
            inverse_bind_matrices: vec![Mat4::IDENTITY.to_cols_array_2d()],
            envelopes: Vec::new(),
            skeleton_root: Some(bone),
        });

        let mesh_node = scene.add_root("mesh", "primitive", Transform::IDENTITY);
        let mut m = quad_mesh();
        m.joints = vec![[0, 0, 0, 0]; 4];
        m.weights = vec![[1.0, 0.0, 0.0, 0.0]; 4];
        scene.set_mesh(mesh_node, m);
        scene.set_skin(mesh_node, skin_id);

        let flat = flatten(&scene, None);

        assert_eq!(flat.batches.len(), 1);
        assert_eq!(flat.batches[0].skin_id, Some(0));

        assert_eq!(flat.skins.len(), 1);
        assert_eq!(flat.skins[0].joint_matrices.len(), 1);
        let m = flat.skins[0].joint_matrices[0];
        for (a, b) in m
            .to_cols_array()
            .iter()
            .zip(Mat4::IDENTITY.to_cols_array().iter())
        {
            assert!((a - b).abs() < 1e-5, "bind palette must be identity");
        }

        let stride = FLOATS_PER_VERTEX;
        for v in 0..4 {
            let base = v * stride;
            assert_eq!(flat.vertices[base + 8], 0.0, "joint.x");
            assert_eq!(flat.vertices[base + 12], 1.0, "weight.x");
        }

        let _ = NodeId(0);
    }

    #[test]
    fn flatten_skinned_vertices_do_not_bake_world_transform() {
        use mogen_core::Skin;

        let mut scene = SceneGraph::new();
        let bone = scene.add_root("bone", "bone", Transform::IDENTITY);
        let skin_id = scene.add_skin(Skin {
            name: "skel".into(),
            joints: vec![bone],
            inverse_bind_matrices: vec![Mat4::IDENTITY.to_cols_array_2d()],
            envelopes: Vec::new(),
            skeleton_root: Some(bone),
        });
        let mesh_node = scene.add_root(
            "mesh",
            "primitive",
            Transform::from_trs(Vec3::new(10.0, 0.0, 0.0), Quat::IDENTITY, Vec3::ONE),
        );
        let mut m = quad_mesh();
        m.joints = vec![[0, 0, 0, 0]; 4];
        m.weights = vec![[1.0, 0.0, 0.0, 0.0]; 4];
        scene.set_mesh(mesh_node, m);
        scene.set_skin(mesh_node, skin_id);

        let flat = flatten(&scene, None);
        assert!(
            (flat.vertices[0]).abs() < 1e-5,
            "skinned mesh must not bake mesh-node translation into positions"
        );
    }

    #[test]
    fn flatten_resolves_relative_texture_paths_against_base_dir() {
        let mut scene = SceneGraph::new();
        let mat = scene.add_material(material_with_texture("a", Some("textures/a.png")));
        let id = scene.add_root("n", "primitive", Transform::IDENTITY);
        scene.set_mesh(id, quad_mesh());
        scene.set_material(id, mat);

        let base = PathBuf::from("/tmp/proj");
        let mesh = flatten(&scene, Some(&base));
        let textured = mesh
            .batches
            .iter()
            .find(|b| b.base_color_texture.is_some())
            .unwrap();
        assert_eq!(
            textured.base_color_texture.as_deref().unwrap(),
            Path::new("/tmp/proj/textures/a.png")
        );
    }

    #[test]
    fn flatten_emits_uvs_in_vertex_stream() {
        let mut scene = SceneGraph::new();
        let mat = scene.add_material(material_with_texture("a", Some("a.png")));
        let id = scene.add_root("n", "primitive", Transform::IDENTITY);
        scene.set_mesh(id, quad_mesh());
        scene.set_material(id, mat);

        let mesh = flatten(&scene, None);
        let stride = FLOATS_PER_VERTEX;
        let last = mesh.vertices.len() - stride;
        assert!((mesh.vertices[last + 6] - 0.0).abs() < 1e-6);
        assert!((mesh.vertices[last + 7] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn flatten_applies_material_uv_scale_to_vertex_stream() {
        // The GLB exporter multiplies mesh UVs by `material.uv_scale` at write
        // time; the viewer must do the same so the live preview tiles textures
        // identically to the exported asset.
        let mut scene = SceneGraph::new();
        let mut mat = material_with_texture("tiled", Some("a.png"));
        mat.uv_scale = [3.0, 5.0];
        let mid = scene.add_material(mat);
        let id = scene.add_root("n", "primitive", Transform::IDENTITY);
        scene.set_mesh(id, quad_mesh());
        scene.set_material(id, mid);

        let mesh = flatten(&scene, None);
        let stride = FLOATS_PER_VERTEX;
        let last = mesh.vertices.len() - stride;
        // quad_mesh's last vertex has raw uv [0, 1]; scaled by [3, 5] = [0, 5].
        assert!((mesh.vertices[last + 6] - 0.0).abs() < 1e-6);
        assert!((mesh.vertices[last + 7] - 5.0).abs() < 1e-6);
    }

    #[test]
    fn flatten_propagates_pbr_scalars_and_extra_texture_slots() {
        let mut scene = SceneGraph::new();
        let mut mat = Material::new("metal");
        mat.base_color = [0.2, 0.4, 0.6, 1.0];
        mat.metallic = 0.85;
        mat.roughness = 0.15;
        mat.emissive = [0.1, 0.2, 0.3];
        mat.emissive_strength = 2.5;
        mat.base_color_texture = Some(mogen_core::TextureRef::new(PathBuf::from("albedo.png")));
        mat.metallic_roughness_texture =
            Some(mogen_core::TextureRef::new(PathBuf::from("mr.png")));
        mat.normal_texture = Some(mogen_core::TextureRef::new(PathBuf::from("n.png")));
        mat.occlusion_texture = Some(mogen_core::TextureRef::new(PathBuf::from("ao.png")));
        mat.emissive_texture = Some(mogen_core::TextureRef::new(PathBuf::from("em.png")));
        let mid = scene.add_material(mat);
        let id = scene.add_root("n", "primitive", Transform::IDENTITY);
        scene.set_mesh(id, quad_mesh());
        scene.set_material(id, mid);

        let mesh = flatten(&scene, None);
        let b = &mesh.batches[0];
        assert_eq!(b.base_color, [0.2, 0.4, 0.6]);
        assert!((b.metallic - 0.85).abs() < 1e-6);
        assert!((b.roughness - 0.15).abs() < 1e-6);
        assert_eq!(b.emissive, [0.1, 0.2, 0.3]);
        assert!((b.emissive_strength - 2.5).abs() < 1e-6);
        assert_eq!(b.base_color_texture.as_deref(), Some(Path::new("albedo.png")));
        assert_eq!(
            b.metallic_roughness_texture.as_deref(),
            Some(Path::new("mr.png"))
        );
        assert_eq!(b.normal_texture.as_deref(), Some(Path::new("n.png")));
        assert_eq!(b.occlusion_texture.as_deref(), Some(Path::new("ao.png")));
        assert_eq!(b.emissive_texture.as_deref(), Some(Path::new("em.png")));
    }

    #[test]
    fn flatten_propagates_alpha_pipeline_and_centroid() {
        let mut scene = SceneGraph::new();
        let mut mat = Material::new("glass");
        mat.base_color = [0.4, 0.7, 0.9, 0.35];
        mat.alpha_mode = AlphaMode::Blend;
        mat.alpha_cutoff = 0.5;
        mat.double_sided = true;
        let mid = scene.add_material(mat);
        let id = scene.add_root(
            "n",
            "primitive",
            Transform::from_trs(Vec3::new(2.0, 0.0, 0.0), Quat::IDENTITY, Vec3::ONE),
        );
        scene.set_mesh(id, quad_mesh());
        scene.set_material(id, mid);

        let mesh = flatten(&scene, None);
        let b = &mesh.batches[0];
        assert_eq!(b.alpha_mode, AlphaMode::Blend);
        assert!((b.alpha_cutoff - 0.5).abs() < 1e-6);
        assert!(b.double_sided);
        assert!((b.base_color_alpha - 0.35).abs() < 1e-6);
        let expected = Vec3::new(2.5, 0.5, 0.0);
        assert!(
            (b.centroid - expected).length() < 1e-5,
            "got centroid {:?}, expected {:?}",
            b.centroid,
            expected
        );
    }

    #[test]
    fn snap_translate_rounds_to_quarter_grid_from_start() {
        let got = snap_translate_delta(0.45, 1.1);
        assert!((got - 1.05).abs() < 1e-5, "snap delta was {got}");
        let got = snap_translate_delta(0.0, 0.26);
        assert!((got - 0.25).abs() < 1e-5, "snap delta was {got}");
    }

    #[test]
    fn snap_rotate_rounds_to_fifteen_degrees() {
        use std::f32::consts::PI;
        let deg = |r: f32| r.to_degrees();
        assert!(
            (deg(snap_rotate_delta(22.0_f32.to_radians())) - 15.0).abs() < 1e-3,
            "got {}",
            deg(snap_rotate_delta(22.0_f32.to_radians()))
        );
        assert!((deg(snap_rotate_delta(38.0_f32.to_radians())) - 45.0).abs() < 1e-3);
        assert!((deg(snap_rotate_delta(-6.0_f32.to_radians())) - 0.0).abs() < 1e-3);
        assert!((deg(snap_rotate_delta(-8.0_f32.to_radians())) + 15.0).abs() < 1e-3);
        assert!(
            (snap_rotate_delta(2.0 * PI) - 2.0 * PI).abs() < 1e-4,
            "360° should remain 360°"
        );
    }

    #[test]
    fn snap_scale_factor_floors_at_step() {
        assert!((snap_scale_factor(1.1) - 1.0).abs() < 1e-5);
        assert!((snap_scale_factor(1.2) - 1.25).abs() < 1e-5);
        assert!((snap_scale_factor(0.0) - SCALE_SNAP_STEP).abs() < 1e-5);
        assert!((snap_scale_factor(-5.0) - SCALE_SNAP_STEP).abs() < 1e-5);
    }

    #[test]
    fn commit_gizmo_drag_is_noop_with_zero_delta() {
        let mut st = ViewerState::default();
        st.gizmo_drag = Some(GizmoDrag {
            node: NodeId(0),
            axis: crate::gizmo::Axis::X,
            mode: crate::gizmo::GizmoMode::Translate,
            start_transform: Transform::IDENTITY,
            start_origin: Vec3::ZERO,
            parent_start_world: Mat4::IDENTITY,
            start_ray_origin: Vec3::ZERO,
            start_ray_dir: Vec3::Z,
            delta: 0.0,
        });
        assert!(commit_gizmo_drag(&mut st).is_none());
    }

    #[test]
    fn commit_gizmo_drag_translate_emits_full_pos_vec3() {
        let mut st = ViewerState::default();
        st.gizmo_drag = Some(GizmoDrag {
            node: NodeId(3),
            axis: crate::gizmo::Axis::Y,
            mode: crate::gizmo::GizmoMode::Translate,
            start_transform: Transform::from_trs(
                Vec3::new(0.25, 0.5, 0.0),
                Quat::IDENTITY,
                Vec3::ONE,
            ),
            start_origin: Vec3::ZERO,
            parent_start_world: Mat4::IDENTITY,
            start_ray_origin: Vec3::ZERO,
            start_ray_dir: Vec3::Z,
            delta: 0.75,
        });
        let Some(PendingEdit::SetAttrCanonical {
            node,
            attr,
            value,
            delete,
        }) = commit_gizmo_drag(&mut st)
        else {
            panic!("expected SetAttrCanonical");
        };
        assert_eq!(node, NodeId(3));
        assert_eq!(attr, "pos");
        assert_eq!(value, "[0.25, 1.25, 0]");
        assert_eq!(delete, vec!["x", "y", "z", "from", "to"]);
    }

    #[test]
    fn commit_gizmo_drag_rotate_emits_euler_vec3() {
        let mut st = ViewerState::default();
        st.gizmo_drag = Some(GizmoDrag {
            node: NodeId(7),
            axis: crate::gizmo::Axis::Y,
            mode: crate::gizmo::GizmoMode::Rotate,
            start_transform: Transform::IDENTITY,
            start_origin: Vec3::ZERO,
            parent_start_world: Mat4::IDENTITY,
            start_ray_origin: Vec3::ZERO,
            start_ray_dir: Vec3::Z,
            delta: 45.0_f32.to_radians(),
        });
        let Some(PendingEdit::SetAttrCanonical {
            node,
            attr,
            value,
            delete,
        }) = commit_gizmo_drag(&mut st)
        else {
            panic!("expected SetAttrCanonical from non-trivial rotation");
        };
        assert_eq!(node, NodeId(7));
        assert_eq!(attr, "rot");
        assert_eq!(value, "[0, 45, 0]");
        assert_eq!(delete, vec!["rx", "ry", "rz"]);
    }

    #[test]
    fn translate_drag_pulls_world_delta_through_rotated_parent() {
        // Parent rotated +90° about Y. World +X drag of 1 unit must land
        // as +Z in the child's local translation so the post-compile world
        // position moves along world +X (not the parent's tilted X).
        let parent_rot = Quat::from_axis_angle(Vec3::Y, std::f32::consts::FRAC_PI_2);
        let parent_world = Mat4::from_quat(parent_rot);
        let mut st = ViewerState::default();
        st.gizmo_drag = Some(GizmoDrag {
            node: NodeId(1),
            axis: crate::gizmo::Axis::X,
            mode: crate::gizmo::GizmoMode::Translate,
            start_transform: Transform::IDENTITY,
            start_origin: Vec3::ZERO,
            parent_start_world: parent_world,
            start_ray_origin: Vec3::ZERO,
            start_ray_dir: Vec3::Z,
            delta: 1.0,
        });
        let Some(PendingEdit::SetAttrCanonical { value, .. }) = commit_gizmo_drag(&mut st) else {
            panic!("expected SetAttrCanonical");
        };
        assert_eq!(value, "[0, 0, 1]", "got {value}");
    }

    #[test]
    fn translate_drag_compensates_for_parent_scale() {
        // Parent scales 2x along Y. A 1-unit world-Y drag must shrink to
        // 0.5 in local space so the post-compile world translation is
        // exactly +1 unit, not +2.
        let parent_world = Mat4::from_scale(Vec3::new(1.0, 2.0, 1.0));
        let mut st = ViewerState::default();
        st.gizmo_drag = Some(GizmoDrag {
            node: NodeId(1),
            axis: crate::gizmo::Axis::Y,
            mode: crate::gizmo::GizmoMode::Translate,
            start_transform: Transform::IDENTITY,
            start_origin: Vec3::ZERO,
            parent_start_world: parent_world,
            start_ray_origin: Vec3::ZERO,
            start_ray_dir: Vec3::Z,
            delta: 1.0,
        });
        let Some(PendingEdit::SetAttrCanonical { value, .. }) = commit_gizmo_drag(&mut st) else {
            panic!("expected SetAttrCanonical");
        };
        assert_eq!(value, "[0, 0.5, 0]", "got {value}");
    }

    #[test]
    fn rotate_drag_conjugates_through_rotated_parent() {
        // Parent rotated +90° about Y, child starts identity.
        let parent_rot = Quat::from_axis_angle(Vec3::Y, std::f32::consts::FRAC_PI_2);
        let parent_world = Mat4::from_quat(parent_rot);

        // Drag the world-Y rotation handle 30°. Since Y is the parent's
        // invariant axis, the conjugation is the identity and the local
        // rotation lands as a pure +30° about Y.
        let mut st = ViewerState::default();
        st.gizmo_drag = Some(GizmoDrag {
            node: NodeId(1),
            axis: crate::gizmo::Axis::Y,
            mode: crate::gizmo::GizmoMode::Rotate,
            start_transform: Transform::IDENTITY,
            start_origin: Vec3::ZERO,
            parent_start_world: parent_world,
            start_ray_origin: Vec3::ZERO,
            start_ray_dir: Vec3::Z,
            delta: 30.0_f32.to_radians(),
        });
        let Some(PendingEdit::SetAttrCanonical { value, .. }) = commit_gizmo_drag(&mut st) else {
            panic!("expected SetAttrCanonical");
        };
        assert_eq!(value, "[0, 30, 0]", "got {value}");

        // Now drag the world-X rotation handle 30° under the same parent.
        // The local-space writeback won't be a pure +X rotation, but
        // recomposing parent_rot * local should equal a world-space +30°
        // about world +X.
        let mut st = ViewerState::default();
        st.gizmo_drag = Some(GizmoDrag {
            node: NodeId(1),
            axis: crate::gizmo::Axis::X,
            mode: crate::gizmo::GizmoMode::Rotate,
            start_transform: Transform::IDENTITY,
            start_origin: Vec3::ZERO,
            parent_start_world: parent_world,
            start_ray_origin: Vec3::ZERO,
            start_ray_dir: Vec3::Z,
            delta: 30.0_f32.to_radians(),
        });
        let local = apply_gizmo_drag(st.gizmo_drag.as_ref().unwrap());
        let world_rot = parent_rot * local.rotation;
        // Recover the world-space rotation the drag added: the node's
        // world rotation before the drag was just parent_rot (start
        // transform was identity), so right-multiplying by its inverse
        // peels that back off and what's left should be the +30° about
        // world X the user grabbed.
        let added_world = world_rot * parent_rot.inverse();
        let expected = Quat::from_axis_angle(Vec3::X, 30.0_f32.to_radians());
        let dot = added_world.dot(expected).abs();
        assert!(
            dot > 0.9999,
            "world-space rotation added by the drag should equal +30° about world X; got dot={dot}"
        );
    }
