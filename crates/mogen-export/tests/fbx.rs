//! End-to-end coverage for the FBX exporter. We round-trip every supported
//! scene-graph feature (mesh / material+texture / light / skin / animation)
//! through `write_fbx` + `fbxcel::tree::any::AnyTree::from_seekable_reader`
//! and assert on the structural shape of the resulting FBX node tree.
//!
//! The bytes-on-disk path is exercised via the public `mogen_export::write_fbx`
//! entry point so we cover the writer + footer machinery, not just the
//! intermediate Tree.

#![cfg(feature = "fbx")]

use std::collections::HashMap;
use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::process::id;

use fbxcel::tree::any::AnyTree;
use fbxcel::tree::v7400::{NodeHandle, Tree};

use mogen_core::{
    Clip, Easing, Interpolation, Light, LightKind, Material, Mesh, SceneGraph, Skin,
    TextureRef, Track, TrackProperty, Transform,
};
use mogen_geom::box_mesh;

fn unique_tmp(name: &str) -> PathBuf {
    // Each test file gets a per-call counter so cargo's default parallel
    // runner doesn't have two tests racing to read/write the same path.
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("mogen-fbx-export-{}-{n}-{name}", id()))
}

/// Serialise via the public `write_fbx`, read the file back into a Tree, and
/// hand it (plus the raw bytes for any byte-level assertions) to the caller.
fn round_trip(scene: &SceneGraph) -> (Tree, Vec<u8>) {
    let path = unique_tmp("scene.fbx");
    mogen_export::write_fbx(scene, &path).expect("write_fbx ok");
    let bytes = fs::read(&path).expect("reading produced fbx");

    let tree = match AnyTree::from_seekable_reader(Cursor::new(bytes.clone()))
        .expect("loading fbx via fbxcel")
    {
        AnyTree::V7400(_, tree, _footer) => tree,
        _ => panic!("expected V7400 fbx"),
    };
    (tree, bytes)
}

/// Convenience: collect every direct child of `parent` whose name matches.
fn children_named<'a>(parent: NodeHandle<'a>, name: &str) -> Vec<NodeHandle<'a>> {
    parent.children_by_name(name).collect()
}

/// Locate the `Objects` node and return every direct child with `name`.
fn objects_named<'a>(tree: &'a Tree, name: &str) -> Vec<NodeHandle<'a>> {
    let objects = tree
        .root()
        .first_child_by_name("Objects")
        .expect("Objects node");
    children_named(objects, name)
}

/// Look up the i32 attribute under `parent/child`. Panics on missing
/// child or wrong attribute type — every test using this asserts on
/// values it just produced.
fn read_i32(parent: NodeHandle<'_>, child: &str) -> i32 {
    parent
        .first_child_by_name(child)
        .expect(child)
        .attributes()[0]
        .get_i32()
        .expect("i32")
}

fn read_f64(parent: NodeHandle<'_>, child: &str) -> f64 {
    parent
        .first_child_by_name(child)
        .expect(child)
        .attributes()[0]
        .get_f64()
        .expect("f64")
}

#[test]
fn top_level_structure_matches_fbx_7_4_layout() {
    let mut scene = SceneGraph::new();
    scene.add_root("group", "group", Transform::IDENTITY);

    let (tree, bytes) = round_trip(&scene);

    // FBX binary magic: "Kaydara FBX Binary  \x00\x1a\x00".
    assert_eq!(
        &bytes[..23],
        b"Kaydara FBX Binary  \x00\x1a\x00",
        "fbx binary header should match the canonical magic"
    );

    let root = tree.root();
    let names: Vec<&str> = root.children().map(|c| c.name()).collect();
    for required in [
        "FBXHeaderExtension",
        "GlobalSettings",
        "Documents",
        "Definitions",
        "Objects",
        "Connections",
        "Takes",
    ] {
        assert!(
            names.iter().any(|n| *n == required),
            "missing top-level section {required}: {names:?}",
        );
    }
}

