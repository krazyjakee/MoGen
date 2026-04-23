use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use eframe::egui;
use glam::{Mat4, Quat, Vec3};
use glow::HasContext;
use mgen_core::{
    AlphaMode, Clip, Interpolation, NodeId, SceneGraph, Track, TrackProperty, Transform,
};

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
    /// Resolved (relative paths joined to the `.mg` directory) absolute path of
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
const FLOATS_PER_VERTEX: usize = 16;

/// Upper bound on joints per skin that we'll ship to the shader. The uniform
/// palette is a `mat4[MAX_JOINTS]` so every batch pays this much regardless of
/// how many bones its skin actually declares. 128 covers biped rigs with room
/// to spare while staying well under typical `GL_MAX_VERTEX_UNIFORM_COMPONENTS`.
pub const MAX_JOINTS: usize = 128;

/// Per-frame skinning palette for one [`mgen_core::Skin`]. Element `i` is
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
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);

    for ((skin_id, mat_id), node_ids) in groups {
        let batch_start = indices.len() as u32;
        let material = mat_id.and_then(|id| scene.materials.get(id as usize));
        let mut bmin = Vec3::splat(f32::INFINITY);
        let mut bmax = Vec3::splat(f32::NEG_INFINITY);
        for i in node_ids {
            let node = &scene.nodes[i];
            let mesh = node.mesh.as_ref().expect("node had a mesh in pass 1");
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
                let uv = mesh.uvs.get(vi).copied().unwrap_or([0.0, 0.0]);
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
            indices.extend(mesh.indices.iter().map(|idx| base + idx));
        }
        let batch_count = indices.len() as u32 - batch_start;
        if batch_count == 0 {
            continue;
        }
        let resolve = |t: &mgen_core::TextureRef| resolve_texture_path(&t.path, base_dir);
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

/// Resolve a possibly-relative texture path against the `.mg` file's
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

/// Fold `clip` sampled at time `t` into `locals`. Later calls override earlier
/// ones per (node, property), so driving the same node from multiple active
/// clips is deterministic: the last one wins.
pub fn apply_animation(clip: &Clip, t: f32, locals: &mut [Transform]) {
    for track in &clip.tracks {
        let idx = track.node.0 as usize;
        if idx >= locals.len() {
            continue;
        }
        let v = sample_track(track, t);
        let base = &mut locals[idx];
        match track.property {
            TrackProperty::Translation => base.translation = Vec3::new(v[0], v[1], v[2]),
            TrackProperty::Rotation => {
                base.rotation = Quat::from_xyzw(v[0], v[1], v[2], v[3]).normalize()
            }
            TrackProperty::Scale => base.scale = Vec3::new(v[0], v[1], v[2]),
        }
    }
}

pub fn world_transforms_from_locals(scene: &SceneGraph, locals: &[Transform]) -> Vec<Mat4> {
    let mut out = vec![Mat4::IDENTITY; scene.nodes.len()];
    for root in &scene.roots {
        walk_world(scene, *root, Mat4::IDENTITY, locals, &mut out);
    }
    out
}

fn walk_world(
    scene: &SceneGraph,
    id: NodeId,
    parent: Mat4,
    locals: &[Transform],
    out: &mut [Mat4],
) {
    let world = parent * locals[id.0 as usize].to_mat4();
    out[id.0 as usize] = world;
    for c in &scene.nodes[id.0 as usize].children {
        walk_world(scene, *c, world, locals, out);
    }
}

fn sample_track(track: &Track, t: f32) -> [f32; 4] {
    if track.times.is_empty() {
        return [0.0, 0.0, 0.0, 0.0];
    }
    let n = track.times.len();
    if n == 1 {
        return track.values[0];
    }
    let first = track.times[0];
    let last = track.times[n - 1];
    let t = t.clamp(first, last);
    let mut i = 0;
    while i + 1 < n && track.times[i + 1] < t {
        i += 1;
    }
    let i0 = i;
    let i1 = (i + 1).min(n - 1);
    let t0 = track.times[i0];
    let t1 = track.times[i1];
    let f = if (t1 - t0).abs() < 1e-6 {
        0.0
    } else {
        ((t - t0) / (t1 - t0)).clamp(0.0, 1.0)
    };
    let v0 = track.values[i0];
    let v1 = track.values[i1];
    match (track.property, track.interpolation) {
        (_, Interpolation::Step) => v0,
        (TrackProperty::Rotation, _) => {
            let q0 = Quat::from_xyzw(v0[0], v0[1], v0[2], v0[3]);
            let q1 = Quat::from_xyzw(v1[0], v1[1], v1[2], v1[3]);
            let q = q0.slerp(q1, f);
            [q.x, q.y, q.z, q.w]
        }
        _ => [
            v0[0] + (v1[0] - v0[0]) * f,
            v0[1] + (v1[1] - v0[1]) * f,
            v0[2] + (v1[2] - v0[2]) * f,
            v0[3] + (v1[3] - v0[3]) * f,
        ],
    }
}

pub struct OrbitCamera {
    pub yaw: f32,
    pub pitch: f32,
    /// Distance that exactly fits the current model at the fixed FOV. Derived
    /// from the mesh's bounding sphere on every `fit()` call.
    pub fit_distance: f32,
    /// User-controlled multiplier on top of `fit_distance`. 1.0 = auto-fit;
    /// scroll tweaks this. Reset by `Viewer::reset_view` when switching files
    /// so different models render at the same apparent size.
    pub zoom: f32,
    pub target: Vec3,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        Self {
            yaw: std::f32::consts::FRAC_PI_4,
            // Positive pitch lifts the eye above the target so we get a
            // classic 3/4 view looking slightly down at the model.
            pitch: 0.5,
            fit_distance: 4.0,
            zoom: 1.0,
            target: Vec3::ZERO,
        }
    }
}

impl OrbitCamera {
    pub fn fit(&mut self, mesh: &FlatMesh) {
        self.target = mesh.center;
        self.fit_distance = mesh.radius * 2.8;
    }

    pub fn distance(&self) -> f32 {
        self.fit_distance * self.zoom
    }

    pub fn eye(&self) -> Vec3 {
        let dist = self.distance();
        self.target
            + Vec3::new(
                dist * self.pitch.cos() * self.yaw.sin(),
                dist * self.pitch.sin(),
                dist * self.pitch.cos() * self.yaw.cos(),
            )
    }

    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        let dist = self.distance();
        let eye = self.eye();
        let view = Mat4::look_at_rh(eye, self.target, Vec3::Y);
        let near = (dist * 0.01).max(0.01);
        let far = (dist * 10.0).max(10.0);
        let proj = Mat4::perspective_rh_gl(45.0_f32.to_radians(), aspect.max(0.01), near, far);
        proj * view
    }
}

const VS_SRC: &str = r#"#version 330 core
layout (location = 0) in vec3 a_pos;
layout (location = 1) in vec3 a_normal;
layout (location = 2) in vec2 a_uv;
layout (location = 3) in vec4 a_joints;
layout (location = 4) in vec4 a_weights;

uniform mat4 u_viewproj;
uniform int u_use_skin;
// Keep in sync with MAX_JOINTS in viewer.rs.
uniform mat4 u_joint_mats[128];

out vec3 v_world_pos;
out vec3 v_normal;
out vec2 v_uv;

void main() {
    vec4 pos4 = vec4(a_pos, 1.0);
    vec3 n = a_normal;
    if (u_use_skin == 1) {
        // Clamp defensively: huge skins get their palette truncated on the
        // CPU side, but a vertex might still reference a joint beyond 127 if
        // the caller's skin has more than MAX_JOINTS. The corresponding weight
        // will typically be zero in that case, but out-of-range uniform reads
        // are undefined, so keep the index in range unconditionally.
        ivec4 ji = clamp(ivec4(a_joints), ivec4(0), ivec4(127));
        mat4 skin = u_joint_mats[ji.x] * a_weights.x
                  + u_joint_mats[ji.y] * a_weights.y
                  + u_joint_mats[ji.z] * a_weights.z
                  + u_joint_mats[ji.w] * a_weights.w;
        pos4 = skin * pos4;
        // mat3(skin) is a reasonable approximation of the normal transform so
        // long as joint palettes stay close to rigid; the renderer re-
        // normalizes in the fragment shader regardless.
        n = mat3(skin) * n;
    }
    gl_Position = u_viewproj * pos4;
    v_world_pos = pos4.xyz;
    v_normal = n;
    v_uv = a_uv;
}
"#;

const FS_SRC: &str = r#"#version 330 core
in vec3 v_world_pos;
in vec3 v_normal;
in vec2 v_uv;
out vec4 frag;

uniform vec3 u_camera_pos;

