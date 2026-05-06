//! Binary FBX 7.4 export — sibling to the GLB pipeline.
//!
//! The public API mirrors `write_glb` / `build_glb_with_options_and_source`
//! so callers can swap the format at the file layer without rewiring options
//! or the texture-source plumbing. All emission flows through the `fbxcel`
//! crate's writer + tree feature; this module only assembles the FBX node
//! tree from a `mogen_core::SceneGraph` and hands it to `Writer::write_tree`.
//!
//! Out of scope for now: FBX import, ASCII FBX, and any feature that would
//! be lossy beyond what the type-mapping plan documents.

use std::fs::File;
use std::io::{BufWriter, Cursor};
use std::path::Path;

use anyhow::{Context, Result};

use mogen_core::SceneGraph;

use crate::texture::{FsTextureSource, TextureSource};
use crate::ExportOptions;
#[cfg(feature = "merge")]
use crate::merge;

mod anim;
mod doc;
mod ids;
mod light;
mod material;
mod mesh;
mod nodes;
mod skin;
mod texture;

/// Convenience wrapper that writes the export bytes to disk with default
/// options. Mirrors [`crate::write_glb`].
pub fn write_fbx(scene: &SceneGraph, out: &Path) -> Result<()> {
    write_fbx_with_options(scene, out, &ExportOptions::default(), |_| {})
}

/// Build an FBX into the given file. Streams the resulting bytes through
/// `fbxcel`'s writer directly into a `BufWriter` so we don't double-buffer
/// the (potentially large) document.
pub fn write_fbx_with_options<F: Fn(&str)>(
    scene: &SceneGraph,
    out: &Path,
    opts: &ExportOptions,
    progress: F,
) -> Result<()> {
    let bytes = build_fbx_with_options(scene, opts, progress)?;
    let mut f = BufWriter::new(
        File::create(out).with_context(|| format!("creating {}", out.display()))?,
    );
    use std::io::Write;
    f.write_all(&bytes)
        .with_context(|| format!("writing {}", out.display()))?;
    f.flush()
        .with_context(|| format!("flushing {}", out.display()))?;
    Ok(())
}

/// Build an FBX into a `Vec<u8>` using the filesystem to load any textures
/// referenced by the scene's materials. Mirrors [`crate::build_glb_with_options`].
pub fn build_fbx_with_options<F: Fn(&str)>(
    scene: &SceneGraph,
    opts: &ExportOptions,
    progress: F,
) -> Result<Vec<u8>> {
    build_fbx_with_options_and_source(scene, opts, &FsTextureSource, progress)
}

/// Build an FBX into a `Vec<u8>` with a caller-supplied [`TextureSource`].
/// Mirrors [`crate::build_glb_with_options_and_source`] — same merge stages,
/// same ordering, same texture-source contract.
pub fn build_fbx_with_options_and_source<F: Fn(&str)>(
    scene: &SceneGraph,
    opts: &ExportOptions,
    texture_source: &dyn TextureSource,
    progress: F,
) -> Result<Vec<u8>> {
    // Mirror the GLB merge pipeline: scoped `solid` pass first, then the
    // global sibling-merge if the caller asked for it. Both stages clone
    // and the latest owned graph wins.
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
    let scene: &SceneGraph = merged_owned.as_ref().unwrap_or(scene_after_solid);
    #[cfg(not(feature = "merge"))]
    let scene: &SceneGraph = scene_after_solid;

    // Build the FBX node tree in memory, then ask `fbxcel` to serialize it.
    progress("building fbx tree");
    let tree = doc::build_tree(scene, opts, texture_source, &progress)
        .context("building fbx tree")?;

    progress("writing fbx");
    use fbxcel::low::FbxVersion;
    use fbxcel::writer::v7400::binary::{FbxFooter, Writer};

    let mut sink = Cursor::new(Vec::<u8>::new());
    let mut writer = Writer::new(&mut sink, FbxVersion::V7_4)
        .map_err(|e| anyhow::anyhow!("opening fbx writer: {e}"))?;
    writer
        .write_tree(&tree)
        .map_err(|e| anyhow::anyhow!("serialising fbx tree: {e}"))?;
    let footer = FbxFooter::default();
    writer
        .finalize_and_flush(&footer)
        .map_err(|e| anyhow::anyhow!("finalising fbx writer: {e}"))?;

    Ok(sink.into_inner())
}