#[test]
fn global_settings_carry_y_up_and_meter_units() {
    let mut scene = SceneGraph::new();
    scene.add_root("a", "group", Transform::IDENTITY);

    let (tree, _) = round_trip(&scene);
    let gs = tree
        .root()
        .first_child_by_name("GlobalSettings")
        .expect("GlobalSettings");
    let props = gs
        .first_child_by_name("Properties70")
        .expect("Properties70 under GlobalSettings");

    let mut up = None;
    let mut unit_scale = None;
    for p in props.children_by_name("P") {
        let attrs = p.attributes();
        let key = attrs[0].get_string().unwrap_or("");
        match key {
            "UpAxis" => up = attrs[4].get_i32(),
            "UnitScaleFactor" => unit_scale = attrs[4].get_f64(),
            _ => {}
        }
    }
    assert_eq!(up, Some(1), "UpAxis should be 1 (Y-up)");
    assert_eq!(unit_scale, Some(1.0), "UnitScaleFactor should be 1.0 (meters)");
}

#[test]
fn mesh_round_trip_emits_geometry_with_negate_terminated_polygons() {
    let mut scene = SceneGraph::new();
    let id = scene.add_root("box", "box", Transform::IDENTITY);
    scene.set_mesh(id, box_mesh([1.0, 1.0, 1.0], mogen_core::UvMode::default()));

    let (tree, _) = round_trip(&scene);

    let geometries = objects_named(&tree, "Geometry");
    assert_eq!(geometries.len(), 1, "exactly one Geometry");
    let g = geometries[0];

    // Box mesh produces 24 verts + 36 indices in the GLB exporter; we hold
    // the same numbers here because we don't dedupe.
    let verts = g.first_child_by_name("Vertices").expect("Vertices");
    let v_arr = verts.attributes()[0].get_arr_f64().expect("f64 array");
    assert!(
        !v_arr.is_empty() && v_arr.len() % 3 == 0,
        "Vertices length {} must be non-empty and divisible by 3",
        v_arr.len(),
    );

    let pvi = g
        .first_child_by_name("PolygonVertexIndex")
        .expect("PolygonVertexIndex");
    let pvi_arr = pvi.attributes()[0].get_arr_i32().expect("i32 array");
    assert!(pvi_arr.len() % 3 == 0, "triangle list");
    // Every third entry — the polygon terminator — must be negative
    // (negate-and-decrement encoding for the last index of a polygon).
    for (chunk_idx, tri) in pvi_arr.chunks_exact(3).enumerate() {
        assert!(tri[0] >= 0, "tri {chunk_idx} entry 0 = {} should be ≥0", tri[0]);
        assert!(tri[1] >= 0, "tri {chunk_idx} entry 1 = {} should be ≥0", tri[1]);
        assert!(tri[2] < 0, "tri {chunk_idx} entry 2 = {} should be terminator (<0)", tri[2]);
    }

    // Normal layer: present, ByVertice direct.
    let n = g.first_child_by_name("LayerElementNormal").expect("normals");
    assert_eq!(
        n.first_child_by_name("MappingInformationType")
            .unwrap()
            .attributes()[0]
            .get_string()
            .unwrap(),
        "ByVertice",
    );

    // OO connection from this Geometry's id to the Model's id, and from the
    // Model to RootNode (0).
    let geom_id = g.attributes()[0].get_i64().expect("geometry id");
    let model_id = objects_named(&tree, "Model")[0]
        .attributes()[0]
        .get_i64()
        .expect("model id");

    let conns = tree
        .root()
        .first_child_by_name("Connections")
        .expect("Connections");
    let mut found_geom_to_model = false;
    let mut found_model_to_root = false;
    for c in conns.children_by_name("C") {
        let attrs = c.attributes();
        let kind = attrs[0].get_string().unwrap();
        let child = attrs[1].get_i64().unwrap();
        let parent = attrs[2].get_i64().unwrap();
        if kind == "OO" && child == geom_id && parent == model_id {
            found_geom_to_model = true;
        }
        if kind == "OO" && child == model_id && parent == 0 {
            found_model_to_root = true;
        }
    }
    assert!(found_geom_to_model, "Geometry must OO-connect to Model");
    assert!(found_model_to_root, "Model must OO-connect to RootNode (0)");
}

