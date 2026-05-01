use std::path::PathBuf;

use eframe::egui;

use crate::app::util::{
    delete_material_textures, ellipsize_path, gather_texture_refs, resolve_for_check,
};
use crate::app::MogenStudioApp;

impl MogenStudioApp {
    /// Thumbnails of every generated base-colour texture referenced by the
    /// active scene. Only drawn when at least one PNG exists on disk.
    /// Right-clicking a thumb opens a per-material menu for regenerating
    /// just that texture or deleting the PNG group + clearing its `*_texture`
    /// attrs from the source.
    pub(super) fn ui_texture_thumbs(&mut self, ui: &mut egui::Ui) {
        let i = self.active;
        let Some(result) = &self.files[i].last_result else {
            return;
        };
        let Some(scene) = &result.scene else {
            return;
        };

        let source_dir: Option<PathBuf> = self.files[i]
            .path
            .as_deref()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf());

        // Full slot list (not just albedo) so the per-material delete sweeps
        // every companion `*_texture` attr too. The thumbnail row itself
        // still only paints base_color — normal / MR / AO thumbnails read as
        // noise.
        let all_refs = gather_texture_refs(scene);
        let refs: Vec<(String, PathBuf)> = all_refs
            .iter()
            .filter(|(_, slot, _)| *slot == "base_color")
            .map(|(mat, _, rel)| {
                let abs = resolve_for_check(rel, source_dir.as_deref());
                (mat.clone(), abs)
            })
            .filter(|(_, abs)| abs.is_file())
            .collect();

        if refs.is_empty() {
            return;
        }

        ui.add_space(8.0);
        ui.label("Texture previews:");

        let ctx = ui.ctx().clone();
        let available = ui.available_width();
        let item_spacing = ui.spacing().item_spacing.x;
        // Pick the largest column count whose cells still hit the 48 px floor
        // at this width, capped at 4 wide so thumbs never balloon at huge
        // panel widths. Keeps the grid responsive without ever asking the
        // SidePanel for more room than it currently has — long material names
        // get truncated inside the cell instead of pushing the panel wider.
        let min_thumb = 48.0_f32;
        let max_thumb = 96.0_f32;
        let cols = (((available + item_spacing) / (min_thumb + item_spacing)).floor() as usize)
            .clamp(1, 4);
        let thumb_size = ((available - item_spacing * (cols as f32 - 1.0)) / cols as f32)
            .clamp(min_thumb, max_thumb);

        // Right-click action chosen during the loop. Applied after the UI
        // closures return so we can take a fresh `&mut self` borrow without
        // fighting `horizontal_wrapped`.
        enum ThumbAction {
            Regenerate,
            Delete,
        }
        let mut pending: Option<(String, ThumbAction)> = None;

        // Label height under each thumb: small text ≈ 14 px line + a couple
        // of px padding. Used as the cell's allocated height so the row
        // baseline stays consistent across wraps.
        let label_h = ui.text_style_height(&egui::TextStyle::Small) + 2.0;
        let cell_size = egui::vec2(thumb_size, thumb_size + label_h);

        ui.horizontal_wrapped(|ui| {
            for (mat, abs) in &refs {
                let handle = match self.thumb_handle(&ctx, abs) {
                    Some(h) => h,
                    None => continue,
                };
                // Allocate exactly `cell_size` per entry and cap inner width
                // so the truncating label can't expand its parent — that's
                // what was forcing the SidePanel wider on long material
                // names.
                ui.allocate_ui_with_layout(
                    cell_size,
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_max_width(thumb_size);
                        let resp = ui
                            .add(
                                egui::Image::new((
                                    handle.id(),
                                    egui::vec2(thumb_size, thumb_size),
                                ))
                                .rounding(4.0)
                                .sense(egui::Sense::click()),
                            )
                            .on_hover_text(format!(
                                "{}\nright-click for actions",
                                ellipsize_path(abs, 60),
                            ));
                        resp.context_menu(|ui| {
                            ui.label(egui::RichText::new(mat).strong());
                            ui.separator();
                            if ui
                                .button("Regenerate")
                                .on_hover_text(
                                    "Re-run the textures pipeline for just this material \
                                     (forces overwrite of existing PNGs)",
                                )
                                .clicked()
                            {
                                pending = Some((mat.clone(), ThumbAction::Regenerate));
                                ui.close_menu();
                            }
                            if ui
                                .button("Delete")
                                .on_hover_text(
                                    "Remove the albedo + PBR companion PNGs and clear the \
                                     *_texture attrs on this material",
                                )
                                .clicked()
                            {
                                pending = Some((mat.clone(), ThumbAction::Delete));
                                ui.close_menu();
                            }
                        });
                        ui.add(
                            egui::Label::new(egui::RichText::new(mat).small().weak())
                                .truncate(),
                        )
                        .on_hover_text(mat);
                    },
                );
            }
        });

        if let Some((material, action)) = pending {
            match action {
                ThumbAction::Regenerate => {
                    self.start_llm_textures_for_material(ctx.clone(), material);
                }
                ThumbAction::Delete => {
                    let source = self.files[i].source.clone();
                    let (new_source, status) = delete_material_textures(
                        &source,
                        source_dir.as_deref(),
                        &material,
                        &all_refs,
                    );
                    let changed = new_source != source;
                    self.tex_exists_cache.clear();
                    self.thumb_cache.clear();
                    if changed {
                        {
                            let f = &mut self.files[i];
                            f.source = new_source;
                            f.dirty = f.source != f.last_saved_source;
                            f.needs_compile = true;
                            f.last_edit_at = Some(std::time::Instant::now());
                        }
                        // Texture-cleanup deletes PNGs from disk as a side
                        // effect, so this edit is non-undoable like the
                        // LLM completions. Break the coalesce chain so a
                        // subsequent gizmo / inspector edit doesn't merge
                        // into a pre-cleanup stack entry.
                        self.break_undo_chain(i);
                        self.compile_active();
                    }
                    self.active_mut().status = status;
                }
            }
        }
    }

    /// Look up or lazily upload a thumbnail for `abs`. `None` when the file
    /// isn't a readable PNG.
    fn thumb_handle(
        &mut self,
        ctx: &egui::Context,
        abs: &std::path::Path,
    ) -> Option<egui::TextureHandle> {
        use crate::app::types::ThumbEntry;
        let mtime = std::fs::metadata(abs).ok().and_then(|m| m.modified().ok());
        if let Some(entry) = self.thumb_cache.get(abs) {
            if entry.mtime == mtime {
                return Some(entry.handle.clone());
            }
        }
        let bytes = std::fs::read(abs).ok()?;
        let img = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
            .ok()?
            .to_rgba8();
        // Downscale oversized albedos before upload — a 2048² RGBA8 texture
        // is 16 MB in VRAM per material, and thumbnails render at < 96 px.
        let image_rgba = if img.width().max(img.height()) > 128 {
            image::imageops::resize(&img, 128, 128, image::imageops::FilterType::Triangle)
        } else {
            img
        };
        let (w2, h2) = (image_rgba.width() as usize, image_rgba.height() as usize);
        let handle = ctx.load_texture(
            format!("thumb:{}", abs.display()),
            egui::ColorImage::from_rgba_unmultiplied([w2, h2], image_rgba.as_raw()),
            egui::TextureOptions::LINEAR,
        );
        self.thumb_cache
            .insert(abs.to_path_buf(), ThumbEntry { handle: handle.clone(), mtime });
        Some(handle)
    }
}