// Per-batch material scalars.
uniform vec3 u_base_color;
uniform float u_base_color_alpha;
uniform float u_metallic;
uniform float u_roughness;
uniform vec3 u_emissive;
uniform float u_emissive_strength;
// KHR_materials_transmission factor in [0,1]. We don't have a real
// refraction pass, but we use it to suppress the diffuse term: a glass
// surface mostly lets light pass through and only reflects specularly,
// so for `transmission` close to 1 the diffuse + diffuse-IBL contributions
// are scaled toward zero, leaving the Fresnel highlights intact. Combined
// with `Blend` alpha this is a cheap stand-in that reads as "translucent
// glass" instead of the milky-opaque look you get from alpha alone.
uniform float u_transmission;
// glTF alpha pipeline. 0 = Opaque, 1 = Mask, 2 = Blend.
uniform int u_alpha_mode;
uniform float u_alpha_cutoff;

// Texture toggles + samplers (one set per material slot). Albedo/emissive are
// uploaded as sRGB; the others are linear.
uniform int u_use_base_tex;
uniform int u_use_mr_tex;
uniform int u_use_normal_tex;
uniform int u_use_ao_tex;
uniform int u_use_emissive_tex;
uniform sampler2D u_base_tex;
uniform sampler2D u_mr_tex;
uniform sampler2D u_normal_tex;
uniform sampler2D u_ao_tex;
uniform sampler2D u_emissive_tex;

// Two analytic key/fill lights for direct illumination plus a procedural sky
// dome that doubles as the IBL probe for ambient + specular reflection.
uniform vec3 u_key_dir;
uniform vec3 u_fill_dir;
uniform vec3 u_sky_top;
uniform vec3 u_sky_horizon;
uniform vec3 u_sky_ground;
uniform vec3 u_sun_dir;
uniform vec3 u_sun_color;

const float PI = 3.14159265359;

// AgX tonemap (Troy Sobotka). Polynomial fit by Benjamin "MrLixm" /
// Filament refinements. Maps scene-referred linear sRGB to display-referred
// linear sRGB; matches Blender's default view transform much better than
// Reinhard, especially for bright sky / sun reflections that would otherwise
// pin to white. Final sRGB encode is left to GL_FRAMEBUFFER_SRGB.
const mat3 AgXInsetMatrix = mat3(
    0.842479062253094,  0.0784335999999992, 0.0792237451477643,
    0.0423282422610123, 0.878468636469772,  0.0791661274605434,
    0.0423756549057051, 0.0784336,          0.879142973793104);

const mat3 AgXOutsetMatrix = mat3(
     1.19687900512017,    -0.0980208811401368, -0.0990297440797205,
    -0.0528968517574562,   1.15190312990417,   -0.0989611768448433,
    -0.0529716355144438,  -0.0980434501171241,  1.15107367264116);

vec3 agxDefaultContrastApprox(vec3 x) {
    vec3 x2 = x * x;
    vec3 x4 = x2 * x2;
    return  15.5     * x4 * x2
          - 40.14    * x4 * x
          + 31.96    * x4
          -  6.868   * x2 * x
          +  0.4298  * x2
          +  0.1191  * x
          -  0.00232;
}

vec3 agx_tonemap(vec3 color) {
    color = AgXInsetMatrix * color;
    color = max(color, vec3(1e-10));
    color = log2(color);
    // Remap [-12.47, 4.03] EV → [0, 1].
    color = (color + 12.47393) / (12.47393 + 4.026069);
    color = clamp(color, 0.0, 1.0);
    color = agxDefaultContrastApprox(color);
    color = AgXOutsetMatrix * color;
    // Sigmoid output is in display-encoded sRGB. The framebuffer is sRGB,
    // so undo the display encoding here and let the hardware re-encode on
    // write — otherwise the gamma curve gets applied twice.
    color = pow(max(color, 0.0), vec3(2.2));
    return color;
}

// Three-band sky: zenith → horizon for the upper hemisphere, horizon → ground
// for the lower one, plus a tight sun disc. This is what reflections sample
// in the absence of a real cubemap.
vec3 sample_sky(vec3 dir) {
    float y = clamp(dir.y, -1.0, 1.0);
    vec3 sky;
    if (y >= 0.0) {
        sky = mix(u_sky_horizon, u_sky_top, smoothstep(0.0, 1.0, y));
    } else {
        sky = mix(u_sky_horizon, u_sky_ground, smoothstep(0.0, 1.0, -y));
    }
    float sun = max(dot(dir, -u_sun_dir), 0.0);
    sky += u_sun_color * pow(sun, 256.0) * 8.0;
    return sky;
}

// Schüler's "normal mapping without precomputed tangents": derive a TBN from
// screen-space derivatives of position+UV. Quality dips on coarse meshes but
// it lets us drop tangents from the vertex stream entirely.
mat3 cotangent_frame(vec3 N, vec3 p, vec2 uv) {
    vec3 dp1 = dFdx(p);
    vec3 dp2 = dFdy(p);
    vec2 duv1 = dFdx(uv);
    vec2 duv2 = dFdy(uv);
    vec3 dp2perp = cross(dp2, N);
    vec3 dp1perp = cross(N, dp1);
    vec3 T = dp2perp * duv1.x + dp1perp * duv2.x;
    vec3 B = dp2perp * duv1.y + dp1perp * duv2.y;
    float invmax = inversesqrt(max(dot(T, T), dot(B, B)));
    return mat3(T * invmax, B * invmax, N);
}

// GGX/Trowbridge-Reitz NDF.
float D_GGX(float NdH, float a) {
    float a2 = a * a;
    float d = (NdH * NdH * (a2 - 1.0) + 1.0);
    return a2 / max(PI * d * d, 1e-7);
}

vec3 F_Schlick(float cosT, vec3 F0) {
    return F0 + (1.0 - F0) * pow(clamp(1.0 - cosT, 0.0, 1.0), 5.0);
}

// Sébastien Lagarde's roughness-aware Fresnel for IBL: keeps the Fresnel
// edge from blowing out when the surface is rough.
vec3 F_Schlick_rough(float cosT, vec3 F0, float roughness) {
    return F0 + (max(vec3(1.0 - roughness), F0) - F0)
              * pow(clamp(1.0 - cosT, 0.0, 1.0), 5.0);
}

// Smith joint visibility (numerator absorbs the Cook-Torrance 1/(4 NdL NdV)).
float V_SmithGGX(float NdV, float NdL, float a) {
    float a2 = a * a;
    float ggxV = NdL * sqrt(NdV * NdV * (1.0 - a2) + a2);
    float ggxL = NdV * sqrt(NdL * NdL * (1.0 - a2) + a2);
    return 0.5 / max(ggxV + ggxL, 1e-4);
}

// Karis' fitted env BRDF (UE4 split-sum approximation, scale + bias only).
vec2 env_brdf_approx(float NdV, float roughness) {
    vec4 c0 = vec4(-1.0, -0.0275, -0.572, 0.022);
    vec4 c1 = vec4(1.0,  0.0425,  1.040, -0.040);
    vec4 r = roughness * c0 + c1;
    float a004 = min(r.x * r.x, exp2(-9.28 * NdV)) * r.x + r.y;
    return vec2(-1.04, 1.04) * a004 + r.zw;
}

vec3 brdf_direct(vec3 N, vec3 V, vec3 L, vec3 albedo, float metallic, float roughness, vec3 F0, float diffuse_scale) {
    vec3 H = normalize(V + L);
    float NdL = max(dot(N, L), 0.0);
    float NdV = max(dot(N, V), 0.0);
    float NdH = max(dot(N, H), 0.0);
    float HdV = max(dot(H, V), 0.0);
    float a = roughness * roughness;
    float D = D_GGX(NdH, a);
    vec3 F = F_Schlick(HdV, F0);
    float Vis = V_SmithGGX(NdV, NdL, a);
    vec3 spec = D * F * Vis;
    vec3 kd = (1.0 - F) * (1.0 - metallic);
    // `diffuse_scale` is `(1 - transmission)`; for fully transmissive glass
    // it kills the diffuse term so only the Fresnel-reflected specular
    // survives.
    vec3 diff = kd * albedo * diffuse_scale / PI;
    return (diff + spec) * NdL;
}

