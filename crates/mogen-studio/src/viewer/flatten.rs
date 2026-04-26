use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use glam::{Mat4, Vec3};
use mogen_core::{AlphaMode, NodeId, SceneGraph};

/// One contiguous range of indices that share a single material (and skin).
/// The viewer batches per material so each draw can upload the material's PBR
/// scalars and swap the bound texture set with one set of `sampler2D` bindings.
/// Skinned meshes get their own batches so the joint palette uniform only needs
/// to be uploaded once per skin (and so skinned vertices can opt out of the
/// baked world transform applied to static ones).
#[derive(Clone, Debug, Default)]
pub struct DrawBatch {
    pub index_start: u32,
    pub index_count: u32,
    /// Resolved (relative paths joined to the `.mog` directory) absolute path of
    /// each material texture slot, or `None` if the material doesn't declare
    /// one. Albedo and emissive are uploaded as sRGB; metallic-roughness,
    /// normal, and occlusion are uploaded as linear (see
    /// `Renderer::ensure_texture`).
    pub base_color_texture: Option<PathBuf>,
    pub metallic_roughness_texture: Option<PathBuf>,
    pub normal_texture: Option<PathBuf>,
    pub occlusion_texture: Option<PathBuf>,
    pub emissive_texture: Option<PathBuf>,
    /// PBR scalars copied off the source `Material`. Multiplied with their
    /// corresponding texture (when present) inside the fragment shader.
    pub base_color: [f32; 3],
    pub base_color_alpha: f32,
    pub metallic: f32,
    pub roughness: f32,
    pub emissive: [f32; 3],
    pub emissive_strength: f32,
    /// KHR_materials_transmission factor in [0,1]. Used as a cheap
    /// translucency stand-in: the FS scales diffuse + diffuse-IBL by
    /// `(1 - transmission)` so highly-transmissive materials only contribute
    /// their specular response. Combined with `Blend` alpha this reads as
    /// "glass" instead of a milky tint.
    pub transmission: f32,
    /// glTF alpha pipeline. `Opaque` ignores alpha entirely; `Mask` discards
    /// fragments whose alpha is below `alpha_cutoff`; `Blend` enables additive
    /// alpha blending and skips depth writes (the renderer also depth-sorts
    /// these batches back-to-front each frame so they composite correctly).
    pub alpha_mode: AlphaMode,
    pub alpha_cutoff: f32,
    /// glTF `doubleSided`. When set, the renderer disables back-face culling
    /// for this batch so thin sheets (leaves, cloth, flags) show from both
    /// sides. The FS also flips the geometric normal to face the camera so
    /// the BRDF doesn't go negative on back faces.
    pub double_sided: bool,
    /// World-space AABB centre of the batch. Used to depth-sort `Blend`
    /// batches back-to-front each frame; cheap-but-correct for typical
    /// preview scenes (full per-triangle sort would be overkill).
    pub centroid: Vec3,
    /// Index into [`FlatMesh::skins`] if every vertex in the batch is skinned
    /// by the same `Skin`. `None` means the batch is rigid and the vertex
    /// positions are already baked into world space by `flatten_with_worlds`.
    pub skin_id: Option<u32>,
}

/// One entry per clip, for UI menus.
#[derive(Clone)]
pub struct ClipSummary {
    pub name: String,
    pub duration: f32,
}

/// Number of f32s per vertex in the interleaved VBO:
/// pos(3) | normal(3) | uv(2) | joints(4) | weights(4) = 16.
/// The base colour used to live in the vertex stream; with PBR materials we
/// upload it as a per-batch uniform instead.
pub const FLOATS_PER_VERTEX: usize = 16;

/// Upper bound on joints per skin that we'll ship to the shader. The uniform
/// palette is a `mat4[MAX_JOINTS]` so every batch pays this much regardless of
/// how many bones its skin actually declares. 128 covers biped rigs with room
/// to spare while staying well under typical `GL_MAX_VERTEX_UNIFORM_COMPONENTS`.
pub const MAX_JOINTS: usize = 128;

/// Per-frame skinning palette for one [`mogen_core::Skin`]. Element `i` is
/// `joint_world_i * inverse_bind_i`, i.e. the matrix that the shader
/// pre-multiplies into each influenced vertex.
#[derive(Clone, Debug, Default)]
pub struct SkinPalette {
    pub joint_matrices: Vec<Mat4>,
}

