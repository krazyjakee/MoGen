//! Tests for `viewer::flatten` — scene-graph → vertex-stream conversion,
//! batch coalescing, PBR / UV / alpha plumbing.

use super::super::flatten::{flatten, PaletteSource, FLOATS_PER_VERTEX};
use super::{material_with_texture, quad_mesh};
use glam::{Mat4, Quat, Vec3};
use mogen_core::{AlphaMode, Material, NodeId, SceneGraph, TextureRef, Transform};
use std::path::{Path, PathBuf};

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
fn flatten_emits_color_0_with_white_default() {
    // A mesh with COLOR_0 forwards its colours into the vertex stream; a mesh
    // without one gets opaque white so the shader's unconditional multiply by
    // v_color is a no-op.
    let mut scene = SceneGraph::new();
    let mat = scene.add_material(material_with_texture("m", None));

    let colored = scene.add_root("colored", "primitive", Transform::IDENTITY);
    let mut cm = quad_mesh();
    cm.colors = vec![
        [0.1, 0.2, 0.3, 1.0],
        [0.4, 0.5, 0.6, 1.0],
        [0.7, 0.8, 0.9, 1.0],
        [1.0, 0.0, 0.5, 1.0],
    ];
    scene.set_mesh(colored, cm);
    scene.set_material(colored, mat);

    let plain = scene.add_root("plain", "primitive", Transform::IDENTITY);
    scene.set_mesh(plain, quad_mesh());
    scene.set_material(plain, mat);

    let mesh = flatten(&scene, None);
    let stride = FLOATS_PER_VERTEX;
    // Colour is the last 4 floats of each vertex.
    let color_at = |v: usize| {
        let b = v * stride + 16;
        [
            mesh.vertices[b],
            mesh.vertices[b + 1],
            mesh.vertices[b + 2],
            mesh.vertices[b + 3],
        ]
    };
    // Both quads share a material, so they coalesce; the colored quad's verts
    // come first (root order), then the plain quad's white verts.
    assert_eq!(color_at(0), [0.1, 0.2, 0.3, 1.0]);
    assert_eq!(color_at(3), [1.0, 0.0, 0.5, 1.0]);
    for v in 4..8 {
        assert_eq!(color_at(v), [1.0, 1.0, 1.0, 1.0], "plain quad vert {v}");
    }
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
    mat.base_color_texture = Some(TextureRef::new(PathBuf::from("albedo.png")));
    mat.metallic_roughness_texture = Some(TextureRef::new(PathBuf::from("mr.png")));
    mat.normal_texture = Some(TextureRef::new(PathBuf::from("n.png")));
    mat.occlusion_texture = Some(TextureRef::new(PathBuf::from("ao.png")));
    mat.emissive_texture = Some(TextureRef::new(PathBuf::from("em.png")));
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