void main() {
    // Gather material samples.
    vec4 base_sample = vec4(u_base_color, u_base_color_alpha);
    if (u_use_base_tex == 1) {
        base_sample *= texture(u_base_tex, v_uv);
    }
    // Alpha pipeline. `Mask` discards before any lighting work — cheap exit.
    // `Opaque` ignores the alpha channel; `Blend` propagates the authored
    // alpha; `Opaque` materials with `transmission > 0` start from the
    // authored alpha too so the Fresnel-rim ramp below has something to
    // grow from.
    if (u_alpha_mode == 1 && base_sample.a < u_alpha_cutoff) {
        discard;
    }
    bool will_blend = (u_alpha_mode == 2) || (u_transmission > 0.0);
    float out_alpha = will_blend ? base_sample.a : 1.0;
    vec3 albedo = base_sample.rgb;

    float metallic = u_metallic;
    float roughness = u_roughness;
    if (u_use_mr_tex == 1) {
        // glTF convention: G = roughness, B = metallic. Multiply with the
        // authored scalars so a tint of metallic=0 still kills metalness.
        vec3 mr = texture(u_mr_tex, v_uv).rgb;
        roughness *= mr.g;
        metallic *= mr.b;
    }
    roughness = clamp(roughness, 0.045, 1.0);

    vec3 Ngeom = normalize(v_normal);
    // Double-sided materials see the back face of thin sheets; flip the
    // geometric normal toward the camera so the BRDF stays positive on the
    // back (otherwise leaves and cloth go black through their underside).
    if (!gl_FrontFacing) {
        Ngeom = -Ngeom;
    }
    vec3 V = normalize(u_camera_pos - v_world_pos);
    vec3 N = Ngeom;
    if (u_use_normal_tex == 1) {
        vec3 mapped = texture(u_normal_tex, v_uv).xyz * 2.0 - 1.0;
        // Tangent space uses dFdx/dFdy, which only run on triangles with
        // varying UVs. Mapped vector is renormalised on output.
        N = normalize(cotangent_frame(Ngeom, v_world_pos, v_uv) * mapped);
    }

    float ao = (u_use_ao_tex == 1) ? texture(u_ao_tex, v_uv).r : 1.0;
    vec3 emissive = u_emissive * u_emissive_strength;
    if (u_use_emissive_tex == 1) {
        emissive *= texture(u_emissive_tex, v_uv).rgb;
    }

    vec3 F0 = mix(vec3(0.04), albedo, metallic);
    float diffuse_scale = 1.0 - u_transmission;

    // Direct lighting: a warm key + cool fill.
    vec3 key_color  = vec3(1.00, 0.96, 0.90) * 1.10;
    vec3 fill_color = vec3(0.70, 0.78, 0.95) * 0.40;
    vec3 Lo = brdf_direct(N, V, normalize(-u_key_dir),  albedo, metallic, roughness, F0, diffuse_scale) * key_color
            + brdf_direct(N, V, normalize(-u_fill_dir), albedo, metallic, roughness, F0, diffuse_scale) * fill_color;

    // Image-based lighting from the analytic sky. The diffuse irradiance is a
    // crude average of the sky in the normal direction and straight up so the
    // result still has some directional structure but doesn't track the
    // reflection dir. The specular probe samples the actual reflection ray;
    // we fade it toward the diffuse sample as roughness rises to fake the
    // pre-filtered mip chain a real IBL would provide.
    float NdV = max(dot(N, V), 0.0);
    vec3 R = reflect(-V, N);
    vec3 spec_env = sample_sky(R);
    vec3 diff_env = 0.5 * sample_sky(N) + 0.5 * sample_sky(vec3(0.0, 1.0, 0.0));
    vec3 prefiltered = mix(spec_env, diff_env, roughness * roughness);
    vec3 F_ibl = F_Schlick_rough(NdV, F0, roughness);
    vec2 envBRDF = env_brdf_approx(NdV, roughness);
    vec3 specular_ibl = prefiltered * (F_ibl * envBRDF.x + envBRDF.y);
    vec3 kd_ibl = (1.0 - F_ibl) * (1.0 - metallic) * diffuse_scale;
    vec3 ambient = (kd_ibl * diff_env * albedo + specular_ibl) * ao;

    vec3 color = Lo + ambient + emissive;

    // AgX is the closest match for Blender's default view transform without
    // shipping a 3D LUT — the sigmoid keeps highlights from clipping while
    // preserving saturation in midtones. Linear→sRGB encode is the
    // framebuffer's job (GL_FRAMEBUFFER_SRGB).
    color = agx_tonemap(color);

    // Final alpha. For non-transmissive Blend materials this is just
    // `base.a`. For transmissive materials we ramp the alpha back up at
    // grazing angles so the Fresnel rim of a glass sphere stays visible
    // (otherwise the bright specular highlight gets multiplied to nothing
    // and the orb reads as a flat ghost). pow5 of (1 - NdV) is the same
    // shape Schlick uses for Fresnel, so the alpha rim lines up with the
    // spec rim.
    if (will_blend) {
        float fresnel_rim = pow(1.0 - max(dot(N, V), 0.0), 5.0);
        float rim_alpha = out_alpha + (1.0 - out_alpha) * fresnel_rim;
        out_alpha = mix(out_alpha, rim_alpha, u_transmission);
    }
    frag = vec4(color, out_alpha);
}
"#;

pub struct Renderer {
    program: glow::Program,
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    ebo: glow::Buffer,
    u_viewproj: Option<glow::UniformLocation>,
    u_camera_pos: Option<glow::UniformLocation>,
    u_key_dir: Option<glow::UniformLocation>,
    u_fill_dir: Option<glow::UniformLocation>,
    u_sky_top: Option<glow::UniformLocation>,
    u_sky_horizon: Option<glow::UniformLocation>,
    u_sky_ground: Option<glow::UniformLocation>,
    u_sun_dir: Option<glow::UniformLocation>,
    u_sun_color: Option<glow::UniformLocation>,
    u_base_color: Option<glow::UniformLocation>,
    u_base_color_alpha: Option<glow::UniformLocation>,
    u_metallic: Option<glow::UniformLocation>,
    u_roughness: Option<glow::UniformLocation>,
    u_emissive: Option<glow::UniformLocation>,
    u_emissive_strength: Option<glow::UniformLocation>,
    u_transmission: Option<glow::UniformLocation>,
    u_alpha_mode: Option<glow::UniformLocation>,
    u_alpha_cutoff: Option<glow::UniformLocation>,
    u_use_base_tex: Option<glow::UniformLocation>,
    u_use_mr_tex: Option<glow::UniformLocation>,
    u_use_normal_tex: Option<glow::UniformLocation>,
    u_use_ao_tex: Option<glow::UniformLocation>,
    u_use_emissive_tex: Option<glow::UniformLocation>,
    u_base_tex: Option<glow::UniformLocation>,
    u_mr_tex: Option<glow::UniformLocation>,
    u_normal_tex: Option<glow::UniformLocation>,
    u_ao_tex: Option<glow::UniformLocation>,
    u_emissive_tex: Option<glow::UniformLocation>,
    u_use_skin: Option<glow::UniformLocation>,
    u_joint_mats: Option<glow::UniformLocation>,
    /// Cached uploaded textures keyed by (resolved filesystem path, sRGB flag).
    /// Albedo and emissive maps load as sRGB; normal, MR, and AO load as
    /// linear, so the same PNG used in two roles would need two separate GL
    /// objects. Entries are tagged with the file's mtime at load time so that
    /// regenerating a PNG (e.g. via `Generate Textures`) invalidates the cache
    /// without an explicit refresh button. A `None` `texture` means we tried
    /// and failed to load — kept so we don't re-log the same failure every
    /// frame.
    texture_cache: HashMap<(PathBuf, bool), CachedTexture>,
    vbo_bytes: usize,
    ebo_bytes: usize,
    /// Per-batch draw plan owned by the renderer so the paint callback doesn't
    /// have to re-read the mesh on every frame.
    batches: Vec<DrawBatch>,
    /// Latest per-skin joint palettes. Indexed by `SkinId.0`; `DrawBatch`'s
    /// `skin_id` selects which one to upload before a draw call. Refreshed
    /// in `upload` alongside the vertex buffer.
    skin_palettes: Vec<SkinPalette>,
}

impl Renderer {
    pub fn new(gl: &glow::Context) -> anyhow::Result<Self> {
        unsafe {
            let program = compile_program(gl, VS_SRC, FS_SRC)?;

            let vao = gl
                .create_vertex_array()
                .map_err(|e| anyhow::anyhow!("create_vertex_array: {e}"))?;
            let vbo = gl
                .create_buffer()
                .map_err(|e| anyhow::anyhow!("create_buffer (vbo): {e}"))?;
            let ebo = gl
                .create_buffer()
                .map_err(|e| anyhow::anyhow!("create_buffer (ebo): {e}"))?;

            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(ebo));

            let f = std::mem::size_of::<f32>() as i32;
            let stride = (FLOATS_PER_VERTEX as i32) * f;
            // Layout matches FLOATS_PER_VERTEX: pos | normal | uv | joints | weights.
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 3, glow::FLOAT, false, stride, 0);
            gl.enable_vertex_attrib_array(1);
            gl.vertex_attrib_pointer_f32(1, 3, glow::FLOAT, false, stride, 3 * f);
            gl.enable_vertex_attrib_array(2);
            gl.vertex_attrib_pointer_f32(2, 2, glow::FLOAT, false, stride, 6 * f);
            gl.enable_vertex_attrib_array(3);
            gl.vertex_attrib_pointer_f32(3, 4, glow::FLOAT, false, stride, 8 * f);
            gl.enable_vertex_attrib_array(4);
            gl.vertex_attrib_pointer_f32(4, 4, glow::FLOAT, false, stride, 12 * f);