/// CPU-side flattened geometry for the viewport. Static geometry has its world
/// transform baked into `vertices`; skinned geometry is emitted in its Skin's
/// bind-pose frame and deforms in the shader using `skins`.
#[derive(Default)]
pub struct FlatMesh {
    /// Interleaved: pos (3) | normal (3) | uv (2) | joints (4) | weights (4) =
    /// 16 f32 per vertex. Joints are stored as f32 so the interleaved VBO stays
    /// single-type; the shader `ivec4`-casts them.
    pub vertices: Vec<f32>,
    pub indices: Vec<u32>,
    /// Draw order. Each batch covers a contiguous index range belonging to a
    /// single source material (so PBR scalars and textures can be uploaded
    /// once) and a single skin (or `None` for rigid geometry). Empty when
    /// there is no geometry.
    pub batches: Vec<DrawBatch>,
    /// One palette per skin in the source `SceneGraph`, indexed by
    /// `SkinId.0`. Empty for scenes with no `skeleton` blocks.
    pub skins: Vec<SkinPalette>,
    pub center: Vec3,
    pub radius: f32,
    /// One entry per triangle (parallel to `indices` chunked in 3s) giving the
    /// `SceneNode` id that contributed the triangle. CPU-side only — used by
    /// the viewport picker, never uploaded.
    pub tri_node: Vec<NodeId>,
}

pub fn flatten(scene: &SceneGraph, base_dir: Option<&Path>) -> FlatMesh {
    let worlds = scene.world_transforms();
    flatten_with_worlds(scene, &worlds, base_dir)
}

pub fn flatten_with_worlds(
    scene: &SceneGraph,
    worlds: &[Mat4],
    base_dir: Option<&Path>,
) -> FlatMesh {
    // One batch per (skin, material). Materials carry per-batch PBR uniforms,
    // so we can no longer fold two distinct materials together just because
    // they happen to share a base-colour texture. Skinned meshes still need
    // their own batches because their vertex data lives in the skin's bind
    // frame instead of the baked world space used by rigid geometry.
    // `Option<MaterialId>` (encoded as Option<u32>) preserves a single bucket
    // for nodes that have no material at all.
    // BTreeMap so traversal order is stable across rebuilds — keeps the GPU's
    // index buffer churn predictable.
    type GroupKey = (Option<u32>, Option<u32>);
    let mut groups: BTreeMap<GroupKey, Vec<usize>> = BTreeMap::new();
    for (i, node) in scene.nodes.iter().enumerate() {
        if node.mesh.is_none() {
            continue;
        }
        let skin_id = node.skin.map(|s| s.0);
        let mat_id = node.material.map(|m| m.0);
        groups.entry((skin_id, mat_id)).or_default().push(i);
    }

    let mut vertices: Vec<f32> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    let mut batches: Vec<DrawBatch> = Vec::new();
    let mut tri_node: Vec<NodeId> = Vec::new();
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);

    for ((skin_id, mat_id), node_ids) in groups {
        let batch_start = indices.len() as u32;
        let material = mat_id.and_then(|id| scene.materials.get(id as usize));
        let uv_scale = material.map(|m| m.uv_scale).unwrap_or([1.0, 1.0]);
        let mut bmin = Vec3::splat(f32::INFINITY);
        let mut bmax = Vec3::splat(f32::NEG_INFINITY);
        for i in node_ids {
            let node = &scene.nodes[i];
            let mesh = node.mesh.as_ref().expect("node had a mesh in pass 1");
            let node_id = NodeId(i as u32);
            // glTF 2.0: a skinned mesh's node (and ancestor) transforms are
            // ignored; the skin fully describes the mesh's model-space pose.
            // Our `bind_meshes` pass already baked the bind-pose world into
            // the vertex buffer, so we pass positions through untouched and
            // let the shader apply the joint palette.
            let is_skinned = skin_id.is_some();
            let (world, normal_mat) = if is_skinned {
                (Mat4::IDENTITY, glam::Mat3::IDENTITY)
            } else {
                (worlds[i], glam::Mat3::from_mat4(worlds[i]))
            };

            let has_skin_attrs = is_skinned && mesh.is_skinned();
            let base = (vertices.len() / FLOATS_PER_VERTEX) as u32;
            for (vi, pos) in mesh.positions.iter().enumerate() {
                let p = if is_skinned {
                    Vec3::from_array(*pos)
                } else {
                    world.transform_point3(Vec3::from_array(*pos))
                };
                let n_src = mesh.normals.get(vi).copied().unwrap_or([0.0, 1.0, 0.0]);
                let n = if is_skinned {
                    Vec3::from_array(n_src).normalize_or_zero()
                } else {
                    (normal_mat * Vec3::from_array(n_src)).normalize_or_zero()
                };
                let uv_raw = mesh.uvs.get(vi).copied().unwrap_or([0.0, 0.0]);
                let uv = [uv_raw[0] * uv_scale[0], uv_raw[1] * uv_scale[1]];
                let (j, w) = if has_skin_attrs {
                    let ji = mesh.joints[vi];
                    let wi = mesh.weights[vi];
                    (
                        [ji[0] as f32, ji[1] as f32, ji[2] as f32, ji[3] as f32],
                        [wi[0], wi[1], wi[2], wi[3]],
                    )
                } else {
                    ([0.0; 4], [0.0; 4])
                };
                // For AABB fitting we want the rendered position. For rigid
                // batches that's `p`; for skinned batches it's the bind-pose
                // result we just emitted (also `p`, since bind-pose world
                // was baked during lowering). The per-batch AABB feeds the
                // centroid used for blend-batch depth sorting.
                min = min.min(p);
                max = max.max(p);
                bmin = bmin.min(p);
                bmax = bmax.max(p);
                vertices.extend_from_slice(&[
                    p.x, p.y, p.z, n.x, n.y, n.z, uv[0], uv[1], j[0], j[1], j[2], j[3], w[0],
                    w[1], w[2], w[3],
                ]);
            }
            let idx_before = indices.len();
            indices.extend(mesh.indices.iter().map(|idx| base + idx));
            // Each triangle added consumes 3 indices; tag them with this node.
            let tri_count = (indices.len() - idx_before) / 3;
            tri_node.extend(std::iter::repeat(node_id).take(tri_count));
        }
        let batch_count = indices.len() as u32 - batch_start;
        if batch_count == 0 {
            continue;
        }
        let resolve = |t: &mogen_core::TextureRef| resolve_texture_path(&t.path, base_dir);
        let (
            base_color_texture,
            mr_texture,
            normal_texture,
            occlusion_texture,
            emissive_texture,
            base_color,
            base_color_alpha,
            metallic,
            roughness,
            emissive,
            emissive_strength,
            transmission,
            alpha_mode,
            alpha_cutoff,
            double_sided,
        ) = match material {
            Some(m) => (
                m.base_color_texture.as_ref().map(resolve),
                m.metallic_roughness_texture.as_ref().map(resolve),
                m.normal_texture.as_ref().map(resolve),
                m.occlusion_texture.as_ref().map(resolve),
                m.emissive_texture.as_ref().map(resolve),
                [m.base_color[0], m.base_color[1], m.base_color[2]],
                m.base_color[3],
                m.metallic,
                m.roughness,
                m.emissive,
                m.emissive_strength,
                m.transmission,
                m.alpha_mode,
                m.alpha_cutoff,
                m.double_sided,
            ),
            None => (
                None,
                None,
                None,
                None,
                None,
                [0.78, 0.78, 0.78],
                1.0,
                0.0,
                0.9,
                [0.0, 0.0, 0.0],
                1.0,
                0.0,
                AlphaMode::Opaque,
                0.5,
                false,
            ),
        };
        let centroid = if bmin.is_finite() && bmax.is_finite() {
            (bmin + bmax) * 0.5
        } else {
            Vec3::ZERO
        };
        batches.push(DrawBatch {
            index_start: batch_start,
            index_count: batch_count,
            base_color_texture,
            metallic_roughness_texture: mr_texture,
            normal_texture,
            occlusion_texture,
            emissive_texture,
            base_color,
            base_color_alpha,
            metallic,
            roughness,
            emissive,
            emissive_strength,
            transmission,
            alpha_mode,
            alpha_cutoff,
            double_sided,
            centroid,
            skin_id,
        });
    }

    let skins = compute_skin_palettes(scene, worlds);

    let (center, radius) = if vertices.is_empty() {
        (Vec3::ZERO, 1.0)
    } else {
        let c = (min + max) * 0.5;
        let r = ((max - min).length() * 0.5).max(0.25);
        (c, r)
    };

    FlatMesh {
        vertices,
        indices,
        batches,
        skins,
        center,
        radius,
        tri_node,
    }
}

