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
//! We deduplicate by `(path, slot)`, mirroring the GLB exporter's
//! `(PathBuf, SlotKind)` keying. A path used both as a colour slot and a
//! linear slot ends up with two `Texture`+`Video` pairs because their
//! `MogenSlot` custom prop and `UseMaterial` value differ per slot kind
//! — sharing one Texture object would mis-stamp metadata for one of the
//! two consumers.
//!
//! Bytes are read once per unique `(path, slot)` pair through the
//! caller-supplied [`TextureSource`], matching the GLB pipeline so
//! desktop and wasm callers share the same plumbing. A failure to read
//! propagates back through the writer's `Result` — same contract as the
//! GLB pipeline.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};

use fbxcel::low::v7400::AttributeValue;

use super::doc::ObjectEmitter;
use super::ids::IdAllocator;
use super::material::{
    TextureIndices, SLOT_BASE_COLOR, SLOT_EMISSIVE, SLOT_METALLIC_ROUGHNESS, SLOT_NORMAL,
    SLOT_OCCLUSION,
};
use crate::texture::TextureSource;

pub(super) fn emit_textures_and_videos(
    indices: &TextureIndices,
    source: &dyn TextureSource,
    ids: &mut IdAllocator,
    emit: &mut ObjectEmitter,
) -> Result<()> {
    // (path, slot-name) → (texture_id, video_id). Two materials referencing
    // the same image at the *same* slot share one Texture+Video pair; the
    // same image at different slots gets two pairs because the slot-kind
    // metadata on the Texture object differs.
    let mut cache: HashMap<(PathBuf, &'static str), (i64, i64)> = HashMap::new();

    for (mat_idx, paths) in indices.texture_paths.iter().enumerate() {
        let mat_id = indices.material_ids[mat_idx];

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

            let key = (path.clone(), slot);
            let tex_id = if let Some(&(t, _)) = cache.get(&key) {
                t
            } else {
                let tex_id = ids.alloc();
                let vid_id = ids.alloc();
                cache.insert(key, (tex_id, vid_id));

                // Read once per (path, slot). Propagate errors the same
                // way the GLB pipeline does — silently embedding empty
                // bytes would produce structurally-valid FBX with
                // invisible textures and no diagnostic.
                let raw_bytes = source
                    .read(path)
                    .with_context(|| format!("reading texture {}", path.display()))?;

                let path_owned = path.clone();
                let stem_owned = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let stem_for_tex = stem_owned.clone();

                emit.push_object(
                    "Video",
                    Box::new(move |tree, parent| {
                        let v = tree.append_new(parent, "Video");
                        tree.append_attribute(v, vid_id);
                        tree.append_attribute(v, format!("{stem_owned}\u{0}\u{1}Video"));
                        tree.append_attribute(v, "Clip");

                        let ty = tree.append_new(v, "Type");
                        tree.append_attribute(ty, "Clip");

                        let fname_str = path_owned.to_string_lossy().into_owned();
                        let rel = tree.append_new(v, "RelativeFilename");
                        tree.append_attribute(rel, fname_str.clone());
                        let fname = tree.append_new(v, "FileName");
                        tree.append_attribute(fname, fname_str);

                        let content = tree.append_new(v, "Content");
                        tree.append_attribute(content, AttributeValue::Binary(raw_bytes));

                        let uvw = tree.append_new(v, "UseMipMap");
                        tree.append_attribute(uvw, 0i32);
                    }),
                );

                let slot_owned = slot.to_string();
                emit.push_object(
                    "Texture",
                    Box::new(move |tree, parent| {
                        let t = tree.append_new(parent, "Texture");
                        tree.append_attribute(t, tex_id);
                        tree.append_attribute(t, format!("{stem_for_tex}\u{0}\u{1}Texture"));
                        tree.append_attribute(t, "");

                        let ty = tree.append_new(t, "Type");
                        tree.append_attribute(ty, "TextureVideoClip");
                        let v = tree.append_new(t, "Version");
                        tree.append_attribute(v, 202i32);
                        let tn = tree.append_new(t, "TextureName");
                        tree.append_attribute(tn, format!("{stem_for_tex}\u{0}\u{1}Texture"));
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
                tex_id
            };

            emit.connect_op(tex_id, mat_id, slot);
        }
    }

    Ok(())
}