            gl.bind_vertex_array(None);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);
            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, None);

            let u = |n: &str| gl.get_uniform_location(program, n);
            let u_viewproj = u("u_viewproj");
            let u_camera_pos = u("u_camera_pos");
            let u_key_dir = u("u_key_dir");
            let u_fill_dir = u("u_fill_dir");
            let u_sky_top = u("u_sky_top");
            let u_sky_horizon = u("u_sky_horizon");
            let u_sky_ground = u("u_sky_ground");
            let u_sun_dir = u("u_sun_dir");
            let u_sun_color = u("u_sun_color");
            let u_base_color = u("u_base_color");
            let u_base_color_alpha = u("u_base_color_alpha");
            let u_metallic = u("u_metallic");
            let u_roughness = u("u_roughness");
            let u_emissive = u("u_emissive");
            let u_emissive_strength = u("u_emissive_strength");
            let u_transmission = u("u_transmission");
            let u_alpha_mode = u("u_alpha_mode");
            let u_alpha_cutoff = u("u_alpha_cutoff");
            let u_use_base_tex = u("u_use_base_tex");
            let u_use_mr_tex = u("u_use_mr_tex");
            let u_use_normal_tex = u("u_use_normal_tex");
            let u_use_ao_tex = u("u_use_ao_tex");
            let u_use_emissive_tex = u("u_use_emissive_tex");
            let u_base_tex = u("u_base_tex");
            let u_mr_tex = u("u_mr_tex");
            let u_normal_tex = u("u_normal_tex");
            let u_ao_tex = u("u_ao_tex");
            let u_emissive_tex = u("u_emissive_tex");
            let u_use_skin = u("u_use_skin");
            let u_joint_mats = u("u_joint_mats[0]");

            Ok(Self {
                program,
                vao,
                vbo,
                ebo,
                u_viewproj,
                u_camera_pos,
                u_key_dir,
                u_fill_dir,
                u_sky_top,
                u_sky_horizon,
                u_sky_ground,
                u_sun_dir,
                u_sun_color,
                u_base_color,
                u_base_color_alpha,
                u_metallic,
                u_roughness,
                u_emissive,
                u_emissive_strength,
                u_transmission,
                u_alpha_mode,
                u_alpha_cutoff,
                u_use_base_tex,
                u_use_mr_tex,
                u_use_normal_tex,
                u_use_ao_tex,
                u_use_emissive_tex,
                u_base_tex,
                u_mr_tex,
                u_normal_tex,
                u_ao_tex,
                u_emissive_tex,
                u_use_skin,
                u_joint_mats,
                texture_cache: HashMap::new(),
                vbo_bytes: 0,
                ebo_bytes: 0,
                batches: Vec::new(),
                skin_palettes: Vec::new(),
            })
        }
    }

    pub fn upload(&mut self, gl: &glow::Context, mesh: &FlatMesh) {
        unsafe {
            gl.bind_vertex_array(Some(self.vao));
            let vtx_bytes: &[u8] = bytes_of_f32(&mesh.vertices);
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
            if vtx_bytes.len() > self.vbo_bytes {
                gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, vtx_bytes, glow::DYNAMIC_DRAW);
                self.vbo_bytes = vtx_bytes.len();
            } else {
                gl.buffer_sub_data_u8_slice(glow::ARRAY_BUFFER, 0, vtx_bytes);
            }
            let idx_bytes: &[u8] = bytes_of_u32(&mesh.indices);
            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(self.ebo));
            if idx_bytes.len() > self.ebo_bytes {
                gl.buffer_data_u8_slice(glow::ELEMENT_ARRAY_BUFFER, idx_bytes, glow::DYNAMIC_DRAW);
                self.ebo_bytes = idx_bytes.len();
            } else {
                gl.buffer_sub_data_u8_slice(glow::ELEMENT_ARRAY_BUFFER, 0, idx_bytes);
            }
            gl.bind_vertex_array(None);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);
            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, None);
            self.batches = mesh.batches.clone();
            self.skin_palettes = mesh.skins.clone();
        }
    }

    /// Look up a texture in the cache, decoding the PNG and uploading on the
    /// first miss. `srgb` selects the GPU storage format: albedo and emissive
    /// maps need sRGB so the GL pipeline linearises them on read; metallic-
    /// roughness, normal, and AO maps store data, not colour, and must be
    /// linear. Re-loads when the file's mtime changes so regenerated PNGs
    /// (e.g. after `Generate Textures`) become visible without restarting.
    /// Returns `None` for paths that fail to load — the caller falls back to
    /// the corresponding scalar uniform.
    fn ensure_texture(
        &mut self,
        gl: &glow::Context,
        path: &Path,
        srgb: bool,
    ) -> Option<glow::Texture> {
        let mtime = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok();
        let key = (path.to_path_buf(), srgb);

        if let Some(cached) = self.texture_cache.get(&key) {
            if cached.mtime == mtime {
                return cached.texture;
            }
            // mtime moved — drop the stale GL texture before reloading.
            if let Some(old) = cached.texture {
                unsafe { gl.delete_texture(old) };
            }
        }

        let texture = match unsafe { try_load_texture(gl, path, srgb) } {
            Ok(t) => Some(t),
            Err(e) => {
                eprintln!("viewer: texture load failed for {}: {e}", path.display());
                None
            }
        };
        self.texture_cache
            .insert(key, CachedTexture { mtime, texture });
        texture
    }

    /// Upload the joint palette for `skin_id` (or turn skinning off when
    /// `None`). Uploads only the joints the skin actually declares; unused
    /// palette slots are left as whatever the driver initialised them to
    /// because shader weights for absent bones are zero.
    fn bind_skin(&self, gl: &glow::Context, skin_id: Option<u32>) {
        unsafe {
            match skin_id {
                None => {
                    if let Some(loc) = &self.u_use_skin {
                        gl.uniform_1_i32(Some(loc), 0);
                    }
                }
                Some(id) => {
                    let Some(palette) = self.skin_palettes.get(id as usize) else {
                        if let Some(loc) = &self.u_use_skin {
                            gl.uniform_1_i32(Some(loc), 0);
                        }
                        return;
                    };
                    if let Some(loc) = &self.u_joint_mats {
                        let n = palette.joint_matrices.len().min(MAX_JOINTS);
                        if n > 0 {
                            let mut flat = Vec::with_capacity(n * 16);
                            for m in &palette.joint_matrices[..n] {
                                flat.extend_from_slice(&m.to_cols_array());
                            }
                            gl.uniform_matrix_4_f32_slice(Some(loc), false, &flat);
                        }
                    }
                    if let Some(loc) = &self.u_use_skin {
                        gl.uniform_1_i32(Some(loc), 1);
                    }
                }
            }
        }
    }

    /// Drop GL textures whose source paths are no longer referenced by any
    /// active batch. Keeps memory usage bounded across scene reloads where
    /// the user has swapped texture files.
    fn evict_unused_textures(&mut self, gl: &glow::Context) {
        use std::collections::HashSet;
        let mut alive: HashSet<&PathBuf> = HashSet::new();
        for b in &self.batches {
            for slot in [
                &b.base_color_texture,
                &b.metallic_roughness_texture,
                &b.normal_texture,
                &b.occlusion_texture,
                &b.emissive_texture,
            ] {
                if let Some(p) = slot {
                    alive.insert(p);
                }
            }
        }
        let stale: Vec<(PathBuf, bool)> = self
            .texture_cache
            .keys()
            .filter(|(p, _)| !alive.contains(p))
            .cloned()
            .collect();
        for k in stale {
            if let Some(entry) = self.texture_cache.remove(&k) {
                if let Some(tex) = entry.texture {
                    unsafe { gl.delete_texture(tex) };
                }
            }
        }
    }

    pub fn draw(&mut self, gl: &glow::Context, viewproj: Mat4, camera_pos: Vec3) {
        unsafe {
            // egui_glow only clears color each frame, not depth. Without this,
            // stale depth values from previous frames cause z-fighting artifacts
            // when the camera moves. Scissor is disabled so the clear is bounded
            // to the viewport that egui_glow set for us.
            gl.disable(glow::SCISSOR_TEST);
            gl.clear_depth_f32(1.0);
            gl.clear(glow::DEPTH_BUFFER_BIT);

            if self.batches.is_empty() {
                return;
            }

            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LESS);
            gl.enable(glow::CULL_FACE);
            gl.cull_face(glow::BACK);
            gl.front_face(glow::CCW);
            // sRGB framebuffer write so the gamma curve we apply in the FS
            // lands on a perceptually correct backbuffer.
            gl.enable(glow::FRAMEBUFFER_SRGB);

            gl.use_program(Some(self.program));
            if let Some(loc) = &self.u_viewproj {
                gl.uniform_matrix_4_f32_slice(Some(loc), false, &viewproj.to_cols_array());
            }
            if let Some(loc) = &self.u_camera_pos {
                gl.uniform_3_f32(Some(loc), camera_pos.x, camera_pos.y, camera_pos.z);
            }
            if let Some(loc) = &self.u_key_dir {
                // Key from upper-front-left: lights the top-right-back of the model.
                let d = Vec3::new(-0.4, -1.0, -0.3).normalize();
                gl.uniform_3_f32(Some(loc), d.x, d.y, d.z);
            }
            if let Some(loc) = &self.u_fill_dir {
                // Weaker fill from the opposite side to soften the shadow side.
                let d = Vec3::new(0.6, -0.2, 0.5).normalize();
                gl.uniform_3_f32(Some(loc), d.x, d.y, d.z);
            }
            // Sky probe colours. Soft daylight: blue zenith, warm horizon,
            // muted green-grey ground bounce. The same values feed both diffuse
            // ambient and specular reflection in the shader. Scaled down from
            // raw sky radiance because the shader samples the dome directly
            // instead of integrating a proper diffuse irradiance probe — full
            // radiance overstated ambient on flat-colour (untextured) surfaces.
            if let Some(loc) = &self.u_sky_top {
                gl.uniform_3_f32(Some(loc), 0.33, 0.42, 0.57);
            }
            if let Some(loc) = &self.u_sky_horizon {
                gl.uniform_3_f32(Some(loc), 0.51, 0.51, 0.49);
            }
            if let Some(loc) = &self.u_sky_ground {
                gl.uniform_3_f32(Some(loc), 0.11, 0.10, 0.09);
            }
            if let Some(loc) = &self.u_sun_dir {
                // Sun direction (pointing FROM sun, like a directional light).
                let d = Vec3::new(-0.4, -1.0, -0.3).normalize();
                gl.uniform_3_f32(Some(loc), d.x, d.y, d.z);
            }
            if let Some(loc) = &self.u_sun_color {
                gl.uniform_3_f32(Some(loc), 0.66, 0.63, 0.57);
            }
            // Sampler bindings — texture units stay constant for the whole pass.
            if let Some(loc) = &self.u_base_tex {
                gl.uniform_1_i32(Some(loc), 0);
            }
            if let Some(loc) = &self.u_mr_tex {
                gl.uniform_1_i32(Some(loc), 1);
            }
            if let Some(loc) = &self.u_normal_tex {
                gl.uniform_1_i32(Some(loc), 2);
            }
            if let Some(loc) = &self.u_ao_tex {
                gl.uniform_1_i32(Some(loc), 3);
            }
            if let Some(loc) = &self.u_emissive_tex {
                gl.uniform_1_i32(Some(loc), 4);
            }
            gl.bind_vertex_array(Some(self.vao));
        }

        // Take a copy so we can call &mut self methods (ensure_texture) inside
        // the loop without aliasing self.batches.
        let batches = self.batches.clone();
        // Split into opaque/mask (any draw order, depth writes on) and blend
        // (drawn last, depth-sorted back-to-front, depth writes off). Per-
        // batch sorting on every frame is fine for preview-sized scenes —
        // we're talking dozens of batches, not thousands.
        let mut opaque_indices: Vec<usize> = Vec::with_capacity(batches.len());
        let mut blend_indices: Vec<usize> = Vec::new();
        for (i, b) in batches.iter().enumerate() {
            // Anything authored with `transmission > 0` is treated as a blend
            // batch even if its alpha_mode is Opaque: without a real
            // screen-space refraction pass, the FS will have killed the
            // diffuse term, and rendering the result as opaque (black body
            // with floating specular) reads worse than a depth-sorted blend
            // that lets the actual background show through.
            let is_blend = matches!(b.alpha_mode, AlphaMode::Blend) || b.transmission > 0.0;
            if is_blend {
                blend_indices.push(i);
            } else {
                opaque_indices.push(i);
            }
        }
        blend_indices.sort_by(|&a, &b| {
            let da = (batches[a].centroid - camera_pos).length_squared();
            let db = (batches[b].centroid - camera_pos).length_squared();
            db.partial_cmp(&da).unwrap_or(std::cmp::Ordering::Equal)
        });
        let order: Vec<usize> = opaque_indices.into_iter().chain(blend_indices).collect();

        // Tracks which skin palette is currently on the GPU, so consecutive
        // batches sharing a skin (or both rigid) avoid a redundant reupload.
        let mut current_skin: Option<Option<u32>> = None;
        // Per-batch GL state we shadow to avoid redundant calls when many
        // adjacent batches share the same alpha/cull configuration.
        let mut current_blend = false;
        let mut current_depth_write = true;
        let mut current_cull = true;
        for &idx in &order {
            let b = &batches[idx];
            if current_skin != Some(b.skin_id) {
                self.bind_skin(gl, b.skin_id);
                current_skin = Some(b.skin_id);
            }

            // glTF: `Blend` materials use src-alpha over the framebuffer with
            // depth writes off so transparents behind transparents still pass
            // the depth test against opaques. `Mask` and `Opaque` keep the
            // default (depth-write on, no blending) — `Mask` discards in the
            // FS instead, which is friendlier to early-Z than blending would
            // be.
            let want_blend = matches!(b.alpha_mode, AlphaMode::Blend) || b.transmission > 0.0;
            let want_depth_write = !want_blend;
            let want_cull = !b.double_sided;
            unsafe {
                if want_blend != current_blend {
                    if want_blend {
                        gl.enable(glow::BLEND);
                        gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
                    } else {
                        gl.disable(glow::BLEND);
                    }
                    current_blend = want_blend;
                }
                if want_depth_write != current_depth_write {
                    gl.depth_mask(want_depth_write);
                    current_depth_write = want_depth_write;
                }
                if want_cull != current_cull {
                    if want_cull {
                        gl.enable(glow::CULL_FACE);
                    } else {
                        gl.disable(glow::CULL_FACE);
                    }
                    current_cull = want_cull;
                }
            }

            // Upload material scalars and bind whatever subset of the texture
            // slots this batch declares. Texture units that aren't used keep
            // whatever was previously bound — the FS guards reads with the
            // matching `u_use_*_tex` flag, so undefined samples can't sneak
            // into the result.
            let base_tex = b
                .base_color_texture
                .as_ref()
                .and_then(|p| self.ensure_texture(gl, p, true));
            let mr_tex = b
                .metallic_roughness_texture
                .as_ref()
                .and_then(|p| self.ensure_texture(gl, p, false));
            let normal_tex = b
                .normal_texture
                .as_ref()
                .and_then(|p| self.ensure_texture(gl, p, false));
            let ao_tex = b
                .occlusion_texture
                .as_ref()
                .and_then(|p| self.ensure_texture(gl, p, false));
            let emissive_tex = b
                .emissive_texture
                .as_ref()
                .and_then(|p| self.ensure_texture(gl, p, true));

            unsafe {
                if let Some(loc) = &self.u_base_color {
                    gl.uniform_3_f32(
                        Some(loc),
                        b.base_color[0],
                        b.base_color[1],
                        b.base_color[2],
                    );
                }
                if let Some(loc) = &self.u_base_color_alpha {
                    gl.uniform_1_f32(Some(loc), b.base_color_alpha);
                }
                if let Some(loc) = &self.u_alpha_mode {
                    let mode = match b.alpha_mode {
                        AlphaMode::Opaque => 0,
                        AlphaMode::Mask => 1,
                        AlphaMode::Blend => 2,
                    };
                    gl.uniform_1_i32(Some(loc), mode);
                }
                if let Some(loc) = &self.u_alpha_cutoff {
                    gl.uniform_1_f32(Some(loc), b.alpha_cutoff);
                }
                if let Some(loc) = &self.u_metallic {
                    gl.uniform_1_f32(Some(loc), b.metallic);
                }
                if let Some(loc) = &self.u_roughness {
                    gl.uniform_1_f32(Some(loc), b.roughness);
                }
                if let Some(loc) = &self.u_emissive {
                    gl.uniform_3_f32(Some(loc), b.emissive[0], b.emissive[1], b.emissive[2]);
                }
                if let Some(loc) = &self.u_emissive_strength {
                    gl.uniform_1_f32(Some(loc), b.emissive_strength);
                }
                if let Some(loc) = &self.u_transmission {
                    gl.uniform_1_f32(Some(loc), b.transmission);
                }
                if let Some(loc) = &self.u_use_base_tex {
                    gl.uniform_1_i32(Some(loc), base_tex.is_some() as i32);
                }
                if let Some(loc) = &self.u_use_mr_tex {
                    gl.uniform_1_i32(Some(loc), mr_tex.is_some() as i32);
                }
                if let Some(loc) = &self.u_use_normal_tex {
                    gl.uniform_1_i32(Some(loc), normal_tex.is_some() as i32);
                }
                if let Some(loc) = &self.u_use_ao_tex {
                    gl.uniform_1_i32(Some(loc), ao_tex.is_some() as i32);
                }
                if let Some(loc) = &self.u_use_emissive_tex {
                    gl.uniform_1_i32(Some(loc), emissive_tex.is_some() as i32);
                }
                for (unit, tex) in [base_tex, mr_tex, normal_tex, ao_tex, emissive_tex]
                    .iter()
                    .enumerate()
                {
                    if let Some(t) = tex {
                        gl.active_texture(glow::TEXTURE0 + unit as u32);
                        gl.bind_texture(glow::TEXTURE_2D, Some(*t));
                    }
                }
                let byte_offset = (b.index_start as i32) * std::mem::size_of::<u32>() as i32;
                gl.draw_elements(
                    glow::TRIANGLES,
                    b.index_count as i32,
                    glow::UNSIGNED_INT,
                    byte_offset,
                );
            }
        }

        unsafe {
            for unit in 0..5 {
                gl.active_texture(glow::TEXTURE0 + unit as u32);
                gl.bind_texture(glow::TEXTURE_2D, None);
            }
            gl.bind_vertex_array(None);
            gl.use_program(None);
            // Restore the GL state egui expects on entry — anything we
            // toggled per-batch must be back to the renderer-default before
            // the egui pass paints over us.
            gl.disable(glow::BLEND);
            gl.depth_mask(true);
            gl.enable(glow::CULL_FACE);
            gl.disable(glow::FRAMEBUFFER_SRGB);
        }
    }

    pub fn destroy(&self, gl: &glow::Context) {
        unsafe {
            gl.delete_program(self.program);
            gl.delete_vertex_array(self.vao);
            gl.delete_buffer(self.vbo);
            gl.delete_buffer(self.ebo);
            for entry in self.texture_cache.values() {
                if let Some(t) = entry.texture {
                    gl.delete_texture(t);
                }
            }
        }
    }
}

