//! LOD / imposter preview support.
//!
//! Both previews reuse `mogen-export`'s own pipeline so the viewport shows
//! the exact artifacts a `bundle_lods_and_imposter` build would ship:
//!
//! - **LOD**: [`MogenStudioApp::viewer_scene`] swaps each mesh for its
//!   simplified stage via `mogen_export::scene_with_lod` before the scene
//!   reaches the viewer. `last_result.scene` is left untouched so the
//!   inspector, summary, and export still operate on full-detail geometry.
//! - **Imposter**: a worker thread bakes the spritesheet with
//!   `mogen_export::bake_scene_imposter` (the same headless yaw-grid bake the
//!   export embeds) and the result is shown as an image in a modal.

use std::sync::Arc;

use eframe::egui;
use mogen_core::SceneGraph;

use crate::pipeline::Stage;

use super::MogenStudioApp;

/// Viewport LOD-detail preview level. `Full` renders the compiled geometry
/// untouched; the LOD stages render exactly what the export bundles at that
/// stage (same simplifier, same per-mesh skip/fallback rules).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum PreviewLod {
    #[default]
    Full,
    Lod1,
    Lod2,
    Lod3,
}

pub const PREVIEW_LODS: [PreviewLod; 4] = [
    PreviewLod::Full,
    PreviewLod::Lod1,
    PreviewLod::Lod2,
    PreviewLod::Lod3,
];

impl PreviewLod {
    /// 1-based LOD stage for `mogen_export::scene_with_lod`; `Full` → 0.
    pub fn stage(self) -> usize {
        match self {
            PreviewLod::Full => 0,
            PreviewLod::Lod1 => 1,
            PreviewLod::Lod2 => 2,
            PreviewLod::Lod3 => 3,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PreviewLod::Full => "Full detail (LOD0)",
            PreviewLod::Lod1 => "LOD1 (≈50% triangles)",
            PreviewLod::Lod2 => "LOD2 (≈25% triangles)",
            PreviewLod::Lod3 => "LOD3 (≈12% triangles)",
        }
    }

    /// Compact label for the floating viewport bar.
    pub fn short(self) -> &'static str {
        match self {
            PreviewLod::Full => "Full",
            PreviewLod::Lod1 => "LOD1",
            PreviewLod::Lod2 => "LOD2",
            PreviewLod::Lod3 => "LOD3",
        }
    }
}

/// Raw bake handed back from the worker thread. RGBA is the spritesheet
/// straight from `mogen_export::bake_scene_imposter` (top-left origin), ready
/// for `egui::ColorImage::from_rgba_unmultiplied`.
pub struct ImposterBake {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub view_count: u32,
    pub cell_size: u32,
}

/// GPU-uploaded imposter atlas plus the bake parameters, shown by the
/// imposter-preview modal.
pub struct ImposterPreview {
    pub texture: egui::TextureHandle,
    pub width: u32,
    pub height: u32,
    pub view_count: u32,
    pub cell_size: u32,
}

impl MogenStudioApp {
    /// The scene the viewer should display for `scene`, honouring the active
    /// LOD preview. Full detail (the common case) just clones the shared
    /// `Arc` — no geometry work. A LOD stage runs the export simplifier on a
    /// detached copy so `last_result.scene` (inspector / export source of
    /// truth) keeps its original geometry.
    pub(super) fn viewer_scene(&self, scene: &Arc<SceneGraph>) -> Arc<SceneGraph> {
        let stage = self.preview_lod.stage();
        if stage == 0 {
            return scene.clone();
        }
        Arc::new(mogen_export::scene_with_lod(scene, stage))
    }

    /// Switch the viewport LOD preview and re-feed the active scene so the
    /// change lands immediately. Recompiling (rather than poking the viewer
    /// directly) keeps the LOD swap on the one code path that hands scenes to
    /// the viewer, so animation/selection state stays consistent.
    pub(super) fn set_preview_lod(&mut self, lod: PreviewLod) {
        if self.preview_lod == lod {
            return;
        }
        self.preview_lod = lod;
        self.compile_active();
    }

    /// Bake the scene-wide imposter spritesheet off-thread. No-op (with a
    /// status message) when the active scene hasn't compiled cleanly — the
    /// bake needs real geometry, exactly like the export path.
    pub(super) fn start_imposter_preview(&mut self, ctx: &egui::Context) {
        self.show_imposter = true;
        if self.imposter_rx.is_some() {
            return;
        }
        self.compile_active();
        let i = self.active;
        let scene = match &self.files[i].last_result {
            Some(r) if r.stage == Stage::Ok => r
                .scene
                .as_ref()
                .map(|s| (**s).clone()),
            _ => None,
        };
        let Some(scene) = scene else {
            self.imposter_preview = None;
            self.imposter_err =
                Some("fix scene errors first — the imposter bake needs valid geometry".into());
            return;
        };
        self.imposter_err = None;
        self.imposter_preview = None;
        let (tx, rx) = std::sync::mpsc::channel();
        self.imposter_rx = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let msg = match mogen_export::bake_scene_imposter(&scene) {
                Ok(atlas) => Ok(ImposterBake {
                    rgba: atlas.rgba,
                    width: atlas.width,
                    height: atlas.height,
                    view_count: atlas.view_count,
                    cell_size: atlas.cell_size,
                }),
                Err(e) => Err(format!("{e:#}")),
            };
            let _ = tx.send(msg);
            ctx.request_repaint();
        });
    }

    /// Drain a finished imposter bake and upload it as an egui texture.
    pub(super) fn poll_imposter_preview(&mut self, ctx: &egui::Context) {
        let msg = match self.imposter_rx.as_ref() {
            Some(rx) => match rx.try_recv() {
                Ok(m) => m,
                Err(_) => return,
            },
            None => return,
        };
        self.imposter_rx = None;
        match msg {
            Ok(bake) => {
                let img = egui::ColorImage::from_rgba_unmultiplied(
                    [bake.width as usize, bake.height as usize],
                    &bake.rgba,
                );
                let texture = ctx.load_texture(
                    "mogen-imposter-preview",
                    img,
                    egui::TextureOptions::LINEAR,
                );
                self.imposter_preview = Some(ImposterPreview {
                    texture,
                    width: bake.width,
                    height: bake.height,
                    view_count: bake.view_count,
                    cell_size: bake.cell_size,
                });
            }
            Err(e) => self.imposter_err = Some(e),
        }
    }
}
