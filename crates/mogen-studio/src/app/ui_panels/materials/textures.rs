use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use eframe::egui;

use crate::app::types::{ThumbEntry, TEX_EXISTS_TTL};
use crate::app::util::{ellipsize_path, resolve_for_check};
use crate::app::MogenStudioApp;

use super::TexAction;

impl MogenStudioApp {
    /// Render the textures section for a single material: albedo thumbnail,
    /// generate/delete/reveal action column, slot roster with ✓/✗ existence
    /// marks, and the per-slot manual path picker. A click on one of the
    /// action buttons writes `pending_tex_action`; the caller dispatches it
    /// after the per-material loop. Per-slot Browse / Clear actions write
    /// `pending_tex_path` for the same reason.
    pub(in crate::app::ui_panels::materials) fn material_textures_section(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        mat: &mogen_core::Material,
        mat_slots: &[(String, &'static str, PathBuf)],
        source_dir: Option<&Path>,
        tex_enabled: bool,
        tex_disabled_reason: Option<&str>,
        pending_tex_action: &mut Option<TexAction>,
        pending_tex_path: &mut Option<(String, &'static str, Option<String>)>,
    ) {
        let albedo_path: Option<PathBuf> = mat_slots
            .iter()
            .find(|(_, slot, _)| *slot == "base_color")
            .map(|(_, _, rel)| resolve_for_check(rel, source_dir));
        let albedo_exists = albedo_path
            .as_ref()
            .map(|p| self.cached_exists(p))
            .unwrap_or(false);

        ui.add_space(4.0);
        ui.label(egui::RichText::new("textures").weak());

        ui.horizontal(|ui| {
            let thumb_size = 64.0_f32;
            let cell = egui::vec2(thumb_size, thumb_size);
            if albedo_exists {
                let abs = albedo_path.clone().expect("checked above");
                if let Some(handle) = self.thumb_handle(ctx, &abs) {
                    ui.add(egui::Image::new((handle.id(), cell)).rounding(4.0))
                        .on_hover_text(ellipsize_path(&abs, 60));
                } else {
                    let (rect, _) = ui.allocate_exact_size(cell, egui::Sense::hover());
                    ui.painter()
                        .rect_filled(rect, 4.0, ui.visuals().faint_bg_color);
                }
            } else {
                let (rect, _) = ui.allocate_exact_size(cell, egui::Sense::hover());
                ui.painter()
                    .rect_filled(rect, 4.0, ui.visuals().faint_bg_color);
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "(none)",
                    egui::FontId::proportional(11.0),
                    ui.visuals().weak_text_color(),
                );
            }

            ui.vertical(|ui| {
                let gen_label = if albedo_exists { "Regenerate" } else { "Generate" };
                let gen_tip = if albedo_exists {
                    "Re-run the textures pipeline for this material \
                     (forces overwrite of existing PNGs)"
                } else {
                    "Run the textures pipeline for this material — writes a \
                     base_color PNG (plus derived normal/MR/AO) into ./textures/"
                };
                let gen_resp = ui
                    .add_enabled(tex_enabled, egui::Button::new(gen_label))
                    .on_hover_text(tex_disabled_reason.unwrap_or(gen_tip));
                if gen_resp.clicked() {
                    *pending_tex_action = Some(TexAction::Regenerate(mat.name.clone()));
                }
                if ui
                    .add_enabled(albedo_exists, egui::Button::new("Delete"))
                    .on_hover_text(
                        "Remove the albedo + PBR companion PNGs and clear the \
                         *_texture attrs on this material",
                    )
                    .clicked()
                {
                    *pending_tex_action = Some(TexAction::Delete(mat.name.clone()));
                }
                if ui
                    .add_enabled(albedo_exists, egui::Button::new("Reveal"))
                    .on_hover_text(
                        "Open this PNG's folder in the OS file manager \
                         with the file selected",
                    )
                    .clicked()
                {
                    if let Some(p) = albedo_path.clone() {
                        *pending_tex_action = Some(TexAction::Reveal(p));
                    }
                }
            });
        });

        if !mat_slots.is_empty() {
            for (_, slot, rel_path) in mat_slots {
                let resolved = resolve_for_check(rel_path, source_dir);
                let exists = self.cached_exists(&resolved);
                let (mark, color) = if exists {
                    ("✓", egui::Color32::from_rgb(80, 200, 120))
                } else {
                    ("✗", egui::Color32::from_rgb(230, 100, 100))
                };
                // Concrete hover text on the missing marker so the user sees
                // *which* path the file picker looked for, not just that
                // something is wrong.
                let mark_tip = if exists {
                    format!("Found at {}", resolved.display())
                } else {
                    format!(
                        "File not found at {}\nRegenerate or update the path \
                         attribute in the .mog source.",
                        resolved.display()
                    )
                };
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(color, mark).on_hover_text(mark_tip);
                    ui.label(*slot);
                    let display = ellipsize_path(rel_path, 30);
                    ui.label(display).on_hover_text(rel_path.to_string_lossy());
                });
            }
        }

        // Manual per-slot picker — Browse points the slot at an existing PNG
        // without re-running the LLM. The LLM pipeline does not expose this
        // knob, so authors who already have textures on disk would otherwise
        // have to hand-edit the .mog. Rows for every slot, regardless of
        // whether one is currently authored — Clear hides itself when the
        // slot is empty.
        ui.add_space(4.0);
        ui.label(egui::RichText::new("set path").weak());
        const SLOT_ROWS: [(&str, &str); 5] = [
            ("base_color", "base_color_texture"),
            ("metallic_roughness", "metallic_roughness_texture"),
            ("normal", "normal_texture"),
            ("occlusion", "occlusion_texture"),
            ("emissive", "emissive_texture"),
        ];
        for (slot_label, attr) in SLOT_ROWS {
            let authored = mat_slots.iter().any(|(_, s, _)| *s == slot_label);
            ui.horizontal(|ui| {
                ui.label(slot_label);
                if ui
                    .small_button("Browse…")
                    .on_hover_text(
                        "Pick a PNG and write its path into this slot. \
                         Stored relative to the .mog when possible.",
                    )
                    .clicked()
                {
                    if let Some(picked) = rfd::FileDialog::new()
                        .add_filter("PNG", &["png"])
                        .set_directory(
                            source_dir
                                .map(|p| p.to_path_buf())
                                .unwrap_or_else(|| {
                                    std::env::current_dir().unwrap_or_default()
                                }),
                        )
                        .pick_file()
                    {
                        let rel = source_dir
                            .and_then(|base| {
                                picked.strip_prefix(base).ok().map(|p| p.to_path_buf())
                            })
                            .unwrap_or_else(|| picked.clone());
                        let value = format!("\"{}\"", rel.to_string_lossy());
                        *pending_tex_path = Some((mat.name.clone(), attr, Some(value)));
                    }
                }
                if authored
                    && ui
                        .small_button("Clear")
                        .on_hover_text("Remove this slot's path attr")
                        .clicked()
                {
                    *pending_tex_path = Some((mat.name.clone(), attr, None));
                }
            });
        }
    }