/// One entry in [`Renderer::texture_cache`].
struct CachedTexture {
    /// File mtime captured at load time. Used to detect on-disk changes —
    /// regenerated PNGs will have a newer mtime, which forces a reload.
    mtime: Option<SystemTime>,
    /// The uploaded GL texture, or `None` if loading failed.
    texture: Option<glow::Texture>,
}

/// Read a PNG from disk, decode to 8-bit RGBA, and upload as a 2D texture.
/// `srgb` selects the internal format: `SRGB8_ALPHA8` for colour data so the
/// hardware linearises on sample, `RGBA8` for data maps (normal/MR/AO/etc.)
/// where the bytes are already linear and any conversion would corrupt them.
/// Wraps mode is REPEAT (matches the tileable-albedo intent of the textures
/// pipeline) and mips are generated for trilinear minification.
unsafe fn try_load_texture(
    gl: &glow::Context,
    path: &Path,
    srgb: bool,
) -> anyhow::Result<glow::Texture> {
    let bytes = std::fs::read(path)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
    let img = image::load_from_memory(&bytes)
        .map_err(|e| anyhow::anyhow!("decode {}: {e}", path.display()))?
        .to_rgba8();
    let (w, h) = img.dimensions();
    let pixels = img.into_raw();

    let tex = gl
        .create_texture()
        .map_err(|e| anyhow::anyhow!("create_texture: {e}"))?;
    gl.bind_texture(glow::TEXTURE_2D, Some(tex));
    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::REPEAT as i32);
    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::REPEAT as i32);
    gl.tex_parameter_i32(
        glow::TEXTURE_2D,
        glow::TEXTURE_MIN_FILTER,
        glow::LINEAR_MIPMAP_LINEAR as i32,
    );
    gl.tex_parameter_i32(
        glow::TEXTURE_2D,
        glow::TEXTURE_MAG_FILTER,
        glow::LINEAR as i32,
    );
    let internal = if srgb {
        glow::SRGB8_ALPHA8
    } else {
        glow::RGBA8
    };
    gl.tex_image_2d(
        glow::TEXTURE_2D,
        0,
        internal as i32,
        w as i32,
        h as i32,
        0,
        glow::RGBA,
        glow::UNSIGNED_BYTE,
        Some(&pixels),
    );
    gl.generate_mipmap(glow::TEXTURE_2D);
    gl.bind_texture(glow::TEXTURE_2D, None);
    Ok(tex)
}

