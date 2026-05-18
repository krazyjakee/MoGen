//! LOD / imposter preview support.
//!
//! Both previews reuse `mogen-export`'s own pipeline so the viewport shows
//! the exact artifacts a `bundle_lods_and_imposter` build would ship:
//!
//! - **LOD**: [`MogenStudioApp::viewer_scene`] swaps each mesh for its
//!   simplified stage via `mogen_export::scene_with_lod` before the scene
//!   reaches the viewer. `last_result.scene` is left untouched so the
//!   inspector, summary, and export still operate on full-detail geometry.
//! - **Imposter**: the bake runs on the viewer's live `glow::Context`
//!   (via `Viewer::submit_imposter_request`) instead of the headless CLI
//!   path — eframe owns the only winit `EventLoop` in the process, so
//!   `mogen_render::headless::with_gl_context` can't bring up a second
//!   one. Studio polls the resulting atlas off the viewer one paint later.

use std::sync::Arc;

use eframe::egui;
use mogen_core::SceneGraph;

use crate::pipeline::Stage;

use super::MogenStudioApp;

/// Viewport preview stage. `Full` renders the compiled geometry untouched;
/// the LOD stages render exactly what the export bundles at that stage
/// (same simplifier, same per-mesh skip/fallback rules); `Imposter` hides
/// the scene entirely and shows a single billboard quad rendered from the
/// yaw-grid atlas the export embeds, so orbiting the camera demonstrates
/// the cell-swap behaviour the godot-mog shader applies at runtime.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum PreviewLod {
    #[default]
    Full,
    Lod1,
    Lod2,
    Lod3,
    Imposter,
}

pub const PREVIEW_LODS: [PreviewLod; 5] = [
    PreviewLod::Full,
    PreviewLod::Lod1,
    PreviewLod::Lod2,
    PreviewLod::Lod3,
    PreviewLod::Imposter,
];

impl PreviewLod {
    /// 1-based LOD stage for `mogen_export::scene_with_lod`; `Full` and
    /// `Imposter` both return 0 (the imposter mode doesn't run the LOD
    /// simplifier — it bakes the atlas off the full scene and renders a
    /// billboard instead).
    pub fn stage(self) -> usize {
        match self {
            PreviewLod::Full | PreviewLod::Imposter => 0,
            PreviewLod::Lod1 => 1,
            PreviewLod::Lod2 => 2,
            PreviewLod::Lod3 => 3,
        }
    }

    /// True when this mode replaces the scene draw with the imposter
    /// billboard overlay. Used by the viewer paint callback to gate the
    /// main mesh / overlay draws.
    pub fn is_imposter(self) -> bool {
        matches!(self, PreviewLod::Imposter)
    }

