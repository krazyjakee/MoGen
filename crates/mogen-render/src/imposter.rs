//! Yaw-grid imposter atlas baker.
//!
//! Renders a scene from `N` equally-spaced yaw angles into one `cell_size × N`
//! wide spritesheet — one row of cells, each cell a `cell_size`-square render
//! of the model from that yaw with a transparent background. The atlas comes
//! back as raw RGBA bytes (top-left origin); the caller (typically
//! `mogen-export`) PNG-encodes it and embeds it as the billboard texture.
//!
//! Plain glTF can only show one cell of this atlas at a time without a
//! sampling shader — the companion `godot-mog` runtime applies a shader that
//! picks the correct cell from the camera's view direction. With no shader,
//! the texture renders "as authored" on a quad: it'll show all angles tiled,
//! which looks wrong on its own but is fully spec-compliant glTF and lets
//! the asset open in any viewer.

use std::path::PathBuf;

use mogen_core::SceneGraph;

use crate::camera::OrbitCamera;
use crate::flatten::flatten;
use crate::headless::with_gl_context;
use crate::renderer::Renderer;

/// Knobs for [`bake_yaw_atlas`]. The defaults are tuned to the typical
/// MoGen prop / building output: 8 yaws gives the godot-mog sampler a
/// 45°-spaced grid (one cell every other compass octant), and 256² cells
/// stay legible without bloating the BIN chunk.
#[derive(Clone, Debug)]
pub struct ImposterOptions {
    /// Edge length of one cell in pixels.
    pub cell_size: u32,
    /// Number of yaw views (cells) packed into the atlas, left-to-right.
    pub view_count: u32,
    /// Camera pitch in radians, shared across every cell. A slight downward
    /// gaze (~28°) frames ground-standing assets well — matches what the
    /// Studio uses for its 3/4 thumbnail.
    pub pitch: f32,
    /// Directory the source `.mog` lives in, used to resolve relative
    /// material `*_texture` paths. `None` makes the renderer fall back to
    /// material PBR factors only — fine for the imposter since cell colour
    /// at distance is dominated by silhouette + albedo factor.
    pub base_dir: Option<PathBuf>,
}

impl Default for ImposterOptions {
    fn default() -> Self {
        Self {
            cell_size: 256,
            view_count: 8,
            pitch: 0.5,
            base_dir: None,
        }
    }
}

/// One baked imposter spritesheet. `rgba` is `width × height` top-left
/// origin pixel data, ready to feed into `image::ImageEncoder::write_image`
/// as `Rgba8`.
///
/// `center` and `half_width` / `half_height` describe the world-space
/// placement and size of the billboard quad the consumer should render
/// the atlas onto. The quad matches the *model's* AABB so the imposter
/// occupies the same volume as the original mesh — no quad center
/// floating metres above the model when the silhouette is short and wide.
///
/// `uv_y_top` / `uv_y_bottom` are the V-coordinate bounds (in atlas
/// texture space) of the silhouette inside one cell, computed so that
/// sampling `[uv_y_top, uv_y_bottom]` on the quad puts the silhouette's
/// base on `aabb_min.y` and the silhouette's apex on `aabb_max.y` in
/// world space. The runtime shader linearly remaps the quad's UV.y from
/// `[0, 1]` onto this sub-range so the cell's transparent margins above
/// and below the model are cropped out — gives a tight 1:1 silhouette on
/// the quad without needing per-cell UV data.
pub struct ImposterAtlas {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub view_count: u32,
    pub cell_size: u32,
    pub center: [f32; 3],
    pub half_width: f32,
    pub half_height: f32,
    pub uv_y_top: f32,
    pub uv_y_bottom: f32,
}

/// Render `scene` from `opts.view_count` yaws into a single wide atlas.
///
/// Opens one headless GL context, uploads the flattened scene once, and
/// renders into a freshly-bound MSAA FBO per cell — this is the same render
/// path as [`crate::headless::render_thumbnail`], just driven at a smaller
/// per-cell size and stitched into a wider buffer.
///
/// Background is cleared to `(0, 0, 0, 0)` so cells with no model coverage
/// stay transparent; the glTF importer / godot-mog shader can then alpha-
/// test or alpha-blend the billboard without picking up a coloured border.
///
/// This is the CLI entry point: it owns the headless GL setup. Callers that
/// already have a live `glow::Context` (Studio's egui paint callback) must
/// use [`bake_yaw_atlas_on_gl`] instead — winit forbids creating a second
/// `EventLoop` in a process that already has one (eframe owns the first).
pub fn bake_yaw_atlas(
    scene: &SceneGraph,
    opts: &ImposterOptions,
) -> anyhow::Result<ImposterAtlas> {
    let mesh = flatten(scene, opts.base_dir.as_deref());
    let cell = opts.cell_size.max(1);
    let views = opts.view_count.max(1);
    let pitch = opts.pitch;
    let mesh_for_closure = mesh;

    with_gl_context(move |gl| bake_flat_mesh(gl, &mesh_for_closure, cell, views, pitch))
}

