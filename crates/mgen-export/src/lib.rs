use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::{json, Value};

use mgen_core::{
    AlphaMode, Clip, Interpolation, Material, Mesh, NodeId, SceneGraph, SceneNode, Skin,
    TextureRef, Track, TrackProperty,
};

const GLB_MAGIC: u32 = 0x46546C67;
const CHUNK_JSON: u32 = 0x4E4F534A;
const CHUNK_BIN: u32 = 0x004E4942;

#[derive(Serialize)]
struct Accessor {
    #[serde(rename = "bufferView")]
    buffer_view: usize,
    #[serde(rename = "componentType")]
    component_type: u32,
    count: usize,
    #[serde(rename = "type")]
    ty: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    min: Option<Vec<f32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max: Option<Vec<f32>>,
}

#[derive(Serialize)]
struct BufferView {
    buffer: usize,
    #[serde(rename = "byteOffset")]
    byte_offset: usize,
    #[serde(rename = "byteLength")]
    byte_length: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<u32>,
}

pub fn write_glb(scene: &SceneGraph, out: &Path) -> Result<()> {
    let mut bin: Vec<u8> = Vec::new();
    let mut buffer_views: Vec<BufferView> = Vec::new();
    let mut accessors: Vec<Accessor> = Vec::new();
    let mut meshes: Vec<Value> = Vec::new();
    let mut nodes: Vec<Value> = Vec::with_capacity(scene.nodes.len());

    // Textures get packed first so `images[]` and `textures[]` indices are
    // known by the time we emit materials. Each unique on-disk path produces
    // one image + bufferView in the BIN chunk and one shared glTF texture
    // (sampler is constant and shared across all textures).
    let texture_table = pack_textures(&scene.materials, &mut bin, &mut buffer_views)?;

    let mut mesh_index_for_node: Vec<Option<usize>> = vec![None; scene.nodes.len()];
    // Dedupe identical (geometry, material) pairs so left/right-mirrored parts
    // like shoulders and elbows share one mesh entry + one copy of the buffer
    // data. Skinned meshes opt out: their joint indices are bound to a
    // particular Skin, and sharing would silently cross-wire deformations.
    let mut mesh_cache: HashMap<MeshKey, usize> = HashMap::new();
    for (i, n) in scene.nodes.iter().enumerate() {
        if let Some(mesh) = &n.mesh {
            let skinned = mesh.is_skinned() && mesh.joints.len() == mesh.positions.len();

            if !skinned {
                let key = MeshKey::from_mesh(mesh, n.material.map(|m| m.0));
                if let Some(&mi) = mesh_cache.get(&key) {
                    mesh_index_for_node[i] = Some(mi);
                    continue;
                }

                let pos_acc = push_positions(&mut bin, &mut buffer_views, &mut accessors, mesh);
                let nrm_acc = push_normals(&mut bin, &mut buffer_views, &mut accessors, mesh);
                let idx_acc = push_indices(&mut bin, &mut buffer_views, &mut accessors, mesh);

                let mut attributes = serde_json::Map::new();
                attributes.insert("POSITION".into(), json!(pos_acc));
                attributes.insert("NORMAL".into(), json!(nrm_acc));
                if mesh.has_uvs() {
                    let uv_acc = push_uvs(&mut bin, &mut buffer_views, &mut accessors, mesh);
                    attributes.insert("TEXCOORD_0".into(), json!(uv_acc));
                }

                let mut primitive = json!({
                    "attributes": Value::Object(attributes),
                    "indices": idx_acc,
                });
                if let Some(mat) = n.material {
                    primitive["material"] = json!(mat.0);
                }

                let mi = meshes.len();
                meshes.push(json!({ "name": n.name, "primitives": [primitive] }));
                mesh_index_for_node[i] = Some(mi);
                mesh_cache.insert(key, mi);
            } else {
                let pos_acc = push_positions(&mut bin, &mut buffer_views, &mut accessors, mesh);
                let nrm_acc = push_normals(&mut bin, &mut buffer_views, &mut accessors, mesh);
                let idx_acc = push_indices(&mut bin, &mut buffer_views, &mut accessors, mesh);

                let mut attributes = serde_json::Map::new();
                attributes.insert("POSITION".into(), json!(pos_acc));
                attributes.insert("NORMAL".into(), json!(nrm_acc));
                if mesh.has_uvs() {
                    let uv_acc = push_uvs(&mut bin, &mut buffer_views, &mut accessors, mesh);
                    attributes.insert("TEXCOORD_0".into(), json!(uv_acc));
                }
                let j_acc = push_joints(&mut bin, &mut buffer_views, &mut accessors, mesh);
                let w_acc = push_weights(&mut bin, &mut buffer_views, &mut accessors, mesh);
                attributes.insert("JOINTS_0".into(), json!(j_acc));
                attributes.insert("WEIGHTS_0".into(), json!(w_acc));

                let mut primitive = json!({
                    "attributes": Value::Object(attributes),
                    "indices": idx_acc,
                });
                if let Some(mat) = n.material {
                    primitive["material"] = json!(mat.0);
                }

                let mi = meshes.len();
                meshes.push(json!({ "name": n.name, "primitives": [primitive] }));
                mesh_index_for_node[i] = Some(mi);
            }
        }
    }

    let skins_json: Vec<Value> = scene
        .skins
        .iter()
        .map(|s| emit_skin(s, &mut bin, &mut buffer_views, &mut accessors))
        .collect();

    for (i, n) in scene.nodes.iter().enumerate() {
        nodes.push(emit_node(n, mesh_index_for_node[i]));
    }

    let materials: Vec<Value> = scene
        .materials
        .iter()
        .map(|m| emit_material(m, &texture_table))
        .collect();
    let extensions_used = collect_material_extensions(&scene.materials);

    let animations: Vec<Value> = scene
        .clips
        .iter()
        .map(|c| emit_animation(c, &mut bin, &mut buffer_views, &mut accessors))
        .collect();

    let root_indices: Vec<u32> = scene.roots.iter().map(|NodeId(i)| *i).collect();
    let buffer_len = bin.len();

    let mut gltf = json!({
        "asset": { "version": "2.0", "generator": "mgen" },
        "scene": 0,
        "scenes": [{ "nodes": root_indices }],
        "nodes": nodes,
        "meshes": meshes,
        "accessors": accessors,
        "bufferViews": buffer_views,
        "buffers": [{ "byteLength": buffer_len }],
    });
    if !materials.is_empty() {
        gltf["materials"] = Value::Array(materials);
    }
    if !animations.is_empty() {
        gltf["animations"] = Value::Array(animations);
    }
    if !skins_json.is_empty() {
        gltf["skins"] = Value::Array(skins_json);
    }
    if !texture_table.images.is_empty() {
        gltf["images"] = Value::Array(texture_table.images.clone());
        gltf["textures"] = Value::Array(texture_table.textures.clone());
        gltf["samplers"] = Value::Array(texture_table.samplers.clone());
    }
    if !extensions_used.is_empty() {
        gltf["extensionsUsed"] = json!(extensions_used);
    }

    let json_bytes = serde_json::to_vec(&gltf)?;
    let json_padded = pad_to_4(json_bytes, b' ');
    let bin_padded = pad_to_4(bin, 0);

    let total_len = 12 + 8 + json_padded.len() + 8 + bin_padded.len();

    let mut f = File::create(out)?;
    f.write_all(&GLB_MAGIC.to_le_bytes())?;
    f.write_all(&2u32.to_le_bytes())?;
    f.write_all(&(total_len as u32).to_le_bytes())?;

    f.write_all(&(json_padded.len() as u32).to_le_bytes())?;
    f.write_all(&CHUNK_JSON.to_le_bytes())?;
    f.write_all(&json_padded)?;

    f.write_all(&(bin_padded.len() as u32).to_le_bytes())?;
    f.write_all(&CHUNK_BIN.to_le_bytes())?;
    f.write_all(&bin_padded)?;

    Ok(())
}

