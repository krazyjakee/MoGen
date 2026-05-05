use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MaterialId(pub u32);

/// glTF 2.0 alpha-handling modes.
///
/// `Opaque` ignores the alpha channel (default). `Mask` discards fragments
/// below `alpha_cutoff` — useful for foliage and stencil cutouts. `Blend`
/// composites with the framebuffer — the right choice for glass, smoke,
/// coloured gels, and anything you want to see through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AlphaMode {
    #[default]
    Opaque,
    Mask,
    Blend,
}

/// How a material's textures should be mapped onto the surface.
///
/// `Tile` (default) emits world-space UVs: 1 world unit = 1 texture tile (scaled
/// by `uv_scale`). Texel density is identical across every primitive that uses
/// the material, regardless of object size — the right choice for repeating
/// surfaces (stone, wood, fabric, ground).
///
/// `Fit` keeps the legacy per-face `[0, 1]²` parameterisation: every face of
/// the primitive shows the full image once. The right choice for signs,
/// paintings, decals, stained-glass images, anything where the texture *is*
/// the picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum UvMode {
    #[default]
    Tile,
    Fit,
}

/// Per-material override for the studio's preview shader. `Standard` is the
/// PBR path everyone else uses; `Water` swaps in an animated waves +
/// fresnel-blue + sky-reflection branch. The exporter ignores this field —
/// glTF can't carry custom shaders, so the .glb falls back to whatever PBR
/// scalars the material was authored with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MaterialShader {
    #[default]
    Standard,
    Water,
}

impl MaterialShader {
    /// Integer handed to the fragment shader via `u_material_shader`. Keep in
    /// sync with the branches in `viewer/shaders.rs`.
    pub fn shader_id(self) -> i32 {
        match self {
            MaterialShader::Standard => 0,
            MaterialShader::Water => 1,
        }
    }

    /// Whether this shader animates in the preview and so needs continuous
    /// repaints. Used by the viewer to keep ticking when the scene is
    /// otherwise static.
    pub fn animates(self) -> bool {
        matches!(self, MaterialShader::Water)
    }
}

/// A reference to an on-disk image used as a material texture. Paths are
/// resolved relative to the `.mog` file that declared them. During export the
/// exporter reads the bytes, embeds them into the GLB binary chunk, and writes
/// matching `images[]` / `textures[]` / `samplers[]` entries.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TextureRef {
    pub path: PathBuf,
}

impl TextureRef {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Material {
    pub name: String,
    pub base_color: [f32; 4],
    pub metallic: f32,
    pub roughness: f32,
    /// Slope multiplier baked into the derived normal map during texture
    /// generation. Larger = more pronounced bumps. Default `1.5`. Only
    /// affects the `mogen textures` pipeline — authored `normal_texture`s
    /// are used as-is.
    pub normal_strength: f32,
    /// 0..1 multiplier on how dark the derived AO map can get during
    /// texture generation. `0` = flat white (no darkening), `1` = cavities
    /// reach black. Default `0.7`. Only affects the `mogen textures`
    /// pipeline — authored `occlusion_texture`s are used as-is.
    pub occlusion_strength: f32,
    pub alpha_mode: AlphaMode,
    pub alpha_cutoff: f32,
    pub emissive: [f32; 3],
    /// HDR multiplier on `emissive` (KHR_materials_emissive_strength). Values
    /// above 1.0 drive bloom in renderers that honour it — that's what makes
    /// neon / fluorescent paints pop.
    pub emissive_strength: f32,
    /// Fraction of light transmitted through the surface
    /// (KHR_materials_transmission). 0 = opaque PBR, 1 = fully transmissive
    /// glass. Orthogonal to `alpha_mode`.
    pub transmission: f32,
    /// Disable back-face culling in the renderer (glTF `doubleSided`). Set on
    /// leaves, fins, flags, cloth — any thin surface whose underside can be
    /// seen. The correct fix for that case: mirroring geometry along an axis
    /// that the primitive is bent into produces diverging sheets, not a
    /// double-sided surface.
    pub double_sided: bool,

    /// How the material's textures wrap onto the surface. `Tile` (default)
    /// uses world-space UVs so texel density is constant across primitives;
    /// `Fit` keeps the legacy per-face `[0, 1]²` mapping for sign-style images.
    pub uv_mode: UvMode,
    /// Studio-only shader override. `Standard` (default) goes through the
    /// regular PBR path; `Water` selects an animated waves branch in the
    /// viewer's fragment shader. Skipped during serde when default so saved
    /// scenes stay roundtrip-clean for the common case.
    #[serde(default, skip_serializing_if = "is_default_shader")]
    pub shader: MaterialShader,
    /// Per-axis multiplier applied to UVs at export. In `Tile` mode this sets
    /// "tiles per world unit" (e.g. `[2, 2]` doubles the tiling density on a
    /// brick wall). In `Fit` mode it scales the `[0, 1]` parameter — `> 1`
    /// repeats the image within a face, `< 1` zooms into a sub-region.
    pub uv_scale: [f32; 2],

    /// Albedo texture (multiplied with `base_color`). Expects a sRGB PNG.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_color_texture: Option<TextureRef>,
    /// Packed metallic-roughness map. glTF convention: G = roughness,
    /// B = metallic. Linear, not sRGB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metallic_roughness_texture: Option<TextureRef>,
    /// Tangent-space normal map. Linear, not sRGB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normal_texture: Option<TextureRef>,
    /// Ambient occlusion map (R channel). Linear.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occlusion_texture: Option<TextureRef>,
    /// Emissive colour texture (multiplied with `emissive`). sRGB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emissive_texture: Option<TextureRef>,

    /// Canonical path of the imported `.mog` file this material was hoisted
    /// from. `None` when the material was authored in the file currently
    /// being lowered. Used by tooling (e.g. MoGen Studio's inspector) to
    /// scope what's shown to the user — runtime export ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<PathBuf>,
}

impl Material {
    /// Mutable handles to every texture slot, in a fixed order. Lets callers
    /// iterate slots generically (e.g. to resolve relative paths) without
    /// having to enumerate each field by name.
    pub fn texture_slots_mut(&mut self) -> [&mut Option<TextureRef>; 5] {
        [
            &mut self.base_color_texture,
            &mut self.metallic_roughness_texture,
            &mut self.normal_texture,
            &mut self.occlusion_texture,
            &mut self.emissive_texture,
        ]
    }

    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            base_color: [0.8, 0.8, 0.8, 1.0],
            metallic: 0.0,
            roughness: 0.9,
            normal_strength: 1.5,
            occlusion_strength: 0.7,
            alpha_mode: AlphaMode::Opaque,
            alpha_cutoff: 0.5,
            emissive: [0.0, 0.0, 0.0],
            emissive_strength: 1.0,
            transmission: 0.0,
            double_sided: false,
            uv_mode: UvMode::default(),
            shader: MaterialShader::default(),
            uv_scale: [1.0, 1.0],
            base_color_texture: None,
            metallic_roughness_texture: None,
            normal_texture: None,
            occlusion_texture: None,
            emissive_texture: None,
            origin: None,
        }
    }

    /// True if any texture slot is populated.
    pub fn has_textures(&self) -> bool {
        self.base_color_texture.is_some()
            || self.metallic_roughness_texture.is_some()
            || self.normal_texture.is_some()
            || self.occlusion_texture.is_some()
            || self.emissive_texture.is_some()
    }
}

fn is_default_shader(s: &MaterialShader) -> bool {
    matches!(s, MaterialShader::Standard)
}
