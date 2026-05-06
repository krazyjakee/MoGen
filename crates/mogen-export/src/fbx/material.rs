//! Material emission — produces one FBX `Material` Object per
//! `mogen_core::Material`, connected via OO to every `Model` that uses it
//! (FBX binds materials to Models, not Geometries).
//!
//! Lossy mappings flagged in the plan:
//!
//! - PBR `metallic` / `roughness` get a Phong approximation in
//!   `ShininessExponent`. The raw values are also emitted as `Roughness`
//!   and `Metallic` custom Properties70 entries so importers that look
//!   for them (Blender's principled BSDF importer does) can recover the
//!   PBR values.
//! - DSL-only material extras (`alpha_mode`, `transmission`,
//!   `normal_strength`, `occlusion_strength`, `double_sided`, `uv_mode`,
//!   `uv_scale`) are emitted as Properties70 custom props.
//!
//! Texture connections (the second half of "this material uses image X")
//! are handed off to `texture.rs` via the returned `TextureIndices` map —
//! we hold onto each texture-bearing material's id so the texture module
//! knows where to OP-connect every `Texture` to.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;

use fbxcel::low::v7400::AttributeValue;

use mogen_core::{AlphaMode, Material, SceneGraph, UvMode};

use super::doc::{push_prop, push_prop_vec3, write_properties70, ObjectEmitter};
use super::ids::IdAllocator;
use crate::texture::TextureSource;
use crate::ExportOptions;

/// FBX `Texture` slot names used for OP-connection from a Texture object
/// to a Material. Drives the property-name on the texture-side connection
/// so importers know what slot the image fills on the destination
/// material.
pub(super) const SLOT_BASE_COLOR: &str = "DiffuseColor";
pub(super) const SLOT_NORMAL: &str = "NormalMap";
pub(super) const SLOT_EMISSIVE: &str = "EmissiveColor";
pub(super) const SLOT_OCCLUSION: &str = "AmbientColor";
/// FBX Phong has no metallic-roughness slot. We expose it as a custom
/// property name; importers that recognise "MetallicRoughnessTexture"
/// (Blender's principled BSDF FBX importer does) pick it up there.
pub(super) const SLOT_METALLIC_ROUGHNESS: &str = "MetallicRoughnessTexture";

/// Materials emit owns the bookkeeping the texture module needs to wire
/// each on-disk image onto the right Material slot. Index by
/// `MaterialId.0`.
pub(super) struct TextureIndices {
    pub material_ids: Vec<i64>,
    pub texture_paths: Vec<MaterialTexturePaths>,
}

/// Per-material captured texture paths. Empty `Option`s mean the slot was
/// not authored. We keep the `PathBuf`s here (not `TextureRef`s) so the
/// texture module can use them as cache keys directly.
#[derive(Clone, Default)]
pub(super) struct MaterialTexturePaths {
    pub base_color: Option<PathBuf>,
    pub normal: Option<PathBuf>,
    pub emissive: Option<PathBuf>,
    pub occlusion: Option<PathBuf>,
    pub metallic_roughness: Option<PathBuf>,
}

pub(super) fn emit_materials(
    scene: &SceneGraph,
    _model_ids: &[i64],
    ids: &mut IdAllocator,
    emit: &mut ObjectEmitter,
    opts: &ExportOptions,
    _texture_source: &dyn TextureSource,
) -> Result<TextureIndices> {
    let mut material_ids: Vec<i64> = Vec::with_capacity(scene.materials.len());
    let mut texture_paths: Vec<MaterialTexturePaths> = Vec::with_capacity(scene.materials.len());

    // Reverse the material→models map produced by `mesh::emit_geometries`.
    // We need it from the freshly-built emitter, so re-scan instead — the
    // mesh table is private to that module's return value, threaded
    // through doc::build_tree. To keep this module independent of mesh's
    // table we re-derive the binding here.
    let mut material_to_models: HashMap<u32, Vec<i64>> = HashMap::new();
    {
        // Re-allocate model ids? No — the doc-level orchestrator owns
        // them. We instead let the doc orchestrator pass them in
        // implicitly by walking the scene with `_model_ids`.
        for (i, n) in scene.nodes.iter().enumerate() {
            if let (Some(mat), Some(_mesh)) = (n.material, n.mesh.as_ref()) {
                material_to_models
                    .entry(mat.0)
                    .or_default()
                    .push(_model_ids[i]);
            }
        }
    }

    for (mat_idx, mat) in scene.materials.iter().enumerate() {
        let mat_id = ids.alloc();
        material_ids.push(mat_id);

        let paths = MaterialTexturePaths {
            base_color: opts
                .include_textures
                .then(|| mat.base_color_texture.as_ref().map(|t| t.path.clone()))
                .flatten(),
            normal: opts
                .include_textures
                .then(|| mat.normal_texture.as_ref().map(|t| t.path.clone()))
                .flatten(),
            emissive: opts
                .include_textures
                .then(|| mat.emissive_texture.as_ref().map(|t| t.path.clone()))
                .flatten(),
            occlusion: opts
                .include_textures
                .then(|| mat.occlusion_texture.as_ref().map(|t| t.path.clone()))
                .flatten(),
            metallic_roughness: opts
                .include_textures
                .then(|| mat.metallic_roughness_texture.as_ref().map(|t| t.path.clone()))
                .flatten(),
        };
        texture_paths.push(paths);

        let snapshot = MaterialSnapshot::from(mat);
        emit.push_object(
            "Material",
            Box::new(move |tree, parent| {
                let m = tree.append_new(parent, "Material");
                tree.append_attribute(m, mat_id);
                tree.append_attribute(m, format!("{}\u{0}\u{1}Material", snapshot.name));
                tree.append_attribute(m, "");

                let v = tree.append_new(m, "Version");
                tree.append_attribute(v, 102i32);

                let shading_model = tree.append_new(m, "ShadingModel");
                tree.append_attribute(shading_model, "Phong");

                let multi = tree.append_new(m, "MultiLayer");
                tree.append_attribute(multi, 0i32);

                snapshot.emit_properties(tree, m);
            }),
        );

        // Connect this Material to every Model that referenced it via
        // mesh.material. FBX consumers expect material connections at
        // Model level — Geometry-side material binds aren't part of the
        // 7.4 surface area.
        if let Some(models) = material_to_models.get(&(mat_idx as u32)) {
            for model_id in models {
                emit.connect_oo(mat_id, *model_id);
            }
        }
    }

    Ok(TextureIndices {
        material_ids,
        texture_paths,
    })
}

