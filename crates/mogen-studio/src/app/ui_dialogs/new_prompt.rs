use std::path::Path;

use eframe::egui;

use super::prefs::model_presets;
use crate::app::types::{EnhanceTarget, GenImageInput, MAX_GEN_IMAGE_BYTES};
use crate::app::MogenStudioApp;
use crate::settings::{
    thinking_level_key, thinking_level_label, ProviderSlot, PROVIDER_SLOTS, THINKING_LEVELS,
};

/// Decode the image bytes via the `image` crate, downscale to a thumbnail
/// (96 px on the long side), and upload to the GPU as an `egui::TextureHandle`
/// so the dialog can show a preview without re-decoding every frame. Returns
/// `None` if decoding fails — the dialog will still send the raw bytes to
/// Gemini in that case (the model can handle PNG/JPG/WEBP), it just won't
/// show a thumbnail.
fn make_image_thumbnail(
    ctx: &egui::Context,
    bytes: &[u8],
    name: &str,
) -> Option<egui::TextureHandle> {
    let img = image::load_from_memory(bytes).ok()?;
    let thumb = img.thumbnail(96, 96);
    let rgba = thumb.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    let color_image = egui::ColorImage::from_rgba_unmultiplied(size, &rgba.into_raw());
    Some(ctx.load_texture(
        format!("gen_prompt_thumb:{name}"),
        color_image,
        egui::TextureOptions::LINEAR,
    ))
}

/// Map an image's bytes (sniffed) and file extension to an `image/*` MIME
/// string Gemini accepts. Sniffing wins over the extension because phone
/// cameras frequently mislabel `.jpg` as `.heic` or vice versa; we only
/// fall back to the extension when sniffing fails.
fn guess_image_mime(bytes: &[u8], path: &Path) -> String {
    use image::ImageFormat;
    if let Ok(fmt) = image::guess_format(bytes) {
        return match fmt {
            ImageFormat::Png => "image/png",
            ImageFormat::Jpeg => "image/jpeg",
            ImageFormat::WebP => "image/webp",
            ImageFormat::Gif => "image/gif",
            ImageFormat::Bmp => "image/bmp",
            ImageFormat::Tiff => "image/tiff",
            _ => "application/octet-stream",
        }
        .to_string();
    }
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("bmp") => "image/bmp",
        _ => "application/octet-stream",
    }
    .to_string()
}

