//! Imposter atlas → glTF wiring for the `bundle_lods_and_imposter` option.
//!
//! Runs the headless yaw-grid bake from `mogen-render`, encodes the result
//! as PNG, embeds it into the GLB BIN chunk, and emits a billboard quad
//! mesh + material + node that references the texture. The new node is
//! tagged `"imposter"` in `extras.tags` so the companion `godot-mog`
//! runtime can find it and apply its octahedral / yaw-picking shader; for
//! plain glTF viewers the quad simply renders with the full atlas mapped
//! across its UVs.

use anyhow::{Context, Result};
use image::ImageEncoder;
use serde_json::{json, Value};

use mogen_core::{Mesh, SceneGraph};

pub use mogen_render::imposter::ImposterAtlas;

use crate::accessor::{push_indices, push_normals, push_positions, push_uvs};
use crate::{align_up, Accessor, BufferView};

/// Atlas dimensions baked by [`emit_imposter`]. 512² per cell × 8 yaws =
/// 4 MB raw RGBA before PNG compression; the smaller 256² default looked
/// noticeably pixelated on real props, especially with foliage / detailed
/// silhouettes, so we bias toward sharpness here. Keep these in sync with
/// [`crate::imposter::IMPOSTER_PREVIEW_CELL_SIZE`] in `mogen-studio` so
/// the in-Studio preview shows the same artifact the export embeds.
const CELL_SIZE: u32 = 512;
const VIEW_COUNT: u32 = 8;
const PITCH_RADIANS: f32 = 0.5;

/// Outcome of attaching the imposter to a glTF. `mesh_index` and
/// `node_index` are the freshly-appended entries the writer needs to fold
/// into `nodes[]` / scene roots / extras.
pub(crate) struct ImposterEmission {
    pub(crate) mesh_index: usize,
    pub(crate) material_index: usize,
}

/// Bake the imposter, embed the PNG, and emit the billboard mesh +
/// material. Returns indices into the freshly-mutated tables so the writer
/// can append the new root node and update `scene.roots`.
///
/// The PNG embed reuses the GLB texture-table conventions: one
/// `bufferView` for the bytes, one `image` referencing it, one shared
/// `sampler` (created here if the table didn't already have one), and one
/// `texture` pairing them. Re-uses the table's sampler slot 0 when present
/// so we don't double-emit the standard linear/repeat sampler.
/// Bake the scene-wide imposter spritesheet exactly as
/// `bundle_lods_and_imposter` would embed it — same cell size, view count,
/// and pitch — without writing a GLB. Studio's imposter preview shows the
/// returned atlas so the user sees the precise artifact the export bundles.
/// Requires a working display server (headless GL bake), same as the export
/// path.
pub fn bake_scene_imposter(scene: &SceneGraph) -> Result<ImposterAtlas> {
    mogen_render::imposter::bake_yaw_atlas(
        scene,
        &mogen_render::imposter::ImposterOptions {
            cell_size: CELL_SIZE,
            view_count: VIEW_COUNT,
            pitch: PITCH_RADIANS,
            base_dir: None,
        },
    )
    .context("baking imposter atlas")
}