fn emit_material(m: &Material, textures: &TextureTable) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("name".into(), Value::String(m.name.clone()));

    let mut pbr = serde_json::Map::new();
    pbr.insert("baseColorFactor".into(), json!(m.base_color));
    pbr.insert("metallicFactor".into(), json!(m.metallic));
    pbr.insert("roughnessFactor".into(), json!(m.roughness));
    if let Some(idx) = textures.index_of(&m.base_color_texture) {
        pbr.insert("baseColorTexture".into(), json!({ "index": idx }));
    }
    if let Some(idx) = textures.index_of(&m.metallic_roughness_texture) {
        pbr.insert("metallicRoughnessTexture".into(), json!({ "index": idx }));
    }
    obj.insert("pbrMetallicRoughness".into(), Value::Object(pbr));

    if let Some(idx) = textures.index_of(&m.normal_texture) {
        obj.insert("normalTexture".into(), json!({ "index": idx }));
    }
    if let Some(idx) = textures.index_of(&m.occlusion_texture) {
        obj.insert("occlusionTexture".into(), json!({ "index": idx }));
    }
    if let Some(idx) = textures.index_of(&m.emissive_texture) {
        obj.insert("emissiveTexture".into(), json!({ "index": idx }));
    }

    match m.alpha_mode {
        AlphaMode::Opaque => {}
        AlphaMode::Blend => {
            obj.insert("alphaMode".into(), Value::String("BLEND".into()));
        }
        AlphaMode::Mask => {
            obj.insert("alphaMode".into(), Value::String("MASK".into()));
            obj.insert("alphaCutoff".into(), json!(m.alpha_cutoff));
        }
    }

    if m.emissive != [0.0, 0.0, 0.0] {
        obj.insert("emissiveFactor".into(), json!(m.emissive));
    }

    if m.double_sided {
        obj.insert("doubleSided".into(), json!(true));
    }

    let mut extensions = serde_json::Map::new();
    if m.emissive_strength != 1.0 && m.emissive != [0.0, 0.0, 0.0] {
        extensions.insert(
            "KHR_materials_emissive_strength".into(),
            json!({ "emissiveStrength": m.emissive_strength }),
        );
    }
    if m.transmission > 0.0 {
        extensions.insert(
            "KHR_materials_transmission".into(),
            json!({ "transmissionFactor": m.transmission }),
        );
    }
    if !extensions.is_empty() {
        obj.insert("extensions".into(), Value::Object(extensions));
    }

    Value::Object(obj)
}

