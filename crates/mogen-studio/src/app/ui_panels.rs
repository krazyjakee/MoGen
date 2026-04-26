use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use eframe::egui;
use mogen_core::Severity;

use crate::pipeline::Stage;

use super::types::{ShortcutAction, UndoKey, TEX_EXISTS_TTL};
use super::util::{
    delete_texture_group, ellipsize_path, find_clip_source_span, find_material_source_span,
    format_inspector_scalar, gather_texture_refs, offset_to_line_col, resolve_for_check,
    scan_unused_textures,
};
use super::MogenStudioApp;

impl MogenStudioApp {
    pub(super) fn ui_editor(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        let i = self.active;
        let editor_id = self.active_editor_id();

        // Lock the editor while an LLM call is in flight — the worker will
        // overwrite `source` on completion, so any keystrokes typed during
        // generation would just be discarded.
        let generating = self.files[i].llm_in_flight.is_some();
        if generating {
            self.autocomplete.close();
        }

        // Consume popup navigation keys BEFORE the TextEdit is rendered — Up /
        // Down / Tab / Enter / Esc are only intercepted when the popup is
        // open, so normal editing isn't affected.
        let popup_key = self.autocomplete_key(ui);

        // Block-indent / dedent on Tab / Shift+Tab when the selection covers
        // multiple lines (or for any Shift+Tab). Runs after autocomplete so an
        // open popup keeps its claim on Tab.
        if self.handle_indent_keys(ui, editor_id) {
            changed = true;
        }

        let font_id = egui::TextStyle::Monospace.resolve(ui.style());
        let palette = crate::highlight::Palette::for_visuals(&ui.style().visuals);

        // Layouter closure — runs on every repaint for the visible text. Kept
        // cheap by the single-pass tokeniser in `highlight`; caching on hash
        // would be nice but isn't needed yet at typical .mog sizes.
        let hl_font = font_id.clone();
        let mut layouter = move |ui: &egui::Ui, text: &str, _wrap_width: f32| {
            // Wrap is disabled in `highlight` — long lines scroll horizontally
            // so the gutter's one-number-per-source-line stays aligned.
            let job = crate::highlight::highlight(text, hl_font.clone(), palette);
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

        let mut textedit_output: Option<egui::widgets::text_edit::TextEditOutput> = None;

        egui::ScrollArea::both()
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

                    // Snapshot the cursor range before the widget runs so the
                    // right-click menu has something to restore — egui
                    // collapses the selection on any secondary press.
                    let prior = egui::TextEdit::load_state(ui.ctx(), editor_id)
                        .and_then(|s| s.cursor.char_range());

                    let mut editor = egui::TextEdit::multiline(&mut self.files[i].source)
                        // code_editor() implies lock_focus(true), so Tab inserts
                        // a tab character instead of moving focus out of the
                        // editor — the right behavior for a code surface.
                        .code_editor()
                        .desired_rows(visible_rows)
                        .desired_width(f32::INFINITY)
                        .font(egui::TextStyle::Monospace)
                        .layouter(&mut layouter)
                        .id(editor_id);
                    if generating {
                        editor = editor.interactive(false);
                    }
                    let output = editor.show(ui);

                    let resp = output.response.clone();
                    if resp.changed() {
                        changed = true;
                    }

                    // Re-assert the pre-press selection on secondary press
                    // so the right-click menu can see what the user had
                    // highlighted.
                    if resp.hovered() && ui.input(|i| i.pointer.secondary_pressed()) {
                        if let Some(range) = prior {
                            if range.primary.index != range.secondary.index {
                                if let Some(mut st) =
                                    egui::TextEdit::load_state(ui.ctx(), editor_id)
                                {
                                    st.cursor.set_char_range(Some(range));
                                    st.store(ui.ctx(), editor_id);
                                }
                            }
                        }
                    }
                    let mut menu_changed = false;
                    // (selected_text, label) captured at click-time so opening
                    // the modal later doesn't have to re-read editor state.
                    let mut ask_request: Option<(String, String)> = None;
                    let source_ref = &mut self.files[i].source;
                    resp.context_menu(|ui| {
                        if super::text_menu::show_context_menu(ui, editor_id, source_ref) {
                            menu_changed = true;
                        }
                        ui.separator();
                        if ui
                            .add(egui::Button::new("Ask…"))
                            .on_hover_text(
                                "Ask Gemini Flash a question about the selected code \
                                 (or the whole file if nothing is selected)",
                            )
                            .clicked()
                        {
                            ask_request = Some(super::ask::capture_snippet(
                                ui,
                                editor_id,
                                source_ref,
                            ));
                            ui.close_menu();
                        }
                    });
                    if menu_changed {
                        changed = true;
                    }
                    if let Some((snippet, label)) = ask_request {
                        self.open_ask_modal(snippet, label);
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

                    textedit_output = Some(output);
                });
            });

        if let Some(ref output) = textedit_output {
            // Refresh candidate list + popup anchor after the TextEdit has
            // rendered. Keyboard navigation decoded before the widget is
            // applied here so the selection/accept lands on the current
            // candidates. Skipped while the LLM owns the buffer so Tab/Enter
            // can't splice a completion into source the worker is about to
            // overwrite.
            if !generating {
                self.update_autocomplete_after_textedit(ui, output, editor_id, popup_key);
            }
        }

        if changed {
            self.files[i].dirty = self.files[i].source != self.files[i].last_saved_source;
            self.files[i].needs_compile = true;
            self.files[i].last_edit_at = Some(Instant::now());
            // The TextEdit owns its own native undo for typing — those edits
            // never enter the app stack. Reset the coalesce window so a
            // subsequent gizmo / inspector edit doesn't merge into a stack
            // entry whose `before` predates the user's typing.
            self.break_undo_chain(i);
            // Compilation itself is gated by `drive_compile_debounce` so a
            // burst of keystrokes only re-parses once the user pauses.
        }
    }

    /// True when the active file has at least one error- or warning-level
    /// diagnostic. Drives the editor's footer panel visibility — info-only
    /// or clean states keep the panel hidden so the editor reclaims the
    /// space.
    pub(super) fn has_blocking_diagnostics(&self) -> bool {
        let Some(result) = &self.files[self.active].last_result else {
            return false;
        };
        result
            .diagnostics
            .iter()
            .any(|d| matches!(d.severity, Severity::Error | Severity::Warning))
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
        if node.relative_placed {
            // The viewport gizmo refuses these for the same reason: a layout
            // pass (attach / pack) recomputes their translation every compile,
            // so a `pos=` writeback would silently snap back. Keep the two
            // input paths consistent rather than offering a transform grid
            // that secretly does nothing.
            ui.add_space(6.0);
            ui.colored_label(
                egui::Color32::from_rgb(230, 200, 100),
                "Placed by attach/pack — translation is recomputed each compile. \
                 Detach or edit the layout spec to free this node.",
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

        // DSL shortcut/corner-form attrs that override the canonical
        // transform field. The inspector strips these on commit for the
        // same reason the gizmo does — otherwise a node authored with `x=`
        // / `from=` / `rx=` shorthand would silently win on recompile and
        // make the just-typed value snap back. Kept in sync with the
        // viewport gizmo's shadow lists in `viewer/state.rs`.
        let pos_shadows: Vec<String> = ["x", "y", "z", "from", "to"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let rot_shadows: Vec<String> =
            ["rx", "ry", "rz"].iter().map(|s| s.to_string()).collect();

        ui.add_space(6.0);
        egui::Grid::new("inspector_transform")
            .num_columns(4)
            .spacing([6.0, 4.0])
            .show(ui, |ui| {
                ui.label("Translate");
                let mut emit_pos = false;
                if ui.add(egui::DragValue::new(&mut tx).speed(0.02)).changed() {
                    emit_pos = true;
                }
                if ui.add(egui::DragValue::new(&mut ty).speed(0.02)).changed() {
                    emit_pos = true;
                }
                if ui.add(egui::DragValue::new(&mut tz).speed(0.02)).changed() {
                    emit_pos = true;
                }
                if emit_pos {
                    // Emit the full `pos=[x,y,z]` vector and strip shadow
                    // attrs. The previous per-axis `x=`/`y=`/`z=` writes
                    // left two attrs fighting in the header (`pos=…` plus a
                    // stale `x=`); whichever won depended on
                    // `transform_from_attrs` resolution order.
                    edits.push(PendingEdit::SetAttrCanonical {
                        node: node_id,
                        attr: "pos".into(),
                        value: format!(
                            "[{}, {}, {}]",
                            format_inspector_scalar(tx),
                            format_inspector_scalar(ty),
                            format_inspector_scalar(tz),
                        ),
                        delete: pos_shadows.clone(),
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
                    edits.push(PendingEdit::SetAttrCanonical {
                        node: node_id,
                        attr: "rot".into(),
                        value: format!(
                            "[{}, {}, {}]",
                            format_inspector_scalar(rx),
                            format_inspector_scalar(ry),
                            format_inspector_scalar(rz),
                        ),
                        delete: rot_shadows.clone(),
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
                    edits.push(PendingEdit::SetAttrCanonical {
                        node: node_id,
                        attr: "scale".into(),
                        // Scale has no DSL shortcut attrs.
                        delete: Vec::new(),
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
                    let before = self.files[i].source.clone();
                    let new_src = edit::duplicate_node(&before, span);
                    {
                        let f = &mut self.files[i];
                        f.source = new_src;
                        f.dirty = f.source != f.last_saved_source;
                        f.needs_compile = true;
                        f.last_edit_at = Some(Instant::now());
                    }
                    // Discrete click — never coalesce with a prior entry.
                    self.break_undo_chain(i);
                    self.push_undo(
                        i,
                        before,
                        UndoKey {
                            surface: "inspector-action",
                            attr: None,
                            node_path: None,
                        },
                    );
                }
            }
            if ui
                .add_enabled(span_ok, egui::Button::new("Delete"))
                .on_hover_text("Remove this node from the DSL source")
                .clicked()
            {
                if let Some(span) = node_span {
                    let before = self.files[i].source.clone();
                    let new_src = edit::delete_node(&before, span);
                    {
                        let f = &mut self.files[i];
                        f.source = new_src;
                        f.dirty = f.source != f.last_saved_source;
                        f.needs_compile = true;
                        f.last_edit_at = Some(Instant::now());
                    }
                    self.break_undo_chain(i);
                    self.push_undo(
                        i,
                        before,
                        UndoKey {
                            surface: "inspector-action",
                            attr: None,
                            node_path: None,
                        },
                    );
                    self.viewer.set_selection(None);
                }
            }
        });
    }

    pub(super) fn ui_summary(&mut self, ui: &mut egui::Ui) {
        use crate::edit;

        let i = self.active;
        let counts = {
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
            (
                scene.nodes.len(),
                meshes,
                tris,
                verts,
                scene.materials.len(),
                scene.skins.len(),
                scene.clips.len(),
                scene.joints.len(),
            )
        };
        let (nodes, meshes, tris, verts, mats, skins, clips, joints) = counts;

        ui.label(format!("nodes: {nodes}"));
        ui.label(format!("meshes: {meshes}"));
        ui.label(format!("triangles: {tris}"));
        ui.label(format!("vertices: {verts}"));
        ui.label(format!("materials: {mats}"));
        if skins > 0 {
            ui.label(format!("skins: {skins}"));
        }
        if clips > 0 {
            ui.label(format!("clips: {clips}"));
        }
        if joints > 0 {
            ui.label(format!("joints: {joints}"));
        }

        // Polygon count slider — multiplies primitive default segment/ring
        // counts at lower-time. Reads the live value out of source so it
        // stays in sync if the user edits the directive in the text editor.
        let current = edit::get_lod_scale(&self.files[i].source).unwrap_or(1.0);
        let mut draft = current;
        ui.add_space(6.0);
        let resp = ui
            .add(
                egui::Slider::new(&mut draft, 0.25..=4.0)
                    .text("LOD scale")
                    .logarithmic(true)
                    .fixed_decimals(2),
            )
            .on_hover_text(
                "Multiplies primitive default segment/ring counts.\n\
                 Per-primitive `segments=`/`rings=` overrides still win.",
            );
        if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
            let snapped = (draft * 100.0).round() / 100.0;
            if (snapped - current).abs() > 1e-3 {
                let before = self.files[i].source.clone();
                let new_src = edit::set_lod_scale(&before, snapped);
                {
                    let f = &mut self.files[i];
                    f.source = new_src;
                    f.dirty = f.source != f.last_saved_source;
                    f.needs_compile = true;
                    f.last_edit_at = Some(Instant::now());
                }
                // Slider release is already a discrete event (drag_stopped
                // gate above), so each release is one undoable step.
                self.break_undo_chain(i);
                self.push_undo(
                    i,
                    before,
                    UndoKey {
                        surface: "lod",
                        attr: None,
                        node_path: None,
                    },
                );
            }
        }
    }

    /// Per-material editor panel. Each authored material gets a collapsing
    /// group exposing its PBR values (colour, metallic, roughness, emissive,
    /// transmission, alpha, uv) plus its texture slots with ✓/✗ existence
    /// marks. Edits are spliced straight into the `.mog` source via span-aware
    /// `edit::set_attr`, then an immediate recompile keeps the viewport + the
    /// widgets' bound values in sync with the compiled scene (same pattern
    /// the gizmo commits use — debouncing would flicker during drags).
    ///
    /// Also houses the "unused textures" cleanup list: PNGs sitting in
    /// `./textures/` that no material references.
    pub(super) fn ui_materials(&mut self, ui: &mut egui::Ui) {
        use crate::edit;
        use mogen_core::{AlphaMode, UvMode};

        let i = self.active;
        let Some(result) = &self.files[i].last_result else {
            ui.label("(no build yet)");
            return;
        };
        let Some(scene) = &result.scene else {
            ui.label("(no scene — fix errors first)");
            return;
        };
        if scene.materials.is_empty() {
            ui.label("(no materials declared)");
            return;
        }

        // Clone so the `&scene` borrow can end before we mutate source.
        let materials: Vec<mogen_core::Material> = scene.materials.clone();
        let texture_slots = gather_texture_refs(scene);

        let source_dir: Option<PathBuf> = self.files[i]
            .path
            .as_deref()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf());
        let referenced_abs: std::collections::HashSet<PathBuf> = texture_slots
            .iter()
            .map(|(_, _, rel)| resolve_for_check(rel, source_dir.as_deref()))
            .collect();

        // Edits collected during the UI scan, applied in a second pass so we
        // don't clash with the material clone borrow. Each entry spans a
        // single attr rewrite on the named material.
        let mut pending: Vec<(String, &'static str, String)> = Vec::new();

        for mat in &materials {
            let header_id = egui::Id::new(("mat_editor", mat.name.as_str()));
            egui::CollapsingHeader::new(&mat.name)
                .id_salt(header_id)
                .default_open(false)
                .show(ui, |ui| {
                    // Colour + alpha
                    ui.horizontal(|ui| {
                        ui.label("Color");
                        let mut rgb = [
                            mat.base_color[0],
                            mat.base_color[1],
                            mat.base_color[2],
                        ];
                        if ui.color_edit_button_rgb(&mut rgb).changed() {
                            pending.push((
                                mat.name.clone(),
                                "color",
                                format!(
                                    "[{}, {}, {}]",
                                    format_inspector_scalar(rgb[0]),
                                    format_inspector_scalar(rgb[1]),
                                    format_inspector_scalar(rgb[2]),
                                ),
                            ));
                        }
                        let mut alpha = mat.base_color[3];
                        if ui
                            .add(
                                egui::DragValue::new(&mut alpha)
                                    .speed(0.01)
                                    .range(0.0..=1.0)
                                    .prefix("α "),
                            )
                            .changed()
                        {
                            pending.push((
                                mat.name.clone(),
                                "alpha",
                                format_inspector_scalar(alpha),
                            ));
                        }
                    });

                    // Metallic / Roughness
                    ui.horizontal(|ui| {
                        let mut metallic = mat.metallic;
                        if ui
                            .add(
                                egui::Slider::new(&mut metallic, 0.0..=1.0)
                                    .text("metallic")
                                    .fixed_decimals(2),
                            )
                            .changed()
                        {
                            pending.push((
                                mat.name.clone(),
                                "metallic",
                                format_inspector_scalar(metallic),
                            ));
                        }
                    });
                    ui.horizontal(|ui| {
                        let mut rough = mat.roughness;
                        if ui
                            .add(
                                egui::Slider::new(&mut rough, 0.0..=1.0)
                                    .text("roughness")
                                    .fixed_decimals(2),
                            )
                            .changed()
                        {
                            pending.push((
                                mat.name.clone(),
                                "roughness",
                                format_inspector_scalar(rough),
                            ));
                        }
                    });

                    // Normal / AO strength — apply when the textures pipeline
                    // derives PBR maps for this material. Authored normal/AO
                    // textures ignore these.
                    ui.horizontal(|ui| {
                        let mut ns = mat.normal_strength;
                        if ui
                            .add(
                                egui::Slider::new(&mut ns, 0.0..=8.0)
                                    .text("normal strength")
                                    .fixed_decimals(2),
                            )
                            .on_hover_text(
                                "Slope multiplier baked into the derived normal map \
                                 by `mogen textures`. Larger = more pronounced bumps. \
                                 Ignored when `normal_texture` is authored directly.",
                            )
                            .changed()
                        {
                            pending.push((
                                mat.name.clone(),
                                "normal_strength",
                                format_inspector_scalar(ns),
                            ));
                        }
                    });
                    ui.horizontal(|ui| {
                        let mut os = mat.occlusion_strength;
                        if ui
                            .add(
                                egui::Slider::new(&mut os, 0.0..=1.0)
                                    .text("AO strength")
                                    .fixed_decimals(2),
                            )
                            .on_hover_text(
                                "How dark the derived ambient-occlusion map can get. \
                                 0 = flat white (no darkening), 1 = cavities reach black. \
                                 Ignored when `occlusion_texture` is authored directly.",
                            )
                            .changed()
                        {
                            pending.push((
                                mat.name.clone(),
                                "occlusion_strength",
                                format_inspector_scalar(os),
                            ));
                        }
                    });

                    // Emissive colour + HDR strength
                    ui.horizontal(|ui| {
                        ui.label("Emissive");
                        let mut em = mat.emissive;
                        if ui.color_edit_button_rgb(&mut em).changed() {
                            pending.push((
                                mat.name.clone(),
                                "emissive",
                                format!(
                                    "[{}, {}, {}]",
                                    format_inspector_scalar(em[0]),
                                    format_inspector_scalar(em[1]),
                                    format_inspector_scalar(em[2]),
                                ),
                            ));
                        }
                        let mut strength = mat.emissive_strength;
                        if ui
                            .add(
                                egui::DragValue::new(&mut strength)
                                    .speed(0.05)
                                    .range(0.0..=64.0)
                                    .prefix("×"),
                            )
                            .on_hover_text(
                                "HDR emissive multiplier — values > 1 drive bloom in renderers \
                                 that honour KHR_materials_emissive_strength",
                            )
                            .changed()
                        {
                            pending.push((
                                mat.name.clone(),
                                "emissive_strength",
                                format_inspector_scalar(strength),
                            ));
                        }
                    });

                    // Transmission
                    ui.horizontal(|ui| {
                        let mut trans = mat.transmission;
                        if ui
                            .add(
                                egui::Slider::new(&mut trans, 0.0..=1.0)
                                    .text("transmission")
                                    .fixed_decimals(2),
                            )
                            .on_hover_text(
                                "Fraction of light passing through the surface \
                                 (KHR_materials_transmission) — glass and water",
                            )
                            .changed()
                        {
                            pending.push((
                                mat.name.clone(),
                                "transmission",
                                format_inspector_scalar(trans),
                            ));
                        }
                    });

                    // Alpha mode + cutoff
                    ui.horizontal(|ui| {
                        ui.label("Alpha mode");
                        let mut mode = mat.alpha_mode;
                        let mode_id = egui::Id::new(("alpha_mode", mat.name.as_str()));
                        egui::ComboBox::from_id_salt(mode_id)
                            .selected_text(match mode {
                                AlphaMode::Opaque => "opaque",
                                AlphaMode::Blend => "blend",
                                AlphaMode::Mask => "mask",
                            })
                            .show_ui(ui, |ui| {
                                let mut changed = false;
                                changed |= ui
                                    .selectable_value(&mut mode, AlphaMode::Opaque, "opaque")
                                    .changed();
                                changed |= ui
                                    .selectable_value(&mut mode, AlphaMode::Blend, "blend")
                                    .changed();
                                changed |= ui
                                    .selectable_value(&mut mode, AlphaMode::Mask, "mask")
                                    .changed();
                                if changed {
                                    let v = match mode {
                                        AlphaMode::Opaque => "\"opaque\"",
                                        AlphaMode::Blend => "\"blend\"",
                                        AlphaMode::Mask => "\"mask\"",
                                    };
                                    pending.push((
                                        mat.name.clone(),
                                        "alpha_mode",
                                        v.to_string(),
                                    ));
                                }
                            });
                        if matches!(mode, AlphaMode::Mask) {
                            let mut cutoff = mat.alpha_cutoff;
                            if ui
                                .add(
                                    egui::DragValue::new(&mut cutoff)
                                        .speed(0.01)
                                        .range(0.0..=1.0)
                                        .prefix("cutoff "),
                                )
                                .changed()
                            {
                                pending.push((
                                    mat.name.clone(),
                                    "alpha_cutoff",
                                    format_inspector_scalar(cutoff),
                                ));
                            }
                        }
                    });

                    // Double-sided
                    {
                        let mut ds = mat.double_sided;
                        if ui
                            .checkbox(&mut ds, "Double sided")
                            .on_hover_text(
                                "Draw both triangle faces (glTF doubleSided). \
                                 Use for leaves, fins, flags, cloth",
                            )
                            .changed()
                        {
                            pending.push((
                                mat.name.clone(),
                                "double_sided",
                                if ds { "1".into() } else { "0".into() },
                            ));
                        }
                    }

                    // UV mode + scale
                    ui.horizontal(|ui| {
                        ui.label("UV");
                        let mut uv = mat.uv_mode;
                        let uv_id = egui::Id::new(("uv_mode", mat.name.as_str()));
                        egui::ComboBox::from_id_salt(uv_id)
                            .selected_text(match uv {
                                UvMode::Tile => "tile",
                                UvMode::Fit => "fit",
                            })
                            .show_ui(ui, |ui| {
                                let mut changed = false;
                                changed |= ui
                                    .selectable_value(&mut uv, UvMode::Tile, "tile")
                                    .changed();
                                changed |= ui
                                    .selectable_value(&mut uv, UvMode::Fit, "fit")
                                    .changed();
                                if changed {
                                    let v = match uv {
                                        UvMode::Tile => "\"tile\"",
                                        UvMode::Fit => "\"fit\"",
                                    };
                                    pending.push((
                                        mat.name.clone(),
                                        "uv_mode",
                                        v.to_string(),
                                    ));
                                }
                            });
                        let mut us = mat.uv_scale[0];
                        let mut vs = mat.uv_scale[1];
                        let mut uv_changed = false;
                        if ui
                            .add(egui::DragValue::new(&mut us).speed(0.05).prefix("u "))
                            .changed()
                        {
                            uv_changed = true;
                        }
                        if ui
                            .add(egui::DragValue::new(&mut vs).speed(0.05).prefix("v "))
                            .changed()
                        {
                            uv_changed = true;
                        }
                        if uv_changed {
                            pending.push((
                                mat.name.clone(),
                                "uv_scale",
                                format!(
                                    "[{}, {}]",
                                    format_inspector_scalar(us),
                                    format_inspector_scalar(vs),
                                ),
                            ));
                        }
                    });

                    // Texture slot roster for this material — same ✓/✗
                    // existence check as before, nested under its owner so
                    // the relationship is obvious.
                    let mat_slots: Vec<(String, &'static str, PathBuf)> = texture_slots
                        .iter()
                        .filter(|(m, _, _)| m == &mat.name)
                        .cloned()
                        .collect();
                    if !mat_slots.is_empty() {
                        ui.add_space(4.0);
                        ui.label(egui::RichText::new("textures").weak());
                        for (_, slot, rel_path) in &mat_slots {
                            let resolved = resolve_for_check(rel_path, source_dir.as_deref());
                            let exists = self.cached_exists(&resolved);
                            let (mark, color) = if exists {
                                ("✓", egui::Color32::from_rgb(80, 200, 120))
                            } else {
                                ("✗", egui::Color32::from_rgb(230, 100, 100))
                            };
                            ui.horizontal_wrapped(|ui| {
                                ui.colored_label(color, mark);
                                ui.label(*slot);
                                let display = ellipsize_path(rel_path, 30);
                                ui.label(display)
                                    .on_hover_text(rel_path.to_string_lossy());
                            });
                        }
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
            self.tex_exists_cache.clear();
            self.active_mut().status = outcome;
        }

        // Apply material edits. Re-parse between each one so the splice
        // offsets stay valid after prior inserts shift later attrs. A
        // material without a locatable span (e.g. coming from an imported
        // module) silently skips — the widget state rolls back on the next
        // frame when the compiled scene is re-read.
        if !pending.is_empty() {
            let undo_before = self.files[i].source.clone();
            let mut source = undo_before.clone();
            let mut any_applied = false;
            // Track the last (material, attr) pair in the batch — drives the
            // coalesce key so a continuous DragValue / colour-picker drag on
            // one slot collapses into a single undo entry.
            let mut last_target: Option<(String, &'static str)> = None;
            for (mat_name, attr, value) in pending {
                let Some(span) = find_material_source_span(&source, &mat_name) else {
                    continue;
                };
                source = edit::set_attr(&source, span, attr, &value);
                last_target = Some((mat_name, attr));
                any_applied = true;
            }
            if any_applied {
                {
                    let f = &mut self.files[i];
                    f.source = source;
                    f.dirty = f.source != f.last_saved_source;
                    f.needs_compile = true;
                    f.last_edit_at = Some(Instant::now());
                }
                let coalesce_attr = last_target
                    .as_ref()
                    .map(|(name, attr)| format!("{name}:{attr}"));
                self.push_undo(
                    i,
                    undo_before,
                    UndoKey {
                        surface: "material",
                        attr: coalesce_attr,
                        node_path: None,
                    },
                );
                // Immediate recompile so the widgets read the updated
                // material on the very next frame (matches the gizmo-commit
                // pattern in `drain_viewport_edits`). Debouncing causes
                // DragValue drags to snap back to the old value mid-drag.
                self.compile_active();
            }
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

        ui.horizontal(|ui| {
            let mut speed = self.viewer.playback_speed();
            ui.label("Speed");
            let resp = ui.add(
                egui::Slider::new(&mut speed, -2.0..=4.0)
                    .suffix("×")
                    .fixed_decimals(2)
                    .clamping(egui::SliderClamping::Always),
            );
            if resp.changed() {
                self.viewer.set_playback_speed(speed);
            }
            if ui
                .button("1×")
                .on_hover_text("Reset playback speed to real time")
                .clicked()
                && (speed - 1.0).abs() > f32::EPSILON
            {
                self.viewer.set_playback_speed(1.0);
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
                let before = self.files[file_i].source.clone();
                let new_src = edit::delete_node(&before, span);
                {
                    let f = &mut self.files[file_i];
                    f.source = new_src;
                    f.dirty = f.source != f.last_saved_source;
                    f.needs_compile = true;
                    f.last_edit_at = Some(Instant::now());
                }
                self.break_undo_chain(file_i);
                self.push_undo(
                    file_i,
                    before,
                    UndoKey {
                        surface: "animation",
                        attr: None,
                        node_path: None,
                    },
                );
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
                            let cinema_on = self.viewer.is_cinema_active();
                            ui.add_enabled_ui(!cinema_on, |ui| {
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
                            });
                            ui.separator();
                            // Cinema mode: orbit/pan/zoom + gizmo + grid all
                            // suppressed while on, so its toggle stays
                            // outside the disabled group.
                            if ui
                                .selectable_label(cinema_on, "🎬 Cinema")
                                .on_hover_text(if cinema_on {
                                    "Stop cinema mode and restore the previous camera"
                                } else {
                                    "Play an automated sequence of camera shots"
                                })
                                .clicked()
                            {
                                self.viewer.set_cinema_active(!cinema_on);
                            }
                            ui.separator();
                            if cinema_on {
                                if let Some(name) = self.viewer.cinema_shot_label() {
                                    ui.label(
                                        egui::RichText::new(format!("now: {name}")).weak(),
                                    );
                                }
                            } else {
                                ui.label(
                                    egui::RichText::new(
                                        "click: select · drag: orbit · shift+drag/middle/right: pan · scroll: zoom · ctrl: snap",
                                    )
                                    .weak(),
                                );
                            }
                        });
                    });
            });
    }
}
