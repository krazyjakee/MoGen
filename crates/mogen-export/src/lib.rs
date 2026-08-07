mod accessor;
mod animation;
#[cfg(feature = "imposter")]
mod imposter;
mod lights;
#[cfg(feature = "lod")]
mod lod;
mod material;
#[cfg(feature = "merge")]
pub mod merge;
pub mod options;
mod skin;
#[cfg(feature = "textures-svg")]
pub mod svg;
mod texture;
mod writer;

#[cfg(feature = "fbx")]
pub mod fbx;

pub use options::ExportOptions;

#[cfg(feature = "lod")]
pub use lod::{scene_with_lod, LOD_STAGE_COUNT};

#[cfg(feature = "imposter")]
pub use imposter::{bake_scene_imposter, ImposterAtlas};
pub use texture::{FsTextureSource, MapTextureSource, TextureSource};
#[cfg(feature = "textures-svg")]
pub use svg::{
    is_svg, rasterize_svg_textures, render_svg, resolve_svg_size, OverlayTextureSource,
    SvgRaster, MAX_SVG_SIZE,
};
pub use writer::{
    build_glb_with_options, build_glb_with_options_and_source, write_glb, write_glb_with_options,
};

#[cfg(feature = "imposter")]
pub use writer::write_glb_with_prebaked_imposter;

#[cfg(feature = "fbx")]
pub use fbx::{
    build_fbx_with_options, build_fbx_with_options_and_source, write_fbx, write_fbx_with_options,
};

use serde::Serialize;

pub(crate) const GLB_MAGIC: u32 = 0x46546C67;
pub(crate) const CHUNK_JSON: u32 = 0x4E4F534A;
pub(crate) const CHUNK_BIN: u32 = 0x004E4942;

#[derive(Serialize)]
pub(crate) struct Accessor {
    #[serde(rename = "bufferView")]
    pub(crate) buffer_view: usize,
    #[serde(rename = "componentType")]
    pub(crate) component_type: u32,
    pub(crate) count: usize,
    #[serde(rename = "type")]
    pub(crate) ty: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) min: Option<Vec<f32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max: Option<Vec<f32>>,
}

#[derive(Serialize)]
pub(crate) struct BufferView {
    pub(crate) buffer: usize,
    #[serde(rename = "byteOffset")]
    pub(crate) byte_offset: usize,
    #[serde(rename = "byteLength")]
    pub(crate) byte_length: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) target: Option<u32>,
}

pub(crate) fn bounds(verts: &[[f32; 3]]) -> ([f32; 3], [f32; 3]) {
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

pub(crate) fn align_up(bin: &mut Vec<u8>, align: usize) -> usize {
    while bin.len() % align != 0 {
        bin.push(0);
    }
    bin.len()
}

pub(crate) fn pad_to_4(mut bytes: Vec<u8>, filler: u8) -> Vec<u8> {
    while bytes.len() % 4 != 0 {
        bytes.push(filler);
    }
    bytes
}