fn collect_material_extensions(materials: &[Material]) -> Vec<&'static str> {
    let mut used: Vec<&'static str> = Vec::new();
    let push = |name: &'static str, list: &mut Vec<&'static str>| {
        if !list.contains(&name) {
            list.push(name);
        }
    };
    for m in materials {
        if m.emissive_strength != 1.0 && m.emissive != [0.0, 0.0, 0.0] {
            push("KHR_materials_emissive_strength", &mut used);
        }
        if m.transmission > 0.0 {
            push("KHR_materials_transmission", &mut used);
        }
    }
    used
}

fn emit_node(n: &SceneNode, mesh: Option<usize>) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("name".into(), Value::String(n.name.clone()));

    let t = &n.transform;
    if t.translation != glam::Vec3::ZERO {
        obj.insert("translation".into(), json!([t.translation.x, t.translation.y, t.translation.z]));
    }
    if t.rotation != glam::Quat::IDENTITY {
        let q = t.rotation;
        obj.insert("rotation".into(), json!([q.x, q.y, q.z, q.w]));
    }
    if t.scale != glam::Vec3::ONE {
        obj.insert("scale".into(), json!([t.scale.x, t.scale.y, t.scale.z]));
    }
    if let Some(mi) = mesh {
        obj.insert("mesh".into(), json!(mi));
    }
    if let Some(skin) = n.skin {
        obj.insert("skin".into(), json!(skin.0));
    }
    if !n.children.is_empty() {
        let cs: Vec<u32> = n.children.iter().map(|NodeId(i)| *i).collect();
        obj.insert("children".into(), json!(cs));
    }

    let mut extras = serde_json::Map::new();
    if !n.kind.is_empty() && n.kind != n.name {
        extras.insert("kind".into(), Value::String(n.kind.clone()));
    }
    if let Some(role) = &n.role {
        extras.insert("role".into(), Value::String(role.clone()));
    }
    if !n.tags.is_empty() {
        extras.insert("tags".into(), json!(n.tags));
    }
    if !extras.is_empty() {
        obj.insert("extras".into(), Value::Object(extras));
    }

    Value::Object(obj)
}

