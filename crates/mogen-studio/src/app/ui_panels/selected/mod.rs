use std::time::Instant;

use eframe::egui;

use crate::app::types::UndoKey;
use crate::app::MogenStudioApp;

mod add_attr;
mod connections;
mod deform_rows;
mod geom_params;
mod light_editor;
mod modifiers;
mod node_problems;
mod transform_grid;
mod use_wrap;

use deform_rows::{
    deform_angle_row, deform_faceted_row, deform_seed_row, deform_taper_row, deform_unit_row,
};
use geom_params::geom_params_for_kind;
use use_wrap::{resolve_use_wrap_target, rewrite_node_kind};

impl MogenStudioApp {
    /// Inspector for the currently-selected scene node. Shown above the
    /// scene summary; collapses to a friendly hint when nothing's selected
    /// or when the selection came from a replicator / CSG expansion.
    pub(in crate::app) fn ui_selected(&mut self, ui: &mut egui::Ui) {
        use crate::edit;
        use crate::viewer::PendingEdit;

        let Some(sel) = self.viewer.primary_selection() else {
            ui.label("(click a node in the 3D view to select it)");
            return;
        };
        // Multi-select hint: tell the user the inspector is editing the
        // primary (most-recently-selected) node; the others come along for
        // delete/highlight only. Without this, a shift-click that adds to
        // the selection looks identical in the inspector and the user can't
        // tell the inspector is intentionally pinned to the primary.
        let selected_count = self.viewer.all_selected().len();
        if selected_count > 1 {
            ui.colored_label(
                egui::Color32::from_rgb(170, 200, 240),
                format!("{selected_count} nodes selected — editing primary"),
            )
            .on_hover_text(
                "Shift/Cmd-click adds nodes to the selection. The inspector \
                 shows the most recently clicked node; Delete removes every \
                 selected node.",
            );
            // Spell out which node is primary vs secondary so the user can
            // see at a glance whose attributes the inspector is editing.
            // Pulled from the viewer-side scene snapshot rather than the
            // file's last_result so the names line up with what's painted
            // in the viewport.
            if let Some(scene_arc) = self
                .files
                .get(self.active)
                .and_then(|f| f.last_result.as_ref())
                .and_then(|r| r.scene.as_ref())
            {
                let all = self.viewer.all_selected();
                ui.horizontal_wrapped(|ui| {
                    for (idx, id) in all.iter().enumerate() {
                        let is_primary = idx + 1 == all.len();
                        let name = scene_arc
                            .nodes
                            .get(id.0 as usize)
                            .map(|n| n.name.as_str())
                            .unwrap_or("(stale)");
                        let prefix = if is_primary { "★ " } else { "" };
                        let label = format!("{prefix}{name}");
                        let rich = if is_primary {
                            egui::RichText::new(label).strong()
                        } else {
                            egui::RichText::new(label).weak()
                        };
                        ui.label(rich);
                    }
                });
            }
            ui.add_space(4.0);
        }
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
        if let Some(p) = &node.origin {
            // Make the cross-file provenance discoverable: scoping the
            // sidebar to a specific import is otherwise invisible — without
            // this badge a user wouldn't know why Materials/Animation just
            // grew when they clicked an imported node.
            let stem = p
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("import");
            ui.horizontal(|ui| {
                ui.label("Source:");
                ui.colored_label(
                    egui::Color32::from_rgb(170, 200, 240),
                    format!("⤴ {stem}"),
                )
                .on_hover_text(format!("Imported from {}", p.display()));
            });
        }

        // Read-only relationship navigator. Placed before the editability
        // gates so it works for array/CSG/imported nodes too — navigating
        // *away* from a non-editable node to its parent is exactly what the
        // user wants there. A click only re-targets the selection; no source
        // is touched, so this is safe for every node kind.
        if let Some(target) = connections::render(ui, scene, sel, node) {
            self.viewer.set_primary_selection(Some(target));
        }

        // Validator problems scoped to this node — shown for every kind
        // (including non-editable array/CSG copies) so the user sees why a
        // selection is flagged without scanning the global footer.
        node_problems::render(ui, &result.diagnostics, node.source_span);

        if !node.editable {
            ui.add_space(6.0);
            ui.colored_label(
                egui::Color32::from_rgb(230, 200, 100),
                "Derived from array/mirror/CSG — edit the parent in the text.",
            );
            return;
        }
        if node.use_id.is_some() && !crate::viewer::is_import_wrapper(scene, sel) {
            // Selection landed on an imported-module node. `replace_selection`
            // normally redirects picks to the nearest user-authored wrapper,
            // so this only fires when there is no wrapper to redirect to
            // (e.g. `scene { use "desk" }` with the `use` directly under
            // `scene`). Surface the constraint instead of offering a
            // transform grid that would write back into the imported file.
            //
            // The wrapper of `use "X" (pos=...)` for an imported file is
            // exempt: its source span is the `use` line in the active
            // source, so the inspector's transform grid writes back
            // through `set_attr` cleanly.
            // Resolve the `use "..."` call's source span and a sensible group
            // name BEFORE the warning + button are drawn. Imported (origin=
            // Some) nodes match by the origin file's stem; local-module
            // (origin=None) nodes walk up to the closest user-authored
            // ancestor and pick the first `use` AST child within its span.
            // The lookup is run here so the borrows on `node`/`scene` end
            // before the click handler takes a `&mut self.files[i]`.
            let active_source = self.files[self.active].source.clone();
            let wrap_target = resolve_use_wrap_target(scene, sel, node, &active_source);
            ui.add_space(6.0);
            ui.colored_label(
                egui::Color32::from_rgb(230, 200, 100),
                "Imported via `use` — wrap the `use` in a group to edit its \
                 transform here.",
            );
            let (button, hover) = match &wrap_target {
                Some(_) => (
                    egui::Button::new("Wrap `use` in a group"),
                    "Splice a `group \"<name>\" { … }` around the matching `use` \
                     line in the source so its transform becomes editable here.",
                ),
                None => (
                    egui::Button::new("Wrap `use` in a group"),
                    "Couldn't locate the originating `use` line in the active \
                     source — wrap it manually by editing the text.",
                ),
            };
            let wrap_clicked = ui
                .add_enabled(wrap_target.is_some(), button)
                .on_hover_text(hover)
                .clicked();
            if wrap_clicked {
                if let Some((use_span, group_name)) = wrap_target {
                    let i = self.active;
                    let before = self.files[i].source.clone();
                    let new_src = crate::edit::wrap_node_in_group(
                        &before,
                        use_span,
                        &group_name,
                    );
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
                                surface: "inspector-action",
                                attr: None,
                                node_path: Vec::new(),
                            },
                        );
                    }
                }
            }
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

        let node_span = node.source_span;
        let node_id = sel;
        // Snapshot of connectors taken early so the connector-list section
        // further down can render without keeping the `&node` borrow alive
        // through the wants_* mutation handlers.
        let connectors_snap: Vec<mogen_core::Connector> = node.connectors.clone();

        let mut edits: Vec<PendingEdit> = Vec::new();

        // Gizmo-mode toggle + translate/rotate/scale grid.
        let src_for_tg = self.files[i].source.clone();
        transform_grid::render(
            ui,
            &self.viewer,
            node,
            &mut self.inspector_scale_linked,
            node_id,
            &src_for_tg,
            node_span,
            &mut edits,
        );

        // Material picker — pick from the scene's authored materials and
        // write `material="<name>"` back into the node header. Skip nodes that
        // can't carry a material (lights, groups without geometry handled by
        // attribute being valid in DSL anyway).
        let mut wants_clear_material = false;
        if node.light.is_none() {
            let mat_names: Vec<String> =
                scene.materials.iter().map(|m| m.name.clone()).collect();
            let current_mat: Option<String> = node
                .material
                .and_then(|id| scene.materials.get(id.0 as usize))
                .map(|m| m.name.clone());
            if !mat_names.is_empty() {
                ui.add_space(8.0);
                ui.separator();
                ui.label(egui::RichText::new("Material").strong());
                ui.horizontal(|ui| {
                    let label = current_mat
                        .clone()
                        .unwrap_or_else(|| "(inherit)".to_string());
                    egui::ComboBox::from_id_salt(("inspector_material", node_id.0))
                        .selected_text(label)
                        .show_ui(ui, |ui| {
                            // "(inherit)" clears `material=` and lets the parent's
                            // material flow down — the lowering pass already
                            // propagates parent material when child omits it.
                            let none_selected = current_mat.is_none();
                            if ui
                                .selectable_label(none_selected, "(inherit)")
                                .clicked()
                                && !none_selected
                            {
                                wants_clear_material = true;
                            }
                            for name in &mat_names {
                                let selected = current_mat.as_deref() == Some(name.as_str());
                                if ui
                                    .selectable_label(selected, name)
                                    .clicked()
                                    && !selected
                                {
                                    edits.push(PendingEdit::SetAttrCanonical {
                                        node: node_id,
                                        attr: "material".into(),
                                        value: format!("\"{name}\""),
                                        delete: Vec::new(),
                                    });
                                }
                            }
                        });
                });
            }
        }

        // Primitive geometry parameters (kind-switched). Show scalar attrs the
        // primitive lowering pass actually consumes — see
        // `mogen_dsl::lower::primitive`. List-shaped attrs (points/profile/path)
        // are skipped here; they need a richer editor than a sidebar grid.
        if let Some(span) = node_span {
            let src_view = self.files[i].source.clone();
            let mut params_to_emit: Vec<(&'static str, String)> = Vec::new();
            let mut shown_any = false;
            ui.add_space(8.0);
            ui.separator();
            ui.label(egui::RichText::new("Geometry").strong());
            egui::Grid::new(("inspector_geom", node_id.0))
                .num_columns(2)
                .spacing([6.0, 4.0])
                .show(ui, |ui| {
                    shown_any = geom_params_for_kind(
                        ui,
                        node.kind.as_str(),
                        &src_view,
                        span,
                        &mut params_to_emit,
                    );
                });
            if !shown_any {
                ui.label(
                    egui::RichText::new("(no editable params for this kind)")
                        .italics()
                        .weak(),
                );
            }
            for (attr, value) in params_to_emit {
                edits.push(PendingEdit::SetAttrCanonical {
                    node: node_id,
                    attr: attr.into(),
                    value,
                    delete: Vec::new(),
                });
            }

            // Schema-driven picker for placement/metadata attrs that have no
            // dedicated widget (anchor / from-to / relative placement / lod /
            // role / tags). Emits through the same span-aware edit path.
            let kind = node.kind.clone();
            let add_src = self.files[i].source.clone();
            add_attr::render(ui, &kind, &add_src, span, node_id, &mut edits);
        }

        // Cave debug toggles. `cave` is a generator wrapper, not a primitive, so
        // it has no geometry grid — surface its preview-only flags here. The
        // checkbox writes `debug_show_poi=1`/`0` through the same span-aware
        // edit path so it round-trips with the text editor and undo stack.
        if node.kind == "cave" {
            if let Some(span) = node_span {
                // Cave colliders are managed per-surface by the generator (the
                // shell + decorations get trimesh colliders); these knobs pick
                // which surfaces collide rather than the generic AABB toggle.
                ui.add_space(8.0);
                ui.separator();
                ui.label(egui::RichText::new("Collider").strong());
                let cur_mode = crate::edit::get_attr(&self.files[i].source, span, "colliders")
                    .map(|v| v.trim().trim_matches('"').to_string())
                    .unwrap_or_else(|| "all".to_string());
                ui.horizontal(|ui| {
                    ui.label("Surfaces");
                    egui::ComboBox::from_id_salt(("cave_colliders", node_id.0))
                        .selected_text(&cur_mode)
                        .show_ui(ui, |ui| {
                            for (opt, hint) in [
                                ("all", "Rock shell + every solid decoration"),
                                ("shell", "Only the outer rock shell; decorations walk-through"),
                                ("none", "No colliders on any cave geometry"),
                            ] {
                                let selected = cur_mode == opt;
                                if ui
                                    .selectable_label(selected, opt)
                                    .on_hover_text(hint)
                                    .clicked()
                                    && !selected
                                {
                                    edits.push(PendingEdit::SetAttrCanonical {
                                        node: node_id,
                                        attr: "colliders".into(),
                                        value: format!("\"{opt}\""),
                                        delete: Vec::new(),
                                    });
                                }
                            }
                        });
                });
                let mut water_collider =
                    crate::edit::get_attr(&self.files[i].source, span, "water_collider")
                        .map(|v| v.trim().parse::<f32>().map(|n| n.abs() > 0.5).unwrap_or(false))
                        .unwrap_or(false);
                if ui
                    .checkbox(&mut water_collider, "Water is solid")
                    .on_hover_text(
                        "Give pools and lakes a trimesh collider so a player \
                         stands on the surface. Off by default so water is \
                         wadeable.",
                    )
                    .changed()
                {
                    edits.push(PendingEdit::SetAttrCanonical {
                        node: node_id,
                        attr: "water_collider".into(),
                        value: if water_collider { "1" } else { "0" }.into(),
                        delete: Vec::new(),
                    });
                }

                let mut show_poi = crate::edit::get_attr(&self.files[i].source, span, "debug_show_poi")
                    .map(|v| v.trim().parse::<f32>().map(|n| n.abs() > 0.5).unwrap_or(false))
                    .unwrap_or(false);
                ui.add_space(8.0);
                ui.separator();
                ui.label(egui::RichText::new("Debug").strong());
                if ui
                    .checkbox(&mut show_poi, "Show POI markers")
                    .on_hover_text(
                        "Give every point-of-interest marker (dead-end chambers, \
                         column bases, ladder anchors, mushroom spots) a small \
                         bright sphere so the otherwise-empty markers are visible \
                         in the preview. Off for production bakes.",
                    )
                    .changed()
                {
                    edits.push(PendingEdit::SetAttrCanonical {
                        node: node_id,
                        attr: "debug_show_poi".into(),
                        value: if show_poi { "1" } else { "0" }.into(),
                        delete: Vec::new(),
                    });
                }

                let mut hide_shell =
                    crate::edit::get_attr(&self.files[i].source, span, "debug_hide_shell")
                        .map(|v| v.trim().parse::<f32>().map(|n| n.abs() > 0.5).unwrap_or(false))
                        .unwrap_or(false);
                if ui
                    .checkbox(&mut hide_shell, "Hide outer shell")
                    .on_hover_text(
                        "Slice the front (+Z) half of the rock shell away so the \
                         chambers are visible in cross-section. Off for production \
                         bakes.",
                    )
                    .changed()
                {
                    edits.push(PendingEdit::SetAttrCanonical {
                        node: node_id,
                        attr: "debug_hide_shell".into(),
                        value: if hide_shell { "1" } else { "0" }.into(),
                        delete: Vec::new(),
                    });
                }
            }
        }

        // CSG op switch + array / mirror modifier rows.
        let mut change_kind_to: Option<&'static str> = None;
        {
            let src_view = self.files[i].source.clone();
            modifiers::render(
                ui,
                node,
                node_span,
                &src_view,
                node_id,
                &mut edits,
                &mut change_kind_to,
            );
        }

        // Mesh path picker — a `kind="mesh"` node loads a `.glb` from disk via
        // its `src=` attribute. Show the current path with a Browse button so
        // the user can repoint it without leaving the inspector.
        let mut wants_pick_mesh = false;
        if node.kind == "mesh" {
            ui.add_space(8.0);
            ui.separator();
            ui.label(egui::RichText::new("Mesh source").strong());
            if let Some(span) = node_span {
                let cur_src = crate::edit::get_attr(&self.files[i].source, span, "src")
                    .map(|s| s.trim_matches('"').to_string())
                    .unwrap_or_default();
                ui.horizontal(|ui| {
                    if cur_src.is_empty() {
                        ui.colored_label(
                            egui::Color32::from_rgb(230, 200, 100),
                            "(no path set)",
                        );
                    } else {
                        ui.label(
                            egui::RichText::new(&cur_src)
                                .monospace()
                                .weak(),
                        )
                        .on_hover_text(&cur_src);
                    }
                    if ui
                        .small_button("Browse…")
                        .on_hover_text(
                            "Pick a .glb file. Path is stored relative to the .mog when possible.",
                        )
                        .clicked()
                    {
                        wants_pick_mesh = true;
                    }
                });
            }
        }

        // Shadow casting toggle — present on every editable node that isn't a
        // light. `cast_shadow` defaults to true at lower time, so the absence
        // of an attribute reads as "casts shadow"; toggling off writes
        // `cast_shadow=0` (number, matching the `faceted` convention) and
        // toggling back on deletes the attribute so the source stays clean.
        // The lowering pass propagates `false` down the subtree, so flipping
        // this on a group disables shadows for every descendant mesh.
        let mut wants_set_cast_shadow_off = false;
        let mut wants_remove_cast_shadow = false;
        if node.light.is_none() {
            ui.add_space(8.0);
            ui.separator();
            ui.label(egui::RichText::new("Shadow").strong());
            let mut cast = node.cast_shadow;
            if ui
                .checkbox(&mut cast, "Cast shadow")
                .on_hover_text(
                    "Whether this node (and its subtree) contributes to the \
                     realtime shadow pre-pass. When off, MoGen also writes \
                     `extras.cast_shadow=false` to the exported glTF node so \
                     downstream importers (Godot etc.) can mirror the choice.",
                )
                .changed()
            {
                if cast {
                    wants_remove_cast_shadow = true;
                } else {
                    wants_set_cast_shadow_off = true;
                }
            }
        }

        // Collider editor — single checkbox toggling `collider="aabb"` on
        // the node. Skipped for `light` nodes since the validator rejects
        // `collider=` there (lights have no AABB to enclose). Skipped for
        // `cave` too: the cave manages its own per-surface trimesh colliders
        // via `colliders=` (handled in the cave block above), and an AABB on
        // the wrapper would be a solid box enclosing the hollow cave.
        let collider_present = node.collider.is_some();
        let collider_aabb = node.collider.as_ref().and_then(|c| c.as_aabb());
        let mut wants_set_collider = false;
        let mut wants_remove_collider = false;
        if node.light.is_none() && node.kind != "cave" {
            ui.add_space(8.0);
            ui.separator();
            ui.label(egui::RichText::new("Collider").strong());
            let mut on = collider_present;
            if ui
                .checkbox(&mut on, "AABB")
                .on_hover_text(
                    "Mark this node as a collider. The AABB is derived from \
                     the node's subtree mesh extents at compile time and \
                     written to the .glb as `extras.collider`.",
                )
                .changed()
            {
                if on {
                    wants_set_collider = true;
                } else {
                    wants_remove_collider = true;
                }
            }
            if let Some(aabb) = collider_aabb {
                let extent = aabb.max - aabb.min;
                ui.label(format!(
                    "  size: [{:.3}, {:.3}, {:.3}]",
                    extent.x, extent.y, extent.z
                ));
                let center = (aabb.min + aabb.max) * 0.5;
                ui.label(format!(
                    "  center: [{:.3}, {:.3}, {:.3}]",
                    center.x, center.y, center.z
                ));
            } else if collider_present {
                // Should be unreachable — the field tracks the source attr.
                // Left as a defensive label so an empty subtree (collider
                // requested but no mesh) reads as a tooltip rather than a
                // blank panel.
                ui.colored_label(
                    egui::Color32::from_rgb(230, 200, 100),
                    "  (no mesh in subtree — AABB skipped)",
                );
            }
        }

        // Deform modifiers — variety knobs (`noise`, `jitter`, bend/twist/
        // taper/droop, faceted) that the lowering pipeline applies between
        // primitive construction and anchor shift. Gated to nodes whose mesh
        // actually flows through `apply_deform`: skip loaded `.glb` meshes
        // (`kind="mesh"`), top-level CSG results, and `solid` / group-style
        // nodes that don't carry a primitive mesh of their own.
        let supports_deform = node.mesh.is_some()
            && node.kind != "mesh"
            && !matches!(
                node.kind.as_str(),
                "union" | "difference" | "intersect" | "solid"
            );
        let mut wants_remove_deform: Vec<&'static str> = Vec::new();
        if supports_deform {
            if let Some(span) = node_span {
                let src = &self.files[i].source;
                let read_f32 = |attr: &str| -> Option<f32> {
                    crate::edit::get_attr(src, span, attr)
                        .and_then(|s| s.parse::<f32>().ok())
                };
                let cur_noise = read_f32("noise");
                let cur_jitter = read_f32("jitter");
                let cur_bend_x = read_f32("bend_x");
                let cur_bend_y = read_f32("bend_y");
                let cur_bend_z = read_f32("bend_z");
                let cur_twist_y = read_f32("twist_y");
                let cur_taper = read_f32("taper");
                let cur_droop = read_f32("droop");
                let cur_faceted = read_f32("faceted");
                let cur_seed = crate::edit::get_attr(src, span, "seed")
                    .and_then(|s| s.parse::<u32>().ok());
                let any_set = cur_noise.is_some()
                    || cur_jitter.is_some()
                    || cur_bend_x.is_some()
                    || cur_bend_y.is_some()
                    || cur_bend_z.is_some()
                    || cur_twist_y.is_some()
                    || cur_taper.is_some()
                    || cur_droop.is_some()
                    || cur_faceted.is_some()
                    || cur_seed.is_some();

                ui.add_space(8.0);
                ui.separator();
                egui::CollapsingHeader::new("Deform")
                    .id_salt("inspector_deform_header")
                    .default_open(any_set)
                    .show(ui, |ui| {
                        egui::Grid::new("inspector_deform")
                            .num_columns(2)
                            .spacing([6.0, 4.0])
                            .show(ui, |ui| {
                                deform_unit_row(
                                    ui, "Noise", "noise", cur_noise, 0.3,
                                    node_id, &mut edits, &mut wants_remove_deform,
                                );
                                deform_unit_row(
                                    ui, "Jitter", "jitter", cur_jitter, 0.3,
                                    node_id, &mut edits, &mut wants_remove_deform,
                                );
                                deform_angle_row(
                                    ui, "Bend X", "bend_x", cur_bend_x, 15.0,
                                    node_id, &mut edits, &mut wants_remove_deform,
                                );
                                deform_angle_row(
                                    ui, "Bend Y", "bend_y", cur_bend_y, 15.0,
                                    node_id, &mut edits, &mut wants_remove_deform,
                                );
                                deform_angle_row(
                                    ui, "Bend Z", "bend_z", cur_bend_z, 15.0,
                                    node_id, &mut edits, &mut wants_remove_deform,
                                );
                                deform_angle_row(
                                    ui, "Twist Y", "twist_y", cur_twist_y, 30.0,
                                    node_id, &mut edits, &mut wants_remove_deform,
                                );
                                deform_taper_row(
                                    ui, cur_taper, node_id,
                                    &mut edits, &mut wants_remove_deform,
                                );
                                deform_unit_row(
                                    ui, "Droop", "droop", cur_droop, 0.3,
                                    node_id, &mut edits, &mut wants_remove_deform,
                                );
                                deform_faceted_row(
                                    ui, cur_faceted, node_id,
                                    &mut edits, &mut wants_remove_deform,
                                );
                                deform_seed_row(
                                    ui, cur_seed, node_id,
                                    &mut edits, &mut wants_remove_deform,
                                );
                            });
                    });
            }
        }

        // Light editor — punctual lights expose kind/colour/intensity (and the
        // kind-conditional range / cone angles) through the same span-aware
        // edit pipeline as the transform grid.
        let mut wants_remove_range = false;
        if let Some(light) = node.light.as_ref() {
            wants_remove_range = light_editor::render(ui, light, node_id, &mut edits);
        }
        if wants_remove_range {
            if let Some(span) = node_span {
                let before = self.files[i].source.clone();
                let new_src = crate::edit::delete_attr(&before, span, "range");
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
                        node_path: Vec::new(),
                    },
                );
            }
        }

        // Connector list — read-only summary of what frames the node exposes
        // for `attach`. Synthesised AABB connectors (the six face anchors
        // every mesh gets for free) are tagged so the user can see at a
        // glance which entries the lowering pass added vs. what they
        // authored. Snapshot is taken below `node`'s last UI use so the
        // wants-* mutation handlers further down can take `&mut self`
        // without a borrow conflict.
        if !connectors_snap.is_empty() {
            ui.add_space(8.0);
            ui.separator();
            egui::CollapsingHeader::new(format!("Connectors ({})", connectors_snap.len()))
                .id_salt(("inspector_connectors", node_id.0))
                .default_open(false)
                .show(ui, |ui| {
                    egui::Grid::new(("inspector_conn_grid", node_id.0))
                        .num_columns(3)
                        .spacing([8.0, 2.0])
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new("name").strong().weak());
                            ui.label(egui::RichText::new("tag").strong().weak());
                            ui.label(egui::RichText::new("pos").strong().weak());
                            ui.end_row();
                            for c in &connectors_snap {
                                let synthesised = c.source_span.is_none();
                                let name_label = if synthesised {
                                    egui::RichText::new(format!("{} ⓢ", c.name)).weak()
                                } else {
                                    egui::RichText::new(&c.name).monospace()
                                };
                                ui.label(name_label).on_hover_text(if synthesised {
                                    "Synthesised from the AABB face anchors — no DSL declaration to edit."
                                } else {
                                    "Authored connector — edit the `connector` line in the source."
                                });
                                ui.label(egui::RichText::new(&c.tag).monospace());
                                ui.label(format!(
                                    "[{:.2}, {:.2}, {:.2}]",
                                    c.pos.x, c.pos.y, c.pos.z
                                ));
                                ui.end_row();
                            }
                        });
                });
        }

        if wants_set_cast_shadow_off {
            edits.push(PendingEdit::SetAttrCanonical {
                node: node_id,
                attr: "cast_shadow".into(),
                value: "0".into(),
                delete: Vec::new(),
            });
        }
        if wants_remove_cast_shadow {
            if let Some(span) = node_span {
                let before = self.files[i].source.clone();
                let new_src = crate::edit::delete_attr(&before, span, "cast_shadow");
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
                        attr: Some("cast_shadow".into()),
                        node_path: Vec::new(),
                    },
                );
            }
        }

        if wants_set_collider {
            edits.push(PendingEdit::SetAttrCanonical {
                node: node_id,
                attr: "collider".into(),
                value: "\"aabb\"".into(),
                delete: Vec::new(),
            });
        }
        if wants_remove_collider {
            if let Some(span) = node_span {
                let before = self.files[i].source.clone();
                let new_src = crate::edit::delete_attr(&before, span, "collider");
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
                        attr: Some("collider".into()),
                        node_path: Vec::new(),
                    },
                );
            }
        }

        for attr in wants_remove_deform {
            if let Some(span) = node_span {
                let before = self.files[i].source.clone();
                let new_src = crate::edit::delete_attr(&before, span, attr);
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
                        attr: Some(attr.into()),
                        node_path: Vec::new(),
                    },
                );
            }
        }

        // Apply CSG kind rewrite. We can't reuse PendingEdit (it only mutates
        // attrs); rewrite the kind keyword in the node header directly.
        if let Some(new_kind) = change_kind_to {
            if let Some(span) = node_span {
                let before = self.files[i].source.clone();
                let new_src = rewrite_node_kind(&before, span, new_kind);
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
                        attr: Some("kind".into()),
                        node_path: Vec::new(),
                    },
                );
            }
        }

        if wants_clear_material {
            if let Some(span) = node_span {
                let before = self.files[i].source.clone();
                let new_src = crate::edit::delete_attr(&before, span, "material");
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
                        attr: Some("material".into()),
                        node_path: Vec::new(),
                    },
                );
            }
        }

        if wants_pick_mesh && node_span.is_some() {
            {
                if let Some(picked) = rfd::FileDialog::new()
                    .add_filter("glTF binary", &["glb"])
                    .set_directory(
                        self.files[i]
                            .path
                            .as_deref()
                            .and_then(|p| p.parent())
                            .map(|p| p.to_path_buf())
                            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default()),
                    )
                    .pick_file()
                {
                    // Store the path relative to the .mog when possible — the
                    // mesh loader resolves `src=` against the source file's
                    // directory, so a relative path is portable.
                    let rel = self.files[i]
                        .path
                        .as_deref()
                        .and_then(|p| p.parent())
                        .and_then(|base| picked.strip_prefix(base).ok().map(|p| p.to_path_buf()))
                        .unwrap_or_else(|| picked.clone());
                    let value = format!("\"{}\"", rel.to_string_lossy());
                    edits.push(PendingEdit::SetAttrCanonical {
                        node: node_id,
                        attr: "src".into(),
                        value,
                        delete: Vec::new(),
                    });
                }
            }
        }

        let any_edits = !edits.is_empty();
        for edit in edits {
            self.viewer.push_pending_edit(edit);
        }
        // Mirror the viewport-pick behaviour: when an inspector field commits
        // a transform / attribute write, jump the editor caret to the node's
        // declaration so the user can see what just changed in source. The
        // span comes from the lowered scene graph and is None for derived
        // nodes (CSG output, replicators) — they have nothing to point at.
        if any_edits {
            if let Some(span) = node_span {
                let i = self.active;
                self.files[i].pending_caret = Some(span.start);
            }
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
                            node_path: Vec::new(),
                        },
                    );
                }
            }
            // Multi-select aware delete: removes every node in the current
            // selection, not just the inspector's primary. Spans come from
            // the last compile result and are applied right-to-left so
            // earlier byte offsets stay valid as later regions are removed —
            // same reason `drain_viewport_edits` does the sort. Disabled
            // when no spans resolve (rare; only in stale-selection-after-
            // failed-compile cases).
            let all_selected = self.viewer.all_selected();
            let delete_label = if all_selected.len() > 1 {
                format!("Delete {} nodes", all_selected.len())
            } else {
                "Delete".to_string()
            };
            let mut delete_spans: Vec<mogen_core::Span> = Vec::new();
            if let Some(result) = &self.files[i].last_result {
                for n in &all_selected {
                    if let Some(s) = result
                        .node_spans
                        .get(n.0 as usize)
                        .and_then(|s| *s)
                    {
                        delete_spans.push(s);
                    }
                }
            }
            // If the user shift-selected a parent and a descendant, the
            // parent's delete already removes the descendant — keep only
            // the outermost spans so the right-to-left pass below can't
            // fire a stale child-span delete after its parent is gone.
            let mut delete_spans = edit::dedup_contained_spans(&delete_spans);
            delete_spans.sort_by(|a, b| b.start.cmp(&a.start));
            let delete_ok = !delete_spans.is_empty();
            let hover_text = if all_selected.len() > 1 {
                "Remove every selected node from the DSL source"
            } else {
                "Remove this node from the DSL source"
            };
            if ui
                .add_enabled(delete_ok, egui::Button::new(delete_label))
                .on_hover_text(hover_text)
                .clicked()
                && delete_ok
            {
                let before = self.files[i].source.clone();
                let mut src = before.clone();
                for span in &delete_spans {
                    src = edit::delete_node(&src, *span);
                }
                {
                    let f = &mut self.files[i];
                    f.source = src;
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
                        node_path: Vec::new(),
                    },
                );
                self.viewer.set_primary_selection(None);
            }
        });
    }
}