fn bytes_of_f32(s: &[f32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(s.as_ptr() as *const u8, std::mem::size_of_val(s)) }
}

fn bytes_of_u32(s: &[u32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(s.as_ptr() as *const u8, std::mem::size_of_val(s)) }
}

unsafe fn compile_program(
    gl: &glow::Context,
    vs: &str,
    fs: &str,
) -> anyhow::Result<glow::Program> {
    let program = gl
        .create_program()
        .map_err(|e| anyhow::anyhow!("create_program: {e}"))?;
    let stages = [(glow::VERTEX_SHADER, vs), (glow::FRAGMENT_SHADER, fs)];
    let mut shaders = Vec::with_capacity(stages.len());
    for (kind, src) in stages {
        let sh = gl
            .create_shader(kind)
            .map_err(|e| anyhow::anyhow!("create_shader: {e}"))?;
        gl.shader_source(sh, src);
        gl.compile_shader(sh);
        if !gl.get_shader_compile_status(sh) {
            let log = gl.get_shader_info_log(sh);
            gl.delete_shader(sh);
            gl.delete_program(program);
            anyhow::bail!("shader compile failed: {log}");
        }
        gl.attach_shader(program, sh);
        shaders.push(sh);
    }
    gl.link_program(program);
    for sh in shaders {
        gl.detach_shader(program, sh);
        gl.delete_shader(sh);
    }
    if !gl.get_program_link_status(program) {
        let log = gl.get_program_info_log(program);
        gl.delete_program(program);
        anyhow::bail!("program link failed: {log}");
    }
    Ok(program)
}

/// Shared state between the egui main thread and the render-time paint callback.
#[derive(Default)]
pub struct ViewerState {
    pub camera: OrbitCamera,
    pub mesh: FlatMesh,
    pub mesh_dirty: bool,
    pub scene: Option<SceneGraph>,
    /// Directory of the source `.mg` file — used to resolve relative texture
    /// paths declared on materials. `None` for unsaved buffers.
    pub base_dir: Option<PathBuf>,
    /// Parallel to `scene.clips`. A `true` entry means that clip contributes
    /// to the pose this frame.
    pub clip_active: Vec<bool>,
    /// Parallel to `scene.clips`. Each clip advances its own timer, wrapped
    /// to its own duration, so clips with different durations stay in phase
    /// with themselves (not with each other).
    pub anim_times: Vec<f32>,
    pub anim_playing: bool,
}

impl ViewerState {
    fn any_active(&self) -> bool {
        self.clip_active.iter().any(|&b| b)
    }

    fn rebuild_mesh(&mut self) {
        let Some(scene) = &self.scene else {
            return;
        };
        let base_dir = self.base_dir.as_deref();
        let mesh = if self.any_active() {
            let mut locals: Vec<Transform> =
                scene.nodes.iter().map(|n| n.transform).collect();
            for (i, &active) in self.clip_active.iter().enumerate() {
                if !active {
                    continue;
                }
                if let Some(clip) = scene.clips.get(i) {
                    apply_animation(clip, self.anim_times[i], &mut locals);
                }
            }
            let worlds = world_transforms_from_locals(scene, &locals);
            flatten_with_worlds(scene, &worlds, base_dir)
        } else {
            flatten(scene, base_dir)
        };
        self.mesh = mesh;
        self.mesh_dirty = true;
    }
}

pub struct Viewer {
    pub state: Arc<Mutex<ViewerState>>,
    pub renderer: Arc<Mutex<Renderer>>,
}

impl Viewer {
    pub fn new(gl: &glow::Context) -> anyhow::Result<Self> {
        Ok(Self {
            state: Arc::new(Mutex::new(ViewerState::default())),
            renderer: Arc::new(Mutex::new(Renderer::new(gl)?)),
        })
    }