fn push_positions(
    bin: &mut Vec<u8>,
    views: &mut Vec<BufferView>,
    accessors: &mut Vec<Accessor>,
    mesh: &Mesh,
) -> usize {
    let offset = align_up(bin, 4);
    for v in &mesh.positions {
        for c in v {
            bin.extend_from_slice(&c.to_le_bytes());
        }
    }
    let byte_length = mesh.positions.len() * 12;
    views.push(BufferView { buffer: 0, byte_offset: offset, byte_length, target: Some(34962) });
    let (min, max) = bounds(&mesh.positions);
    accessors.push(Accessor {
        buffer_view: views.len() - 1,
        component_type: 5126,
        count: mesh.positions.len(),
        ty: "VEC3",
        min: Some(min.to_vec()),
        max: Some(max.to_vec()),
    });
    accessors.len() - 1
}

fn push_normals(
    bin: &mut Vec<u8>,
    views: &mut Vec<BufferView>,
    accessors: &mut Vec<Accessor>,
    mesh: &Mesh,
) -> usize {
    let offset = align_up(bin, 4);
    for v in &mesh.normals {
        for c in v {
            bin.extend_from_slice(&c.to_le_bytes());
        }
    }
    let byte_length = mesh.normals.len() * 12;
    views.push(BufferView { buffer: 0, byte_offset: offset, byte_length, target: Some(34962) });
    accessors.push(Accessor {
        buffer_view: views.len() - 1,
        component_type: 5126,
        count: mesh.normals.len(),
        ty: "VEC3",
        min: None,
        max: None,
    });
    accessors.len() - 1
}

fn push_uvs(
    bin: &mut Vec<u8>,
    views: &mut Vec<BufferView>,
    accessors: &mut Vec<Accessor>,
    mesh: &Mesh,
) -> usize {
    let offset = align_up(bin, 4);
    for uv in &mesh.uvs {
        for c in uv {
            bin.extend_from_slice(&c.to_le_bytes());
        }
    }
    let byte_length = mesh.uvs.len() * 8;
    views.push(BufferView { buffer: 0, byte_offset: offset, byte_length, target: Some(34962) });
    accessors.push(Accessor {
        buffer_view: views.len() - 1,
        component_type: 5126,
        count: mesh.uvs.len(),
        ty: "VEC2",
        min: None,
        max: None,
    });
    accessors.len() - 1
}

fn push_indices(
    bin: &mut Vec<u8>,
    views: &mut Vec<BufferView>,
    accessors: &mut Vec<Accessor>,
    mesh: &Mesh,
) -> usize {
    // align_up(.., 4) keeps the next view aligned for either component size.
    let offset = align_up(bin, 4);
    let use_u16 = mesh.positions.len() <= u16::MAX as usize;
    let (byte_length, component_type) = if use_u16 {
        for i in &mesh.indices {
            bin.extend_from_slice(&(*i as u16).to_le_bytes());
        }
        (mesh.indices.len() * 2, 5123u32) // UNSIGNED_SHORT
    } else {
        for i in &mesh.indices {
            bin.extend_from_slice(&i.to_le_bytes());
        }
        (mesh.indices.len() * 4, 5125u32) // UNSIGNED_INT
    };
    views.push(BufferView { buffer: 0, byte_offset: offset, byte_length, target: Some(34963) });
    accessors.push(Accessor {
        buffer_view: views.len() - 1,
        component_type,
        count: mesh.indices.len(),
        ty: "SCALAR",
        min: None,
        max: None,
    });
    accessors.len() - 1
}