impl MogenStudioApp {
    /// "New from Prompt…" dialog — the only place the LLM *generator* is
    /// surfaced. Producing a new scene always spawns a fresh untitled tab
    /// so the active MOG file isn't silently overwritten.
    ///
    /// The dialog accepts an optional reference image alongside the text
    /// prompt (image-to-3D). Either input on its own is enough to enable
    /// the Generate button — the Pro vision model can interpret the image
    /// without supplementing text, and a text-only prompt keeps the legacy
    /// flow working unchanged.
    pub(in crate::app) fn ui_new_prompt(&mut self, ctx: &egui::Context) {
        if !self.show_new_prompt {
            return;
        }
        let mut open = true;
        let mut close_after = false;
        let mut spawn_now = false;
        let mut pick_image = false;
        let mut clear_image = false;
        let mut image_status: Option<String> = None;
        egui::Window::new("New from Prompt")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(500.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(format!(
                    "Describe the scene you want {} to generate:",
                    self.settings.provider().display_name(),
                ));
                let prompt_id = egui::Id::new("new_prompt_draft");
                crate::app::text_menu::text_edit_with_menu(
                    ui,
                    prompt_id,
                    &mut self.new_prompt_draft,
                    |ui, text| {
                        ui.add(
                            egui::TextEdit::multiline(text)
                                .hint_text(
                                    "e.g. a wooden stool with three legs \
                                     (optional when an image is attached)",
                                )
                                .desired_rows(4)
                                .desired_width(f32::INFINITY)
                                .id(prompt_id),
                        )
                    },
                );
                if self.new_prompt_focus_pending {
                    ui.ctx().memory_mut(|m| m.request_focus(prompt_id));
                    self.new_prompt_focus_pending = false;
                }
                self.ui_enhance_button(
                    ui,
                    EnhanceTarget::Generate,
                    "Rewrite the prompt with the fast model",
                );

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);
                ui.label("Reference image (optional):");
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    if let Some(img) = self.new_prompt_image.as_ref() {
                        if let Some(tex) = img.thumbnail.as_ref() {
                            // Letterbox into a 64×64 slot so the row height is
                            // stable regardless of the source image's aspect.
                            let aspect = {
                                let s = tex.size_vec2();
                                if s.y > 0.0 { s.x / s.y } else { 1.0 }
                            };
                            let h = 64.0;
                            let w = (h * aspect).clamp(24.0, 96.0);
                            ui.image((tex.id(), egui::vec2(w, h)));
                        }
                        let label = img
                            .path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| img.path.display().to_string());
                        ui.vertical(|ui| {
                            ui.label(label);
                            let kib = (img.data.len() as f64) / 1024.0;
                            let size_label = if kib >= 1024.0 {
                                format!("{:.1} MB · {}", kib / 1024.0, img.mime_type)
                            } else {
                                format!("{:.0} KB · {}", kib, img.mime_type)
                            };
                            ui.weak(size_label);
                        });
                        if ui.button("Remove").clicked() {
                            clear_image = true;
                        }
                        if ui.button("Replace…").clicked() {
                            pick_image = true;
                        }
                    } else if ui.button("Choose image…").clicked() {
                        pick_image = true;
                    }
                });

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);
                ui.label("Generation:");
                ui.add_space(2.0);

                // --- provider dropdown ---
                let current_slot = self.settings.provider_slot();
                let mut new_slot = current_slot;
                egui::ComboBox::from_id_salt("new_prompt_provider")
                    .selected_text(current_slot.label())
                    .show_ui(ui, |ui| {
                        for slot in PROVIDER_SLOTS {
                            if ui
                                .selectable_label(slot == current_slot, slot.label())
                                .clicked()
                            {
                                new_slot = slot;
                            }
                        }
                    });
                if new_slot != current_slot {
                    self.settings.set_provider_slot(new_slot);
                }
                let active_slot: ProviderSlot = self.settings.provider_slot();
                let active_provider = active_slot.to_provider();

                // --- model dropdown (skipped when the provider has no
                // dedicated model field — currently only Claude Code).
                if self
                    .settings
                    .thinking_model_field_mut(active_provider)
                    .is_some()
                {
                    let presets = model_presets(active_slot);
                    let model_default = active_provider.default_model();
                    let mut model_draft = self
                        .settings
                        .thinking_model_field(active_provider)
                        .to_string();
                    let selected_text = if model_draft.is_empty() {
                        format!("(default: {model_default})")
                    } else {
                        model_draft.clone()
                    };
                    ui.add_space(4.0);
                    ui.label("Model");
                    egui::ComboBox::from_id_salt((
                        "new_prompt_model",
                        active_provider.key(),
                    ))
                    .selected_text(selected_text)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(
                                model_draft.is_empty(),
                                format!("(default: {model_default})"),
                            )
                            .clicked()
                        {
                            model_draft.clear();
                        }
                        for m in presets {
                            if ui.selectable_label(model_draft == *m, *m).clicked() {
                                model_draft = (*m).to_string();
                            }
                        }
                    });
                    if let Some(buf) =
                        self.settings.thinking_model_field_mut(active_provider)
                    {
                        *buf = model_draft;
                    }
                }

                // --- thinking budget dropdown ---
                ui.add_space(4.0);
                ui.label("Thinking budget").on_hover_text(
                    "Cap on hidden reasoning tokens per call. Higher = better DSL on \
                     hard prompts but slower. Ignored by providers / models that don't \
                     expose a budget.",
                );
                let current_level = self.settings.thinking_level();
                egui::ComboBox::from_id_salt("new_prompt_thinking")
                    .selected_text(thinking_level_label(current_level))
                    .show_ui(ui, |ui| {
                        for level in THINKING_LEVELS {
                            if ui
                                .selectable_label(
                                    level == current_level,
                                    thinking_level_label(level),
                                )
                                .clicked()
                            {
                                self.settings.thinking_level =
                                    thinking_level_key(level).to_string();
                            }
                        }
                    });

                let has_key = self.resolve_api_key().is_some();
                if !has_key {
                    ui.add_space(4.0);
                    ui.colored_label(
                        egui::Color32::from_rgb(230, 200, 100),
                        format!(
                            "no {} API key — set one in Edit → Preferences…",
                            self.settings.provider().display_name(),
                        ),
                    );
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let prompt_ok = !self.new_prompt_draft.trim().is_empty();
                    let image_ok = self.new_prompt_image.is_some();
                    let can_generate = has_key && (prompt_ok || image_ok);
                    let hover = if image_ok && !prompt_ok {
                        "Create a new tab and generate from the attached image"
                    } else if image_ok {
                        "Create a new tab and generate from the prompt + image"
                    } else {
                        "Create a new tab and run the generator on it"
                    };
                    if ui
                        .add_enabled(can_generate, egui::Button::new("Generate"))
                        .on_hover_text(hover)
                        .clicked()
                    {
                        spawn_now = true;
                        close_after = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close_after = true;
                    }
                });
            });

        if pick_image {
            if let Err(msg) = self.pick_gen_prompt_image(ctx) {
                image_status = Some(msg);
            }
        }
        if clear_image {
            self.new_prompt_image = None;
        }
        if let Some(msg) = image_status {
            // Surface picker errors (too large / unreadable) on the active
            // tab's status bar so the user sees them after the dialog closes.
            self.active_mut().status = msg;
        }

        if !open || close_after {
            self.show_new_prompt = false;
        }
        if spawn_now {
            // Persist the provider / model / thinking selection picked in
            // the dialog so the choice survives app restarts (matches the
            // behaviour of the same controls in Preferences).
            let _ = self.settings.save();
            let prompt = self.new_prompt_draft.trim().to_string();
            // Move the staged image (if any) into the new tab so Retry can
            // re-issue the same call without re-picking the file. Strip the
            // GPU thumbnail handle before transfer — the worker thread
            // doesn't need it and TextureHandle isn't worth shuttling around.
            let mut staged = self.new_prompt_image.take();
            if let Some(img) = staged.as_mut() {
                img.thumbnail = None;
            }
            self.new_untitled();
            self.active_mut().gen_prompt = prompt;
            self.active_mut().gen_image = staged;
            let ctx_clone = ctx.clone();
            self.start_llm_generate(ctx_clone);
            self.new_prompt_draft.clear();
        }
        if !self.show_new_prompt {
            // Drop any staged image when the dialog closes without submitting,
            // so re-opening starts fresh and the GPU thumbnail handle is freed.
            self.new_prompt_image = None;
        }
    }

    /// Open a native file dialog for an image and stash the result on
    /// `self.new_prompt_image`. Returns `Err(msg)` on read / size failures so
    /// the caller can surface the message to the user.
    fn pick_gen_prompt_image(&mut self, ctx: &egui::Context) -> Result<(), String> {
        let dialog = rfd::FileDialog::new()
            .add_filter("Images", &["png", "jpg", "jpeg", "webp", "gif", "bmp"])
            .set_directory(&self.project_root);
        let Some(path) = dialog.pick_file() else {
            return Ok(());
        };
        let bytes = std::fs::read(&path)
            .map_err(|e| format!("couldn't read image: {e}"))?;
        if bytes.len() > MAX_GEN_IMAGE_BYTES {
            return Err(format!(
                "image too large ({:.1} MB); cap is {} MB — try a smaller file",
                (bytes.len() as f64) / (1024.0 * 1024.0),
                MAX_GEN_IMAGE_BYTES / (1024 * 1024),
            ));
        }
        let mime_type = guess_image_mime(&bytes, &path);
        if !mime_type.starts_with("image/") {
            return Err(format!(
                "unsupported file type ({}) — pick a PNG, JPG, WEBP, GIF, or BMP",
                mime_type,
            ));
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "image".to_string());
        let thumbnail = make_image_thumbnail(ctx, &bytes, &name);
        self.new_prompt_image = Some(GenImageInput {
            path,
            mime_type,
            data: bytes,
            thumbnail,
        });
        Ok(())
    }
}