/// Bake variant that reuses a caller-supplied `glow::Context`. Used by the
/// Studio so the bake runs on the eframe paint thread's existing GL state
/// instead of trying to spin up a second winit event loop.
///
/// The renderer instance is created and destroyed inside this call; nothing
/// from the caller's render state is mutated (the underlying
/// `render_to_pixels` saves and restores the bound FBO + viewport).
pub fn bake_yaw_atlas_on_gl(
    gl: &glow::Context,
    scene: &SceneGraph,
    opts: &ImposterOptions,
) -> anyhow::Result<ImposterAtlas> {
    let mesh = flatten(scene, opts.base_dir.as_deref());
    let cell = opts.cell_size.max(1);
    let views = opts.view_count.max(1);
    bake_flat_mesh(gl, &mesh, cell, views, opts.pitch)
}

/// Walk every vertex in `mesh` and find the smallest and largest NDC y
/// the silhouette occupies at yaw=0 with the given bake camera. We use
/// this instead of projecting AABB corners because for sparse shapes
/// (e.g. a tree with a thin trunk and a wide canopy) the corners of
/// the bounding box live in empty space — the silhouette is much
/// tighter than the AABB projection suggests. Without this, the
/// imposter quad's UV crop reaches into transparent cell rows and the
/// rendered silhouette ends up floating above the model's actual base.
fn silhouette_clip_extent_yaw0(
    mesh: &crate::flatten::FlatMesh,
    camera_target: glam::Vec3,
    fit_distance: f32,
    alpha: f32, // cos(pitch)
    beta: f32,  // sin(pitch)
    cot_half_fov: f32,
) -> (f32, f32) {
    use crate::flatten::FLOATS_PER_VERTEX;
    let stride = FLOATS_PER_VERTEX;
    let count = mesh.vertices.len() / stride;
    if count == 0 {
        return (-1.0, 1.0);
    }
    let mut y_clip_min = f32::INFINITY;
    let mut y_clip_max = f32::NEG_INFINITY;
    for i in 0..count {
        let base = i * stride;
        let px = mesh.vertices[base];
        let py = mesh.vertices[base + 1];
        let pz = mesh.vertices[base + 2];
        let _ = px; // x doesn't affect y_view at yaw=0
        // Working in view-relative coords: y_view at yaw=0 only depends
        // on (py - camera_target.y) and (pz - camera_target.z). The
        // camera height/depth contribution from `fit_distance * sin(p)`
        // and `fit_distance * cos(p)` cancels symmetrically (see the
        // closed-form derivation in `bake_flat_mesh`).
        let dy = py - camera_target.y;
        let dz = pz - camera_target.z;
        let y_view = alpha * dy - beta * dz;
        let depth = fit_distance - beta * dy - alpha * dz;
        if depth <= 0.001 {
            continue;
        }
        let y_clip = y_view * cot_half_fov / depth;
        if y_clip < y_clip_min {
            y_clip_min = y_clip;
        }
        if y_clip > y_clip_max {
            y_clip_max = y_clip;
        }
    }
    if !y_clip_min.is_finite() || !y_clip_max.is_finite() {
        return (-1.0, 1.0);
    }
    (y_clip_min, y_clip_max)
}