#[derive(Hash, Eq, PartialEq)]
struct MeshKey {
    // f32 bit patterns so the key is hashable; NaN/±0 are treated as distinct
    // bit-for-bit, which is fine for dedup.
    positions: Vec<[u32; 3]>,
    normals: Vec<[u32; 3]>,
    uvs: Vec<[u32; 2]>,
    indices: Vec<u32>,
    material: Option<u32>,
}

impl MeshKey {
    fn from_mesh(mesh: &Mesh, material: Option<u32>) -> Self {
        let positions = mesh
            .positions
            .iter()
            .map(|v| [v[0].to_bits(), v[1].to_bits(), v[2].to_bits()])
            .collect();
        let normals = mesh
            .normals
            .iter()
            .map(|v| [v[0].to_bits(), v[1].to_bits(), v[2].to_bits()])
            .collect();
        let uvs = mesh
            .uvs
            .iter()
            .map(|v| [v[0].to_bits(), v[1].to_bits()])
            .collect();
        MeshKey {
            positions,
            normals,
            uvs,
            indices: mesh.indices.clone(),
            material,
        }
    }
}

/// Packed image + texture metadata, indexed by canonical file path. Built
/// before material emission so `emit_material` can resolve texture slots to
/// glTF indices.
#[derive(Default)]
struct TextureTable {
    /// glTF `images[]` entries — each points at a bufferView holding the raw
    /// PNG bytes.
    images: Vec<Value>,
    /// glTF `textures[]` entries — paired with the shared sampler.
    textures: Vec<Value>,
    /// glTF `samplers[]` — currently always one shared sampler.
    samplers: Vec<Value>,
    /// path → index into `textures[]`.
    by_path: HashMap<PathBuf, usize>,
}

impl TextureTable {
    fn index_of(&self, tex: &Option<TextureRef>) -> Option<usize> {
        let t = tex.as_ref()?;
        self.by_path.get(&t.path).copied()
    }
}

/// Read every unique texture file referenced by materials, embed the raw PNG
/// bytes into the BIN chunk, and fill out the glTF image / texture / sampler
/// tables. Returns the populated table plus side-effects on `bin` and
/// `buffer_views`.
fn pack_textures(
    materials: &[Material],
    bin: &mut Vec<u8>,
    buffer_views: &mut Vec<BufferView>,
) -> Result<TextureTable> {
    let mut table = TextureTable::default();

    // Collect every texture reference in authored order so indices are stable
    // across rebuilds.
    let mut slots: Vec<&TextureRef> = Vec::new();
    for m in materials {
        for slot in [
            &m.base_color_texture,
            &m.metallic_roughness_texture,
            &m.normal_texture,
            &m.occlusion_texture,
            &m.emissive_texture,
        ] {
            if let Some(t) = slot {
                slots.push(t);
            }
        }
    }
    if slots.is_empty() {
        return Ok(table);
    }

    // One shared sampler: linear mipmapping, linear mag, repeat on both axes.
    // Matches Godot's default texture import and is the right choice for
    // tileable PBR maps. Per-texture sampler overrides can come later.
    table.samplers.push(json!({
        "magFilter": 9729,        // LINEAR
        "minFilter": 9987,        // LINEAR_MIPMAP_LINEAR
        "wrapS": 10497,           // REPEAT
        "wrapT": 10497,
    }));

    for t in slots {
        if table.by_path.contains_key(&t.path) {
            continue;
        }
        let bytes = fs::read(&t.path).with_context(|| {
            format!("reading texture file {}", t.path.display())
        })?;
        let mime = mime_type_for(&t.path).unwrap_or("image/png");

        let offset = align_up(bin, 4);
        let byte_length = bytes.len();
        bin.extend_from_slice(&bytes);
        buffer_views.push(BufferView {
            buffer: 0,
            byte_offset: offset,
            byte_length,
            target: None,
        });

        let image_idx = table.images.len();
        table.images.push(json!({
            "bufferView": buffer_views.len() - 1,
            "mimeType": mime,
            "name": t.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
        }));

        let texture_idx = table.textures.len();
        table.textures.push(json!({
            "source": image_idx,
            "sampler": 0,
        }));
        table.by_path.insert(t.path.clone(), texture_idx);
    }

    Ok(table)
}