    pub fn set_scene(&self, scene: &SceneGraph, base_dir: Option<&Path>) {
        // Fit the camera using the static (unanimated) pose so the framing
        // stays stable across animation frames — using an animated mesh would
        // make the camera jump as the bounding box swings.
        let base_mesh = flatten(scene, base_dir);
        let mut st = self.state.lock().unwrap();
        st.camera.fit(&base_mesh);
        st.base_dir = base_dir.map(|p| p.to_path_buf());

        // Carry the previous active set across a recompile by matching on
        // clip name. Clips that disappear drop silently; new clips default
        // to inactive unless this is the first load (where we auto-activate
        // everything so users see animation immediately).
        let prev_active: Vec<String> = match &st.scene {
            Some(prev) => prev
                .clips
                .iter()
                .enumerate()
                .filter_map(|(i, c)| {
                    (*st.clip_active.get(i).unwrap_or(&false)).then(|| c.name.clone())
                })
                .collect(),
            None => Vec::new(),
        };
        let first_load = st.scene.is_none();

        let mut clip_active: Vec<bool> = vec![false; scene.clips.len()];
        if first_load {
            // Auto-activate every clip on first load so models with
            // animations animate without any extra user action.
            for a in &mut clip_active {
                *a = true;
            }
            st.anim_playing = !scene.clips.is_empty();
        } else {
            for (i, clip) in scene.clips.iter().enumerate() {
                if prev_active.iter().any(|n| n == &clip.name) {
                    clip_active[i] = true;
                }
            }
        }
        st.clip_active = clip_active;
        st.anim_times = vec![0.0; scene.clips.len()];
        st.scene = Some(scene.clone());
        st.rebuild_mesh();
    }

    pub fn clear(&self) {
        let mut st = self.state.lock().unwrap();
        st.mesh = FlatMesh::default();
        st.mesh_dirty = true;
        st.scene = None;
        st.base_dir = None;
        st.clip_active.clear();
        st.anim_times.clear();
    }

