use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::Path;

use anyhow::Result;
use serde_json::{json, Value};

use mogen_core::{ColliderShape, MaterialId, Mesh, NodeId, SceneGraph, SceneNode};

use crate::accessor::{
    push_indices, push_joints, push_normals, push_positions, push_uvs, push_weights,
};
use crate::animation::emit_animation;
#[cfg(feature = "imposter")]
use crate::imposter::{self, ImposterAtlas};
use crate::lights::collect_lights;
#[cfg(feature = "lod")]
use crate::lod;
use crate::material::{collect_material_extensions, emit_material};
#[cfg(feature = "merge")]
use crate::merge;
use crate::skin::emit_skin;
use crate::texture::{pack_textures, FsTextureSource, TextureSource, TextureTable};
use crate::{
    pad_to_4, Accessor, BufferView, ExportOptions, CHUNK_BIN, CHUNK_JSON, GLB_MAGIC,
};

pub fn write_glb(scene: &SceneGraph, out: &Path) -> Result<()> {
    write_glb_with_options(scene, out, &ExportOptions::default(), |_| {})
}

/// File-writing wrapper around [`build_glb_with_options`]. The pipeline is the
/// same; this just streams the resulting bytes to disk. Use
/// [`build_glb_with_options`] directly when you need the GLB in memory (e.g.
/// from a wasm caller that has no filesystem).
pub fn write_glb_with_options<F: Fn(&str)>(
    scene: &SceneGraph,
    out: &Path,
    opts: &ExportOptions,
    progress: F,
) -> Result<()> {
    let bytes = build_glb_with_options(scene, opts, progress)?;
    let mut f = File::create(out)?;
    f.write_all(&bytes)?;
    Ok(())
}

/// Write a GLB to disk using a caller-supplied pre-baked imposter atlas
/// instead of running the headless bake inside the export pipeline. Used by
/// Studio (eframe owns the only winit `EventLoop`, so the headless bake
/// fails — Studio bakes via its live GL context first and hands the result
/// in here).
///
/// `prebaked` is honoured only when `opts.bundle_lods_and_imposter` is on;
/// passing `Some(_)` with the flag off silently ignores the atlas.
#[cfg(feature = "imposter")]
pub fn write_glb_with_prebaked_imposter<F: Fn(&str)>(
    scene: &SceneGraph,
    out: &Path,
    opts: &ExportOptions,
    prebaked: Option<ImposterAtlas>,
    progress: F,
) -> Result<()> {
    let bytes =
        build_glb_with_options_and_source_imp(scene, opts, &FsTextureSource, prebaked, progress)?;
    let mut f = File::create(out)?;
    f.write_all(&bytes)?;
    Ok(())
}

/// Build a GLB into a `Vec<u8>` using the filesystem to load any textures
/// referenced by the scene's materials. Convenience wrapper around
/// [`build_glb_with_options_and_source`] for desktop callers — wasm callers
/// (no filesystem) construct a [`MapTextureSource`](crate::MapTextureSource)
/// and call the underlying function directly.
pub fn build_glb_with_options<F: Fn(&str)>(
    scene: &SceneGraph,
    opts: &ExportOptions,
    progress: F,
) -> Result<Vec<u8>> {
    build_glb_with_options_and_source(scene, opts, &FsTextureSource, progress)
}

/// Build a GLB into a `Vec<u8>` with a caller-supplied [`TextureSource`].
/// The wasm preview uses this with a [`MapTextureSource`](crate::MapTextureSource)
/// of in-memory PNG bytes; desktop callers go through
/// [`build_glb_with_options`] which plugs in [`FsTextureSource`].
pub fn build_glb_with_options_and_source<F: Fn(&str)>(
    scene: &SceneGraph,
    opts: &ExportOptions,
    texture_source: &dyn TextureSource,
    progress: F,
) -> Result<Vec<u8>> {
    #[cfg(feature = "imposter")]
    {
        build_glb_with_options_and_source_imp(scene, opts, texture_source, None, progress)
    }
    #[cfg(not(feature = "imposter"))]
    {
        build_glb_with_options_and_source_inner(scene, opts, texture_source, progress)
    }
}

