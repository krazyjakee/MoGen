//! Toggles for `write_glb_with_options`. Kept in its own module so callers
//! (CLI, GUI) can pass an options struct around without pulling in the whole
//! exporter.

#[derive(Debug, Clone)]
pub struct ExportOptions {
    /// Emit `animations[]` and retain node-transform clips. Off = produce a
    /// static GLB regardless of what the scene declared.
    pub include_animations: bool,
    /// Pack texture image data into the BIN chunk and wire
    /// `baseColorTexture` / `normalTexture` / etc. Off = materials export with
    /// only their numeric PBR factors; the GLB stays a compact "flat colour"
    /// asset usable without texture dependencies.
    pub include_textures: bool,
    /// Merge groups of same-material, non-skinned, leaf sibling meshes under
    /// each parent into a single CSG-unioned mesh. This removes interior
    /// geometry where shapes overlap (e.g. a leg buried in a seat). UVs are
    /// preserved through the CSG when all operands in a group have them, so
    /// textured materials still render correctly on merged output.
    pub merge_sibling_meshes: bool,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            include_animations: true,
            include_textures: true,
            merge_sibling_meshes: false,
        }
    }
}