/// Owned snapshot of a material's scalar fields so the emit closure does
/// not need to capture a borrow of the scene.
struct MaterialSnapshot {
    name: String,
    base_color: [f32; 4],
    metallic: f32,
    roughness: f32,
    emissive: [f32; 3],
    emissive_strength: f32,
    transmission: f32,
    normal_strength: f32,
    occlusion_strength: f32,
    alpha_mode: AlphaMode,
    alpha_cutoff: f32,
    double_sided: bool,
    uv_mode: UvMode,
    uv_scale: [f32; 2],
}

impl From<&Material> for MaterialSnapshot {
    fn from(m: &Material) -> Self {
        Self {
            name: m.name.clone(),
            base_color: m.base_color,
            metallic: m.metallic,
            roughness: m.roughness,
            emissive: m.emissive,
            emissive_strength: m.emissive_strength,
            transmission: m.transmission,
            normal_strength: m.normal_strength,
            occlusion_strength: m.occlusion_strength,
            alpha_mode: m.alpha_mode,
            alpha_cutoff: m.alpha_cutoff,
            double_sided: m.double_sided,
            uv_mode: m.uv_mode,
            uv_scale: m.uv_scale,
        }
    }
}

impl MaterialSnapshot {
    fn emit_properties(&self, tree: &mut fbxcel::tree::v7400::Tree, parent: fbxcel::tree::v7400::NodeId) {
        write_properties70(tree, parent, |t, props| {
            // Phong-approximated PBR. Diffuse colour gets the RGB; alpha
            // rides on `DiffuseFactor`. Specular is left at the FBX
            // default `(0.2, 0.2, 0.2)` because Phong specular is
            // perceptually orthogonal to PBR roughness.
            push_prop_vec3(
                t,
                props,
                "DiffuseColor",
                "Color",
                "",
                "A",
                [self.base_color[0] as f64, self.base_color[1] as f64, self.base_color[2] as f64],
            );
            push_prop(
                t,
                props,
                "DiffuseFactor",
                "Number",
                "",
                "A",
                AttributeValue::F64(self.base_color[3] as f64),
            );
            push_prop_vec3(
                t,
                props,
                "EmissiveColor",
                "Color",
                "",
                "A",
                [self.emissive[0] as f64, self.emissive[1] as f64, self.emissive[2] as f64],
            );
            push_prop(
                t,
                props,
                "EmissiveFactor",
                "Number",
                "",
                "A",
                AttributeValue::F64(self.emissive_strength as f64),
            );
            // Phong shininess approximation: `(1 - roughness) * 128`.
            // Picked to match the inverse remap most importers do when
            // they harvest a roughness value from a Phong material.
            push_prop(
                t,
                props,
                "ShininessExponent",
                "Number",
                "",
                "A",
                AttributeValue::F64(((1.0 - self.roughness as f64) * 128.0).max(0.0)),
            );

            // Raw PBR factors as custom props so an importer with a PBR
            // path can recover the originals losslessly.
            push_prop(
                t,
                props,
                "Roughness",
                "Number",
                "",
                "A",
                AttributeValue::F64(self.roughness as f64),
            );
            push_prop(
                t,
                props,
                "Metallic",
                "Number",
                "",
                "A",
                AttributeValue::F64(self.metallic as f64),
            );
            push_prop(
                t,
                props,
                "Transmission",
                "Number",
                "",
                "A",
                AttributeValue::F64(self.transmission as f64),
            );
            push_prop(
                t,
                props,
                "NormalStrength",
                "Number",
                "",
                "A",
                AttributeValue::F64(self.normal_strength as f64),
            );
            push_prop(
                t,
                props,
                "OcclusionStrength",
                "Number",
                "",
                "A",
                AttributeValue::F64(self.occlusion_strength as f64),
            );
            push_prop(
                t,
                props,
                "AlphaMode",
                "KString",
                "",
                "U",
                AttributeValue::String(
                    match self.alpha_mode {
                        AlphaMode::Opaque => "OPAQUE",
                        AlphaMode::Mask => "MASK",
                        AlphaMode::Blend => "BLEND",
                    }
                    .to_string(),
                ),
            );
            push_prop(
                t,
                props,
                "AlphaCutoff",
                "Number",
                "",
                "A",
                AttributeValue::F64(self.alpha_cutoff as f64),
            );
            if self.double_sided {
                push_prop(t, props, "DoubleSided", "bool", "", "", AttributeValue::I32(1));
            }
            push_prop(
                t,
                props,
                "UvMode",
                "KString",
                "",
                "U",
                AttributeValue::String(
                    match self.uv_mode {
                        UvMode::Tile => "TILE",
                        UvMode::Fit => "FIT",
                    }
                    .to_string(),
                ),
            );
            push_prop_vec3(
                t,
                props,
                "UvScale",
                "Vector3D",
                "Vector",
                "U",
                [self.uv_scale[0] as f64, self.uv_scale[1] as f64, 0.0],
            );
        });
    }
}