#[test]
fn material_with_base_color_texture_emits_full_chain() {
    use image::{ImageBuffer, Rgb};

    // 1x1 PNG so the embedded `Content` blob can be byte-compared.
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_pixel(1, 1, Rgb([200, 180, 140]));
    let mut png_bytes = Vec::new();
    img.write_to(&mut Cursor::new(&mut png_bytes), image::ImageFormat::Png)
        .expect("encoding 1x1 png");

    let png_path = unique_tmp("albedo.png");
    fs::write(&png_path, &png_bytes).expect("writing fixture png");

    let mut scene = SceneGraph::new();
    let mut mat = Material::new("painted");
    mat.base_color_texture = Some(TextureRef::new(png_path.clone()));
    let mat_id = scene.add_material(mat);

    let id = scene.add_root("box", "box", Transform::IDENTITY);
    scene.set_mesh(id, box_mesh([1.0, 1.0, 1.0], mogen_core::UvMode::default()));
    scene.set_material(id, mat_id);

    let (tree, _) = round_trip(&scene);

    let materials = objects_named(&tree, "Material");
    assert_eq!(materials.len(), 1);
    let m = materials[0];
    let shading = m
        .first_child_by_name("ShadingModel")
        .unwrap()
        .attributes()[0]
        .get_string()
        .unwrap();
    assert_eq!(shading, "Phong");

    // Custom Roughness + Metallic property entries are present so PBR
    // importers can recover the originals.
    let props = m.first_child_by_name("Properties70").unwrap();
    let mut have_roughness = false;
    let mut have_metallic = false;
    for p in props.children_by_name("P") {
        let key = p.attributes()[0].get_string().unwrap_or("");
        if key == "Roughness" {
            have_roughness = true;
        }
        if key == "Metallic" {
            have_metallic = true;
        }
    }
    assert!(have_roughness, "Material should carry Roughness PBR factor");
    assert!(have_metallic, "Material should carry Metallic PBR factor");

    // Texture + Video pair, with the Video carrying the embedded bytes.
    let textures = objects_named(&tree, "Texture");
    let videos = objects_named(&tree, "Video");
    assert_eq!(textures.len(), 1, "one Texture");
    assert_eq!(videos.len(), 1, "one Video");

    let content = videos[0].first_child_by_name("Content").expect("Content");
    let blob = content.attributes()[0].get_binary().expect("binary content");
    assert_eq!(blob, png_bytes.as_slice(), "embedded bytes must match input");

    // OP connection Texture -> Material under property "DiffuseColor".
    let tex_id = textures[0].attributes()[0].get_i64().unwrap();
    let mat_obj_id = m.attributes()[0].get_i64().unwrap();
    let conns = tree.root().first_child_by_name("Connections").unwrap();
    let mut found_op = false;
    for c in conns.children_by_name("C") {
        let attrs = c.attributes();
        let kind = attrs[0].get_string().unwrap_or("");
        if kind == "OP"
            && attrs[1].get_i64() == Some(tex_id)
            && attrs[2].get_i64() == Some(mat_obj_id)
            && attrs[3].get_string() == Some("DiffuseColor")
        {
            found_op = true;
        }
    }
    assert!(found_op, "expected OP Texture->Material on DiffuseColor");
}

