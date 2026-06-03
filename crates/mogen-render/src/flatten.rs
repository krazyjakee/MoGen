use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use glam::{Mat4, Vec3};
use mogen_core::{AlphaMode, NodeId, SceneGraph, Transform};

use crate::anim::world_transforms_from_locals;

/// One contiguous range of indices that share a single material and palette.
/// The viewer batches per material so each draw can upload the material's PBR
/// scalars and swap the bound texture set with one set of `sampler2D` bindings.
/// Each batch additionally references a single matrix palette by `palette_id`
/// — for rigid batches that's the world transforms of the nodes contributing
/// vertices; for skinned batches it's `joint_world * inverse_bind`. Both feed
/// the same `u_joint_mats` shader uniform, so the renderer's draw path is
/// uniform across the two cases.
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
    /// Index into [`FlatMesh::palettes`]. Every batch has one palette: rigid
    /// batches deform a single bone (vertex `joints[0]`, `weights = [1,0,0,0]`)
    /// against per-node world transforms; skinned batches use the standard
    /// glTF skin path. Both feed the same `u_joint_mats` uniform.
    pub palette_id: u32,
}

/// One entry per clip, for UI menus.
#[derive(Clone)]
pub struct ClipSummary {
    pub name: String,
    pub duration: f32,
    /// Canonical path of the imported `.mog` file the clip was lowered from.
    /// `None` when the clip was authored in the active file.
    pub origin: Option<std::path::PathBuf>,
}

/// Number of f32s per vertex in the interleaved VBO:
/// pos(3) | normal(3) | uv(2) | joints(4) | weights(4) | color(4) = 20.
/// The PBR base colour is a per-batch uniform; `color` here is the optional
/// per-vertex `COLOR_0` channel (terrain grass/rock/sand/mud bake, gradient
/// ramps). Vertices from meshes without `COLOR_0` get an opaque-white colour so
/// the shader can always multiply by it unconditionally.
pub const FLOATS_PER_VERTEX: usize = 20;

/// Upper bound on joints per skin that we'll ship to the shader. The uniform
/// palette is a `mat4[MAX_JOINTS]` so every batch pays this much regardless of
/// how many bones its skin actually declares. 128 covers biped rigs with room
/// to spare while staying well under typical `GL_MAX_VERTEX_UNIFORM_COMPONENTS`.
/// Rigid batches reuse the same uniform: if a single (material) group contains
/// more than [`MAX_JOINTS`] distinct nodes, [`flatten_with_worlds`] splits it
/// into multiple batches so the per-batch palette never overflows the array.
pub const MAX_JOINTS: usize = 128;

/// Per-frame matrix palette uploaded to `u_joint_mats[]`. Element `i` is the
/// matrix the shader pre-multiplies into each vertex weighted by `weights[i]`.
/// For skinned batches: `joint_world_i * inverse_bind_i`. For rigid batches:
/// `current_world(node_i) * inverse(rest_world(node_i))`, applied to vertices
/// baked at rest-pose world (so the result is `current_world * pos_local`).
#[derive(Clone, Debug, Default)]
pub struct SkinPalette {
    pub joint_matrices: Vec<Mat4>,
}

/// How to recompute `palettes[i]` when only the pose changed (animation tick
/// or gizmo drag). Stored alongside [`FlatMesh::palettes`] so the per-frame
/// refresh doesn't have to re-walk the scene's batches.
#[derive(Clone, Debug)]
pub enum PaletteSource {
    /// Rigid batch. `palette[k] = worlds[nodes[k]] * inv_rest_worlds[k]`. The
    /// inverse-rest factor compensates for the rest-pose world transform that
    /// was baked into the vertex stream at flatten time, leaving the runtime
    /// palette free to express any new pose.
    Rigid {
        nodes: Vec<NodeId>,
        inv_rest_worlds: Vec<Mat4>,
    },
    /// Skinned batch. `palette[k] = world(skin.joints[k]) * skin.inverse_bind[k]`,
    /// truncated to [`MAX_JOINTS`]. Identical to the v1 skinning path.
    Skin { skin_id: u32 },
}

