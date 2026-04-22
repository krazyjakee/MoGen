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

/// A reference to an on-disk image used as a material texture. Paths are
/// resolved relative to the `.mg` file that declared them. During export the
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
}

impl Material {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            base_color: [0.8, 0.8, 0.8, 1.0],
            metallic: 0.0,
            roughness: 0.9,
            alpha_mode: AlphaMode::Opaque,
            alpha_cutoff: 0.5,
            emissive: [0.0, 0.0, 0.0],
            emissive_strength: 1.0,
            transmission: 0.0,
            double_sided: false,
            base_color_texture: None,
            metallic_roughness_texture: None,
            normal_texture: None,
            occlusion_texture: None,
            emissive_texture: None,
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