    /// Look up or lazily upload an albedo thumbnail for `abs`. `None` when the
    /// file isn't a readable PNG. Same cache (keyed by absolute path + mtime)
    /// the LLM panel previously used.
    pub(in crate::app::ui_panels::materials) fn thumb_handle(
        &mut self,
        ctx: &egui::Context,
        abs: &Path,
    ) -> Option<egui::TextureHandle> {
        let mtime = fs::metadata(abs).ok().and_then(|m| m.modified().ok());
        if let Some(entry) = self.thumb_cache.get(abs) {
            if entry.mtime == mtime {
                return Some(entry.handle.clone());
            }
        }
        let bytes = fs::read(abs).ok()?;
        let img = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
            .ok()?
            .to_rgba8();
        // Downscale oversized albedos before upload — a 2048² RGBA8 texture
        // is 16 MB in VRAM per material, and thumbnails render at 64 px.
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
        self.thumb_cache.insert(
            abs.to_path_buf(),
            ThumbEntry {
                handle: handle.clone(),
                mtime,
            },
        );
        Some(handle)
    }

    /// Stat-cached existence check. The texture-roster paint runs every frame
    /// so a naive `Path::exists()` would hit the FS once per slot per frame.
    pub(in crate::app::ui_panels::materials) fn cached_exists(&mut self, path: &Path) -> bool {
        let now = Instant::now();
        if let Some((_mtime, exists, checked)) = self.tex_exists_cache.get(path) {
            if now.duration_since(*checked) < TEX_EXISTS_TTL {
                return *exists;
            }
        }
        let meta = fs::metadata(path);
        let exists = meta.is_ok();
        let mtime = meta.ok().and_then(|m| m.modified().ok());
        self.tex_exists_cache
            .insert(path.to_path_buf(), (mtime, exists, now));
        exists
    }
}
