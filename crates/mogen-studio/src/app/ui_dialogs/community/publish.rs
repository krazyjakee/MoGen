//! Publish dialog: form, thumbnail capture round-trip, request build,
//! result handling (including stamping `meta(moghub_*)` so the next
//! publish targets the same MoGHub model).

use std::path::PathBuf;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use mogen_moghub_client::{PublishFileInput, PublishRequest};

use crate::app::moghub::{publish_model, MoghubMessage};
use crate::app::publish_textures::collect_publish_textures;
use crate::app::MogenStudioApp;
use crate::viewer::{CaptureFrame, CaptureKind, CaptureRequest};

use super::state::{
    PublishDialogState, UpdateTarget, PUBLISH_THUMB_PREVIEW_SIZE, PUBLISH_THUMB_SIZE,
};
use super::util::{format_err, publish_thumb_temp_path, slug_from_url_path};

impl MogenStudioApp {
    /// Seed and open the publish dialog from the active tab. Caller
    /// (the Community menu handler) has already gated on `me.is_some()`,
    /// so reaching this with no signed-in user means a bug — we still
    /// guard defensively by returning early.
    pub(in crate::app) fn open_publish_dialog(&mut self) {
        if self.community.me.is_none() {
            return;
        }
        let active = self.active();
        let active_source = active.source.clone();
        let active_display = active.display_name();
        let active_path = active.path.clone();
        let suggested_filename = if active_display.ends_with(".mog") {
            active_display.clone()
        } else if active_display == "untitled" {
            "scene.mog".to_string()
        } else {
            format!("{active_display}.mog")
        };

        // Pull title/description/tags off the source's `meta(...)` block.
        // Auto-default `publish_as_module` to on when there are no
        // top-level `import` declarations — a self-contained file is the
        // common shape of a registry-importable module. Parse failures
        // fall back to empty meta + scene; the dialog will block
        // publishing on the missing title.
        let (meta, has_imports) = match mogen_dsl::parse(&active_source) {
            Ok(ast) => {
                let meta = mogen_dsl::extract_meta(&ast).unwrap_or_default();
                let has_imports = ast.iter().any(|n| n.kind == "import");
                (meta, has_imports)
            }
            Err(_) => (Default::default(), false),
        };
        let tags_input = meta
            .tags
            .iter()
            .map(|t| t.trim())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(", ");

        // Lift the previous publish's identity (if any) off the source's
        // meta block. All three keys must be present + parseable — a
        // half-stamped file (e.g. user hand-edited it) falls back to the
        // first-publish path, which is the safer default.
        let update_target = mogen_dsl::read_meta_attr(&active_source, "moghub_model_id")
            .and_then(|model_id| {
                let slug =
                    mogen_dsl::read_meta_attr(&active_source, "moghub_slug")?;
                let last_version: i32 = mogen_dsl::read_meta_attr(
                    &active_source,
                    "moghub_version",
                )?
                .parse()
                .ok()?;
                Some(UpdateTarget {
                    model_id,
                    slug,
                    last_version,
                })
            });

        // Kick off a thumbnail capture of the live viewport. The GL paint
        // callback consumes the request on the next frame and writes a PNG
        // to `thumbnail_temp`; `publish_dialog` drains the outcome and
        // uploads a preview texture. Submitting the request here (rather
        // than on first paint of the dialog) keeps the typical fast path
        // — open dialog, glance at preview, click Publish — under one frame
        // of dead air.
        let thumbnail_temp = self.submit_publish_thumbnail_capture();

        self.community.publish = Some(PublishDialogState {
            title: meta.name.unwrap_or_default(),
            description: meta.description.unwrap_or_default(),
            tags_input,
            license: "CC0-1.0".to_string(),
            visibility: "public".to_string(),
            publish_message: String::new(),
            publish_as_module: !has_imports,
            filename: suggested_filename,
            source: active_source,
            entry_path: active_path,
            error: None,
            success: None,
            thumbnail_temp: Some(thumbnail_temp),
            thumbnail_bytes: None,
            thumbnail_texture: None,
            thumbnail_error: None,
            update_target,
            publish_as_new: false,
        });
    }