#[cfg(feature = "imposter")]
fn build_glb_with_options_and_source_imp<F: Fn(&str)>(
    scene: &SceneGraph,
    opts: &ExportOptions,
    texture_source: &dyn TextureSource,
    prebaked_imposter: Option<ImposterAtlas>,
    progress: F,
) -> Result<Vec<u8>> {
    build_glb_with_options_and_source_inner(
        scene,
        opts,
        texture_source,
        prebaked_imposter,
        progress,
    )
}

fn build_glb_with_options_and_source_inner<F: Fn(&str)>(
    scene: &SceneGraph,
    opts: &ExportOptions,
    texture_source: &dyn TextureSource,
    #[cfg(feature = "imposter")] prebaked_imposter: Option<ImposterAtlas>,
    progress: F,
) -> Result<Vec<u8>> {
    // Two-stage merge. First, the scoped `solid` pass runs whenever the scene
    // carries any `"solid"`-tagged nodes (opt-in from the DSL — no flag
    // needed). Its clone is skipped if no solid groups are present. Then, if
    // the caller also requested the global pass via ExportOptions, it runs on
    // top. Each stage clones so we hold the owning graph in an Option and
    // rebind `scene` to the latest one. The whole merge stage is feature-
    // gated because it pulls in `mogen-geom`'s CSG path; wasm builds disable
    // the `merge` feature and skip it entirely.
    #[cfg(feature = "merge")]
    let solid_owned: Option<SceneGraph> = {
        let has_solid = scene
            .nodes
            .iter()
            .any(|n| n.tags.iter().any(|t| t == "solid"));
        if has_solid {
            Some(merge::merge_solid_groups(scene, |s| progress(s)))
        } else {
            None
        }
    };
    #[cfg(feature = "merge")]
    let scene_after_solid: &SceneGraph = solid_owned.as_ref().unwrap_or(scene);
    #[cfg(not(feature = "merge"))]
    let scene_after_solid: &SceneGraph = scene;

    #[cfg(feature = "merge")]
    let merged_owned: Option<SceneGraph> = if opts.merge_sibling_meshes {
        Some(merge::merge_sibling_meshes(scene_after_solid, |s| {
            progress(s)
        }))
    } else {
        None
    };
    #[cfg(feature = "merge")]
    let scene: &SceneGraph = match &merged_owned {
        Some(s) => s,
        None => scene_after_solid,
    };
    #[cfg(not(feature = "merge"))]
    let scene: &SceneGraph = scene_after_solid;

    let mut bin: Vec<u8> = Vec::new();
    let mut buffer_views: Vec<BufferView> = Vec::new();
    let mut accessors: Vec<Accessor> = Vec::new();
    let mut meshes: Vec<Value> = Vec::new();
    let mut nodes: Vec<Value> = Vec::with_capacity(scene.nodes.len());

    // Textures get packed first so `images[]` and `textures[]` indices are
    // known by the time we emit materials. Each unique on-disk path produces
    // one image + bufferView in the BIN chunk and one shared glTF texture
    // (sampler is constant and shared across all textures). Skipping this
    // step leaves the table empty — `emit_material` then omits every
    // `*Texture` slot and the materials export as pure PBR factors.
    let texture_table = if opts.include_textures {
        progress("packing textures");
        pack_textures(&scene.materials, &mut bin, &mut buffer_views, texture_source)?
    } else {
        TextureTable::default()
    };

    let mut mesh_index_for_node: Vec<Option<usize>> = vec![None; scene.nodes.len()];
    // Per-source-node list of LOD mesh indices (LOD1, LOD2, LOD3 …). Only
    // populated when `bundle_lods_and_imposter` is on and the source mesh
    // qualified for simplification. Used after node emission to attach
    // `MSFT_lod` to the owning source node.
    #[cfg(feature = "lod")]
    let mut lod_meshes_for_node: Vec<Vec<usize>> = vec![Vec::new(); scene.nodes.len()];
    // Dedupe identical (geometry, material) pairs so left/right-mirrored parts
    // like shoulders and elbows share one mesh entry + one copy of the buffer
    // data. Skinned meshes opt out: their joint indices are bound to a
    // particular Skin, and sharing would silently cross-wire deformations.
    let mut mesh_cache: HashMap<MeshKey, usize> = HashMap::new();
    #[cfg(feature = "lod")]
    let mut lod_cache: HashMap<MeshKey, Vec<usize>> = HashMap::new();
    for (i, n) in scene.nodes.iter().enumerate() {
        if let Some(mesh) = &n.mesh {
            let skinned = mesh.is_skinned() && mesh.joints.len() == mesh.positions.len();

            if !skinned {
                let key = MeshKey::from_mesh(mesh, n.material.map(|m| m.0));
                if let Some(&mi) = mesh_cache.get(&key) {
                    mesh_index_for_node[i] = Some(mi);
                    #[cfg(feature = "lod")]
                    if opts.bundle_lods_and_imposter {
                        if let Some(cached_lods) = lod_cache.get(&key) {
                            lod_meshes_for_node[i] = cached_lods.clone();
                        }
                    }
                    continue;
                }

                let pos_acc = push_positions(&mut bin, &mut buffer_views, &mut accessors, mesh);
                let nrm_acc = push_normals(&mut bin, &mut buffer_views, &mut accessors, mesh);
                let idx_acc = push_indices(&mut bin, &mut buffer_views, &mut accessors, mesh);

                let uv_acc_opt = if mesh.has_uvs() {
                    let scale = uv_scale_for(scene, n.material);
                    Some(push_uvs(
                        &mut bin,
                        &mut buffer_views,
                        &mut accessors,
                        mesh,
                        scale,
                    ))
                } else {
                    None
                };

                let mut attributes = serde_json::Map::new();
                attributes.insert("POSITION".into(), json!(pos_acc));
                attributes.insert("NORMAL".into(), json!(nrm_acc));
                if let Some(uv_acc) = uv_acc_opt {
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
                mesh_cache.insert(key.clone(), mi);

                // LOD1..LODn share the original POSITION / NORMAL / TEXCOORD_0
                // accessors — only the index buffer is regenerated, so the
                // bin-chunk growth per LOD is just the smaller index list.
                #[cfg(feature = "lod")]
                if opts.bundle_lods_and_imposter {
                    let lod_meshes = lod::build_lod_meshes(mesh);
                    let mut lod_indices = Vec::with_capacity(lod_meshes.len());
                    for (lod_idx, lod_mesh) in lod_meshes.iter().enumerate() {
                        let lod_idx_acc = push_indices(
                            &mut bin,
                            &mut buffer_views,
                            &mut accessors,
                            lod_mesh,
                        );
                        let mut lod_attrs = serde_json::Map::new();
                        lod_attrs.insert("POSITION".into(), json!(pos_acc));
                        lod_attrs.insert("NORMAL".into(), json!(nrm_acc));
                        if let Some(uv_acc) = uv_acc_opt {
                            lod_attrs.insert("TEXCOORD_0".into(), json!(uv_acc));
                        }
                        let mut lod_prim = json!({
                            "attributes": Value::Object(lod_attrs),
                            "indices": lod_idx_acc,
                        });
                        if let Some(mat) = n.material {
                            lod_prim["material"] = json!(mat.0);
                        }
                        let lod_mi = meshes.len();
                        meshes.push(json!({
                            "name": format!("{}__lod{}", n.name, lod_idx + 1),
                            "primitives": [lod_prim],
                        }));
                        lod_indices.push(lod_mi);
                    }
                    if !lod_indices.is_empty() {
                        lod_cache.insert(key, lod_indices.clone());
                        lod_meshes_for_node[i] = lod_indices;
                    }
                }
            } else {
                let pos_acc = push_positions(&mut bin, &mut buffer_views, &mut accessors, mesh);
                let nrm_acc = push_normals(&mut bin, &mut buffer_views, &mut accessors, mesh);
                let idx_acc = push_indices(&mut bin, &mut buffer_views, &mut accessors, mesh);

                let mut attributes = serde_json::Map::new();
                attributes.insert("POSITION".into(), json!(pos_acc));
                attributes.insert("NORMAL".into(), json!(nrm_acc));
                if mesh.has_uvs() {
                    let scale = uv_scale_for(scene, n.material);
                    let uv_acc =
                        push_uvs(&mut bin, &mut buffer_views, &mut accessors, mesh, scale);
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

    let light_table = collect_lights(scene);

    for (i, n) in scene.nodes.iter().enumerate() {
        nodes.push(emit_node(n, mesh_index_for_node[i], light_table.node_to_index[i]));
    }

    let mut materials: Vec<Value> = scene
        .materials
        .iter()
        .map(|m| emit_material(m, &texture_table))
        .collect();
    let mut extensions_used = collect_material_extensions(&scene.materials);
    if !light_table.is_empty() {
        extensions_used.push("KHR_lights_punctual");
    }

    let animations: Vec<Value> = if opts.include_animations {
        scene
            .clips
            .iter()
            .map(|c| emit_animation(c, &mut bin, &mut buffer_views, &mut accessors))
            .collect()
    } else {
        Vec::new()
    };

    // Texture tables move out by-value here so the imposter pass can append
    // its own image/texture/sampler entries before we serialise. Cheap
    // (cloned later via `Value::Array` anyway) and lets us avoid double-
    // cloning into the JSON.
    let TextureTable {
        mut images,
        mut textures,
        mut samplers,
        ..
    } = texture_table;

    let mut root_indices: Vec<u32> = scene.roots.iter().map(|NodeId(i)| *i).collect();

    // LOD post-processing. For each source node that produced LOD meshes
    // during the mesh-emission loop, allocate orphan nodes pointing at
    // those meshes and stamp `MSFT_lod` onto the source node's JSON. The
    // orphans live outside `scene.roots` — `MSFT_lod.ids` is the only
    // place that references them, so importers that don't recognise the
    // extension see exactly the original scene.
    #[cfg(feature = "lod")]
    {
        let mut any_lods = false;
        for src_i in 0..scene.nodes.len() {
            if lod_meshes_for_node[src_i].is_empty() {
                continue;
            }
            any_lods = true;
            let lod_meshes = std::mem::take(&mut lod_meshes_for_node[src_i]);
            let mut lod_node_ids: Vec<u32> = Vec::with_capacity(lod_meshes.len());
            for (stage_i, lod_mi) in lod_meshes.iter().enumerate() {
                let node_idx = nodes.len() as u32;
                nodes.push(json!({
                    "name": format!("{}__lod{}", scene.nodes[src_i].name, stage_i + 1),
                    "mesh": lod_mi,
                }));
                lod_node_ids.push(node_idx);
            }
            // `extras.MSFT_screencoverage` parallels [source, LOD1..N]: one
            // gate per representation. We clamp the slice to lod_node_ids
            // + 1 so a 1- or 2-LOD chain doesn't carry stale tail thresholds.
            let coverage: Vec<f32> = lod::SCREEN_COVERAGE[..=lod_node_ids.len()].to_vec();

            let obj = nodes[src_i].as_object_mut().expect("node JSON is an object");
            let ext_map = obj
                .entry("extensions")
                .or_insert_with(|| Value::Object(Default::default()))
                .as_object_mut()
                .expect("extensions is an object");
            ext_map.insert("MSFT_lod".into(), json!({ "ids": lod_node_ids }));

            let extras_map = obj
                .entry("extras")
                .or_insert_with(|| Value::Object(Default::default()))
                .as_object_mut()
                .expect("extras is an object");
            extras_map.insert("MSFT_screencoverage".into(), json!(coverage));
        }
        if any_lods && !extensions_used.iter().any(|e| *e == "MSFT_lod") {
            extensions_used.push("MSFT_lod");
        }
    }

    // Imposter emission. Runs after every other geometry / material pass so
    // the bake captures the final post-merge scene, and so the new mesh /
    // material indices land at the end of their respective tables.
    #[cfg(feature = "imposter")]
    if opts.bundle_lods_and_imposter {
        let emission = imposter::emit_imposter(
            scene,
            prebaked_imposter,
            &mut bin,
            &mut buffer_views,
            &mut accessors,
            &mut meshes,
            &mut materials,
            &mut images,
            &mut textures,
            &mut samplers,
            &|s| progress(s),
        )?;
        let node_idx = nodes.len() as u32;
        nodes.push(json!({
            "name": "imposter",
            "mesh": emission.mesh_index,
            "extras": {
                "tags": ["imposter"],
                "imposter": {
                    "view_count": 8,
                    "layout": "yaw_grid",
                    "material": emission.material_index,
                },
            },
        }));
        root_indices.push(node_idx);
        if !extensions_used.iter().any(|e| *e == "KHR_materials_unlit") {
            extensions_used.push("KHR_materials_unlit");
        }
    }

    let buffer_len = bin.len();

    let mut gltf = json!({
        "asset": { "version": "2.0", "generator": "MoGen" },
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
    if !images.is_empty() {
        gltf["images"] = Value::Array(images);
        gltf["textures"] = Value::Array(textures);
        gltf["samplers"] = Value::Array(samplers);
    }
    if !extensions_used.is_empty() {
        gltf["extensionsUsed"] = json!(extensions_used);
    }
    if !light_table.is_empty() {
        gltf["extensions"] = json!({
            "KHR_lights_punctual": { "lights": light_table.lights }
        });
    }

    progress("writing glb");
    let json_bytes = serde_json::to_vec(&gltf)?;
    let json_padded = pad_to_4(json_bytes, b' ');
    let bin_padded = pad_to_4(bin, 0);

    let total_len = 12 + 8 + json_padded.len() + 8 + bin_padded.len();

    let mut out = Vec::with_capacity(total_len);
    out.extend_from_slice(&GLB_MAGIC.to_le_bytes());
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total_len as u32).to_le_bytes());

    out.extend_from_slice(&(json_padded.len() as u32).to_le_bytes());
    out.extend_from_slice(&CHUNK_JSON.to_le_bytes());
    out.extend_from_slice(&json_padded);

    out.extend_from_slice(&(bin_padded.len() as u32).to_le_bytes());
    out.extend_from_slice(&CHUNK_BIN.to_le_bytes());
    out.extend_from_slice(&bin_padded);

    Ok(out)
}

fn emit_node(n: &SceneNode, mesh: Option<usize>, light: Option<usize>) -> Value {
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
    if let Some(li) = light {
        obj.insert(
            "extensions".into(),
            json!({ "KHR_lights_punctual": { "light": li } }),
        );
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
    if let Some(shape) = &n.collider {
        let payload = match shape {
            ColliderShape::Aabb { aabb } => json!({
                "type": "aabb",
                "min": [aabb.min.x, aabb.min.y, aabb.min.z],
                "max": [aabb.max.x, aabb.max.y, aabb.max.z],
            }),
            // Trimesh and Convex reference `node.mesh` implicitly — the
            // importer reads positions + indices from the glTF primitive
            // already attached to this node and builds the matching
            // collision shape, so no extra geometry is serialised here.
            ColliderShape::Trimesh => json!({ "type": "trimesh" }),
            ColliderShape::Convex => json!({ "type": "convex" }),
        };
        extras.insert("collider".into(), payload);
    }
    if let Some(slot) = &n.slot {
        let mut obj = serde_json::Map::new();
        obj.insert("kind".into(), Value::String(slot.kind.clone()));
        obj.insert("width".into(), json!(slot.width));
        obj.insert("height".into(), json!(slot.height));
        if slot.depth != 0.0 {
            obj.insert("depth".into(), json!(slot.depth));
        }
        extras.insert("slot".into(), Value::Object(obj));
    }
    // Default is true; only stamp the hint when the author opted out so the
    // typical case keeps the JSON chunk lean. Importers that don't recognise
    // the key fall back to "casts shadows" — the spec-default behaviour.
    if !n.cast_shadow {
        extras.insert("cast_shadow".into(), json!(false));
    }
    if !extras.is_empty() {
        obj.insert("extras".into(), Value::Object(extras));
    }

    Value::Object(obj)
}

/// Resolve a node's material UV scale, falling back to `[1, 1]` when no
/// material is bound. Pulled out as a helper so both the skinned and
/// unskinned export paths agree on the lookup.
fn uv_scale_for(scene: &SceneGraph, mat: Option<MaterialId>) -> [f32; 2] {
    mat.and_then(|m| scene.materials.get(m.0 as usize))
        .map(|m| m.uv_scale)
        .unwrap_or([1.0, 1.0])
}

#[derive(Hash, Eq, PartialEq, Clone)]
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