/// Build the per-skin shader palette from the currently-posed `worlds`. Each
/// palette entry is `joint_world_current * inverse_bind`, which maps a vertex
/// from bind-pose skin space to current pose skin space. Joints beyond
/// [`MAX_JOINTS`] are dropped with a one-shot warning — the preview caps out
/// before the shader's uniform array does.
fn compute_skin_palettes(scene: &SceneGraph, worlds: &[Mat4]) -> Vec<SkinPalette> {
    scene
        .skins
        .iter()
        .map(|skin| {
            let n = skin.joints.len().min(MAX_JOINTS);
            if skin.joints.len() > MAX_JOINTS {
                eprintln!(
                    "viewer: skin \"{}\" has {} joints, truncating to {} for preview",
                    skin.name,
                    skin.joints.len(),
                    MAX_JOINTS
                );
            }
            let mut joint_matrices = Vec::with_capacity(n);
            for i in 0..n {
                let node_id = skin.joints[i];
                let jw = worlds
                    .get(node_id.0 as usize)
                    .copied()
                    .unwrap_or(Mat4::IDENTITY);
                let ibm = Mat4::from_cols_array_2d(&skin.inverse_bind_matrices[i]);
                joint_matrices.push(jw * ibm);
            }
            SkinPalette { joint_matrices }
        })
        .collect()
}

/// Resolve a possibly-relative texture path against the `.mog` file's
/// directory. Absolute paths are returned as-is; missing `base_dir` leaves the
/// path unchanged so the renderer's later open-file error is meaningful.
fn resolve_texture_path(path: &Path, base_dir: Option<&Path>) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    match base_dir {
        Some(b) => b.join(path),
        None => path.to_path_buf(),
    }
}