fn mime_type_for(path: &Path) -> Option<&'static str> {
    match path.extension().and_then(|e| e.to_str()).map(|s| s.to_ascii_lowercase()) {
        Some(ref s) if s == "png" => Some("image/png"),
        Some(ref s) if s == "jpg" || s == "jpeg" => Some("image/jpeg"),
        _ => None,
    }
}

fn bounds(verts: &[[f32; 3]]) -> ([f32; 3], [f32; 3]) {
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for v in verts {
        for i in 0..3 {
            if v[i] < min[i] { min[i] = v[i]; }
            if v[i] > max[i] { max[i] = v[i]; }
        }
    }
    (min, max)
}

fn align_up(bin: &mut Vec<u8>, align: usize) -> usize {
    while bin.len() % align != 0 {
        bin.push(0);
    }
    bin.len()
}

fn pad_to_4(mut bytes: Vec<u8>, filler: u8) -> Vec<u8> {
    while bytes.len() % 4 != 0 {
        bytes.push(filler);
    }
    bytes
}

fn emit_animation(
    clip: &Clip,
    bin: &mut Vec<u8>,
    views: &mut Vec<BufferView>,
    accessors: &mut Vec<Accessor>,
) -> Value {
    let mut samplers: Vec<Value> = Vec::with_capacity(clip.tracks.len());
    let mut channels: Vec<Value> = Vec::with_capacity(clip.tracks.len());

    for track in &clip.tracks {
        let input_acc = push_times(bin, views, accessors, &track.times);
        let output_acc = push_track_values(bin, views, accessors, track);
        let interp = match track.interpolation {
            Interpolation::Linear => "LINEAR",
            Interpolation::Step => "STEP",
        };
        let sampler_idx = samplers.len();
        samplers.push(json!({
            "input": input_acc,
            "output": output_acc,
            "interpolation": interp,
        }));
        let path = match track.property {
            TrackProperty::Translation => "translation",
            TrackProperty::Rotation => "rotation",
            TrackProperty::Scale => "scale",
        };
        channels.push(json!({
            "sampler": sampler_idx,
            "target": { "node": track.node.0, "path": path },
        }));
    }

    json!({
        "name": clip.name,
        "channels": channels,
        "samplers": samplers,
    })
}

fn push_times(
    bin: &mut Vec<u8>,
    views: &mut Vec<BufferView>,
    accessors: &mut Vec<Accessor>,
    times: &[f32],
) -> usize {
    let offset = align_up(bin, 4);
    for t in times {
        bin.extend_from_slice(&t.to_le_bytes());
    }
    let byte_length = times.len() * 4;
    views.push(BufferView { buffer: 0, byte_offset: offset, byte_length, target: None });
    // Animation input accessors must declare min/max per the glTF spec.
    let (mut min_t, mut max_t) = (f32::INFINITY, f32::NEG_INFINITY);
    for t in times {
        if *t < min_t { min_t = *t; }
        if *t > max_t { max_t = *t; }
    }
    accessors.push(Accessor {
        buffer_view: views.len() - 1,
        component_type: 5126,
        count: times.len(),
        ty: "SCALAR",
        min: Some(vec![min_t]),
        max: Some(vec![max_t]),
    });
    accessors.len() - 1
}