/// CPU-side flattened geometry for the viewport. Vertex data is baked at
/// rest-pose world (so picker rays continue to hit the resting silhouette);
/// per-batch matrix palettes carry the running pose and are refreshed each
/// animation tick without rewriting the VBO.
#[derive(Default)]
pub struct FlatMesh {
    /// Interleaved: pos (3) | normal (3) | uv (2) | joints (4) | weights (4) =
    /// 16 f32 per vertex. Joints are stored as f32 so the interleaved VBO stays
    /// single-type; the shader `ivec4`-casts them. For rigid vertices,
    /// `joints[0]` is the index into the batch's palette (`weights = [1,0,0,0]`).
    pub vertices: Vec<f32>,
    pub indices: Vec<u32>,
    /// Draw order. Each batch covers a contiguous index range belonging to a
    /// single source material plus a single palette. Empty when there is no
    /// geometry.
    pub batches: Vec<DrawBatch>,
    /// One palette per batch (parallel to `palette_sources`). Indexed by
    /// `DrawBatch::palette_id`. Rebuilt by [`update_palettes`] each animation
    /// tick — the rest of the mesh stays put.
    pub palettes: Vec<SkinPalette>,
    /// Recipe for refreshing each palette without re-flattening the mesh.
    pub palette_sources: Vec<PaletteSource>,
    pub center: Vec3,
    pub radius: f32,
    /// Axis-aligned bounding box of every world-space vertex contributed
    /// by the flattened scene. Used by the imposter bake to frame the
    /// camera tight against the model (rather than against the looser
    /// bounding-sphere `radius`, which leaves visible padding on every
    /// cell and makes the billboard appear to float above the ground).
    pub aabb_min: Vec3,
    pub aabb_max: Vec3,
    /// One entry per triangle (parallel to `indices` chunked in 3s) giving the
    /// `SceneNode` id that contributed the triangle. CPU-side only — used by
    /// the viewport picker, never uploaded.
    pub tri_node: Vec<NodeId>,
}

pub fn flatten(scene: &SceneGraph, base_dir: Option<&Path>) -> FlatMesh {
    let worlds = scene.world_transforms();
    flatten_with_worlds(scene, &worlds, base_dir)
}

/// Refresh `mesh.palettes` from the current `locals` (animation- and
/// drag-modulated). Cheap relative to [`flatten_with_worlds`] — work scales
/// with the number of palette entries, not vertex count, and the VBO is left
/// untouched. The renderer's `mesh_dirty` flag must NOT be set by callers;
/// instead they should mark `palettes_dirty` so the paint callback uploads
/// only the uniform palette.
pub fn update_palettes(scene: &SceneGraph, locals: &[Transform], mesh: &mut FlatMesh) {
    if mesh.palette_sources.is_empty() {
        return;
    }
    let worlds = world_transforms_from_locals(scene, locals);
    if mesh.palettes.len() != mesh.palette_sources.len() {
        mesh.palettes
            .resize(mesh.palette_sources.len(), SkinPalette::default());
    }
    for (i, src) in mesh.palette_sources.iter().enumerate() {
        let palette = &mut mesh.palettes[i];
        match src {
            PaletteSource::Rigid {
                nodes,
                inv_rest_worlds,
            } => {
                palette.joint_matrices.clear();
                palette.joint_matrices.reserve(nodes.len());
                for (k, node) in nodes.iter().enumerate() {
                    let cur = worlds
                        .get(node.0 as usize)
                        .copied()
                        .unwrap_or(Mat4::IDENTITY);
                    palette.joint_matrices.push(cur * inv_rest_worlds[k]);
                }
            }
            PaletteSource::Skin { skin_id } => {
                let Some(skin) = scene.skins.get(*skin_id as usize) else {
                    palette.joint_matrices.clear();
                    continue;
                };
                let n = skin.joints.len().min(MAX_JOINTS);
                palette.joint_matrices.clear();
                palette.joint_matrices.reserve(n);
                for k in 0..n {
                    let node_id = skin.joints[k];
                    let jw = worlds
                        .get(node_id.0 as usize)
                        .copied()
                        .unwrap_or(Mat4::IDENTITY);
                    let ibm = Mat4::from_cols_array_2d(&skin.inverse_bind_matrices[k]);
                    palette.joint_matrices.push(jw * ibm);
                }
            }
        }
    }
}