    pub fn clips_snapshot(&self) -> Vec<ClipSummary> {
        let st = self.state.lock().unwrap();
        st.scene
            .as_ref()
            .map(|s| {
                s.clips
                    .iter()
                    .map(|c| ClipSummary {
                        name: c.name.clone(),
                        duration: c.duration,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn active_clips(&self) -> Vec<bool> {
        self.state.lock().unwrap().clip_active.clone()
    }

    pub fn set_clip_active(&self, idx: usize, active: bool) {
        let mut st = self.state.lock().unwrap();
        if idx >= st.clip_active.len() || st.clip_active[idx] == active {
            return;
        }
        st.clip_active[idx] = active;
        // Reset this clip's timer so re-enabling it starts at t=0, which is
        // less confusing than resuming from wherever it was paused.
        st.anim_times[idx] = 0.0;
        st.rebuild_mesh();
    }

    pub fn set_all_clips_active(&self, active: bool) {
        let mut st = self.state.lock().unwrap();
        let mut changed = false;
        let n = st.clip_active.len();
        for i in 0..n {
            if st.clip_active[i] != active {
                st.clip_active[i] = active;
                st.anim_times[i] = 0.0;
                changed = true;
            }
        }
        if changed {
            st.rebuild_mesh();
        }
    }

    pub fn is_playing(&self) -> bool {
        self.state.lock().unwrap().anim_playing
    }

    pub fn set_playing(&self, playing: bool) {
        self.state.lock().unwrap().anim_playing = playing;
    }

    pub fn reset_anim_times(&self) {
        let mut st = self.state.lock().unwrap();
        for t in st.anim_times.iter_mut() {
            *t = 0.0;
        }
        st.rebuild_mesh();
    }

    /// Reset the user zoom multiplier back to 1.0 so the next `set_scene`
    /// renders the model at the uniform fit distance. Call this when loading
    /// a different file, so a previous model's zoom doesn't carry over.
    pub fn reset_view(&self) {
        let mut st = self.state.lock().unwrap();
        st.camera.zoom = 1.0;
    }

    pub fn destroy(&self, gl: &glow::Context) {
        if let Ok(r) = self.renderer.lock() {
            r.destroy(gl);
        }
    }

    /// Render the viewport into the given `ui`. Returns the allocated response
    /// so callers can layer overlays on top.
    pub fn show(&self, ui: &mut egui::Ui) -> egui::Response {
        let available = ui.available_size();
        let desired = egui::vec2(available.x.max(64.0), available.y.max(64.0));
        let (rect, response) = ui.allocate_exact_size(desired, egui::Sense::click_and_drag());

        // Drive camera from input before dispatching the paint callback.
        let dt = ui.input(|i| i.stable_dt);
        let mut needs_repaint = false;
        {
            let mut st = self.state.lock().unwrap();
            if response.dragged_by(egui::PointerButton::Primary)
                || response.dragged_by(egui::PointerButton::Middle)
            {
                let d = response.drag_delta();
                st.camera.yaw -= d.x * 0.01;
                st.camera.pitch = (st.camera.pitch - d.y * 0.01)
                    .clamp(-1.54, 1.54);
            }
            if response.hovered() {
                let scroll = ui.input(|i| i.smooth_scroll_delta.y);
                if scroll != 0.0 {
                    let factor = (1.0 - scroll * 0.0015).clamp(0.5, 1.5);
                    st.camera.zoom = (st.camera.zoom * factor).clamp(0.1, 10.0);
                }
            }

            // Advance every active clip independently (each wrapping to its
            // own duration) and re-flatten if anything moved. Paused or fully
            // static scenes do no work here.
            if st.anim_playing && st.any_active() {
                let mut advanced = false;
                let n = st.clip_active.len();
                for i in 0..n {
                    if !st.clip_active[i] {
                        continue;
                    }
                    let duration = st
                        .scene
                        .as_ref()
                        .and_then(|s| s.clips.get(i))
                        .map(|c| c.duration)
                        .unwrap_or(0.0);
                    if duration > 0.0 {
                        st.anim_times[i] = (st.anim_times[i] + dt).rem_euclid(duration);
                        advanced = true;
                    }
                }
                if advanced {
                    st.rebuild_mesh();
                    needs_repaint = true;
                }
            }
        }
        if needs_repaint {
            ui.ctx().request_repaint();
        }

        let aspect = (rect.width() / rect.height()).max(0.01);
        let state_for_paint = self.state.clone();
        let renderer_for_paint = self.renderer.clone();

        let cb = egui_glow::CallbackFn::new(move |_info, painter| {
            let gl = painter.gl();
            let mut st = state_for_paint.lock().unwrap();
            let mut rr = renderer_for_paint.lock().unwrap();
            if st.mesh_dirty {
                rr.upload(gl, &st.mesh);
                st.mesh_dirty = false;
                // Free GL textures whose paths just dropped out of the batch
                // list — usually because the user reloaded a different scene.
                rr.evict_unused_textures(gl);
            }
            let viewproj = st.camera.view_proj(aspect);
            let eye = st.camera.eye();
            rr.draw(gl, viewproj, eye);
        });

        ui.painter().add(egui::PaintCallback {
            rect,
            callback: Arc::new(cb),
        });

        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mgen_core::{Material, Mesh, Transform};

    fn quad_mesh() -> Mesh {
        let mut m = Mesh::new(
            vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [1.0, 1.0, 0.0],
                [0.0, 1.0, 0.0],
            ],
            vec![[0.0, 0.0, 1.0]; 4],
            vec![0, 1, 2, 0, 2, 3],
        );
        m.uvs = vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        m
    }

    fn material_with_texture(name: &str, path: Option<&str>) -> Material {
        let mut m = Material::new(name);
        if let Some(p) = path {
            m.base_color_texture = Some(mgen_core::TextureRef::new(PathBuf::from(p)));
        }
        m
    }

    #[test]
    fn flatten_groups_nodes_by_material_id() {
        let mut scene = SceneGraph::new();
        let m_plain = scene.add_material(material_with_texture("plain", None));
        let m_a = scene.add_material(material_with_texture("a", Some("a.png")));
        let m_b = scene.add_material(material_with_texture("b", Some("b.png")));

        // Four nodes referencing three distinct materials, with `m_a` reused
        // so we can confirm batches collapse along material id rather than
        // insertion order. (Pre-PBR this test grouped on the texture path; the
        // material-id key is finer-grained but yields the same result here
        // because each material has its own unique texture set.)
        for (i, mat) in [m_plain, m_a, m_b, m_a].iter().enumerate() {
            let id = scene.add_root(format!("n{i}"), "primitive", Transform::IDENTITY);
            scene.set_mesh(id, quad_mesh());
            scene.set_material(id, *mat);
        }

        let mesh = flatten(&scene, None);
        assert_eq!(mesh.batches.len(), 3, "one batch per material id");

        // The two `m_a` nodes must coalesce into a single batch.
        let a_batch = mesh
            .batches
            .iter()
            .find(|b| b.base_color_texture.as_deref() == Some(Path::new("a.png")))
            .expect("a.png batch present");
        // Each quad has 6 indices, two of them = 12.
        assert_eq!(a_batch.index_count, 12, "two m_a quads coalesce");

        // Plain (None texture) batch contains a single quad's worth.
        let plain_batch = mesh
            .batches
            .iter()
            .find(|b| b.base_color_texture.is_none())
            .expect("plain batch present");
        assert_eq!(plain_batch.index_count, 6);

        // Index ranges are contiguous and cover everything.
        let total: u32 = mesh.batches.iter().map(|b| b.index_count).sum();
        assert_eq!(total as usize, mesh.indices.len());

        // 4 nodes × 4 vertices × FLOATS_PER_VERTEX.
        assert_eq!(mesh.vertices.len(), 4 * 4 * FLOATS_PER_VERTEX);
    }

    #[test]
    fn flatten_skinned_mesh_emits_skin_batch_and_identity_palette_at_bind() {
        use mgen_core::{NodeId, Skin};

        let mut scene = SceneGraph::new();
        // Mesh at origin with a single bone at origin. bind-pose IBM is the
        // inverse of the bone's world (also identity), so the palette should
        // come out as identity in bind pose.
        let bone = scene.add_root("bone", "bone", Transform::IDENTITY);
        let skin_id = scene.add_skin(Skin {
            name: "skel".into(),
            joints: vec![bone],
            inverse_bind_matrices: vec![Mat4::IDENTITY.to_cols_array_2d()],
            envelopes: Vec::new(),
            skeleton_root: Some(bone),
        });

        let mesh_node = scene.add_root("mesh", "primitive", Transform::IDENTITY);
        let mut m = quad_mesh();
        m.joints = vec![[0, 0, 0, 0]; 4];
        m.weights = vec![[1.0, 0.0, 0.0, 0.0]; 4];
        scene.set_mesh(mesh_node, m);
        scene.set_skin(mesh_node, skin_id);

        let flat = flatten(&scene, None);

        // One batch tagged with the skin id.
        assert_eq!(flat.batches.len(), 1);
        assert_eq!(flat.batches[0].skin_id, Some(0));

        // Palette present and identity-ish in bind pose.
        assert_eq!(flat.skins.len(), 1);
        assert_eq!(flat.skins[0].joint_matrices.len(), 1);
        let m = flat.skins[0].joint_matrices[0];
        for (a, b) in m
            .to_cols_array()
            .iter()
            .zip(Mat4::IDENTITY.to_cols_array().iter())
        {
            assert!((a - b).abs() < 1e-5, "bind palette must be identity");
        }

        // Joint indices and weights landed at the expected stride offsets
        // (pos 3 + normal 3 + uv 2 = 8; joints start at 8, weights at 12).
        let stride = FLOATS_PER_VERTEX;
        for v in 0..4 {
            let base = v * stride;
            assert_eq!(flat.vertices[base + 8], 0.0, "joint.x");
            assert_eq!(flat.vertices[base + 12], 1.0, "weight.x");
        }

        // NodeId is used for the IBM key check below.
        let _ = NodeId(0);
    }

    #[test]
    fn flatten_skinned_vertices_do_not_bake_world_transform() {
        use mgen_core::Skin;

        // Give the mesh node a translation that would be visible if the
        // flatten pass were still baking world into skinned positions.
        let mut scene = SceneGraph::new();
        let bone = scene.add_root("bone", "bone", Transform::IDENTITY);
        let skin_id = scene.add_skin(Skin {
            name: "skel".into(),
            joints: vec![bone],
            inverse_bind_matrices: vec![Mat4::IDENTITY.to_cols_array_2d()],
            envelopes: Vec::new(),
            skeleton_root: Some(bone),
        });
        let mesh_node = scene.add_root(
            "mesh",
            "primitive",
            Transform::from_trs(Vec3::new(10.0, 0.0, 0.0), Quat::IDENTITY, Vec3::ONE),
        );
        let mut m = quad_mesh();
        m.joints = vec![[0, 0, 0, 0]; 4];
        m.weights = vec![[1.0, 0.0, 0.0, 0.0]; 4];
        scene.set_mesh(mesh_node, m);
        scene.set_skin(mesh_node, skin_id);

        let flat = flatten(&scene, None);
        // Vertex 0's x coord should still be 0.0 from the rest mesh, NOT 10.0.
        assert!(
            (flat.vertices[0]).abs() < 1e-5,
            "skinned mesh must not bake mesh-node translation into positions"
        );
    }

    #[test]
    fn flatten_resolves_relative_texture_paths_against_base_dir() {
        let mut scene = SceneGraph::new();
        let mat = scene.add_material(material_with_texture("a", Some("textures/a.png")));
        let id = scene.add_root("n", "primitive", Transform::IDENTITY);
        scene.set_mesh(id, quad_mesh());
        scene.set_material(id, mat);

        let base = PathBuf::from("/tmp/proj");
        let mesh = flatten(&scene, Some(&base));
        let textured = mesh
            .batches
            .iter()
            .find(|b| b.base_color_texture.is_some())
            .unwrap();
        assert_eq!(
            textured.base_color_texture.as_deref().unwrap(),
            Path::new("/tmp/proj/textures/a.png")
        );
    }

    #[test]
    fn flatten_emits_uvs_in_vertex_stream() {
        let mut scene = SceneGraph::new();
        let mat = scene.add_material(material_with_texture("a", Some("a.png")));
        let id = scene.add_root("n", "primitive", Transform::IDENTITY);
        scene.set_mesh(id, quad_mesh());
        scene.set_material(id, mat);

        let mesh = flatten(&scene, None);
        // UV lives at slots 6 and 7 of the new layout — should be (0.0, 1.0)
        // for the last vertex from quad_mesh.
        let stride = FLOATS_PER_VERTEX;
        let last = mesh.vertices.len() - stride;
        assert!((mesh.vertices[last + 6] - 0.0).abs() < 1e-6);
        assert!((mesh.vertices[last + 7] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn flatten_propagates_pbr_scalars_and_extra_texture_slots() {
        let mut scene = SceneGraph::new();
        let mut mat = Material::new("metal");
        mat.base_color = [0.2, 0.4, 0.6, 1.0];
        mat.metallic = 0.85;
        mat.roughness = 0.15;
        mat.emissive = [0.1, 0.2, 0.3];
        mat.emissive_strength = 2.5;
        mat.base_color_texture = Some(mgen_core::TextureRef::new(PathBuf::from("albedo.png")));
        mat.metallic_roughness_texture =
            Some(mgen_core::TextureRef::new(PathBuf::from("mr.png")));
        mat.normal_texture = Some(mgen_core::TextureRef::new(PathBuf::from("n.png")));
        mat.occlusion_texture = Some(mgen_core::TextureRef::new(PathBuf::from("ao.png")));
        mat.emissive_texture = Some(mgen_core::TextureRef::new(PathBuf::from("em.png")));
        let mid = scene.add_material(mat);
        let id = scene.add_root("n", "primitive", Transform::IDENTITY);
        scene.set_mesh(id, quad_mesh());
        scene.set_material(id, mid);

        let mesh = flatten(&scene, None);
        let b = &mesh.batches[0];
        assert_eq!(b.base_color, [0.2, 0.4, 0.6]);
        assert!((b.metallic - 0.85).abs() < 1e-6);
        assert!((b.roughness - 0.15).abs() < 1e-6);
        assert_eq!(b.emissive, [0.1, 0.2, 0.3]);
        assert!((b.emissive_strength - 2.5).abs() < 1e-6);
        assert_eq!(b.base_color_texture.as_deref(), Some(Path::new("albedo.png")));
        assert_eq!(
            b.metallic_roughness_texture.as_deref(),
            Some(Path::new("mr.png"))
        );
        assert_eq!(b.normal_texture.as_deref(), Some(Path::new("n.png")));
        assert_eq!(b.occlusion_texture.as_deref(), Some(Path::new("ao.png")));
        assert_eq!(b.emissive_texture.as_deref(), Some(Path::new("em.png")));
    }

    #[test]
    fn flatten_propagates_alpha_pipeline_and_centroid() {
        let mut scene = SceneGraph::new();
        let mut mat = Material::new("glass");
        mat.base_color = [0.4, 0.7, 0.9, 0.35];
        mat.alpha_mode = AlphaMode::Blend;
        mat.alpha_cutoff = 0.5;
        mat.double_sided = true;
        let mid = scene.add_material(mat);
        // Place the mesh node off-origin so the centroid computation has
        // something non-trivial to land on (the quad spans [0,1]² so its
        // centroid lives at (0.5, 0.5, 0.0) before the node translation).
        let id = scene.add_root(
            "n",
            "primitive",
            Transform::from_trs(Vec3::new(2.0, 0.0, 0.0), Quat::IDENTITY, Vec3::ONE),
        );
        scene.set_mesh(id, quad_mesh());
        scene.set_material(id, mid);

        let mesh = flatten(&scene, None);
        let b = &mesh.batches[0];
        assert_eq!(b.alpha_mode, AlphaMode::Blend);
        assert!((b.alpha_cutoff - 0.5).abs() < 1e-6);
        assert!(b.double_sided);
        assert!((b.base_color_alpha - 0.35).abs() < 1e-6);
        // Centroid is the per-batch AABB centre after the node transform was
        // baked into the rigid vertex stream.
        let expected = Vec3::new(2.5, 0.5, 0.0);
        assert!(
            (b.centroid - expected).length() < 1e-5,
            "got centroid {:?}, expected {:?}",
            b.centroid,
            expected
        );
    }
}