fn emit_skin(
    skin: &Skin,
    bin: &mut Vec<u8>,
    views: &mut Vec<BufferView>,
    accessors: &mut Vec<Accessor>,
) -> Value {
    let ibm_acc = push_inverse_bind_matrices(bin, views, accessors, &skin.inverse_bind_matrices);
    let joints: Vec<u32> = skin.joints.iter().map(|NodeId(i)| *i).collect();
    let mut obj = serde_json::Map::new();
    obj.insert("name".into(), Value::String(skin.name.clone()));
    obj.insert("joints".into(), json!(joints));
    obj.insert("inverseBindMatrices".into(), json!(ibm_acc));
    if let Some(root) = skin.skeleton_root {
        obj.insert("skeleton".into(), json!(root.0));
    }
    Value::Object(obj)
}

fn push_inverse_bind_matrices(
    bin: &mut Vec<u8>,
    views: &mut Vec<BufferView>,
    accessors: &mut Vec<Accessor>,
    ibms: &[[[f32; 4]; 4]],
) -> usize {
    let offset = align_up(bin, 4);
    // glTF stores MAT4 column-major. `Mat4::to_cols_array_2d` already returns
    // columns-first ([[col0], [col1], [col2], [col3]]), so a straight float
    // dump in row-major-of-columns order hits the spec.
    for m in ibms {
        for col in m {
            for c in col {
                bin.extend_from_slice(&c.to_le_bytes());
            }
        }
    }
    let byte_length = ibms.len() * 64;
    views.push(BufferView { buffer: 0, byte_offset: offset, byte_length, target: None });
    accessors.push(Accessor {
        buffer_view: views.len() - 1,
        component_type: 5126,
        count: ibms.len(),
        ty: "MAT4",
        min: None,
        max: None,
    });
    accessors.len() - 1
}

fn push_joints(
    bin: &mut Vec<u8>,
    views: &mut Vec<BufferView>,
    accessors: &mut Vec<Accessor>,
    mesh: &Mesh,
) -> usize {
    let offset = align_up(bin, 4);
    for row in &mesh.joints {
        for j in row {
            bin.extend_from_slice(&j.to_le_bytes());
        }
    }
    let byte_length = mesh.joints.len() * 8;
    views.push(BufferView { buffer: 0, byte_offset: offset, byte_length, target: Some(34962) });
    accessors.push(Accessor {
        buffer_view: views.len() - 1,
        component_type: 5123, // UNSIGNED_SHORT
        count: mesh.joints.len(),
        ty: "VEC4",
        min: None,
        max: None,
    });
    accessors.len() - 1
}

fn push_weights(
    bin: &mut Vec<u8>,
    views: &mut Vec<BufferView>,
    accessors: &mut Vec<Accessor>,
    mesh: &Mesh,
) -> usize {
    let offset = align_up(bin, 4);
    for row in &mesh.weights {
        for w in row {
            bin.extend_from_slice(&w.to_le_bytes());
        }
    }
    let byte_length = mesh.weights.len() * 16;
    views.push(BufferView { buffer: 0, byte_offset: offset, byte_length, target: Some(34962) });
    accessors.push(Accessor {
        buffer_view: views.len() - 1,
        component_type: 5126, // FLOAT
        count: mesh.weights.len(),
        ty: "VEC4",
        min: None,
        max: None,
    });
    accessors.len() - 1
}

