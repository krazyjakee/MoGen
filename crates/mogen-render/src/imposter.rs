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
pub struct ImposterAtlas {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub view_count: u32,
    pub cell_size: u32,
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
pub fn bake_yaw_atlas(
    scene: &SceneGraph,
    opts: &ImposterOptions,
) -> anyhow::Result<ImposterAtlas> {
    let mesh = flatten(scene, opts.base_dir.as_deref());
    let center = mesh.center;
    let radius = mesh.radius.max(0.001);

    let cell = opts.cell_size.max(1);
    let views = opts.view_count.max(1);
    let atlas_w = cell * views;
    let atlas_h = cell;

    let pitch = opts.pitch;
    let mesh_for_closure = mesh;

    let atlas_rgba: Vec<u8> = with_gl_context(move |gl| -> anyhow::Result<Vec<u8>> {
        let mut renderer = Renderer::new(gl)?;
        renderer.upload(gl, &mesh_for_closure);

        let mut atlas = vec![0u8; (atlas_w as usize) * (atlas_h as usize) * 4];
        let stride_atlas = (atlas_w as usize) * 4;
        let stride_cell = (cell as usize) * 4;

        for v in 0..views {
            let yaw = (v as f32 / views as f32) * std::f32::consts::TAU;
            let cam = OrbitCamera {
                yaw,
                pitch,
                fit_distance: radius * 2.8,
                zoom: 1.0,
                target: center,
            };
            let viewproj = cam.view_proj(1.0);
            let eye = cam.eye();
            // Transparent background — model fragments overwrite with their
            // own alpha (1.0 for opaque materials, < 1 for blend/mask).
            let cell_pixels = renderer.render_to_pixels(gl, cell, viewproj, eye, [0, 0, 0, 0])?;
            let col_byte_offset = (v as usize) * stride_cell;
            for row in 0..cell as usize {
                let src = row * stride_cell;
                let dst = row * stride_atlas + col_byte_offset;
                atlas[dst..dst + stride_cell]
                    .copy_from_slice(&cell_pixels[src..src + stride_cell]);
            }
        }
        Ok(atlas)
    })?;

    Ok(ImposterAtlas {
        rgba: atlas_rgba,
        width: atlas_w,
        height: atlas_h,
        view_count: views,
        cell_size: cell,
    })
}