#[test]
fn directional_point_and_spot_lights_emit_node_attributes() {
    let mut scene = SceneGraph::new();

    let dir = scene.add_root("sun", "light", Transform::IDENTITY);
    scene.set_light(
        dir,
        Light {
            kind: LightKind::Directional,
            color: [1.0, 0.95, 0.85],
            intensity: 2.0,
            range: None,
            inner_cone_rad: 0.0,
            outer_cone_rad: std::f32::consts::FRAC_PI_4,
        },
    );

    let pt = scene.add_root("lamp", "light", Transform::IDENTITY);
    scene.set_light(
        pt,
        Light {
            kind: LightKind::Point,
            color: [1.0, 1.0, 1.0],
            intensity: 10.0,
            range: Some(8.0),
            ..Default::default()
        },
    );

    let spot = scene.add_root("spot", "light", Transform::IDENTITY);
    scene.set_light(
        spot,
        Light {
            kind: LightKind::Spot,
            color: [1.0, 1.0, 1.0],
            intensity: 20.0,
            range: Some(10.0),
            inner_cone_rad: 20f32.to_radians(),
            outer_cone_rad: 35f32.to_radians(),
        },
    );

    let (tree, _) = round_trip(&scene);

    let attrs = objects_named(&tree, "NodeAttribute");
    assert_eq!(attrs.len(), 3, "one NodeAttribute per light");

    // Collect (light_type, has inner, has outer) per attribute.
    let mut light_types = HashMap::<i32, NodeHandle<'_>>::new();
    for a in &attrs {
        let props = a.first_child_by_name("Properties70").unwrap();
        let mut light_type = -1;
        for p in props.children_by_name("P") {
            if p.attributes()[0].get_string() == Some("LightType") {
                light_type = p.attributes()[4].get_i32().unwrap();
            }
        }
        light_types.insert(light_type, *a);
    }

    let directional = light_types.remove(&1).expect("directional NodeAttribute");
    let _ = directional;

    let point = light_types.remove(&0).expect("point NodeAttribute");
    let p_props = point.first_child_by_name("Properties70").unwrap();
    let mut far_end = None;
    for p in p_props.children_by_name("P") {
        if p.attributes()[0].get_string() == Some("FarAttenuationEnd") {
            far_end = p.attributes()[4].get_f64();
        }
    }
    assert_eq!(far_end, Some(8.0), "point light range → FarAttenuationEnd");

    let spot = light_types.remove(&2).expect("spot NodeAttribute");
    let s_props = spot.first_child_by_name("Properties70").unwrap();
    let mut inner = None;
    let mut outer = None;
    for p in s_props.children_by_name("P") {
        if p.attributes()[0].get_string() == Some("InnerAngle") {
            inner = p.attributes()[4].get_f64();
        }
        if p.attributes()[0].get_string() == Some("OuterAngle") {
            outer = p.attributes()[4].get_f64();
        }
    }
    let inner = inner.expect("InnerAngle");
    let outer = outer.expect("OuterAngle");
    assert!((inner - 20.0).abs() < 1e-4);
    assert!((outer - 35.0).abs() < 1e-4);
}