    pub fn label(self) -> &'static str {
        match self {
            PreviewLod::Full => "Full detail (LOD0)",
            PreviewLod::Lod1 => "LOD1 (≈50% triangles)",
            PreviewLod::Lod2 => "LOD2 (≈25% triangles)",
            PreviewLod::Lod3 => "LOD3 (≈12% triangles)",
            PreviewLod::Imposter => "Imposter (billboard)",
        }
    }

    /// Compact label for the floating viewport bar.
    pub fn short(self) -> &'static str {
        match self {
            PreviewLod::Full => "Full",
            PreviewLod::Lod1 => "LOD1",
            PreviewLod::Lod2 => "LOD2",
            PreviewLod::Lod3 => "LOD3",
            PreviewLod::Imposter => "Imposter",
        }
    }
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
    ///
    /// `PreviewLod::Imposter` is special-cased: instead of swapping the
    /// rendered geometry, we flip a flag on the viewer that replaces the
    /// mesh draw with a billboard sampled from the baked atlas. The bake
    /// itself runs lazily in the paint callback when the flag flips dirty.
    pub(super) fn set_preview_lod(&mut self, lod: PreviewLod) {
        if self.preview_lod == lod {
            return;
        }
        let prev = self.preview_lod;
        self.preview_lod = lod;
        self.compile_active();
        // `compile_active` already syncs the imposter view through
        // `set_scene` in the success branch; if we just left imposter mode
        // and the active scene didn't compile cleanly, the viewer still
        // needs the flag flipped so the cached billboard is freed.
        if prev.is_imposter() && !lod.is_imposter() {
            self.viewer.set_imposter_view(false, None);
        }
    }

    /// Queue an imposter bake on the viewer. No-op (with a status message)
    /// when the active scene hasn't compiled cleanly — the bake needs real
    /// geometry, exactly like the export path. The actual render runs on
    /// the viewer's GL context next paint; `poll_imposter_preview` picks
    /// up the result.
    pub(super) fn start_imposter_preview(&mut self, ctx: &egui::Context) {
        self.show_imposter = true;
        if self.imposter_preview_pending {
            return;
        }
        self.compile_active();
        let i = self.active;
        let scene = match &self.files[i].last_result {
            Some(r) if r.stage == Stage::Ok => r.scene.clone(),
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
        self.imposter_preview_pending = true;
        // The bake runs synchronously inside the next paint callback, so
        // its outcome can be ready by the time the *next* update() polls.
        // Without this gate, the polling drains the outcome before the
        // modal renders, and the user never sees the "baking…" spinner —
        // making a same-scene Re-bake look like the button did nothing.
        self.imposter_preview_just_submitted = true;
        let base_dir = self.files[i]
            .path
            .as_deref()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf());
        self.viewer
            .submit_imposter_request(crate::viewer::ImposterRequest {
                scene,
                cell_size: IMPOSTER_PREVIEW_CELL_SIZE,
                view_count: IMPOSTER_PREVIEW_VIEW_COUNT,
                pitch: IMPOSTER_PREVIEW_PITCH,
                base_dir,
            });
        ctx.request_repaint();
    }

    /// Drain a finished imposter bake from the viewer and upload it as an
    /// egui texture. Skips when the outcome belongs to an in-flight export
    /// (the export path owns the outcome until it's spawned the build).
    pub(super) fn poll_imposter_preview(&mut self, ctx: &egui::Context) {
        if !self.imposter_preview_pending {
            return;
        }
        // Export takes precedence — its handler drains the outcome before
        // we get here. If both flags are set (shouldn't normally happen),
        // wait until the export path has cleared its slot.
        if self.imposter_export_pending.is_some() {
            return;
        }
        // Hold off one update tick so the modal renders the in-flight
        // spinner at least once. Without this, a fast bake completes
        // between `submit_imposter_request` and the very next poll, and
        // the modal swaps the old atlas for the new one with no visible
        // feedback — looks like the button did nothing on a same-scene
        // re-bake. Request another repaint so the actual drain still
        // happens promptly (otherwise we'd wait for the heartbeat tick).
        if self.imposter_preview_just_submitted {
            self.imposter_preview_just_submitted = false;
            ctx.request_repaint();
            return;
        }
        let Some(outcome) = self.viewer.take_imposter_outcome() else {
            return;
        };
        self.imposter_preview_pending = false;
        match outcome {
            Ok(atlas) => {
                let img = egui::ColorImage::from_rgba_unmultiplied(
                    [atlas.width as usize, atlas.height as usize],
                    &atlas.rgba,
                );
                let texture = ctx.load_texture(
                    "mogen-imposter-preview",
                    img,
                    egui::TextureOptions::LINEAR,
                );
                self.imposter_preview = Some(ImposterPreview {
                    texture,
                    width: atlas.width,
                    height: atlas.height,
                    view_count: atlas.view_count,
                    cell_size: atlas.cell_size,
                });
            }
            Err(e) => self.imposter_err = Some(e),
        }
    }
}

/// Bake parameters used by the imposter-preview modal. Mirrors the
/// constants `mogen_export::imposter` uses for the bundled-LODs export so
/// the preview shows the exact artifact the build embeds. Keep these in
/// sync with the writer-side `CELL_SIZE` / `VIEW_COUNT` / `PITCH_RADIANS`
/// — diverging would mean the preview shows a different atlas than the
/// shipped GLB.
pub(super) const IMPOSTER_PREVIEW_CELL_SIZE: u32 = 512;
pub(super) const IMPOSTER_PREVIEW_VIEW_COUNT: u32 = 8;
pub(super) const IMPOSTER_PREVIEW_PITCH: f32 = 0.5;