pub(crate) fn emit_imposter(
    scene: &SceneGraph,
    prebaked: Option<ImposterAtlas>,
    bin: &mut Vec<u8>,
    buffer_views: &mut Vec<BufferView>,
    accessors: &mut Vec<Accessor>,
    meshes: &mut Vec<Value>,
    materials: &mut Vec<Value>,
    images: &mut Vec<Value>,
    textures: &mut Vec<Value>,
    samplers: &mut Vec<Value>,
    progress: &dyn Fn(&str),
) -> Result<ImposterEmission> {
    let atlas = if let Some(atlas) = prebaked {
        progress("embedding prebaked imposter atlas");
        atlas
    } else {
        progress("baking imposter atlas");
        mogen_render::imposter::bake_yaw_atlas(
            scene,
            &mogen_render::imposter::ImposterOptions {
                cell_size: CELL_SIZE,
                view_count: VIEW_COUNT,
                pitch: PITCH_RADIANS,
                base_dir: None,
            },
        )
        .context("baking imposter atlas")?
    };

    let mut png_bytes: Vec<u8> = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png_bytes)
        .write_image(
            &atlas.rgba,
            atlas.width,
            atlas.height,
            image::ExtendedColorType::Rgba8,
        )
        .context("encoding imposter atlas PNG")?;

    progress("embedding imposter atlas");
    let offset = align_up(bin, 4);
    let png_len = png_bytes.len();
    bin.extend_from_slice(&png_bytes);
    buffer_views.push(BufferView {
        buffer: 0,
        byte_offset: offset,
        byte_length: png_len,
        target: None,
    });
    let image_idx = images.len();
    images.push(json!({
        "bufferView": buffer_views.len() - 1,
        "mimeType": "image/png",
        "name": "imposter_atlas",
    }));

    // CLAMP_TO_EDGE so cell borders don't bleed into each other under
    // bilinear sampling — REPEAT (the shared PBR sampler) would smear yaw 0
    // into yaw N-1 at the seam. The standard PBR sampler at slot 0 (if
    // present) uses REPEAT, so the imposter gets its own sampler entry.
    let sampler_idx = samplers.len();
    samplers.push(json!({
        "magFilter": 9729,        // LINEAR
        "minFilter": 9729,        // LINEAR (no mipmaps — sharper at near-cell sizes)
        "wrapS": 33071,           // CLAMP_TO_EDGE
        "wrapT": 33071,
    }));

    let texture_idx = textures.len();
    textures.push(json!({
        "source": image_idx,
        "sampler": sampler_idx,
    }));

    // Unlit, double-sided, alpha-masked material — the spritesheet brings
    // its own lighting via the bake, and the bake's transparent background
    // needs alpha-test so the quad's empty regions don't draw.
    let material_index = materials.len();
    materials.push(json!({
        "name": "imposter",
        "pbrMetallicRoughness": {
            "baseColorTexture": { "index": texture_idx },
            "metallicFactor": 0.0,
            "roughnessFactor": 1.0,
        },
        "alphaMode": "MASK",
        "alphaCutoff": 0.1,
        "doubleSided": true,
        "extensions": { "KHR_materials_unlit": {} },
    }));

    // Build the billboard at the model's AABB extent (centred on its
    // midpoint, half-width = worst-yaw silhouette radius, half-height =
    // actual model height). The UV.y is mapped onto `[uv_y_top,
    // uv_y_bottom]` from the bake so the cell's transparent margins
    // get cropped out and the silhouette stretches across the full
    // quad — no quad floating above the model just because the
    // square cell happened to be padded.
    let half_w = atlas.half_width.max(0.5);
    let half_h = atlas.half_height.max(0.5);
    let quad = billboard_quad(atlas.center, half_w, half_h, atlas.uv_y_top, atlas.uv_y_bottom);

    let pos_acc = push_positions(bin, buffer_views, accessors, &quad);
    let nrm_acc = push_normals(bin, buffer_views, accessors, &quad);
    let uv_acc = push_uvs(bin, buffer_views, accessors, &quad, [1.0, 1.0]);
    let idx_acc = push_indices(bin, buffer_views, accessors, &quad);

    let mut attributes = serde_json::Map::new();
    attributes.insert("POSITION".into(), json!(pos_acc));
    attributes.insert("NORMAL".into(), json!(nrm_acc));
    attributes.insert("TEXCOORD_0".into(), json!(uv_acc));

    let primitive = json!({
        "attributes": Value::Object(attributes),
        "indices": idx_acc,
        "material": material_index,
    });

    let mesh_index = meshes.len();
    meshes.push(json!({
        "name": "imposter_quad",
        "primitives": [primitive],
    }));

    Ok(ImposterEmission {
        mesh_index,
        material_index,
    })
}

/// Build the camera-facing billboard quad as a [`Mesh`]. Centred on
/// `center`, sized to the model's AABB (`half_w` × `half_h` half-extents).
/// Faces +Z, with UV.x spanning the full atlas width (the godot-mog
/// shader narrows to a single cell at runtime; plain viewers show the
/// whole sheet across the quad) and UV.y mapped to `[uv_y_top,
/// uv_y_bottom]` so the silhouette inside one cell stretches to fill
/// the quad in world space — no transparent padding at top/bottom of
/// the quad.
fn billboard_quad(
    center: [f32; 3],
    half_w: f32,
    half_h: f32,
    uv_y_top: f32,
    uv_y_bottom: f32,
) -> Mesh {
    let [cx, cy, cz] = center;
    let positions = vec![
        [cx - half_w, cy - half_h, cz],
        [cx + half_w, cy - half_h, cz],
        [cx + half_w, cy + half_h, cz],
        [cx - half_w, cy + half_h, cz],
    ];
    let normals = vec![[0.0, 0.0, 1.0]; 4];
    // UV.y bounds straight from the bake: the silhouette's apex sits
    // at `uv_y_top` in atlas space, its base at `uv_y_bottom`. The
    // bottom-vertex UV.y is `uv_y_bottom` so the quad's bottom edge
    // shows the silhouette base; symmetric at the top.
    let uvs = vec![
        [0.0, uv_y_bottom],
        [1.0, uv_y_bottom],
        [1.0, uv_y_top],
        [0.0, uv_y_top],
    ];
    let indices = vec![0u32, 1, 2, 0, 2, 3];
    Mesh {
        positions,
        normals,
        indices,
        uvs,
        joints: Vec::new(),
        weights: Vec::new(),
        colors: Vec::new(),
    }
}