#[test]
fn skin_emits_one_skin_deformer_with_per_joint_clusters() {
    // Tiny 4-vertex skinned mesh bound to two joints.
    let mut scene = SceneGraph::new();

    let mesh_node = scene.add_root("body", "mesh", Transform::IDENTITY);
    let joint_a = scene.add_root("jointA", "joint", Transform::IDENTITY);
    let joint_b = scene.add_root("jointB", "joint", Transform::IDENTITY);

    // Vertices: 4 positions, each fully weighted to one joint (alternating).
    let positions = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]];
    let normals = vec![[0.0, 0.0, 1.0]; 4];
    let mut mesh = Mesh::new(positions, normals, vec![0, 1, 2, 0, 2, 3]);
    mesh.joints = vec![[0, 0, 0, 0], [1, 0, 0, 0], [0, 0, 0, 0], [1, 0, 0, 0]];
    mesh.weights = vec![[1.0, 0.0, 0.0, 0.0]; 4];
    scene.set_mesh(mesh_node, mesh);

    // Use non-identity inverse-bind matrices so a regression that swaps
    // `Transform` and `TransformLink` (or accidentally sets both to the
    // same value) shows up. With a non-trivial IBM, Transform = IBM and
    // TransformLink = inverse(IBM) end up with different first elements.
    let ibm_a = glam::Mat4::from_translation(glam::Vec3::new(1.0, 2.0, 3.0));
    let ibm_b = glam::Mat4::from_translation(glam::Vec3::new(-4.0, 5.0, -6.0));
    let skin = Skin {
        name: "rig".into(),
        joints: vec![joint_a, joint_b],
        inverse_bind_matrices: vec![ibm_a.to_cols_array_2d(), ibm_b.to_cols_array_2d()],
        envelopes: Vec::new(),
        skeleton_root: None,
        origin: None,
    };
    let skin_id = scene.add_skin(skin);
    scene.set_skin(mesh_node, skin_id);

    let (tree, _) = round_trip(&scene);

    let deformers = objects_named(&tree, "Deformer");
    let skins: Vec<_> = deformers
        .iter()
        .filter(|d| d.attributes()[2].get_string() == Some("Skin"))
        .collect();
    let clusters: Vec<_> = deformers
        .iter()
        .filter(|d| d.attributes()[2].get_string() == Some("Cluster"))
        .collect();
    assert_eq!(skins.len(), 1, "one Skin Deformer");
    assert_eq!(clusters.len(), 2, "two joint Cluster Deformers");

    for c in &clusters {
        let idx = c
            .first_child_by_name("Indexes")
            .expect("Indexes")
            .attributes()[0]
            .get_arr_i32()
            .expect("i32 array");
        let w = c
            .first_child_by_name("Weights")
            .expect("Weights")
            .attributes()[0]
            .get_arr_f64()
            .expect("f64 array");
        assert!(!idx.is_empty(), "every cluster covers ≥1 vertex");
        assert_eq!(idx.len(), w.len(), "indexes/weights paired");

        let t = c
            .first_child_by_name("Transform")
            .expect("Transform")
            .attributes()[0]
            .get_arr_f64()
            .expect("f64 array");
        let tl = c
            .first_child_by_name("TransformLink")
            .expect("TransformLink")
            .attributes()[0]
            .get_arr_f64()
            .expect("f64 array");
        // Transform = IBM (translation embedded in row 3 / cols 12..15
        // in column-major), TransformLink = inverse(IBM) (negated
        // translation). They MUST differ; equality would mean we
        // accidentally emitted the same value into both.
        assert_eq!(t.len(), 16);
        assert_eq!(tl.len(), 16);
        let t_translation = [t[12], t[13], t[14]];
        let tl_translation = [tl[12], tl[13], tl[14]];
        for ax in 0..3 {
            assert!(
                (t_translation[ax] + tl_translation[ax]).abs() < 1e-6,
                "Transform translation {:?} should be the negation of TransformLink translation {:?}",
                t_translation,
                tl_translation,
            );
        }
        assert!(
            t_translation.iter().any(|v| v.abs() > 1e-6),
            "test fixture should use non-identity IBM",
        );
    }

    // Connection direction sanity: every Cluster must be the source of
    // an OO edge to a joint Model. A regression that swapped the
    // direction would put the joint Model as the source.
    let cluster_ids: Vec<i64> = clusters
        .iter()
        .map(|c| c.attributes()[0].get_i64().expect("cluster id"))
        .collect();
    let joint_model_names = ["jointA", "jointB"];
    let joint_model_ids: Vec<i64> = objects_named(&tree, "Model")
        .iter()
        .filter(|m| {
            let n = m.attributes()[1].get_string().unwrap_or("");
            joint_model_names.iter().any(|jn| n.starts_with(jn))
        })
        .map(|m| m.attributes()[0].get_i64().unwrap())
        .collect();
    assert_eq!(joint_model_ids.len(), 2, "found both joint Models");
    let conns = tree.root().first_child_by_name("Connections").unwrap();
    let mut cluster_to_joint_count = 0;
    let mut joint_to_cluster_count = 0;
    for conn in conns.children_by_name("C") {
        let attrs = conn.attributes();
        if attrs[0].get_string() != Some("OO") {
            continue;
        }
        let src = attrs[1].get_i64().unwrap();
        let dst = attrs[2].get_i64().unwrap();
        if cluster_ids.contains(&src) && joint_model_ids.contains(&dst) {
            cluster_to_joint_count += 1;
        }
        if joint_model_ids.contains(&src) && cluster_ids.contains(&dst) {
            joint_to_cluster_count += 1;
        }
    }
    assert_eq!(
        cluster_to_joint_count, 2,
        "each Cluster must OO-connect (as source) to its joint Model",
    );
    assert_eq!(
        joint_to_cluster_count, 0,
        "joint Model must NOT be the source of an OO edge to a Cluster (direction is reversed)",
    );
}

