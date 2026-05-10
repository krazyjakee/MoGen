    use super::flatten::{flatten, PaletteSource, FLOATS_PER_VERTEX};
    use super::state::{
        apply_gizmo_drag, commit_gizmo_drag, find_deepest_node_at_offset, gizmo_handles_supported,
        is_import_wrapper, node_path, redirect_pick, replace_selection, replace_selection_cycling,
        resolve_node_path, snap_rotate_delta, snap_scale_factor, snap_translate_delta,
        toggle_selection, GizmoDrag, PendingEdit, ViewerState, PICK_CYCLE_RADIUS_PX,
        SCALE_SNAP_STEP,
    };
    use eframe::egui;
    use glam::{Mat4, Quat, Vec3};
    use mogen_core::{AlphaMode, Material, Mesh, NodeId, SceneGraph, Span, Transform};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

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
            origin: None,
        });

        let mesh_node = scene.add_root("mesh", "primitive", Transform::IDENTITY);
        let mut m = quad_mesh();
        m.joints = vec![[0, 0, 0, 0]; 4];
        m.weights = vec![[1.0, 0.0, 0.0, 0.0]; 4];
        scene.set_mesh(mesh_node, m);
        scene.set_skin(mesh_node, skin_id);

        let flat = flatten(&scene, None);

        assert_eq!(flat.batches.len(), 1);
        let palette_id = flat.batches[0].palette_id as usize;
        assert!(matches!(
            flat.palette_sources[palette_id],
            PaletteSource::Skin { skin_id: 0 }
        ));

        assert_eq!(flat.palettes.len(), 1);
        assert_eq!(flat.palettes[palette_id].joint_matrices.len(), 1);
        let m = flat.palettes[palette_id].joint_matrices[0];
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
    fn flatten_rigid_batch_emits_per_node_palette_with_single_bone_weights() {
        // Two rigid nodes share a material → one batch, palette with two
        // entries. Each vertex gets `joints[0] = node_index_in_palette` and
        // `weights = [1, 0, 0, 0]` so the runtime shader applies that node's
        // current world matrix even when nothing is animating.
        let mut scene = SceneGraph::new();
        let mat = scene.add_material(material_with_texture("a", None));
        let a = scene.add_root(
            "a",
            "primitive",
            Transform::from_trs(Vec3::ZERO, Quat::IDENTITY, Vec3::ONE),
        );
        scene.set_mesh(a, quad_mesh());
        scene.set_material(a, mat);
        let b = scene.add_root(
            "b",
            "primitive",
            Transform::from_trs(Vec3::new(5.0, 0.0, 0.0), Quat::IDENTITY, Vec3::ONE),
        );
        scene.set_mesh(b, quad_mesh());
        scene.set_material(b, mat);

        let flat = flatten(&scene, None);

        assert_eq!(flat.batches.len(), 1, "shared material → one batch");
        let palette_id = flat.batches[0].palette_id as usize;
        match &flat.palette_sources[palette_id] {
            PaletteSource::Rigid {
                nodes,
                inv_rest_worlds,
            } => {
                assert_eq!(nodes.len(), 2);
                assert_eq!(inv_rest_worlds.len(), 2);
            }
            _ => panic!("expected rigid palette source"),
        }

        // First quad's verts (4 of them) should reference palette slot 0;
        // second quad's should reference slot 1. weights[0] is always 1.
        let stride = FLOATS_PER_VERTEX;
        for v in 0..4 {
            let base = v * stride;
            assert_eq!(flat.vertices[base + 8], 0.0, "first quad joints[0]");
            assert_eq!(flat.vertices[base + 12], 1.0, "first quad weights[0]");
        }
        for v in 4..8 {
            let base = v * stride;
            assert_eq!(flat.vertices[base + 8], 1.0, "second quad joints[0]");
            assert_eq!(flat.vertices[base + 12], 1.0, "second quad weights[0]");
        }
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
            origin: None,
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
        assert!(commit_gizmo_drag(&mut st).is_empty());
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
        let mut edits = commit_gizmo_drag(&mut st).into_iter();
        let Some(PendingEdit::SetAttrCanonical {
            node,
            attr,
            value,
            delete,
        }) = edits.next()
        else {
            panic!("expected SetAttrCanonical");
        };
        assert!(edits.next().is_none(), "non-relative_placed should emit one edit");
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
        let mut edits = commit_gizmo_drag(&mut st).into_iter();
        let Some(PendingEdit::SetAttrCanonical {
            node,
            attr,
            value,
            delete,
        }) = edits.next()
        else {
            panic!("expected SetAttrCanonical from non-trivial rotation");
        };
        assert!(edits.next().is_none(), "rotate should emit a single edit");
        assert_eq!(node, NodeId(7));
        assert_eq!(attr, "rot");
        assert_eq!(value, "[0, 45, 0]");
        assert_eq!(delete, vec!["rx", "ry", "rz", "dir"]);
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
        let edits = commit_gizmo_drag(&mut st);
        let Some(PendingEdit::SetAttrCanonical { value, .. }) = edits.into_iter().next() else {
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
        let edits = commit_gizmo_drag(&mut st);
        let Some(PendingEdit::SetAttrCanonical { value, .. }) = edits.into_iter().next() else {
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
        let edits = commit_gizmo_drag(&mut st);
        let Some(PendingEdit::SetAttrCanonical { value, .. }) = edits.into_iter().next() else {
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

    /// Build a scene mirroring the office assetpack pattern:
    ///   group "lptp" { use "laptop" }
    /// — a user-authored wrapper group with one imported child carrying a
    /// non-`None` `use_id` AND `origin = Some(path)` (the latter is what
    /// distinguishes a real cross-file import from a same-file `use`
    /// expansion: only imported nodes have a foreign source path stamped
    /// on them by `set_origin_recursive`). The wrapper has
    /// `use_id = None`; the imported child has `use_id = Some(7)` and
    /// `origin = Some("laptop.mog")`. Returns `(wrapper_id, imported_id)`.
    fn scene_with_imported_child() -> (SceneGraph, NodeId, NodeId) {
        let mut scene = SceneGraph::new();
        let wrapper = scene.add_root("lptp", "group", Transform::IDENTITY);
        let imported = scene.add_child(wrapper, "laptop_body", "box", Transform::IDENTITY);
        scene.nodes[imported.0 as usize].use_id = Some(7);
        scene.nodes[imported.0 as usize].origin =
            Some(std::path::PathBuf::from("laptop.mog"));
        (scene, wrapper, imported)
    }

    #[test]
    fn redirect_pick_returns_user_authored_node_unchanged() {
        let (scene, wrapper, _) = scene_with_imported_child();
        assert_eq!(redirect_pick(&scene, wrapper), Some(wrapper));
    }

    #[test]
    fn redirect_pick_walks_up_to_wrapper_for_imported_child() {
        let (scene, wrapper, imported) = scene_with_imported_child();
        assert_eq!(redirect_pick(&scene, imported), Some(wrapper));
    }

    #[test]
    fn redirect_pick_walks_through_nested_imported_chain() {
        // Outer wrapper → imported group → imported leaf. Both imported
        // nodes share the same `use_id` (matches how nested module bodies
        // are flattened by `expand_node_into`). The redirect must skip past
        // the inner imported group, not stop at it.
        let mut scene = SceneGraph::new();
        let wrapper = scene.add_root("dsk", "group", Transform::IDENTITY);
        let inner = scene.add_child(wrapper, "desk_top", "group", Transform::IDENTITY);
        scene.nodes[inner.0 as usize].use_id = Some(3);
        let leaf = scene.add_child(inner, "desk_top_box", "box", Transform::IDENTITY);
        scene.nodes[leaf.0 as usize].use_id = Some(3);
        assert_eq!(redirect_pick(&scene, leaf), Some(wrapper));
    }

    #[test]
    fn redirect_pick_returns_none_when_no_user_authored_ancestor() {
        // `scene { use "desk" }` with no wrapping group: the imported node
        // is a root, every parent walk halts immediately with no
        // user-authored wrapper. The redirect bails to `None` so picking
        // doesn't latch onto a node we can't safely write back. `origin`
        // must be `Some(...)` here — that's what marks the node as
        // imported (its `source_span` lives in another file). A
        // locally-expanded `use` of a module declared in the same `.mog`
        // would have `origin = None` and the redirect would return self.
        let mut scene = SceneGraph::new();
        let imported = scene.add_root("desk_top", "box", Transform::IDENTITY);
        scene.nodes[imported.0 as usize].use_id = Some(1);
        scene.nodes[imported.0 as usize].origin =
            Some(std::path::PathBuf::from("desk.mog"));
        assert_eq!(redirect_pick(&scene, imported), None);
    }

    #[test]
    fn redirect_pick_returns_self_for_local_module_top_level_node() {
        // `module "outfit" () { box "panel" (...) }` followed by
        // `use "outfit" ()` — the expanded `panel` lands at scene root with
        // `use_id = Some(...)` and `origin = None` (its `source_span`
        // points at editable bytes in the active file). The redirect must
        // return the panel itself; otherwise the viewport click wipes the
        // selection and the user can't grab the gizmo handle on local
        // outfit / clothing modules.
        let mut scene = SceneGraph::new();
        let panel = scene.add_root("panel", "box", Transform::IDENTITY);
        scene.nodes[panel.0 as usize].use_id = Some(7);
        // origin stays None — local module body sits in the active source.
        assert_eq!(redirect_pick(&scene, panel), Some(panel));
    }

    #[test]
    fn replace_selection_redirects_pick_to_wrapper() {
        // Picking the imported child must land selection on the wrapper —
        // the gizmo + inspector both read `st.selected`, so this is what
        // makes the visual editor "edit the group, not the import".
        let (scene, wrapper, imported) = scene_with_imported_child();
        let mut st = ViewerState::default();
        st.scene = Some(Arc::new(scene));
        replace_selection(&mut st, Some(imported));
        assert_eq!(st.selected, vec![wrapper]);
        assert_eq!(
            st.selected_paths,
            vec![vec![("lptp".to_string(), 0u32)]],
        );
    }

    #[test]
    fn replace_selection_clears_selection_when_redirect_finds_no_wrapper() {
        // Bare imported root (no enclosing user-authored group): the
        // redirect returns `None`, so the viewer should clear its
        // selection rather than latch onto an un-editable node. Imported
        // nodes carry `origin = Some(path)`; without that flag the
        // redirect treats the node as a local module body and returns
        // self.
        let mut scene = SceneGraph::new();
        let imported = scene.add_root("desk_top", "box", Transform::IDENTITY);
        scene.nodes[imported.0 as usize].use_id = Some(1);
        scene.nodes[imported.0 as usize].origin =
            Some(std::path::PathBuf::from("desk.mog"));
        let mut st = ViewerState::default();
        st.scene = Some(Arc::new(scene));
        replace_selection(&mut st, Some(imported));
        assert!(st.selected.is_empty());
        assert!(st.selected_paths.is_empty());
    }

    /// Build a tiny scene whose nodes carry hand-rolled `source_span`s so
    /// the offset-lookup tests can pick precise byte positions without
    /// running the real DSL parser. Layout (byte offsets in brackets):
    ///   `[0..40] group "outer" { [10..30] box "inner" { } }`
    /// — a parent span that fully contains a child span. The third node
    /// (`imported`) is a sibling of `inner` whose `origin = Some(...)` to
    /// exercise the imported-skip rule.
    fn scene_with_overlapping_spans() -> (SceneGraph, NodeId, NodeId, NodeId) {
        let mut scene = SceneGraph::new();
        let outer = scene.add_root("outer", "group", Transform::IDENTITY);
        let inner = scene.add_child(outer, "inner", "box", Transform::IDENTITY);
        let imported = scene.add_child(outer, "imported", "box", Transform::IDENTITY);
        scene.nodes[outer.0 as usize].source_span = Some(Span::new(0, 40));
        scene.nodes[inner.0 as usize].source_span = Some(Span::new(10, 30));
        // Picked to overlap `inner`'s range so the deepest-by-length tiebreak
        // genuinely depends on whether we let an imported span participate.
        scene.nodes[imported.0 as usize].source_span = Some(Span::new(15, 25));
        scene.nodes[imported.0 as usize].origin = Some(PathBuf::from("other.mog"));
        (scene, outer, inner, imported)
    }

    #[test]
    fn find_deepest_node_at_offset_picks_smallest_containing_span() {
        // Offset 20 sits inside both the outer group (0..40) and the inner
        // box (10..30). The deepest match wins so a code-side click on the
        // child's source line selects the child, not its enclosing group.
        let (scene, _outer, inner, _imported) = scene_with_overlapping_spans();
        assert_eq!(find_deepest_node_at_offset(&scene, 20), Some(inner));
    }

    #[test]
    fn find_deepest_node_at_offset_skips_imported_nodes() {
        // The imported sibling's span (15..25) is the smallest one covering
        // offset 20, but it lives in another file — selecting it would land
        // the gizmo at byte offsets that don't index the active source.
        // The lookup must skip it and fall back to the inner user-authored
        // node instead.
        let (scene, _outer, inner, _imported) = scene_with_overlapping_spans();
        assert_eq!(find_deepest_node_at_offset(&scene, 20), Some(inner));
    }

    #[test]
    fn find_deepest_node_at_offset_falls_back_to_outer_when_only_outer_contains() {
        // Offset 5 lands inside the outer group's span but before the inner
        // child's. The outer group is the only valid candidate.
        let (scene, outer, _inner, _imported) = scene_with_overlapping_spans();
        assert_eq!(find_deepest_node_at_offset(&scene, 5), Some(outer));
    }

    #[test]
    fn find_deepest_node_at_offset_returns_none_outside_every_span() {
        // Offset past every node's range — represents a click in trailing
        // whitespace or a top-level comment. The caller preserves the
        // existing selection in that case rather than treating it as a
        // deselect.
        let (scene, _outer, _inner, _imported) = scene_with_overlapping_spans();
        assert_eq!(find_deepest_node_at_offset(&scene, 1000), None);
    }

    #[test]
    fn find_deepest_node_at_offset_treats_span_end_as_exclusive() {
        // A caret resting exactly at a node's `span.end` belongs to whatever
        // structure starts there (or to none). Without the half-open
        // boundary, two adjacent siblings with `prev.end == next.start`
        // would both claim the boundary offset and the deepest-by-length
        // tiebreak would silently pick whichever happened to enumerate last.
        let mut scene = SceneGraph::new();
        let a = scene.add_root("a", "box", Transform::IDENTITY);
        let b = scene.add_root("b", "box", Transform::IDENTITY);
        scene.nodes[a.0 as usize].source_span = Some(Span::new(0, 10));
        scene.nodes[b.0 as usize].source_span = Some(Span::new(10, 20));
        // 9 → still inside `a`.
        assert_eq!(find_deepest_node_at_offset(&scene, 9), Some(a));
        // 10 → boundary; belongs to `b`, not `a`.
        assert_eq!(find_deepest_node_at_offset(&scene, 10), Some(b));
    }

    /// Three-sibling scene with three user-authored boxes under one root.
    /// Used by the toggle-selection tests to exercise add / remove / primary
    /// transitions. Returns `(scene, [a, b, c])`.
    fn scene_with_three_siblings() -> (SceneGraph, [NodeId; 3]) {
        let mut scene = SceneGraph::new();
        let root = scene.add_root("scene", "group", Transform::IDENTITY);
        let a = scene.add_child(root, "a", "box", Transform::IDENTITY);
        let b = scene.add_child(root, "b", "box", Transform::IDENTITY);
        let c = scene.add_child(root, "c", "box", Transform::IDENTITY);
        (scene, [a, b, c])
    }

    #[test]
    fn node_path_round_trips_same_named_replicas() {
        // Three siblings sharing one name — what `array(...)` /
        // `mirror` produce. Without the sibling-disambiguator, all three
        // collapse to the first match on resolve, so a gizmo move on any
        // copy would re-select the first one after the recompile.
        let mut scene = SceneGraph::new();
        let root = scene.add_root("scene", "group", Transform::IDENTITY);
        let a = scene.add_child(root, "leg", "box", Transform::IDENTITY);
        let b = scene.add_child(root, "leg", "box", Transform::IDENTITY);
        let c = scene.add_child(root, "leg", "box", Transform::IDENTITY);

        let pa = node_path(&scene, a).unwrap();
        let pb = node_path(&scene, b).unwrap();
        let pc = node_path(&scene, c).unwrap();
        assert_eq!(pa.last().unwrap().1, 0);
        assert_eq!(pb.last().unwrap().1, 1);
        assert_eq!(pc.last().unwrap().1, 2);

        assert_eq!(resolve_node_path(&scene, &pa), Some(a));
        assert_eq!(resolve_node_path(&scene, &pb), Some(b));
        assert_eq!(resolve_node_path(&scene, &pc), Some(c));
    }

    #[test]
    fn toggle_selection_appends_new_node_as_primary() {
        let (scene, [a, b, _]) = scene_with_three_siblings();
        let mut st = ViewerState::default();
        st.scene = Some(Arc::new(scene));
        replace_selection(&mut st, Some(a));
        toggle_selection(&mut st, b);
        assert_eq!(st.selected, vec![a, b]);
        assert_eq!(st.primary_selected(), Some(b));
    }

    #[test]
    fn toggle_selection_removes_already_selected_node() {
        let (scene, [a, b, _]) = scene_with_three_siblings();
        let mut st = ViewerState::default();
        st.scene = Some(Arc::new(scene));
        replace_selection(&mut st, Some(a));
        toggle_selection(&mut st, b);
        toggle_selection(&mut st, b);
        assert_eq!(st.selected, vec![a]);
        assert_eq!(st.primary_selected(), Some(a));
    }

    #[test]
    fn toggle_selection_removing_primary_promotes_previous_to_primary() {
        // a then b — b is primary. Toggling b removes it; a remains and
        // becomes primary again.
        let (scene, [a, b, _]) = scene_with_three_siblings();
        let mut st = ViewerState::default();
        st.scene = Some(Arc::new(scene));
        replace_selection(&mut st, Some(a));
        toggle_selection(&mut st, b);
        assert_eq!(st.primary_selected(), Some(b));
        toggle_selection(&mut st, b);
        assert_eq!(st.primary_selected(), Some(a));
        assert_eq!(st.selected.len(), 1);
    }

    #[test]
    fn toggle_selection_redirects_through_imported_subtree() {
        // Toggling an imported node should land on the wrapper, mirroring
        // what `replace_selection` does for plain clicks. Otherwise
        // shift-click on an imported leaf would silently no-op.
        let (scene, wrapper, imported) = scene_with_imported_child();
        let mut st = ViewerState::default();
        st.scene = Some(Arc::new(scene));
        toggle_selection(&mut st, imported);
        assert_eq!(st.selected, vec![wrapper]);
    }

    #[test]
    fn gizmo_handles_refused_for_imported_subtree() {
        // Defense-in-depth: if a stale `selected_path` resolves into an
        // imported subtree post-recompile, the gizmo handles must still
        // refuse. Otherwise the user could grab a handle on a node whose
        // span points at a different file.
        let (scene, _, imported) = scene_with_imported_child();
        assert!(!gizmo_handles_supported(
            &scene,
            imported,
            crate::gizmo::GizmoMode::Translate,
        ));
    }

    #[test]
    fn gizmo_handles_allowed_on_user_wrapper_around_use() {
        let (scene, wrapper, _) = scene_with_imported_child();
        assert!(gizmo_handles_supported(
            &scene,
            wrapper,
            crate::gizmo::GizmoMode::Translate,
        ));
    }

    #[test]
    fn gizmo_handles_allowed_on_local_module_expansion() {
        // `module "outfit" () { box "panel" (...) }` followed by
        // `use "outfit" ()` — the expanded `panel` has
        // `use_id = Some(...)` (it came from a `use` call) but
        // `origin = None` (the module body lives in the active source, so
        // the panel's `source_span` is still editable). The gizmo must be
        // allowed; without this, drag-to-edit on local outfit / clothing
        // modules silently no-ops.
        let mut scene = SceneGraph::new();
        let panel = scene.add_root("panel", "box", Transform::IDENTITY);
        scene.nodes[panel.0 as usize].use_id = Some(7);
        // origin stays None.
        assert!(gizmo_handles_supported(
            &scene,
            panel,
            crate::gizmo::GizmoMode::Translate,
        ));
    }

    /// Mirror the office assetpack pattern: a top-level
    /// `use "watercooler" (pos=...)` of an imported file. After expansion
    /// the synthesised wrapper group has `use_id = Some(...)` (it opens a
    /// new frame) and `origin = None` (the `use` line lives in the active
    /// source); its imported body has `use_id = Some(...)` (same frame)
    /// and `origin = Some("watercooler.mog")`. The wrapper is a scene root
    /// with no parent, so the plain walk-up to `use_id == None` finds no
    /// match. Returns `(scene, wrapper_id, imported_body_id)`.
    fn scene_with_imported_use_wrapper() -> (SceneGraph, NodeId, NodeId) {
        let mut scene = SceneGraph::new();
        let wrapper = scene.add_root("watercooler", "group", Transform::IDENTITY);
        scene.nodes[wrapper.0 as usize].use_id = Some(2);
        scene.nodes[wrapper.0 as usize].origin = None;
        let body = scene.add_child(wrapper, "lower_cabinet", "post", Transform::IDENTITY);
        scene.nodes[body.0 as usize].use_id = Some(2);
        scene.nodes[body.0 as usize].origin = Some(PathBuf::from("watercooler.mog"));
        (scene, wrapper, body)
    }

    #[test]
    fn redirect_pick_lands_on_import_wrapper_when_wrapper_is_a_root() {
        // Regression for the office assetpack bug: a top-level
        // `use "watercooler" (pos=...)` of an imported file used to be
        // unselectable because the redirect walked up looking for a
        // `use_id == None` ancestor and bottomed out at `parent = None`.
        // The wrapper itself is editable (its span is the `use` line in
        // the active source), so we now fall back to it.
        let (scene, wrapper, body) = scene_with_imported_use_wrapper();
        assert_eq!(redirect_pick(&scene, body), Some(wrapper));
    }

    #[test]
    fn is_import_wrapper_detects_use_wrapper_for_imported_file() {
        let (scene, wrapper, body) = scene_with_imported_use_wrapper();
        assert!(is_import_wrapper(&scene, wrapper));
        // The body itself is imported (origin=Some), so it's not the
        // wrapper — only the active-source synthesised group is.
        assert!(!is_import_wrapper(&scene, body));
    }

    #[test]
    fn is_import_wrapper_rejects_local_use_wrapper() {
        // A `use` of a locally-defined module (no import involved) also
        // produces a wrapper with `use_id = Some(...)` and `origin = None`,
        // but its body has `origin = None` too. We must NOT treat that as
        // an import wrapper — the existing redirect-up-to-the-user-group
        // behavior is the right answer for local prototypes.
        let mut scene = SceneGraph::new();
        let wrapper = scene.add_root("legs", "group", Transform::IDENTITY);
        scene.nodes[wrapper.0 as usize].use_id = Some(5);
        let body = scene.add_child(wrapper, "leg", "cylinder", Transform::IDENTITY);
        scene.nodes[body.0 as usize].use_id = Some(5);
        let _ = body;
        assert!(!is_import_wrapper(&scene, wrapper));
    }

    #[test]
    fn gizmo_handles_allowed_on_import_wrapper() {
        // The wrapper group of `use "watercooler" (pos=...)` for an
        // imported file is editable: dragging its gizmo writes pos= back
        // to the `use` line in the active source.
        let (scene, wrapper, _) = scene_with_imported_use_wrapper();
        assert!(gizmo_handles_supported(
            &scene,
            wrapper,
            crate::gizmo::GizmoMode::Translate,
        ));
    }

    #[test]
    fn gizmo_handles_still_refused_on_imported_body_inside_wrapper() {
        // Defense-in-depth: even with the wrapper exemption, the imported
        // body itself stays un-editable — its source span lives in the
        // imported file, so a writeback would corrupt the wrong file.
        let (scene, _, body) = scene_with_imported_use_wrapper();
        assert!(!gizmo_handles_supported(
            &scene,
            body,
            crate::gizmo::GizmoMode::Translate,
        ));
    }

    #[test]
    fn replace_selection_lands_on_import_wrapper() {
        let (scene, wrapper, body) = scene_with_imported_use_wrapper();
        let mut st = ViewerState::default();
        st.scene = Some(Arc::new(scene));
        replace_selection(&mut st, Some(body));
        assert_eq!(st.selected, vec![wrapper]);
    }

    #[test]
    fn gizmo_handles_supported_for_relative_placed_in_every_mode() {
        // `above="..."` (and friends) recompute one axis of translation each
        // compile, but the Translate commit emits per-axis shortcuts that
        // trip `pos_axis_explicit` and freeze the dragged position. Rotate
        // and Scale aren't touched by the layout pass at all. So none of the
        // three modes should refuse handles on a `relative_placed` node.
        let mut scene = SceneGraph::new();
        let parent = scene.add_root("group", "group", Transform::IDENTITY);
        let child = scene.add_child(parent, "tier2", "box", Transform::IDENTITY);
        scene.nodes[child.0 as usize].relative_placed = true;
        for mode in [
            crate::gizmo::GizmoMode::Translate,
            crate::gizmo::GizmoMode::Rotate,
            crate::gizmo::GizmoMode::Scale,
        ] {
            assert!(
                gizmo_handles_supported(&scene, child, mode),
                "expected handles for mode {mode:?}"
            );
        }
    }

    #[test]
    fn commit_gizmo_drag_translate_relative_placed_emits_axis_shortcuts() {
        // `relative_placed` Translate commits write per-axis shortcuts so the
        // snap-axis value trips `pos_axis_explicit` even when it lands on 0
        // (a plain `pos=[…]` would lose the snap-axis component to the next
        // layout pass when the resolved value is 0). The first edit also
        // strips `pos`/`from`/`to` so they don't fight the new shortcuts.
        let mut scene = SceneGraph::new();
        let parent = scene.add_root("group", "group", Transform::IDENTITY);
        let child = scene.add_child(parent, "tier2", "box", Transform::IDENTITY);
        scene.nodes[child.0 as usize].relative_placed = true;
        let mut st = ViewerState::default();
        st.scene = Some(Arc::new(scene));
        st.gizmo_drag = Some(GizmoDrag {
            node: child,
            axis: crate::gizmo::Axis::Y,
            mode: crate::gizmo::GizmoMode::Translate,
            start_transform: Transform::from_trs(
                Vec3::new(0.1, 0.5, 0.0),
                Quat::IDENTITY,
                Vec3::ONE,
            ),
            start_origin: Vec3::ZERO,
            parent_start_world: Mat4::IDENTITY,
            start_ray_origin: Vec3::ZERO,
            start_ray_dir: Vec3::Z,
            delta: 0.75,
        });
        let edits = commit_gizmo_drag(&mut st);
        assert_eq!(edits.len(), 3, "expected three shortcut edits, got {edits:?}");
        let unwrap_set = |e: &PendingEdit| match e {
            PendingEdit::SetAttrCanonical { node, attr, value, delete } => {
                (*node, attr.clone(), value.clone(), delete.clone())
            }
            _ => panic!("expected SetAttrCanonical, got {e:?}"),
        };
        let (n0, a0, v0, d0) = unwrap_set(&edits[0]);
        let (n1, a1, v1, d1) = unwrap_set(&edits[1]);
        let (n2, a2, v2, d2) = unwrap_set(&edits[2]);
        assert_eq!(n0, child);
        assert_eq!(n1, child);
        assert_eq!(n2, child);
        assert_eq!(a0, "x");
        assert_eq!(a1, "y");
        assert_eq!(a2, "z");
        assert_eq!(v0, "0.1");
        assert_eq!(v1, "1.25");
        assert_eq!(v2, "0");
        assert_eq!(d0, vec!["pos", "from", "to"]);
        assert!(d1.is_empty());
        assert!(d2.is_empty());
    }

    /// Three-level scene: outer user-authored group containing a `use`
    /// wrapper of an imported file. Mirrors a real `.mog` whose top
    /// declares `group "scene" { use "trash_bin" (pos=...) }`.
    /// Returns `(scene, outer_group, wrapper, imported_body)`.
    fn scene_with_use_inside_outer_group() -> (SceneGraph, NodeId, NodeId, NodeId) {
        let mut scene = SceneGraph::new();
        let outer = scene.add_root("scene", "group", Transform::IDENTITY);
        let wrapper = scene.add_child(outer, "trash_bin", "group", Transform::IDENTITY);
        scene.nodes[wrapper.0 as usize].use_id = Some(11);
        scene.nodes[wrapper.0 as usize].origin = None;
        let body = scene.add_child(wrapper, "bin_body", "cylinder", Transform::IDENTITY);
        scene.nodes[body.0 as usize].use_id = Some(11);
        scene.nodes[body.0 as usize].origin = Some(PathBuf::from("trash_bin.mog"));
        (scene, outer, wrapper, body)
    }

    #[test]
    fn cycling_first_click_matches_today_redirect_pick() {
        // Depth-0 must reproduce existing behavior so a single click on an
        // imported leaf still lands on the outermost user-authored group.
        let (scene, outer, _wrapper, body) = scene_with_use_inside_outer_group();
        let mut st = ViewerState::default();
        st.scene = Some(Arc::new(scene));
        replace_selection_cycling(&mut st, body, egui::pos2(100.0, 100.0));
        assert_eq!(st.selected, vec![outer]);
        assert_eq!(st.pick_cycle.map(|c| c.depth), Some(0));
    }

    #[test]
    fn cycling_second_click_drills_to_use_wrapper() {
        // Same screen point, same leaf → walk one ancestor closer to the
        // leaf. The `use` line wrapper is the next editable target;
        // crossing further would land on imported geometry.
        let (scene, _outer, wrapper, body) = scene_with_use_inside_outer_group();
        let mut st = ViewerState::default();
        st.scene = Some(Arc::new(scene));
        let cursor = egui::pos2(100.0, 100.0);
        replace_selection_cycling(&mut st, body, cursor);
        replace_selection_cycling(&mut st, body, cursor);
        assert_eq!(st.selected, vec![wrapper]);
        assert_eq!(st.pick_cycle.map(|c| c.depth), Some(1));
    }

    #[test]
    fn cycling_clamps_at_editability_boundary() {
        // A third click on the same target must not cross into the
        // imported body — its source span lives in another file. The
        // depth saturates one short of the leaf.
        let (scene, _outer, wrapper, body) = scene_with_use_inside_outer_group();
        let mut st = ViewerState::default();
        st.scene = Some(Arc::new(scene));
        let cursor = egui::pos2(100.0, 100.0);
        replace_selection_cycling(&mut st, body, cursor);
        replace_selection_cycling(&mut st, body, cursor);
        replace_selection_cycling(&mut st, body, cursor);
        assert_eq!(st.selected, vec![wrapper]);
        assert_eq!(st.pick_cycle.map(|c| c.depth), Some(1));
    }

    #[test]
    fn cycling_resets_when_cursor_moves_past_radius() {
        // Cursor delta beyond `PICK_CYCLE_RADIUS_PX` is "a different
        // click", not a repeat — depth resets to 0 even when the leaf
        // matches.
        let (scene, outer, _wrapper, body) = scene_with_use_inside_outer_group();
        let mut st = ViewerState::default();
        st.scene = Some(Arc::new(scene));
        replace_selection_cycling(&mut st, body, egui::pos2(100.0, 100.0));
        let far = egui::pos2(100.0 + PICK_CYCLE_RADIUS_PX + 1.0, 100.0);
        replace_selection_cycling(&mut st, body, far);
        assert_eq!(st.selected, vec![outer]);
        assert_eq!(st.pick_cycle.map(|c| c.depth), Some(0));
    }

    #[test]
    fn cycling_preserves_state_when_cursor_drifts_within_radius() {
        // Tiny cursor drift between clicks (hand jitter, sub-pixel egui
        // rounding) must not reset the cycle. Anything within the radius
        // counts as the same click.
        let (scene, _outer, wrapper, body) = scene_with_use_inside_outer_group();
        let mut st = ViewerState::default();
        st.scene = Some(Arc::new(scene));
        replace_selection_cycling(&mut st, body, egui::pos2(100.0, 100.0));
        let drifted = egui::pos2(100.0 + PICK_CYCLE_RADIUS_PX - 0.5, 100.0);
        replace_selection_cycling(&mut st, body, drifted);
        assert_eq!(st.selected, vec![wrapper]);
        assert_eq!(st.pick_cycle.map(|c| c.depth), Some(1));
    }

    #[test]
    fn cycling_resets_when_leaf_changes() {
        // Three sibling boxes under one group plus an imported wrapper:
        // clicking from one leaf to a different leaf restarts the cycle
        // even at the same screen point.
        let (mut scene, outer, _wrapper, body) = scene_with_use_inside_outer_group();
        let other = scene.add_child(outer, "other", "box", Transform::IDENTITY);
        let mut st = ViewerState::default();
        st.scene = Some(Arc::new(scene));
        let cursor = egui::pos2(100.0, 100.0);
        replace_selection_cycling(&mut st, body, cursor);
        replace_selection_cycling(&mut st, body, cursor); // depth=1, wrapper
        replace_selection_cycling(&mut st, other, cursor); // different leaf
        // `other` has use_id=None → redirect_pick returns it unchanged,
        // so depth-0 selects the leaf directly.
        assert_eq!(st.selected, vec![other]);
        assert_eq!(st.pick_cycle.map(|c| c.depth), Some(0));
    }

    #[test]
    fn cycling_clears_selection_when_redirect_finds_no_wrapper() {
        // Imported root with no editable ancestor: cycling must mirror
        // `replace_selection`'s clear-on-None behavior so a click never
        // latches onto a node whose source span lives in another file.
        // `origin = Some(...)` is what flags the node as imported — a
        // local `use "module" ()` expansion has `origin = None` and the
        // redirect would return the node itself.
        let mut scene = SceneGraph::new();
        let imported = scene.add_root("desk_top", "box", Transform::IDENTITY);
        scene.nodes[imported.0 as usize].use_id = Some(1);
        scene.nodes[imported.0 as usize].origin =
            Some(std::path::PathBuf::from("desk.mog"));
        let mut st = ViewerState::default();
        st.scene = Some(Arc::new(scene));
        replace_selection_cycling(&mut st, imported, egui::pos2(100.0, 100.0));
        assert!(st.selected.is_empty());
        assert!(st.pick_cycle.is_none());
    }

    #[test]
    fn cycling_on_plain_user_authored_leaf_is_a_no_op() {
        // Active-source geometry without any `use` involved: depth-0 is
        // the leaf itself, and there are no editable ancestors closer
        // than the leaf. Repeat clicks pin at depth 0.
        let mut scene = SceneGraph::new();
        let group = scene.add_root("scene", "group", Transform::IDENTITY);
        let leaf = scene.add_child(group, "box", "box", Transform::IDENTITY);
        let mut st = ViewerState::default();
        st.scene = Some(Arc::new(scene));
        let cursor = egui::pos2(100.0, 100.0);
        replace_selection_cycling(&mut st, leaf, cursor);
        replace_selection_cycling(&mut st, leaf, cursor);
        assert_eq!(st.selected, vec![leaf]);
        assert_eq!(st.pick_cycle.map(|c| c.depth), Some(0));
    }