    /// Render the publish dialog. No-op when closed. Designed to be
    /// called from the central app paint pass after the menu and
    /// status bar are drawn, so it floats above them.
    pub(in crate::app) fn publish_dialog(&mut self, ctx: &egui::Context) {
        if self.community.publish.is_none() {
            return;
        }
        // Drain any in-flight publish result so the dialog's success /
        // error state reflects the latest worker reply.
        self.poll_publish_worker();
        // Drain a Publish-kind capture outcome (if any) before the dialog
        // body draws so the preview shows up in the same frame the GL
        // worker reports completion.
        self.poll_publish_thumbnail(ctx);
        let mut keep_open = true;
        let me_handle = self
            .community
            .me
            .as_ref()
            .map(|u| u.handle.clone())
            .unwrap_or_default();
        let posting = self.community.pending_publish.is_some();
        let mut submit = false;
        let mut retry_capture = false;
        let mut open_in_browser: Option<String> = None;
        egui::Window::new("Publish to MoGHub")
            .open(&mut keep_open)
            .default_width(440.0)
            .resizable(true)
            .show(ctx, |ui| {
                let Some(state) = self.community.publish.as_mut() else {
                    return;
                };
                if let Some(success) = state.success.clone() {
                    ui.heading("Published ✓");
                    ui.label(format!(
                        "Your model is live at {}{}",
                        self.settings.moghub_url.trim_end_matches('/'),
                        success.url_path,
                    ));
                    ui.horizontal(|ui| {
                        if ui.button("Open in browser").clicked() {
                            open_in_browser = Some(format!(
                                "{}{}",
                                self.settings.moghub_url.trim_end_matches('/'),
                                success.url_path,
                            ));
                        }
                        if ui.button("Close").clicked() {
                            // Close handled by setting publish to None
                            // outside the closure.
                            state.success = None; // sentinel so the
                                                  // outer code closes
                                                  // the window
                        }
                    });
                    if state.success.is_none() {
                        // User clicked Close inside the success branch.
                    }
                    return;
                }

                // Title / description / tags default to the source's
                // `meta(...)` block on open — edits here override for
                // this publish without rewriting the file.
                ui.horizontal(|ui| {
                    ui.label("Title");
                    ui.add(
                        egui::TextEdit::singleline(&mut state.title)
                            .hint_text("from meta(name = \"…\")"),
                    );
                });
                let updating = state
                    .update_target
                    .as_ref()
                    .filter(|_| !state.publish_as_new);
                if let Some(target) = updating {
                    ui.weak(format!(
                        "Updating @{me_handle}/{} → v{}",
                        target.slug,
                        target.last_version + 1,
                    ));
                } else {
                    ui.weak(format!(
                        "Publishing as @{me_handle} (slug auto-assigned)"
                    ));
                }
                ui.horizontal(|ui| {
                    ui.label("Filename");
                    ui.add(egui::TextEdit::singleline(&mut state.filename));
                });
                ui.label("Description");
                ui.add(
                    egui::TextEdit::multiline(&mut state.description)
                        .desired_rows(3)
                        .hint_text("from meta(description = \"…\")"),
                );

                ui.horizontal(|ui| {
                    ui.label("License");
                    egui::ComboBox::from_id_salt("publish_license")
                        .selected_text(state.license.as_str())
                        .show_ui(ui, |ui| {
                            for opt in ["CC0-1.0", "CC-BY-4.0", "CC-BY-SA-4.0", "MIT"] {
                                ui.selectable_value(
                                    &mut state.license,
                                    opt.to_string(),
                                    opt,
                                );
                            }
                        });
                    ui.label("Visibility");
                    egui::ComboBox::from_id_salt("publish_visibility")
                        .selected_text(state.visibility.as_str())
                        .show_ui(ui, |ui| {
                            for opt in ["public", "unlisted", "private"] {
                                ui.selectable_value(
                                    &mut state.visibility,
                                    opt.to_string(),
                                    opt,
                                );
                            }
                        });
                });

                ui.horizontal(|ui| {
                    ui.label("Tags");
                    ui.add(
                        egui::TextEdit::singleline(&mut state.tags_input)
                            .hint_text("comma-separated, max 8"),
                    );
                });

                ui.horizontal(|ui| {
                    ui.label("Publish message");
                    ui.add(egui::TextEdit::singleline(&mut state.publish_message));
                });

                ui.checkbox(
                    &mut state.publish_as_module,
                    "Publish as module (other authors can `use \"@…\"` it)",
                );

                // Escape hatch when republishing: lets the user fork the
                // file off the previous identity without hand-editing the
                // meta block. Only relevant when an `update_target` was
                // lifted from the source.
                if state.update_target.is_some() {
                    ui.checkbox(
                        &mut state.publish_as_new,
                        "Publish as new (allocate a fresh slug)",
                    );
                }

                ui.label("Preview");
                ui.horizontal(|ui| {
                    let dim = egui::vec2(
                        PUBLISH_THUMB_PREVIEW_SIZE,
                        PUBLISH_THUMB_PREVIEW_SIZE,
                    );
                    if let Some(tex) = state.thumbnail_texture.as_ref() {
                        ui.add(egui::Image::new(tex).fit_to_exact_size(dim));
                    } else {
                        let (rect, _) = ui.allocate_exact_size(dim, egui::Sense::hover());
                        ui.painter().rect_filled(
                            rect,
                            4.0,
                            ui.style().visuals.widgets.inactive.bg_fill,
                        );
                    }
                    ui.vertical(|ui| {
                        if let Some(err) = state.thumbnail_error.as_deref() {
                            ui.colored_label(
                                egui::Color32::LIGHT_RED,
                                format!("preview render failed: {err}"),
                            );
                            ui.weak("A preview is required before publishing.");
                            if ui.button("Retry capture").clicked() {
                                retry_capture = true;
                            }
                        } else if state.thumbnail_bytes.is_none() {
                            ui.weak("Rendering preview…");
                        } else {
                            ui.weak("Captured from the live viewport.");
                        }
                    });
                });

                if let Some(err) = &state.error {
                    ui.colored_label(egui::Color32::LIGHT_RED, err);
                }

                ui.horizontal(|ui| {
                    let updating = state
                        .update_target
                        .as_ref()
                        .filter(|_| !state.publish_as_new);
                    let has_thumbnail = state.thumbnail_bytes.is_some();
                    let label = match (posting, updating) {
                        (true, Some(_)) => "Publishing update…".to_string(),
                        (true, None) => "Publishing…".to_string(),
                        (false, Some(t)) => {
                            format!("Publish update v{}", t.last_version + 1)
                        }
                        (false, None) => "Publish".to_string(),
                    };
                    let publish_btn = ui.add_enabled(
                        !posting && has_thumbnail,
                        egui::Button::new(label),
                    );
                    let publish_btn = if !has_thumbnail && !posting {
                        publish_btn.on_disabled_hover_text(
                            "Waiting for the preview render — the thumbnail is required.",
                        )
                    } else {
                        publish_btn
                    };
                    if publish_btn.clicked() {
                        submit = true;
                    }
                    if !posting && ui.button("Cancel").clicked() {
                        // Sentinel: clear title + flag a cancel so the
                        // outer close-logic shuts the window.
                        state.title.clear();
                        state.success = None;
                        state.error = Some("__cancel__".to_string());
                    }
                });
            });

        if let Some(url) = open_in_browser {
            let _ = webbrowser::open(&url);
        }
        // Close logic: window closed by chrome, by Cancel sentinel, or
        // by the success branch's Close button (which clears success).
        let cancelled = self
            .community
            .publish
            .as_ref()
            .map(|s| s.error.as_deref() == Some("__cancel__"))
            .unwrap_or(false);
        if !keep_open || cancelled {
            // Best-effort: if the capture is still in flight when the user
            // bails out of the dialog, scrub the temp file so it doesn't
            // sit around in /tmp. The capture pipeline will still write
            // it once on the next paint, but `poll_publish_thumbnail`'s
            // "dialog closed" branch will mop that up next time it fires.
            if let Some(state) = self.community.publish.as_ref() {
                if let Some(p) = state.thumbnail_temp.as_deref() {
                    let _ = std::fs::remove_file(p);
                }
            }
            self.community.publish = None;
        }
        if retry_capture {
            let temp = self.submit_publish_thumbnail_capture();
            if let Some(state) = self.community.publish.as_mut() {
                state.thumbnail_temp = Some(temp);
                state.thumbnail_error = None;
            }
        }
        if submit {
            self.kick_publish(ctx);
        }
    }

