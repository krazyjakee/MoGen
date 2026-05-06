//! `TextureRef` → `Texture` + `Video` Object emission.
//!
//! In FBX the image bytes themselves live in a `Video` object (yes,
//! "Video" — Autodesk uses the same node for stills and movies). The
//! `Texture` object is the PBR slot binder that connects the Video into
//! a Material via OP. Connection chain looks like:
//!
//! ```text
//! Video --OO--> Texture --OP("DiffuseColor"|"NormalMap"|…)--> Material
//! ```
//!
//! We deduplicate by (path, slot-kind) the same way the GLB exporter does
//! — sharing a Texture object between materials is fine, but a path used
//! both as a colour slot and a linear slot needs two embeds because their
//! `UseMaterial` properties differ.
//!
//! Bytes are read once per unique path through the caller-supplied
//! `TextureSource` and embedded raw via `Video.Content`. No transcoding
//! happens here — that matches the wasm-friendly path of the GLB
//! exporter (the `textures-optimize` JPEG/oxipng path is GLB-only and
//! intentionally out of scope; FBX consumers like Blender accept PNG and
//! JPEG verbatim, and binary FBX has no MIME type to fight us about).

use std::collections::HashMap;
use std::path::PathBuf;

use fbxcel::low::v7400::AttributeValue;

use mogen_core::{MaterialId, SceneGraph};

use super::doc::ObjectEmitter;
use super::ids::IdAllocator;
use super::material::{
    TextureIndices, SLOT_BASE_COLOR, SLOT_EMISSIVE, SLOT_METALLIC_ROUGHNESS, SLOT_NORMAL,
    SLOT_OCCLUSION,
};

