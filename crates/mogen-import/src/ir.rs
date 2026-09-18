//! The format-neutral mesh scene every reader produces and every pass consumes.
//!
//! Deliberately small: a node tree with local transforms, triangle meshes split
//! by material, and a material table. Anything a reader finds that does not fit
//! here goes into the [`Report`](crate::Report) rather than being stretched to
//! fit — the header of the emitted file is where lossy decisions are recorded.
//!
//! Everything is index-addressed `Vec`s. No maps: iteration order is the
//! source file's order, which is what makes the output reproducible.

use glam::{Quat, Vec3};

#[derive(Debug, Default, Clone)]
pub struct MeshScene {
    pub nodes: Vec<Node>,
    /// Indices into `nodes`, in source order.
    pub roots: Vec<usize>,
    pub meshes: Vec<MeshData>,
    pub materials: Vec<Material>,
}

#[derive(Debug, Clone)]
pub struct Node {
    /// The name as the source file had it, if it had one. Sanitised and made
    /// unique by [`normalize`](crate::normalize), not by readers.
    pub source_name: Option<String>,
    /// Filled in by `normalize`: a unique `.mog`-safe identifier.
    pub name: String,
    pub translation: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
    pub mesh: Option<usize>,
    pub children: Vec<usize>,
    /// Semantic label, when the source recorded one (MoGen GLBs do, in node
    /// `extras`).
    pub role: Option<String>,
    /// Free-form labels, in source order. `floating` matters: it exempts a
    /// subtree from `mogen build`'s connectivity check.
    pub tags: Vec<String>,
}

#[derive(Debug, Default, Clone)]
pub struct MeshData {
    pub source_name: Option<String>,
    pub name: String,
    /// One per material: `poly` takes a single `mat=`.
    pub primitives: Vec<Primitive>,
}

#[derive(Debug, Default, Clone)]
pub struct Primitive {
    pub positions: Vec<[f32; 3]>,
    /// Empty, or one per position.
    pub normals: Vec<[f32; 3]>,
    /// Empty, or one per position.
    pub uvs: Vec<[f32; 2]>,
    /// Three per triangle. Readers expand non-indexed geometry to `0, 1, 2, …`.
    pub indices: Vec<u32>,
    /// Index into [`MeshScene::materials`]; `None` is the format's default.
    pub material: Option<usize>,
}

impl Primitive {
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }
}

#[derive(Debug, Clone)]
pub struct Material {
    pub source_name: Option<String>,
    pub name: String,
    /// Linear RGBA.
    pub base_color: [f32; 4],
    pub metallic: f32,
    pub roughness: f32,
    pub emissive: [f32; 3],
    pub emissive_strength: f32,
    pub transmission: f32,
    pub alpha_mode: AlphaMode,
    pub alpha_cutoff: f32,
    pub double_sided: bool,
    pub textures: Textures,
}

impl Default for Material {
    /// The glTF default material, which is also what a primitive with no
    /// material is rendered with.
    fn default() -> Self {
        Self {
            source_name: None,
            name: String::new(),
            base_color: [1.0; 4],
            metallic: 1.0,
            roughness: 1.0,
            emissive: [0.0; 3],
            emissive_strength: 1.0,
            transmission: 0.0,
            alpha_mode: AlphaMode::Opaque,
            alpha_cutoff: 0.5,
            double_sided: false,
            textures: Textures::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AlphaMode {
    #[default]
    Opaque,
    Mask,
    Blend,
}

/// Indices into [`Import::assets`](crate::Import::assets).
#[derive(Debug, Default, Clone)]
pub struct Textures {
    pub base_color: Option<usize>,
    pub metallic_roughness: Option<usize>,
    pub normal: Option<usize>,
    pub occlusion: Option<usize>,
    pub emissive: Option<usize>,
}