    /// Submit a Publish-kind viewport capture and return the temp path the
    /// GL worker will write to. Used by `open_publish_dialog` for the
    /// initial capture and by the dialog's Retry button when the prior
    /// capture errored.
    fn submit_publish_thumbnail_capture(&mut self) -> PathBuf {
        let thumbnail_temp = publish_thumb_temp_path();
        self.viewer.submit_capture(CaptureRequest {
            kind: CaptureKind::Publish,
            size: PUBLISH_THUMB_SIZE,
            bg: self.settings.viewer_bg_rgb(),
            frames: vec![CaptureFrame {
                yaw: std::f32::consts::FRAC_PI_4,
                pitch: 0.5,
                time: 0.0,
                path: thumbnail_temp.clone(),
            }],
            total: 0,
            written: Vec::new(),
            error: None,
        });
        thumbnail_temp
    }

    fn kick_publish(&mut self, ctx: &egui::Context) {
        let Some(state) = self.community.publish.as_mut() else {
            return;
        };
        if state.title.trim().is_empty() {
            state.error = Some("title is required".to_string());
            return;
        }
        state.error = None;

        // Bundle every sibling `.mog` reachable through `import "..."`. Skips
        // registry uses (`use "@user/slug"`) — those resolve through `mog.lock`
        // on the consumer side, so re-uploading them would duplicate bytes.
        // Untitled buffers (no `entry_path`) skip the walk entirely; their
        // imports — if any — can't resolve without an on-disk base dir, so
        // bundling fails gracefully and the publish proceeds with the entry
        // alone.
        let mut imports: Vec<(String, String)> = Vec::new();
        let entry_dir_for_textures = state
            .entry_path
            .as_ref()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()));
        if let Some(entry_dir) = entry_dir_for_textures.as_deref() {
            match mogen_dsl::collect_local_import_files(entry_dir, &state.source) {
                Ok(found) => {
                    for (name, source) in found {
                        if name == state.filename {
                            state.error = Some(format!(
                                "import filename collides with entry filename `{}` — \
                                 rename one before publishing",
                                state.filename
                            ));
                            return;
                        }
                        imports.push((name, source));
                    }
                }
                Err(e) => {
                    state.error = Some(format!("collecting imports: {e}"));
                    return;
                }
            }
        }

        let mut files = vec![PublishFileInput {
            filename: state.filename.clone(),
            source: state.source.clone(),
            is_entry: true,
        }];
        for (name, source) in &imports {
            files.push(PublishFileInput {
                filename: name.clone(),
                source: source.clone(),
                is_entry: false,
            });
        }

        // Bundle every PNG/JPG/JPEG/WebP referenced from a material in the
        // entry or any imported `.mog`. Untitled buffers can't resolve
        // texture paths without an on-disk base, so they publish without
        // textures (matches the import-bundling fallback above).
        let textures = if let Some(entry_dir) = entry_dir_for_textures.as_deref() {
            match collect_publish_textures(entry_dir, &state.source, &imports) {
                Ok(t) => t,
                Err(e) => {
                    state.error = Some(format!("bundling textures: {e}"));
                    return;
                }
            }
        } else {
            Vec::new()
        };

        // Defensive: the Publish button is gated on this being Some, so we
        // shouldn't reach here without a thumbnail. Bail loudly rather than
        // ship an incomplete request.
        let Some(thumb_bytes) = state.thumbnail_bytes.as_ref() else {
            state.error =
                Some("preview not captured yet — wait for the render to finish".to_string());
            return;
        };
        let thumbnail_png_base64 = STANDARD.encode(thumb_bytes);
        let target_model_id = state
            .update_target
            .as_ref()
            .filter(|_| !state.publish_as_new)
            .map(|t| t.model_id.clone());
        let req = PublishRequest {
            title: state.title.clone(),
            description: state.description.clone(),
            license: state.license.clone(),
            visibility: state.visibility.clone(),
            publish_message: state.publish_message.clone(),
            tags: state
                .tags_input
                .split(',')
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty())
                .take(8)
                .collect(),
            files,
            textures,
            thumbnail_png_base64,
            parent_version_id: None,
            publish_as_module: state.publish_as_module,
            target_model_id,
        };
        let url = self.settings.moghub_url.clone();
        let token = self.settings.moghub_session.clone();
        self.community.pending_publish = Some(publish_model(url, token, ctx.clone(), req));
    }

    /// Drain a `Publish`-kind capture outcome from the viewer, if one is
    /// ready. On success: read the temp PNG into `thumbnail_bytes`, decode
    /// it, and upload an in-dialog preview texture. The temp file is
    /// deleted either way — its bytes are owned in memory from this point
    /// on.
    fn poll_publish_thumbnail(&mut self, ctx: &egui::Context) {
        let Some(outcome) = self
            .viewer
            .take_capture_outcome_if(|kind| matches!(kind, CaptureKind::Publish))
        else {
            return;
        };
        let Some(state) = self.community.publish.as_mut() else {
            // Dialog closed between capture submission and outcome — clean
            // up the temp file the worker just wrote and drop the bytes.
            for path in &outcome.frame_paths {
                let _ = std::fs::remove_file(path);
            }
            return;
        };
        let temp = state.thumbnail_temp.take();
        if let Some(err) = outcome.error {
            state.thumbnail_error = Some(err);
            if let Some(p) = temp.as_deref() {
                let _ = std::fs::remove_file(p);
            }
            return;
        }
        let path = match outcome.frame_paths.last().cloned().or(temp.clone()) {
            Some(p) => p,
            None => {
                state.thumbnail_error =
                    Some("capture produced no output".to_string());
                return;
            }
        };
        match std::fs::read(&path) {
            Ok(bytes) => {
                if let Ok(img) = image::load_from_memory(&bytes) {
                    let rgba = img.to_rgba8();
                    let size =
                        [rgba.width() as usize, rgba.height() as usize];
                    let pixels = rgba.into_raw();
                    let color = egui::ColorImage::from_rgba_unmultiplied(
                        size, &pixels,
                    );
                    state.thumbnail_texture = Some(ctx.load_texture(
                        "publish_thumbnail",
                        color,
                        egui::TextureOptions::LINEAR,
                    ));
                }
                state.thumbnail_bytes = Some(bytes);
            }
            Err(e) => {
                state.thumbnail_error =
                    Some(format!("read preview: {e}"));
            }
        }
        let _ = std::fs::remove_file(&path);
    }

    fn poll_publish_worker(&mut self) {
        let Some(inflight) = &self.community.pending_publish else {
            return;
        };
        let Some(msg) = inflight.try_recv() else {
            return;
        };
        self.community.pending_publish = None;
        let MoghubMessage::Published(result) = msg else {
            return;
        };
        let Some(state) = self.community.publish.as_mut() else {
            return;
        };
        match result {
            Ok(r) => {
                // Stamp the published identity into the source so the
                // *next* publish targets the same MoGHub model. Computed
                // here rather than on the server because PublishResponse
                // doesn't carry the version number — but the dialog
                // always knows the last version (None → 1, Some(N) →
                // N+1) and the slug parses out of the canonical
                // `/m/<user>/<slug>` URL.
                let next_version = state
                    .update_target
                    .as_ref()
                    .filter(|_| !state.publish_as_new)
                    .map(|t| t.last_version + 1)
                    .unwrap_or(1);
                let stamped_slug = slug_from_url_path(&r.url_path);
                let stamped_model_id = r.model_id.clone();
                let entry_path = state.entry_path.clone();
                state.success = Some(r);
                state.error = None;
                self.stamp_publish_meta(
                    entry_path.as_deref(),
                    &stamped_model_id,
                    stamped_slug.as_deref(),
                    next_version,
                );
            }
            Err(e) => state.error = Some(format_err(&e)),
        }
    }

    /// Persist the MoGHub identity into the active tab's source, then
    /// save it to disk so re-opening the file reproduces the update
    /// path. Best-effort: an untitled buffer (no `entry_path`) gets the
    /// stamp applied in memory only; the next manual save will land it
    /// on disk. Mismatched-active-tab is a no-op — the user might have
    /// switched tabs while the publish was in flight, and we'd rather
    /// drop the stamp than overwrite an unrelated file.
    fn stamp_publish_meta(
        &mut self,
        entry_path: Option<&std::path::Path>,
        model_id: &str,
        slug: Option<&str>,
        version: i32,
    ) {
        let target_index = match entry_path {
            Some(p) => self.file_index_by_path(p),
            None => {
                if self.active().path.is_none() {
                    Some(self.active)
                } else {
                    None
                }
            }
        };
        let Some(i) = target_index else {
            return;
        };
        let mut src = self.files[i].source.clone();
        src = mogen_dsl::upsert_meta_attr(&src, "moghub_model_id", model_id);
        if let Some(s) = slug {
            src = mogen_dsl::upsert_meta_attr(&src, "moghub_slug", s);
        }
        src = mogen_dsl::upsert_meta_attr(
            &src,
            "moghub_version",
            &version.to_string(),
        );
        self.files[i].source = src;
        self.files[i].dirty = self.files[i].source != self.files[i].last_saved_source;
        self.files[i].needs_compile = true;
        if let Some(path) = self.files[i].path.clone() {
            self.save_index_to(i, &path);
        }
    }
}