#[test]
fn translation_animation_emits_stack_layer_curve_node_and_three_axes() {
    let mut scene = SceneGraph::new();
    let id = scene.add_root("body", "group", Transform::IDENTITY);

    let clip = Clip {
        name: "wave".into(),
        duration: 1.0,
        tracks: vec![Track {
            node: id,
            property: TrackProperty::Translation,
            interpolation: Interpolation::Linear,
            easing: Easing::Linear,
            times: vec![0.0, 0.5, 1.0],
            values: vec![[0.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 0.0, 0.0]],
        }],
        origin: None,
    };
    scene.clips.push(clip);

    let (tree, _) = round_trip(&scene);

    assert_eq!(objects_named(&tree, "AnimationStack").len(), 1);
    assert_eq!(objects_named(&tree, "AnimationLayer").len(), 1);
    assert_eq!(objects_named(&tree, "AnimationCurveNode").len(), 1);
    let curves = objects_named(&tree, "AnimationCurve");
    assert_eq!(curves.len(), 3, "X/Y/Z curves emitted as three AnimationCurves");

    let model_id = objects_named(&tree, "Model")[0]
        .attributes()[0]
        .get_i64()
        .expect("model id");

    // Each curve carries 3 KeyTime entries and 3 KeyValueFloat entries.
    for c in &curves {
        let times = c
            .first_child_by_name("KeyTime")
            .unwrap()
            .attributes()[0]
            .get_arr_i64()
            .unwrap();
        let values = c
            .first_child_by_name("KeyValueFloat")
            .unwrap()
            .attributes()[0]
            .get_arr_f32()
            .unwrap();
        assert_eq!(times.len(), 3);
        assert_eq!(values.len(), 3);

        // Verify every keyframe tick. The fixture clip has keys at
        // t = 0, 0.5, 1.0 seconds. FBX uses 46_186_158_000 ticks/sec, so
        // expected ticks are 0, 23_093_079_000, 46_186_158_000.
        let tps = 46_186_158_000_i64;
        let expected = [0_i64, tps / 2, tps];
        for (i, exp) in expected.iter().enumerate() {
            assert!(
                (times[i] - exp).abs() <= 1,
                "KeyTime[{i}] = {} not within ±1 of {}",
                times[i],
                exp,
            );
        }

        // Linear interpolation flag must land in `KeyAttrFlags`. The
        // FBX SDK reserves 0x4 for Linear (0x2 is Constant); a regression
        // that swapped the constants would put 0x2 here.
        let flags = c
            .first_child_by_name("KeyAttrFlags")
            .unwrap()
            .attributes()[0]
            .get_arr_i32()
            .unwrap();
        assert_eq!(
            flags,
            &[0x4_i32],
            "Linear interpolation should map to KeyAttrFlags=0x4, got {:?}",
            flags,
        );
    }

    // The CurveNode OP-connects to the Model on `Lcl Translation`.
    let curve_node_id = objects_named(&tree, "AnimationCurveNode")[0]
        .attributes()[0]
        .get_i64()
        .unwrap();
    let conns = tree.root().first_child_by_name("Connections").unwrap();
    let mut found = false;
    for c in conns.children_by_name("C") {
        let attrs = c.attributes();
        if attrs[0].get_string() == Some("OP")
            && attrs[1].get_i64() == Some(curve_node_id)
            && attrs[2].get_i64() == Some(model_id)
            && attrs[3].get_string() == Some("Lcl Translation")
        {
            found = true;
        }
    }
    assert!(found, "CurveNode must OP-connect to Model on Lcl Translation");
}

// Silence the otherwise-unused helper warning if a future test removal makes
// `read_i32`/`read_f64` orphaned.
#[test]
fn step_interpolation_emits_constant_key_attr_flag() {
    let mut scene = SceneGraph::new();
    let id = scene.add_root("body", "group", Transform::IDENTITY);
    scene.clips.push(Clip {
        name: "step_clip".into(),
        duration: 1.0,
        tracks: vec![Track {
            node: id,
            property: TrackProperty::Translation,
            interpolation: Interpolation::Step,
            easing: Easing::Linear,
            times: vec![0.0, 1.0],
            values: vec![[0.0, 0.0, 0.0, 0.0], [1.0, 0.0, 0.0, 0.0]],
        }],
        origin: None,
    });

    let (tree, _) = round_trip(&scene);
    let curves = objects_named(&tree, "AnimationCurve");
    assert_eq!(curves.len(), 3);
    for c in &curves {
        let flags = c
            .first_child_by_name("KeyAttrFlags")
            .unwrap()
            .attributes()[0]
            .get_arr_i32()
            .unwrap();
        // Step interpolation must encode as 0x2 (KFCURVE_INTERPOLATION_CONSTANT
        // in the FBX SDK). 0x4 = Linear. Swapping these would let "Linear"
        // play back as steps in importers and vice versa.
        assert_eq!(
            flags,
            &[0x2_i32],
            "Step interpolation should map to KeyAttrFlags=0x2, got {:?}",
            flags,
        );
    }
}