fn push_track_values(
    bin: &mut Vec<u8>,
    views: &mut Vec<BufferView>,
    accessors: &mut Vec<Accessor>,
    track: &Track,
) -> usize {
    let offset = align_up(bin, 4);
    let ty = match track.property {
        TrackProperty::Rotation => "VEC4",
        TrackProperty::Translation | TrackProperty::Scale => "VEC3",
    };
    let components = if ty == "VEC4" { 4 } else { 3 };
    for v in &track.values {
        for c in &v[..components] {
            bin.extend_from_slice(&c.to_le_bytes());
        }
    }
    let byte_length = track.values.len() * components * 4;
    views.push(BufferView { buffer: 0, byte_offset: offset, byte_length, target: None });
    accessors.push(Accessor {
        buffer_view: views.len() - 1,
        component_type: 5126,
        count: track.values.len(),
        ty: if ty == "VEC4" { "VEC4" } else { "VEC3" },
        min: None,
        max: None,
    });
    accessors.len() - 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mat(name: &str) -> Material {
        Material::new(name)
    }

    #[test]
    fn opaque_material_omits_extensions_and_alpha_mode() {
        let m = mat("base");
        let v = emit_material(&m, &TextureTable::default());
        assert!(v.get("alphaMode").is_none());
        assert!(v.get("emissiveFactor").is_none());
        assert!(v.get("extensions").is_none());
    }

    #[test]
    fn blend_material_emits_alpha_mode_but_no_cutoff() {
        let mut m = mat("gel");
        m.alpha_mode = AlphaMode::Blend;
        m.base_color[3] = 0.3;
        let v = emit_material(&m, &TextureTable::default());
        assert_eq!(v["alphaMode"], json!("BLEND"));
        assert!(v.get("alphaCutoff").is_none());
    }

    #[test]
    fn mask_material_emits_cutoff() {
        let mut m = mat("leaf");
        m.alpha_mode = AlphaMode::Mask;
        m.alpha_cutoff = 0.25;
        let v = emit_material(&m, &TextureTable::default());
        assert_eq!(v["alphaMode"], json!("MASK"));
        assert_eq!(v["alphaCutoff"], json!(0.25));
    }

    #[test]
    fn double_sided_emits_core_flag_without_extension() {
        // `doubleSided` is a core glTF property — no extension to register.
        let mut m = mat("leaf");
        m.double_sided = true;
        let v = emit_material(&m, &TextureTable::default());
        assert_eq!(v["doubleSided"], json!(true));
        assert!(v.get("extensions").is_none());
        assert!(collect_material_extensions(std::slice::from_ref(&m)).is_empty());

        // Default stays off, and off materials omit the field entirely.
        let off = mat("wood");
        let v_off = emit_material(&off, &TextureTable::default());
        assert!(v_off.get("doubleSided").is_none());
    }

    #[test]
    fn transmission_triggers_extension_and_extensions_used() {
        let mut m = mat("glass");
        m.transmission = 0.8;
        let v = emit_material(&m, &TextureTable::default());
        let got = v["extensions"]["KHR_materials_transmission"]["transmissionFactor"]
            .as_f64()
            .unwrap();
        assert_eq!(got as f32, 0.8);
        let used = collect_material_extensions(std::slice::from_ref(&m));
        assert_eq!(used, vec!["KHR_materials_transmission"]);
    }

    #[test]
    fn emissive_strength_only_counts_when_emissive_is_nonzero() {
        // Strength alone, with zero emissive, is not a useful extension.
        let mut m = mat("dim");
        m.emissive_strength = 5.0;
        assert!(collect_material_extensions(std::slice::from_ref(&m)).is_empty());

        // With a real emissive colour, the extension should ship.
        m.emissive = [1.0, 0.2, 1.0];
        let v = emit_material(&m, &TextureTable::default());
        assert!(v.get("emissiveFactor").is_some());
        let got = v["extensions"]["KHR_materials_emissive_strength"]["emissiveStrength"]
            .as_f64()
            .unwrap();
        assert_eq!(got as f32, 5.0);
    }

    #[test]
    fn default_emissive_strength_with_emissive_does_not_add_extension() {
        let mut m = mat("dim_glow");
        m.emissive = [0.1, 0.1, 0.1];
        // strength stays at the 1.0 default
        let v = emit_material(&m, &TextureTable::default());
        assert!(v.get("emissiveFactor").is_some());
        assert!(v.get("extensions").is_none());
    }

    #[test]
    fn extensions_used_deduplicates_across_materials() {
        let mut a = mat("a");
        a.transmission = 0.5;
        let mut b = mat("b");
        b.transmission = 0.7;
        let used = collect_material_extensions(&[a, b]);
        assert_eq!(used, vec!["KHR_materials_transmission"]);
    }
}
