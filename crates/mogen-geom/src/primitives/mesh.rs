//! Imported-mesh primitive: load a glTF binary (`.glb`) and flatten every
//! sub-mesh into a single [`Mesh`].
//!
//! All node sub-transforms are baked into vertex positions during flatten, so
//! the result behaves identically to any procedural primitive — `pos`/`rot`/
//! `scale` set on the DSL `mesh` node still apply on top. Materials, skinning,
//! animations, second UV sets, tangents, and vertex colors are dropped on
//! purpose: the DSL controls those at a higher level (`mat="…"`, `skin="…"`,
//! `clip "…"`).
//!
//! Path resolution:
//!   - `src="path/to/file.glb"` — read from disk; resolved against the
//!     directory holding the calling `.mog` file when `source_dir` is `Some`,
//!     otherwise from the current working directory.
//!   - `src="/abs/path/file.glb"` — absolute paths are always taken literally.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use glam::{Mat4, Vec3};

use mogen_core::Mesh;

/// Resolve `src` against `source_dir` and return the bytes of the GLB.
pub fn read_glb_bytes(src: &str, source_dir: Option<&Path>) -> Result<Vec<u8>> {
    let path = PathBuf::from(src);
    let resolved = if path.is_absolute() {
        path
    } else if let Some(dir) = source_dir {
        dir.join(&path)
    } else {
        path
    };
    fs::read(&resolved).with_context(|| format!("reading mesh source: {}", resolved.display()))
}

/// Parse a flat mesh out of a `.glb` byte buffer. Every primitive across every
/// mesh is concatenated into one `mogen-core::Mesh`. Sub-node transforms are
/// applied to vertex positions and normals (rotation-only for normals).
pub fn mesh_from_glb_bytes(bytes: &[u8]) -> Result<Mesh> {
    let (doc, buffers, _) = gltf::import_slice(bytes).context("parsing GLB")?;

    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    // Walk every scene's node tree, baking parent transforms into world space
    // as we descend. If the file has no scenes (rare but valid) we still walk
    // every top-level node.
    let walk_root_nodes: Vec<gltf::Node> = match doc.default_scene() {
        Some(s) => s.nodes().collect(),
        None => doc.nodes().filter(|n| !has_parent(&doc, n.index())).collect(),
    };
    for node in walk_root_nodes {
        walk_node(node, Mat4::IDENTITY, &buffers, &mut positions, &mut normals, &mut uvs, &mut indices);
    }

    if positions.is_empty() {
        bail!("imported mesh contains no geometry");
    }

    // UVs must either be empty or cover every position. If only some primitives
    // had UVs we pad the rest with zeros so the slot stays consistent.
    if !uvs.is_empty() && uvs.len() != positions.len() {
        uvs.resize(positions.len(), [0.0, 0.0]);
    }
    // Mesh::has_uvs treats len-mismatched UVs as missing. If every primitive
    // we saw had no UV0, leave the slot empty.

    Ok(Mesh { positions, normals, uvs, indices, ..Default::default() })
}

/// True if `idx` appears as a child anywhere in `doc`. Used for the "no
/// default scene" fallback to find genuine roots.
fn has_parent(doc: &gltf::Document, idx: usize) -> bool {
    doc.nodes().any(|n| n.children().any(|c| c.index() == idx))
}

fn walk_node(
    node: gltf::Node,
    parent_world: Mat4,
    buffers: &[gltf::buffer::Data],
    positions: &mut Vec<[f32; 3]>,
    normals: &mut Vec<[f32; 3]>,
    uvs: &mut Vec<[f32; 2]>,
    indices: &mut Vec<u32>,
) {
    let local: Mat4 = Mat4::from_cols_array_2d(&node.transform().matrix());
    let world = parent_world * local;
    if let Some(mesh) = node.mesh() {
        for prim in mesh.primitives() {
            append_primitive(prim, world, buffers, positions, normals, uvs, indices);
        }
    }
    for child in node.children() {
        walk_node(child, world, buffers, positions, normals, uvs, indices);
    }
}

fn append_primitive(
    prim: gltf::Primitive,
    world: Mat4,
    buffers: &[gltf::buffer::Data],
    positions: &mut Vec<[f32; 3]>,
    normals: &mut Vec<[f32; 3]>,
    uvs: &mut Vec<[f32; 2]>,
    indices: &mut Vec<u32>,
) {
    let reader = prim.reader(|b| Some(&buffers[b.index()]));
    let base = positions.len() as u32;

    let Some(pos_iter) = reader.read_positions() else { return };
    // Normal matrix for transforming normals: inverse-transpose of upper 3×3.
    let world3: glam::Mat3 = glam::Mat3::from_mat4(world);
    let normal_mat = world3.inverse().transpose();

    let pos_added: Vec<[f32; 3]> = pos_iter
        .map(|p| world.transform_point3(Vec3::from_array(p)).into())
        .collect();
    let added_count = pos_added.len();
    positions.extend_from_slice(&pos_added);

    if let Some(n_iter) = reader.read_normals() {
        for n in n_iter {
            let nv = (normal_mat * Vec3::from_array(n)).normalize_or_zero();
            normals.push(nv.into());
        }
    } else {
        // Pad with placeholder normals; the export side handles missing
        // normals if every primitive lacks them, but we keep alignment with
        // positions so per-vertex indices line up.
        normals.resize(positions.len(), [0.0, 1.0, 0.0]);
    }

    if let Some(uv_iter) = reader.read_tex_coords(0) {
        // Pad existing UV slot up to the new run if earlier primitives had no
        // UV0 — keeps the array aligned with positions.
        if uvs.len() < base as usize {
            uvs.resize(base as usize, [0.0, 0.0]);
        }
        for uv in uv_iter.into_f32() {
            uvs.push(uv);
        }
    }

    if let Some(idx_iter) = reader.read_indices() {
        for i in idx_iter.into_u32() {
            indices.push(base + i);
        }
    } else {
        // Non-indexed primitive: synthesise sequential indices over the run
        // we just appended.
        for i in 0..added_count as u32 {
            indices.push(base + i);
        }
    }
}