fn bake_flat_mesh(
    gl: &glow::Context,
    mesh: &crate::flatten::FlatMesh,
    cell: u32,
    views: u32,
    pitch: f32,
) -> anyhow::Result<ImposterAtlas> {
    // Frame the bake's perspective camera so the AABB corners stay
    // inside NDC ±1 for the worst-case yaw (yaw=45 for a yaw-grid bake,
    // where the AABB diagonal aligns with the view direction). The
    // front-bottom corner is *closer* to the camera than the AABB
    // centre, so under perspective it projects further toward the
    // bottom of the image than a centroid-only estimate; the
    // asymmetry term below accounts for that without clipping tall
    // silhouettes.
    let alpha = pitch.cos();
    let beta = pitch.sin();
    // cot(22.5°) — half the renderer's 45° vertical FOV.
    let cot_half_fov = 1.0 / 0.41421356_f32;
    let aabb_size = mesh.aabb_max - mesh.aabb_min;
    let h = aabb_size.y.max(0.001);
    let horiz_radius = (aabb_size.x * 0.5).hypot(aabb_size.z * 0.5).max(0.001);
    // Half the apparent vertical extent of the AABB in view space at
    // the worst-case yaw — the +β·horiz term is the silhouette depth
    // revealed by pitch tilting the camera.
    let h_half_proj = alpha * h * 0.5 + beta * horiz_radius;
    let asymmetry = (alpha * horiz_radius - beta * h * 0.5).abs();
    let d_vertical = h_half_proj * cot_half_fov + asymmetry;
    let d_horizontal = horiz_radius * cot_half_fov;
    let d_min = d_vertical.max(d_horizontal);
    // 4 % margin keeps anti-aliased silhouette pixels off the cell
    // border, where CLAMP_TO_EDGE sampling in the runtime shader would
    // otherwise show a hard cutoff.
    let fit_distance = (d_min * 1.04).max(0.001);
    let aabb_centre = (mesh.aabb_min + mesh.aabb_max) * 0.5;
    let camera_target = aabb_centre;
    // Quad matches the model's own AABB so the imposter occupies the
    // same volume as the original mesh — base on the ground, apex at
    // the canopy. The cell is square-pixel and contains the silhouette
    // padded by however much space the worst-case yaw needed; we crop
    // those margins out via the returned UV bounds, so the silhouette
    // stretches exactly across the quad in world coords.
    //
    // Horizontal half: `horiz_radius` so the quad accommodates the
    // worst-yaw silhouette width. The runtime billboard rotates the
    // quad around Y so this single width works for any view direction.
    let quad_half_width = horiz_radius;
    let quad_half_height = h * 0.5;
    let quad_centre = aabb_centre;
    // UV bounds that map the quad's [0, 1] V range onto the cell's
    // silhouette extent. We need the *actual* silhouette extent in the
    // yaw=0 cell — using the AABB corners over-estimates it badly for
    // models with sparse shapes (e.g. a tree, whose AABB bottom-corners
    // sit in empty space at the canopy radius × ground, even though
    // only a thin trunk lives down there). Iterate the mesh's real
    // vertex positions and find the actual min/max projected NDC y at
    // yaw=0; the resulting UV crop matches the visible silhouette so
    // the quad's bottom edge lands on the lowest *drawn* pixel
    // (= model's true base) rather than an empty AABB corner.
    let (y_clip_min_yaw0, y_clip_max_yaw0) = silhouette_clip_extent_yaw0(
        mesh,
        camera_target,
        fit_distance,
        alpha,
        beta,
        cot_half_fov,
    );
    // Convert NDC y to texture V coords. The uploaded atlas is
    // top-left origin, so the first buffer row (top of image,
    // NDC y = +1) lands at GL's V = 0 (bottom of texture); V increases
    // downward in atlas pixel terms. NDC +1 → V 0, NDC -1 → V 1.
    let uv_y_top = ((1.0 - y_clip_max_yaw0) * 0.5).clamp(0.0, 1.0);
    let uv_y_bottom = ((1.0 - y_clip_min_yaw0) * 0.5).clamp(0.0, 1.0);

    let atlas_w = cell * views;
    let atlas_h = cell;

    let mut renderer = Renderer::new(gl)?;
    renderer.upload(gl, mesh);

    let mut atlas = vec![0u8; (atlas_w as usize) * (atlas_h as usize) * 4];
    let stride_atlas = (atlas_w as usize) * 4;
    let stride_cell = (cell as usize) * 4;

    for v in 0..views {
        let yaw = (v as f32 / views as f32) * std::f32::consts::TAU;
        let cam = OrbitCamera {
            yaw,
            pitch,
            fit_distance,
            zoom: 1.0,
            target: camera_target,
        };
        let viewproj = cam.view_proj(1.0);
        let eye = cam.eye();
        let cell_pixels = renderer.render_to_pixels(gl, cell, viewproj, eye, [0, 0, 0, 0])?;
        let col_byte_offset = (v as usize) * stride_cell;
        for row in 0..cell as usize {
            let src = row * stride_cell;
            let dst = row * stride_atlas + col_byte_offset;
            atlas[dst..dst + stride_cell]
                .copy_from_slice(&cell_pixels[src..src + stride_cell]);
        }
    }

    renderer.destroy(gl);

    Ok(ImposterAtlas {
        rgba: atlas,
        width: atlas_w,
        height: atlas_h,
        view_count: views,
        cell_size: cell,
        center: quad_centre.to_array(),
        half_width: quad_half_width,
        half_height: quad_half_height,
        uv_y_top,
        uv_y_bottom,
    })
}
