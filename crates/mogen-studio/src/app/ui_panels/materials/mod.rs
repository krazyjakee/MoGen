use std::path::PathBuf;
use std::time::Instant;

use eframe::egui;

use crate::app::types::UndoKey;
use crate::app::util::{
    delete_material_textures, delete_texture_group, ellipsize_path, find_material_source_span,
    gather_texture_refs, materials_referenced_by_visible_nodes, origin_in_visible_set,
    resolve_for_check, scan_unused_textures, visible_origins,
};
use crate::app::MogenStudioApp;

mod gradient;
mod manage;
mod pbr;
mod textures;

/// Per-material texture-management actions queued during the UI scan and
/// applied after the for-loop so we can take a fresh `&mut self` borrow
/// without fighting the material-clone iteration.
pub(super) enum TexAction {
    Regenerate(String),
    Delete(String),
    Reveal(PathBuf),
}

impl MogenStudioApp {
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
    pub(in crate::app) fn ui_materials(&mut self, ui: &mut egui::Ui) {
        use crate::edit;

        let i = self.active;
        let selection = self.viewer.primary_selection();
        let busy = self.files[i].llm_in_flight.is_some();
        let has_key = self.resolve_api_key().is_some();
        let has_path = self.files[i].path.is_some();
        let src_empty = self.files[i].source.trim().is_empty();
        let tex_enabled = has_key && !busy && !src_empty && has_path;
        let provider_name = self.settings.provider().display_name();
        // Spelled out so the user knows which prerequisite to fix without
        // having to discover them one by one.
        let tex_disabled_reason: Option<String> = if tex_enabled {
            None
        } else {
            let mut blockers: Vec<String> = Vec::new();
            if !has_key {
                blockers.push(format!("set a {provider_name} API key in Preferences"));
            }
            if busy {
                blockers.push("another LLM call is in flight on this tab".into());
            }
            if src_empty {
                blockers.push("open or paste a .mog file first".into());
            }
            if !has_path {
                blockers.push("save the file first — textures writes PNGs next to it".into());
            }
            Some(if blockers.len() == 1 {
                format!("Disabled — {}", blockers[0])
            } else {
                format!("Disabled:\n  • {}", blockers.join("\n  • "))
            })
        };
        let ctx = ui.ctx().clone();
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

        // Scope to the active scene by default; pull in an imported file's
        // materials only when one of its nodes is selected. A material that
        // local geometry references is always visible — the user explicitly
        // asked for that exception so a colour tweak in their scene isn't
        // hidden behind "select an import first".
        let visible = visible_origins(scene, selection);
        let mat_refs = materials_referenced_by_visible_nodes(scene, &visible);
        // Clone so the `&scene` borrow can end before we mutate source.
        let materials: Vec<(usize, mogen_core::Material)> = scene
            .materials
            .iter()
            .enumerate()
            .filter(|(idx, m)| {
                origin_in_visible_set(&m.origin, &visible)
                    || mat_refs.contains(&(*idx as u32))
            })
            .map(|(idx, m)| (idx, m.clone()))
            .collect();
        if materials.is_empty() {
            ui.label("(no materials in this scope)");
            return;
        }
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
        // Per-material texture-management click queued by the per-material
        // thumbnail row; applied after both loops so the action handler can
        // mutate source / spawn workers freely.
        let mut pending_tex_action: Option<TexAction> = None;
        // Material lifecycle actions deferred for the same reason as `pending` —
        // applied after the per-material loop so the borrow on `materials`
        // (cloned slice) doesn't outlive the source mutations.
        let mut pending_delete_material: Option<String> = None;
        let mut pending_rename_material: Option<(String, String)> = None;
        // Per-slot manual texture path: (material_name, slot_attr, new_value | None).
        // None means "clear the slot" (delete attr).
        let mut pending_tex_path: Option<(String, &'static str, Option<String>)> = None;
        // Material name whose `gradient=` attr should be stripped this frame
        // (the gradient editor's "Remove gradient" button). Goes through
        // `delete_attr` rather than the `pending` channel because `set_attr`
        // can't express "drop this attribute entirely".
        let mut pending_gradient_delete: Option<String> = None;
        let mut wants_add_material = false;

        // Add-material affordance — appends a `material "name" { color = … }`
        // declaration to the active scene with a unique numeric suffix so it
        // doesn't shadow an existing entry. Authored above the loop so it
        // sits at the top of the panel where a new-action chip belongs.
        ui.horizontal(|ui| {
            if ui
                .button("+ New material")
                .on_hover_text(
                    "Append a new material declaration to the scene. \
                     The material starts with a neutral grey colour and \
                     can be edited below.",
                )
                .clicked()
            {
                wants_add_material = true;
            }
        });

        // Salt every per-material widget ID with the scene-graph index in
        // addition to the material name. Imports can introduce a second
        // material with the same name (e.g. a local `wood` plus a `wood`
        // hoisted from `import "drawer.mog"`); without the index, both
        // CollapsingHeaders / ComboBoxes share an egui ID and the runtime
        // paints "First/Second use of widget ID …" warnings inline in the
        // panel. Indices are unique by definition so this disambiguates
        // without leaking the origin path into the salt.
        for (idx, mat) in &materials {
            let header_id = egui::Id::new(("mat_editor", *idx, mat.name.as_str()));
            let header_label = match &mat.origin {
                Some(p) => {
                    let stem = p
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("import");
                    egui::RichText::new(format!("{}  ⤴ {stem}", mat.name))
                }
                None => egui::RichText::new(mat.name.clone()),
            };
            egui::CollapsingHeader::new(header_label)
                .id_salt(header_id)
                .default_open(false)
                .show(ui, |ui| {
                    pbr::render(ui, *idx, mat, &mut pending);
                    gradient::render(
                        ui,
                        *idx,
                        mat,
                        &mut pending,
                        &mut pending_gradient_delete,
                    );

                    let mat_slots: Vec<(String, &'static str, PathBuf)> = texture_slots
                        .iter()
                        .filter(|(m, _, _)| m == &mat.name)
                        .cloned()
                        .collect();
                    self.material_textures_section(
                        ui,
                        &ctx,
                        mat,
                        &mat_slots,
                        source_dir.as_deref(),
                        tex_enabled,
                        tex_disabled_reason.as_deref(),
                        &mut pending_tex_action,
                        &mut pending_tex_path,
                    );

                    manage::render(
                        ui,
                        *idx,
                        mat,
                        &mut self.material_name_drafts,
                        &mut pending_delete_material,
                        &mut pending_rename_material,
                    );
                });
        }

        if wants_add_material {
            let suggested = manage::next_material_name(&self.files[i].source);
            let body = format!(
                "material \"{suggested}\" {{\n  color = [0.7, 0.7, 0.7]\n}}",
            );
            let before = self.files[i].source.clone();
            let new_src = crate::edit::append_to_scene(&before, &body);
            if new_src != before {
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
                        surface: "material",
                        attr: Some("__add".into()),
                        node_path: Vec::new(),
                    },
                );
            }
        }

        if let Some(mat_name) = pending_delete_material {
            if let Some(span) = find_material_source_span(&self.files[i].source, &mat_name) {
                let before = self.files[i].source.clone();
                let new_src = crate::edit::delete_node(&before, span);
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
                        surface: "material",
                        attr: Some("__delete".into()),
                        node_path: Vec::new(),
                    },
                );
            }
        }

        if let Some((old_name, new_name)) = pending_rename_material {
            // Two-pass: rewrite the material declaration's name literal, then
            // sweep every `material="<old>"` reference across the source. The
            // reference sweep is a verbatim substring replace bounded by the
            // quote pair so we don't clobber an unrelated token that happens
            // to equal the old name.
            if let Some(span) = find_material_source_span(&self.files[i].source, &old_name) {
                let before = self.files[i].source.clone();
                let with_decl = manage::rewrite_material_decl_name(&before, span, &new_name);
                let new_src = with_decl.replace(
                    &format!("material=\"{old_name}\""),
                    &format!("material=\"{new_name}\""),
                );
                if new_src != before {
                    {
                        let f = &mut self.files[i];
                        f.source = new_src;
                        f.dirty = f.source != f.last_saved_source;
                        f.needs_compile = true;
                        f.last_edit_at = Some(Instant::now());
                    }
                    self.material_name_drafts.remove(&old_name);
                    self.break_undo_chain(i);
                    self.push_undo(
                        i,
                        before,
                        UndoKey {
                            surface: "material",
                            attr: Some("__rename".into()),
                            node_path: Vec::new(),
                        },
                    );
                }
            }
        }

        if let Some(mat_name) = pending_gradient_delete {
            if let Some(span) = find_material_source_span(&self.files[i].source, &mat_name) {
                let before = self.files[i].source.clone();
                let new_src = crate::edit::delete_attr(&before, span, "gradient");
                if new_src != before {
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
                            surface: "material",
                            attr: Some("gradient:__delete".into()),
                            node_path: Vec::new(),
                        },
                    );
                }
            }
        }

        if let Some((mat_name, attr, value)) = pending_tex_path {
            if let Some(span) = find_material_source_span(&self.files[i].source, &mat_name) {
                let before = self.files[i].source.clone();
                let new_src = match value {
                    Some(v) => crate::edit::set_attr(&before, span, attr, &v),
                    None => crate::edit::delete_attr(&before, span, attr),
                };
                if new_src != before {
                    {
                        let f = &mut self.files[i];
                        f.source = new_src;
                        f.dirty = f.source != f.last_saved_source;
                        f.needs_compile = true;
                        f.last_edit_at = Some(Instant::now());
                    }
                    self.tex_exists_cache.clear();
                    self.thumb_cache.clear();
                    self.break_undo_chain(i);
                    self.push_undo(
                        i,
                        before,
                        UndoKey {
                            surface: "material",
                            attr: Some(attr.into()),
                            node_path: Vec::new(),
                        },
                    );
                }
            }
        }

        // Unused textures: PNGs sitting in ./textures/ next to the .mog that
        // aren't referenced by any material. These are typically leftovers
        // from earlier generate-textures runs where the material name or
        // style changed. Offer a delete button that also sweeps the companion
        // PBR maps (_normal, _metallicRoughness, _ao) since the textures
        // pipeline always writes them as a group.
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
                        node_path: Vec::new(),
                    },
                );
                // Immediate recompile so the widgets read the updated
                // material on the very next frame (matches the gizmo-commit
                // pattern in `drain_viewport_edits`). Debouncing causes
                // DragValue drags to snap back to the old value mid-drag.
                self.compile_active();
            }
        }

        // Apply the per-material texture click. Mirrors the right-click
        // actions that previously lived on the LLM panel's thumbnail strip,
        // now wired directly to the buttons under each material.
        if let Some(action) = pending_tex_action {
            match action {
                TexAction::Regenerate(material) => {
                    self.start_llm_textures_for_material(ctx.clone(), material);
                }
                TexAction::Reveal(path) => {
                    let status = match crate::app::editor_link::reveal_in_os(&path) {
                        Ok(()) => format!("revealed {}", path.display()),
                        Err(e) => format!("reveal failed: {} ({e})", path.display()),
                    };
                    self.active_mut().status = status;
                }
                TexAction::Delete(material) => {
                    let source = self.files[i].source.clone();
                    let (new_source, status) = delete_material_textures(
                        &source,
                        source_dir.as_deref(),
                        &material,
                        &texture_slots,
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
                            f.last_edit_at = Some(Instant::now());
                        }
                        // Texture-cleanup deletes PNGs from disk as a side
                        // effect, so this edit is non-undoable like the LLM
                        // completions. Break the coalesce chain so a
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
}