pub fn flatten_with_worlds(
    scene: &SceneGraph,
    worlds: &[Mat4],
    base_dir: Option<&Path>,
) -> FlatMesh {
    // One bucket per (skin, material). Rigid buckets may then be split when
    // their distinct-node count exceeds MAX_JOINTS — the per-batch palette
    // uniform array can't hold more entries than the shader's `u_joint_mats`
    // declares. BTreeMap so traversal order is stable across rebuilds.
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
    let mut palette_sources: Vec<PaletteSource> = Vec::new();
    let mut tri_node: Vec<NodeId> = Vec::new();
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);

    for ((skin_id, mat_id), node_ids) in groups {
        let material = mat_id.and_then(|id| scene.materials.get(id as usize));
        let uv_scale = material.map(|m| m.uv_scale).unwrap_or([1.0, 1.0]);
        let is_skinned = skin_id.is_some();

        // Plan the batch boundaries. Skinned groups are always one batch (their
        // palette is the single skin and is sized at MAX_JOINTS regardless of
        // how many mesh nodes share it). Rigid groups split into chunks of at
        // most MAX_JOINTS distinct nodes so the palette never overflows.
        let chunks: Vec<&[usize]> = if is_skinned {
            vec![&node_ids[..]]
        } else {
            let mut out: Vec<&[usize]> = Vec::new();
            let chunk_size = MAX_JOINTS.max(1);
            let mut start = 0;
            while start < node_ids.len() {
                let end = (start + chunk_size).min(node_ids.len());
                out.push(&node_ids[start..end]);
                start = end;
            }
            out
        };

        for chunk in chunks {
            let batch_start = indices.len() as u32;
            let mut bmin = Vec3::splat(f32::INFINITY);
            let mut bmax = Vec3::splat(f32::NEG_INFINITY);
            // Per-batch rigid palette. Each unique node referenced by this
            // batch becomes one palette slot; vertex `joints[0]` indexes into
            // it. Skinned batches leave both vectors empty.
            let mut rigid_nodes: Vec<NodeId> = Vec::new();
            let mut rigid_inv_rest: Vec<Mat4> = Vec::new();

            for &i in chunk {
                let node = &scene.nodes[i];
                let mesh = node.mesh.as_ref().expect("node had a mesh in pass 1");
                let node_id = NodeId(i as u32);

                // glTF 2.0: a skinned mesh's node (and ancestor) transforms are
                // ignored; the skin fully describes the mesh's model-space pose.
                // Our `bind_meshes` pass already baked the bind-pose world into
                // the vertex buffer, so we pass positions through untouched and
                // let the shader apply the joint palette.
                let (world, normal_mat) = if is_skinned {
                    (Mat4::IDENTITY, glam::Mat3::IDENTITY)
                } else {
                    (worlds[i], glam::Mat3::from_mat4(worlds[i]))
                };

                // Rigid batches: register this node in the palette (de-duped
                // even though the grouping above gives us each node once) and
                // assign every vertex its slot via `joints[0]`. The inverse of
                // the rest world cancels the bake we apply below, so the
                // runtime palette can express any pose.
                let palette_idx_for_vertex: u32 = if is_skinned {
                    0
                } else {
                    let idx = rigid_nodes.len() as u32;
                    rigid_nodes.push(node_id);
                    rigid_inv_rest.push(worlds[i].inverse());
                    idx
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
                    } else if is_skinned {
                        // Skinned mesh missing per-vertex skin attrs: leave it
                        // unweighted (weights all zero). The shader will treat
                        // the result as un-deformed bind-pose geometry.
                        ([0.0; 4], [0.0; 4])
                    } else {
                        // Rigid vertex: single-bone weight 1.0 against the
                        // per-batch node palette. The rest-pose world bake is
                        // already in the position; the runtime palette adds
                        // any subsequent animation/drag delta.
                        (
                            [palette_idx_for_vertex as f32, 0.0, 0.0, 0.0],
                            [1.0, 0.0, 0.0, 0.0],
                        )
                    };
                    // Per-vertex COLOR_0; opaque white when the mesh carries
                    // none, so the shader's unconditional multiply is a no-op.
                    let c = mesh.colors.get(vi).copied().unwrap_or([1.0, 1.0, 1.0, 1.0]);
                    min = min.min(p);
                    max = max.max(p);
                    bmin = bmin.min(p);
                    bmax = bmax.max(p);
                    vertices.extend_from_slice(&[
                        p.x, p.y, p.z, n.x, n.y, n.z, uv[0], uv[1], j[0], j[1], j[2], j[3], w[0],
                        w[1], w[2], w[3], c[0], c[1], c[2], c[3],
                    ]);
                }
                let idx_before = indices.len();
                indices.extend(mesh.indices.iter().map(|idx| base + idx));
                let tri_count = (indices.len() - idx_before) / 3;
                tri_node.extend(std::iter::repeat(node_id).take(tri_count));
            }
            let batch_count = indices.len() as u32 - batch_start;
            if batch_count == 0 {
                continue;
            }
            let palette_id = palette_sources.len() as u32;
            if is_skinned {
                palette_sources.push(PaletteSource::Skin {
                    skin_id: skin_id.expect("is_skinned implies Some(skin_id)"),
                });
            } else {
                palette_sources.push(PaletteSource::Rigid {
                    nodes: rigid_nodes,
                    inv_rest_worlds: rigid_inv_rest,
                });
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
                palette_id,
            });
        }
    }

    let palettes = compute_initial_palettes(scene, worlds, &palette_sources);

    let (center, radius, aabb_min, aabb_max) = if vertices.is_empty() {
        (Vec3::ZERO, 1.0, Vec3::splat(-0.5), Vec3::splat(0.5))
    } else {
        let c = (min + max) * 0.5;
        let r = ((max - min).length() * 0.5).max(0.25);
        (c, r, min, max)
    };

    FlatMesh {
        vertices,
        indices,
        batches,
        palettes,
        palette_sources,
        center,
        radius,
        aabb_min,
        aabb_max,
        tri_node,
    }
}

/// Build the initial palette set at scene-load time. For rigid sources this
/// produces near-identity matrices (`world * inverse(world) ≈ I`), but a
/// scene with no animation playing still relies on these palettes — the
/// shader unconditionally applies them, so leaving the array empty would
/// project every rigid vertex through a zero matrix.
fn compute_initial_palettes(
    scene: &SceneGraph,
    worlds: &[Mat4],
    sources: &[PaletteSource],
) -> Vec<SkinPalette> {
    sources
        .iter()
        .map(|src| match src {
            PaletteSource::Rigid {
                nodes,
                inv_rest_worlds,
            } => SkinPalette {
                joint_matrices: nodes
                    .iter()
                    .zip(inv_rest_worlds.iter())
                    .map(|(node, inv_rest)| {
                        let cur = worlds
                            .get(node.0 as usize)
                            .copied()
                            .unwrap_or(Mat4::IDENTITY);
                        cur * *inv_rest
                    })
                    .collect(),
            },
            PaletteSource::Skin { skin_id } => {
                let Some(skin) = scene.skins.get(*skin_id as usize) else {
                    return SkinPalette::default();
                };
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
            }
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