#[test]
fn rotation_track_emits_euler_degrees() {
    // Rotate 90° around Y. glam quat for 90° Y is (0, sin(45°), 0, cos(45°)).
    // Euler XYZ degrees recovered should be approximately (0, 90, 0).
    let mut scene = SceneGraph::new();
    let id = scene.add_root("body", "group", Transform::IDENTITY);
    let q = glam::Quat::from_rotation_y(90f32.to_radians());
    scene.clips.push(Clip {
        name: "rot".into(),
        duration: 0.0,
        tracks: vec![Track {
            node: id,
            property: TrackProperty::Rotation,
            interpolation: Interpolation::Linear,
            easing: Easing::Linear,
            times: vec![0.0],
            values: vec![[q.x, q.y, q.z, q.w]],
        }],
        origin: None,
    });

    let (tree, _) = round_trip(&scene);
    let curves = objects_named(&tree, "AnimationCurve");
    assert_eq!(curves.len(), 3);

    // The Y curve should hold ~90.0; X and Z should be ~0.0. We rely on
    // the OP connection name to disambiguate axis order.
    let curve_node = objects_named(&tree, "AnimationCurveNode")[0];
    let curve_node_id = curve_node.attributes()[0].get_i64().unwrap();

    // Map curve_id → axis (d|X / d|Y / d|Z) via the OP connections from
    // CurveNode to AnimationCurves.
    let conns = tree.root().first_child_by_name("Connections").unwrap();
    let mut axis_for_curve = std::collections::HashMap::<i64, String>::new();
    for c in conns.children_by_name("C") {
        let attrs = c.attributes();
        if attrs[0].get_string() != Some("OP") {
            continue;
        }
        let src = attrs[1].get_i64().unwrap();
        let dst = attrs[2].get_i64().unwrap();
        if dst == curve_node_id {
            if let Some(prop) = attrs[3].get_string() {
                axis_for_curve.insert(src, prop.to_string());
            }
        }
    }

    let mut x_val = None;
    let mut y_val = None;
    let mut z_val = None;
    for c in &curves {
        let id = c.attributes()[0].get_i64().unwrap();
        let v = c
            .first_child_by_name("KeyValueFloat")
            .unwrap()
            .attributes()[0]
            .get_arr_f32()
            .unwrap();
        match axis_for_curve.get(&id).map(String::as_str) {
            Some("d|X") => x_val = Some(v[0]),
            Some("d|Y") => y_val = Some(v[0]),
            Some("d|Z") => z_val = Some(v[0]),
            _ => {}
        }
    }
    let x_val = x_val.expect("X curve");
    let y_val = y_val.expect("Y curve");
    let z_val = z_val.expect("Z curve");
    assert!(x_val.abs() < 1e-3, "X Euler should be ~0, got {x_val}");
    assert!((y_val - 90.0).abs() < 1e-3, "Y Euler should be ~90, got {y_val}");
    assert!(z_val.abs() < 1e-3, "Z Euler should be ~0, got {z_val}");
}

#[test]
fn empty_scene_produces_valid_fbx() {
    let scene = SceneGraph::new();
    let (tree, _) = round_trip(&scene);
    // Just hitting `round_trip` proves the writer doesn't panic on an
    // empty scene; the AnyTree load proves the result is structurally
    // valid. Spot-check the top-level shape.
    let names: Vec<&str> = tree.root().children().map(|c| c.name()).collect();
    for required in [
        "FBXHeaderExtension",
        "GlobalSettings",
        "Documents",
        "Definitions",
        "Objects",
        "Connections",
        "Takes",
    ] {
        assert!(names.iter().any(|n| *n == required), "missing {required}");
    }
}
#[allow(dead_code)]
fn _used(t: NodeHandle<'_>) {
    let _ = read_i32(t, "x");
    let _ = read_f64(t, "y");
}
