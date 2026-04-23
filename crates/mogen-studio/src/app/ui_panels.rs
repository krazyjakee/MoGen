use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use eframe::egui;
use mogen_core::Severity;

use crate::pipeline::Stage;

use super::types::{ShortcutAction, TEX_EXISTS_TTL};
use super::util::{
    delete_texture_group, ellipsize_path, find_clip_source_span, format_inspector_scalar,
    gather_texture_refs, offset_to_line_col, resolve_for_check, scan_unused_textures,
};
use super::MogenStudioApp;

impl MogenStudioApp {
    pub(super) fn ui_editor(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        let i = self.active;

        let font_id = egui::TextStyle::Monospace.resolve(ui.style());
        let palette = crate::highlight::Palette::for_visuals(&ui.style().visuals);

        // Layouter closure — runs on every repaint for the visible text. Kept
        // cheap by the single-pass tokeniser in `highlight`; caching on hash
        // would be nice but isn't needed yet at typical .mog sizes.
        let hl_font = font_id.clone();
        let mut layouter = move |ui: &egui::Ui, text: &str, wrap_width: f32| {
            let job = crate::highlight::highlight(text, hl_font.clone(), palette, wrap_width);
            ui.fonts(|f| f.layout_job(job))
        };

        // Compute how many text rows fit in the visible panel so the editor
        // and the gutter column always fill the full available height —
        // regardless of whether the .mog is 3 lines or 300. With fewer
        // content rows than fit, `desired_rows` keeps the TextEdit tall and
        // clickable; with more, the outer ScrollArea takes over.
        let row_height = ui.fonts(|f| f.row_height(&font_id));
        // TextEdit reserves ~2px top + 2px bottom as inner margin; account
        // for that so the last visible row doesn't clip.
        let available_height = (ui.available_height() - 4.0).max(row_height);
        let visible_rows = ((available_height / row_height).floor() as usize).max(1);

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;

                    // Gutter: one right-aligned line number per source row,
                    // padded with blank cells so the column visually extends
                    // to match the editor even when content is short.
                    // Wrapped in a Frame with the same vertical padding
                    // egui's TextEdit uses so the first row of the gutter
                    // sits on the first row of the editor.
                    let (gutter, _digits) = crate::highlight::gutter_job_padded(
                        &self.files[i].source,
                        visible_rows,
                        font_id.clone(),
                        palette,
                    );
                    egui::Frame::none()
                        .inner_margin(egui::Margin {
                            left: 4.0,
                            right: 4.0,
                            top: 2.0,
                            bottom: 2.0,
                        })
                        .show(ui, |ui| {
                            ui.add(egui::Label::new(gutter).selectable(false).wrap_mode(egui::TextWrapMode::Extend));
                        });

                    let pending_caret = self.files[i].pending_caret.take();
                    let editor_id = egui::Id::new("mog_editor_textedit");

                    let resp = super::text_menu::text_edit_with_menu(
                        ui,
                        editor_id,
                        &mut self.files[i].source,
                        |ui, text| {
                            ui.add_sized(
                                [ui.available_width(), 0.0],
                                egui::TextEdit::multiline(text)
                                    // code_editor() implies lock_focus(true), so Tab inserts
                                    // a tab character instead of moving focus out of the
                                    // editor — the right behavior for a code surface.
                                    .code_editor()
                                    .desired_rows(visible_rows)
                                    .desired_width(f32::INFINITY)
                                    .font(egui::TextStyle::Monospace)
                                    .layouter(&mut layouter)
                                    .id(editor_id),
                            )
                        },
                    );
                    if resp.changed() {
                        changed = true;
                    }

                    // Move the editor caret onto the selected node's
                    // declaration when the viewport reported a new pick.
                    if let Some(offset) = pending_caret {
                        let src = &self.files[i].source;
                        let clamped = offset.min(src.len());
                        let char_idx = src[..clamped].chars().count();
                        use egui::text::{CCursor, CCursorRange};
                        if let Some(mut state) =
                            egui::TextEdit::load_state(ui.ctx(), editor_id)
                        {
                            state.cursor.set_char_range(Some(CCursorRange::one(
                                CCursor::new(char_idx),
                            )));
                            state.store(ui.ctx(), editor_id);
                            ui.ctx().memory_mut(|m| m.request_focus(editor_id));
                        }
                    }
                });
            });

        if changed {
            self.files[i].dirty = self.files[i].source != self.files[i].last_saved_source;
            self.files[i].needs_compile = true;
            self.files[i].last_edit_at = Some(Instant::now());
            // Compilation itself is gated by `drive_compile_debounce` so a
            // burst of keystrokes only re-parses once the user pauses.
        }
    }

    /// One-line summary of the active MOG file's validator state, shown as
    /// the header for the collapsible diagnostics footer panel. Callers only
    /// need the string.
    pub(super) fn diagnostics_header_label(&self) -> String {
        let f = &self.files[self.active];
        let Some(result) = &f.last_result else {
            return "Diagnostics — (no build yet)".to_string();
        };
        if result.diagnostics.is_empty() {
            return match result.stage {
                Stage::Ok => "Diagnostics — ✓ ok".to_string(),
                other => format!("Diagnostics — {other:?}"),
            };
        }
        let mut errs = 0usize;
        let mut warns = 0usize;
        let mut infos = 0usize;
        for d in &result.diagnostics {
            match d.severity {
                Severity::Error => errs += 1,
                Severity::Warning => warns += 1,
                Severity::Info => infos += 1,
            }
        }
        let mut parts = Vec::new();
        if errs > 0 {
            parts.push(format!("{errs} error{}", if errs == 1 { "" } else { "s" }));
        }
        if warns > 0 {
            parts.push(format!("{warns} warning{}", if warns == 1 { "" } else { "s" }));
        }
        if infos > 0 {
            parts.push(format!("{infos} info"));
        }
        format!("Diagnostics — {}", parts.join(", "))
    }

    pub(super) fn ui_diagnostics(&mut self, ui: &mut egui::Ui) {
        let f = &self.files[self.active];
        let Some(result) = &f.last_result else {
            ui.label("(no build yet)");
            return;
        };
        if result.diagnostics.is_empty() {
            match result.stage {
                Stage::Ok => {
                    ui.colored_label(egui::Color32::from_rgb(80, 200, 120), "✓ ok");
                }
                _ => {
                    ui.label(format!("{:?}", result.stage));
                }
            }
            return;
        }
        for d in &result.diagnostics {
            let (color, tag) = match d.severity {
                Severity::Error => (egui::Color32::from_rgb(230, 100, 100), "error"),
                Severity::Warning => (egui::Color32::from_rgb(230, 200, 100), "warn"),
                Severity::Info => (egui::Color32::from_rgb(150, 180, 230), "info"),
            };
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(color, format!("[{tag}] {}", d.code));
                if let Some(span) = d.span {
                    let (line, col) = offset_to_line_col(&f.source, span.start);
                    ui.label(format!("{line}:{col}"));
                }
                ui.label(&d.message);
            });
        }
    }

    /// Inspector for the currently-selected scene node. Shown above the
    /// scene summary; collapses to a friendly hint when nothing's selected
    /// or when the selection came from a replicator / CSG expansion.
    pub(super) fn ui_selected(&mut self, ui: &mut egui::Ui) {
        use crate::edit;
        use crate::gizmo::GizmoMode;
        use crate::viewer::PendingEdit;

        let Some(sel) = self.viewer.selection() else {
            ui.label("(click a node in the 3D view to select it)");
            return;
        };
        let i = self.active;
        let Some(result) = &self.files[i].last_result else {
            ui.label("(no build yet)");
            return;
        };
        let Some(scene) = &result.scene else {
            ui.label("(no scene — fix errors first)");
            return;
        };
        let Some(node) = scene.nodes.get(sel.0 as usize) else {
            ui.label("(stale selection — recompile dropped it)");
            return;
        };

        ui.horizontal(|ui| {
            ui.label("Name:");
            ui.monospace(&node.name);
        });
        ui.horizontal(|ui| {
            ui.label("Kind:");
            ui.monospace(&node.kind);
        });

        if !node.editable {
            ui.add_space(6.0);
            ui.colored_label(
                egui::Color32::from_rgb(230, 200, 100),
                "Derived from array/mirror/CSG — edit the parent in the text.",
            );
            return;
        }

        // Gizmo mode switch — mirrors the viewport overlay buttons so a user
        // staring at the inspector can flip modes without pointing at the
        // canvas.
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let cur = self.viewer.gizmo_mode();
            for (label, mode) in [
                ("Move", GizmoMode::Translate),
                ("Rotate", GizmoMode::Rotate),
                ("Scale", GizmoMode::Scale),
            ] {
                if ui.selectable_label(cur == mode, label).clicked() {
                    self.viewer.set_gizmo_mode(mode);
                }
            }
        });

        let t = node.transform;
        let (rx_rad, ry_rad, rz_rad) = t.rotation.to_euler(glam::EulerRot::XYZ);
        let mut tx = t.translation.x;
        let mut ty = t.translation.y;
        let mut tz = t.translation.z;
        let mut rx = rx_rad.to_degrees();
        let mut ry = ry_rad.to_degrees();
        let mut rz = rz_rad.to_degrees();
        let mut sx = t.scale.x;
        let mut sy = t.scale.y;
        let mut sz = t.scale.z;
        let node_span = node.source_span;
        let node_id = sel;

        let mut edits: Vec<PendingEdit> = Vec::new();

        ui.add_space(6.0);
        egui::Grid::new("inspector_transform")
            .num_columns(4)
            .spacing([6.0, 4.0])
            .show(ui, |ui| {
                ui.label("Translate");
                if ui.add(egui::DragValue::new(&mut tx).speed(0.02)).changed() {
                    edits.push(PendingEdit::SetAttr {
                        node: node_id,
                        attr: "x".into(),
                        value: format_inspector_scalar(tx),
                    });
                }
                if ui.add(egui::DragValue::new(&mut ty).speed(0.02)).changed() {
                    edits.push(PendingEdit::SetAttr {
                        node: node_id,
                        attr: "y".into(),
                        value: format_inspector_scalar(ty),
                    });
                }
                if ui.add(egui::DragValue::new(&mut tz).speed(0.02)).changed() {
                    edits.push(PendingEdit::SetAttr {
                        node: node_id,
                        attr: "z".into(),
                        value: format_inspector_scalar(tz),
                    });
                }
                ui.end_row();

                ui.label("Rotate°");
                let mut emit_rot = false;
                if ui.add(egui::DragValue::new(&mut rx).speed(0.5).suffix("°")).changed() {
                    emit_rot = true;
                }
                if ui.add(egui::DragValue::new(&mut ry).speed(0.5).suffix("°")).changed() {
                    emit_rot = true;
                }
                if ui.add(egui::DragValue::new(&mut rz).speed(0.5).suffix("°")).changed() {
                    emit_rot = true;
                }
                if emit_rot {
                    edits.push(PendingEdit::SetAttr {
                        node: node_id,
                        attr: "rot".into(),
                        value: format!(
                            "[{}, {}, {}]",
                            format_inspector_scalar(rx),
                            format_inspector_scalar(ry),
                            format_inspector_scalar(rz),
                        ),
                    });
                }
                ui.end_row();

                ui.label("Scale");
                let mut emit_scale = false;
                if ui.add(egui::DragValue::new(&mut sx).speed(0.02)).changed() {
                    emit_scale = true;
                }
                if ui.add(egui::DragValue::new(&mut sy).speed(0.02)).changed() {
                    emit_scale = true;
                }
                if ui.add(egui::DragValue::new(&mut sz).speed(0.02)).changed() {
                    emit_scale = true;
                }
                if emit_scale {
                    edits.push(PendingEdit::SetAttr {
                        node: node_id,
                        attr: "scale".into(),
                        value: format!(
                            "[{}, {}, {}]",
                            format_inspector_scalar(sx),
                            format_inspector_scalar(sy),
                            format_inspector_scalar(sz),
                        ),
                    });
                }
                ui.end_row();
            });

        for edit in edits {
            self.viewer.push_pending_edit(edit);
        }

        // Delete / Duplicate operate straight on the source string because
        // they need to rewrite the node's full span, not a single attr.
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let span_ok = node_span.is_some();
            if ui
                .add_enabled(span_ok, egui::Button::new("Duplicate"))
                .on_hover_text("Duplicate this node in the DSL source")
                .clicked()
            {
                if let Some(span) = node_span {
                    let src = &self.files[i].source;
                    let new_src = edit::duplicate_node(src, span);
                    let f = &mut self.files[i];
                    f.source = new_src;
                    f.dirty = f.source != f.last_saved_source;
                    f.needs_compile = true;
                    f.last_edit_at = Some(Instant::now());
                }
            }
            if ui
                .add_enabled(span_ok, egui::Button::new("Delete"))
                .on_hover_text("Remove this node from the DSL source")
                .clicked()
            {
                if let Some(span) = node_span {
                    let src = &self.files[i].source;
                    let new_src = edit::delete_node(src, span);
                    let f = &mut self.files[i];
                    f.source = new_src;
                    f.dirty = f.source != f.last_saved_source;
                    f.needs_compile = true;
                    f.last_edit_at = Some(Instant::now());
                    self.viewer.set_selection(None);
                }
            }
        });
    }

    pub(super) fn ui_summary(&mut self, ui: &mut egui::Ui) {
        let i = self.active;
        let Some(result) = &self.files[i].last_result else {
            ui.label("(no build yet)");
            return;
        };
        let Some(scene) = &result.scene else {
            ui.label("(no scene — fix errors first)");
            return;
        };
        let mut tris = 0usize;
        let mut verts = 0usize;
        let mut meshes = 0usize;
        for n in &scene.nodes {
            if let Some(m) = &n.mesh {
                tris += m.indices.len() / 3;
                verts += m.positions.len();
                meshes += 1;
            }
        }
        ui.label(format!("nodes: {}", scene.nodes.len()));
        ui.label(format!("meshes: {meshes}"));
        ui.label(format!("triangles: {tris}"));
        ui.label(format!("vertices: {verts}"));
        ui.label(format!("materials: {}", scene.materials.len()));
        if !scene.skins.is_empty() {
            ui.label(format!("skins: {}", scene.skins.len()));
        }
        if !scene.clips.is_empty() {
            ui.label(format!("clips: {}", scene.clips.len()));
        }
        if !scene.joints.is_empty() {
            ui.label(format!("joints: {}", scene.joints.len()));
        }

        // Texture roster. Listing each path (resolved against the .mog dir)
        // with a green ✓ / red ✗ lets users verify their files exist without
        // waiting for the export failure. Existence is cached for ~1.5s so
        // we don't stat every PNG every frame.
        // Own `source_dir` so the existence-cache call below can take
        // `&mut self` without overlapping borrows from `self.files[i].path`.
        let source_dir: Option<PathBuf> = self.files[i]
            .path
            .as_deref()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf());
        let texture_slots = gather_texture_refs(scene);
        // Set of referenced PNG absolute paths — used to decide which files
        // sitting in ./textures/ are dead weight the user can sweep out.
        let referenced_abs: std::collections::HashSet<PathBuf> = texture_slots
            .iter()
            .map(|(_, _, rel)| resolve_for_check(rel, source_dir.as_deref()))
            .collect();
        if !texture_slots.is_empty() {
            ui.add_space(8.0);
            ui.label(format!("textures: {}", texture_slots.len()));
            // Pre-resolve and check existence once before the ScrollArea so
            // we don't double-borrow self in the closure.
            let rows: Vec<(String, &'static str, PathBuf, bool)> = texture_slots
                .into_iter()
                .map(|(mat, slot, rel)| {
                    let resolved = resolve_for_check(&rel, source_dir.as_deref());
                    let exists = self.cached_exists(&resolved);
                    (mat, slot, rel, exists)
                })
                .collect();
            egui::ScrollArea::vertical()
                .auto_shrink([false, true])
                .max_height(140.0)
                .show(ui, |ui| {
                    for (mat_name, slot, rel_path, exists) in &rows {
                        let (mark, color) = if *exists {
                            ("✓", egui::Color32::from_rgb(80, 200, 120))
                        } else {
                            ("✗", egui::Color32::from_rgb(230, 100, 100))
                        };
                        ui.horizontal_wrapped(|ui| {
                            ui.colored_label(color, mark);
                            ui.label(format!("{mat_name}.{slot}"));
                            let display = ellipsize_path(rel_path, 36);
                            ui.label(display)
                                .on_hover_text(rel_path.to_string_lossy());
                        });
                    }
                });
        }

        // Unused textures: PNGs sitting in ./textures/ next to the .mog that
        // aren't referenced by any material. These are typically leftovers
        // from earlier generate-textures runs where the material name or
        // style changed. Offer a delete button that also sweeps the
        // companion PBR maps (_normal, _metallicRoughness, _ao) since the
        // textures pipeline always writes them as a group.
        let mut to_delete: Option<PathBuf> = None;
        if let Some(dir) = source_dir.as_deref() {
            let unused = scan_unused_textures(&dir.join("textures"), &referenced_abs);
            if !unused.is_empty() {
                ui.add_space(8.0);
                ui.colored_label(
                    egui::Color32::from_rgb(230, 200, 100),
                    format!("unused textures: {}", unused.len()),
                )
                .on_hover_text(
                    "PNG files in ./textures/ that no material references. \
                     Typically left over from earlier texture runs.",
                );
                egui::ScrollArea::vertical()
                    .auto_shrink([false, true])
                    .id_salt("unused_textures_scroll")
                    .max_height(140.0)
                    .show(ui, |ui| {
                        for path in &unused {
                            ui.horizontal(|ui| {
                                let display = ellipsize_path(path, 32);
                                ui.label(display)
                                    .on_hover_text(path.to_string_lossy());
                                if ui
                                    .small_button("Delete")
                                    .on_hover_text(
                                        "Delete this PNG and its companion PBR maps \
                                         (_normal, _metallicRoughness, _ao) if present",
                                    )
                                    .clicked()
                                {
                                    to_delete = Some(path.clone());
                                }
                            });
                        }
                    });
            }
        }
        if let Some(path) = to_delete {
            let outcome = delete_texture_group(&path);
            // Invalidate the existence cache so the ✓/✗ list refreshes
            // without waiting for the 1.5s TTL.
            self.tex_exists_cache.clear();
            self.active_mut().status = outcome;
        }
    }

    /// Stat-cached existence check. The texture-roster paint runs every frame
    /// so a naive `Path::exists()` would hit the FS once per slot per frame.
    pub(super) fn cached_exists(&mut self, path: &Path) -> bool {
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

    pub(super) fn ui_animation(&mut self, ui: &mut egui::Ui) {
        let clips = self.viewer.clips_snapshot();
        if clips.is_empty() {
            return;
        }

        let mut active = self.viewer.active_clips();
        let mut times = self.viewer.anim_times();
        // A fresh compile can briefly desync the snapshot lengths; pad so
        // iteration below is safe.
        if active.len() != clips.len() {
            active.resize(clips.len(), false);
        }
        if times.len() != clips.len() {
            times.resize(clips.len(), 0.0);
        }

        let playing = self.viewer.is_playing();
        ui.horizontal(|ui| {
            let label = if playing { "⏸ Pause" } else { "▶ Play" };
            if ui
                .button(label)
                .on_hover_text("Toggle clip playback")
                .clicked()
            {
                self.viewer.set_playing(!playing);
            }
            if ui
                .button("Reset")
                .on_hover_text("Rewind every clip to t = 0")
                .clicked()
            {
                self.viewer.reset_anim_times();
            }
            if ui
                .button("All")
                .on_hover_text("Activate every clip")
                .clicked()
            {
                self.viewer.set_all_clips_active(true);
            }
            if ui
                .button("None")
                .on_hover_text("Deactivate every clip")
                .clicked()
            {
                self.viewer.set_all_clips_active(false);
            }
        });

        ui.add_space(4.0);
        let file_i = self.active;
        let mut pending_delete: Option<String> = None;
        for (i, c) in clips.iter().enumerate() {
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    let mut on = active[i];
                    if ui
                        .checkbox(&mut on, "")
                        .on_hover_text("Include this clip in the active pose")
                        .changed()
                    {
                        self.viewer.set_clip_active(i, on);
                    }
                    ui.label(&c.name);
                    ui.label(
                        egui::RichText::new(format!("{:.2}s", c.duration))
                            .small()
                            .weak(),
                    );
                    // Delete: splice the authored clip (or procedural-template
                    // node that produced it) out of the source. Disabled when
                    // no matching AST node is found — multi-target templates
                    // produce `name_0`/`name_1`/… clips whose names don't map
                    // back to a single authored node.
                    let span = find_clip_source_span(&self.files[file_i].source, &c.name);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let btn = egui::Button::new("🗑").small();
                        let resp = ui.add_enabled(span.is_some(), btn).on_hover_text(
                            if span.is_some() {
                                "Delete this clip from the DSL source"
                            } else {
                                "This clip has no authored source to delete\n\
                                 (procedural template with multiple targets)"
                            },
                        );
                        if resp.clicked() {
                            pending_delete = Some(c.name.clone());
                        }
                    });
                });
                let dur = c.duration.max(0.001);
                let mut t = times[i].clamp(0.0, dur);
                let resp = ui.add(
                    egui::Slider::new(&mut t, 0.0..=dur)
                        .text("t")
                        .clamping(egui::SliderClamping::Always)
                        .fixed_decimals(2),
                );
                if resp.changed() {
                    self.viewer.seek_clip(i, t);
                }
            });
        }
        if let Some(name) = pending_delete {
            use crate::edit;
            if let Some(span) = find_clip_source_span(&self.files[file_i].source, &name) {
                let new_src = edit::delete_node(&self.files[file_i].source, span);
                let f = &mut self.files[file_i];
                f.source = new_src;
                f.dirty = f.source != f.last_saved_source;
                f.needs_compile = true;
                f.last_edit_at = Some(Instant::now());
            }
        }
    }

    /// Floating overlay buttons drawn on top of the viewport. Keeps the
    /// camera controls within the user's eye line instead of forcing a trip
    /// to the toolbar.
    pub(super) fn ui_viewport_overlay(&mut self, ctx: &egui::Context, viewport_rect: egui::Rect) {
        use crate::gizmo::GizmoMode;
        egui::Area::new(egui::Id::new("viewport_overlay"))
            .fixed_pos(viewport_rect.left_top() + egui::vec2(8.0, 8.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .fill(ui.visuals().window_fill().linear_multiply(0.85))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let frame_sc = ctx
                                .format_shortcut(&ShortcutAction::Frame.shortcut());
                            if ui
                                .small_button("Frame")
                                .on_hover_text(format!(
                                    "Re-fit the camera to the scene  ({frame_sc})"
                                ))
                                .clicked()
                            {
                                self.viewer.frame_view();
                            }
                            ui.separator();
                            let cur = self.viewer.gizmo_mode();
                            for (label, mode, tip) in [
                                ("T", GizmoMode::Translate, "Translate gizmo"),
                                ("R", GizmoMode::Rotate, "Rotate gizmo"),
                                ("S", GizmoMode::Scale, "Scale gizmo"),
                            ] {
                                let selected = cur == mode;
                                if ui
                                    .selectable_label(selected, label)
                                    .on_hover_text(tip)
                                    .clicked()
                                {
                                    self.viewer.set_gizmo_mode(mode);
                                }
                            }
                            ui.separator();
                            ui.label(
                                egui::RichText::new(
                                    "click: select · drag: orbit · shift+drag/middle/right: pan · scroll: zoom · ctrl: snap",
                                )
                                .weak(),
                            );
                        });
                    });
            });
    }
}