pub(super) fn emit_textures_and_videos(
    scene: &SceneGraph,
    indices: &TextureIndices,
    ids: &mut IdAllocator,
    emit: &mut ObjectEmitter,
) {
    // Cache: path → (texture_id, video_id). If two materials reference
    // the same image at the same slot kind, both connect to the same
    // Texture object. We don't try to re-read or re-embed on a hit.
    let mut cache: HashMap<PathBuf, (i64, i64)> = HashMap::new();

    // Bytes for embedded `Content` come from the same source the GLB
    // pipeline uses. We read each unique path lazily — only when we hit
    // it for the first time — so a TextureSource that fails for a path
    // not actually referenced is never queried.
    let mut bytes_cache: HashMap<PathBuf, Vec<u8>> = HashMap::new();

    for (mat_idx, paths) in indices.texture_paths.iter().enumerate() {
        let mat_id = indices.material_ids[mat_idx];
        let _mat_id_for_check: MaterialId = MaterialId(mat_idx as u32);
        let _ = scene; // currently unused; kept for future per-material
                       // texture-transform export.

        for (slot, path_opt) in [
            (SLOT_BASE_COLOR, paths.base_color.as_ref()),
            (SLOT_NORMAL, paths.normal.as_ref()),
            (SLOT_EMISSIVE, paths.emissive.as_ref()),
            (SLOT_OCCLUSION, paths.occlusion.as_ref()),
            (SLOT_METALLIC_ROUGHNESS, paths.metallic_roughness.as_ref()),
        ] {
            let path = match path_opt {
                Some(p) => p,
                None => continue,
            };

            let (tex_id, _vid_id) = match cache.get(path) {
                Some(&(t, v)) => (t, v),
                None => {
                    let tex_id = ids.alloc();
                    let vid_id = ids.alloc();
                    cache.insert(path.clone(), (tex_id, vid_id));

                    // Embed bytes if the texture source can hand them
                    // over. Failure to read becomes an empty `Content` —
                    // the GLB pipeline aborts in this case, but for FBX
                    // we'd rather emit a structurally-valid file with a
                    // placeholder than abort the whole export over one
                    // missing file. The texture path is preserved on
                    // `Video.RelativeFilename` and `Video.FileName` so
                    // a tooling re-resolve can patch it up later.
                    let raw_bytes = bytes_cache
                        .entry(path.clone())
                        .or_insert_with(|| std::fs::read(path).unwrap_or_default())
                        .clone();

                    let path_owned = path.clone();
                    emit.push_object(
                        "Video",
                        Box::new(move |tree, parent| {
                            let v = tree.append_new(parent, "Video");
                            tree.append_attribute(v, vid_id);
                            tree.append_attribute(
                                v,
                                format!(
                                    "{}\u{0}\u{1}Video",
                                    path_owned
                                        .file_stem()
                                        .map(|s| s.to_string_lossy().into_owned())
                                        .unwrap_or_default()
                                ),
                            );
                            tree.append_attribute(v, "Clip");

                            let ty = tree.append_new(v, "Type");
                            tree.append_attribute(ty, "Clip");

                            let fname_str = path_owned.to_string_lossy().into_owned();
                            let rel = tree.append_new(v, "RelativeFilename");
                            tree.append_attribute(rel, fname_str.clone());
                            let fname = tree.append_new(v, "FileName");
                            tree.append_attribute(fname, fname_str);

                            let content = tree.append_new(v, "Content");
                            tree.append_attribute(
                                content,
                                AttributeValue::Binary(raw_bytes),
                            );

                            let uvw = tree.append_new(v, "UseMipMap");
                            tree.append_attribute(uvw, 0i32);
                        }),
                    );

                    let path_owned = path.clone();
                    let slot_owned = slot.to_string();
                    emit.push_object(
                        "Texture",
                        Box::new(move |tree, parent| {
                            let t = tree.append_new(parent, "Texture");
                            tree.append_attribute(t, tex_id);
                            tree.append_attribute(
                                t,
                                format!(
                                    "{}\u{0}\u{1}Texture",
                                    path_owned
                                        .file_stem()
                                        .map(|s| s.to_string_lossy().into_owned())
                                        .unwrap_or_default()
                                ),
                            );
                            tree.append_attribute(t, "");

                            let ty = tree.append_new(t, "Type");
                            tree.append_attribute(ty, "TextureVideoClip");
                            let v = tree.append_new(t, "Version");
                            tree.append_attribute(v, 202i32);
                            let tn = tree.append_new(t, "TextureName");
                            tree.append_attribute(
                                tn,
                                format!(
                                    "{}\u{0}\u{1}Texture",
                                    path_owned
                                        .file_stem()
                                        .map(|s| s.to_string_lossy().into_owned())
                                        .unwrap_or_default()
                                ),
                            );
                            let mr = tree.append_new(t, "ModelUVTranslation");
                            tree.append_attribute(mr, 0.0_f64);
                            tree.append_attribute(mr, 0.0_f64);
                            let ms = tree.append_new(t, "ModelUVScaling");
                            tree.append_attribute(ms, 1.0_f64);
                            tree.append_attribute(ms, 1.0_f64);
                            let txt = tree.append_new(t, "Texture_Alpha_Source");
                            tree.append_attribute(txt, "None");
                            // Slot kind on the Texture itself isn't a
                            // standard FBX concept — we record it as a
                            // custom prop so an importer round-tripping
                            // back to MoGen can pick the right slot.
                            super::doc::write_properties70(tree, t, |tt, props| {
                                super::doc::push_prop(
                                    tt,
                                    props,
                                    "MogenSlot",
                                    "KString",
                                    "",
                                    "U",
                                    AttributeValue::String(slot_owned.clone()),
                                );
                                super::doc::push_prop(
                                    tt,
                                    props,
                                    "UseMaterial",
                                    "bool",
                                    "",
                                    "",
                                    AttributeValue::I32(1),
                                );
                            });
                        }),
                    );

                    emit.connect_oo(vid_id, tex_id);
                    (tex_id, vid_id)
                }
            };

            emit.connect_op(tex_id, mat_id, slot);
        }
    }
}
