use std::time::Instant;

use eframe::egui;

use crate::app::types::UndoKey;
use crate::app::util::format_inspector_scalar;
use crate::app::MogenStudioApp;

impl MogenStudioApp {
    /// Inspector for the currently-selected scene node. Shown above the
    /// scene summary; collapses to a friendly hint when nothing's selected
    /// or when the selection came from a replicator / CSG expansion.
    pub(in crate::app) fn ui_selected(&mut self, ui: &mut egui::Ui) {
        use crate::edit;
        use crate::gizmo::GizmoMode;
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
            ui.add_space(6.0);
            ui.colored_label(
                egui::Color32::from_rgb(230, 200, 100),
                "Imported via `use` — wrap the `use` in a group to edit its \
                 transform here.",
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

        // For attached nodes the live transform is `attach + user`. Show
        // the user-authored portion so the inspector reflects what's in
        // the source — and so a writeback doesn't double-count the attach
        // contribution on the next compile.
        let (effective_translation, effective_rotation) = match node.attach_binding.as_ref() {
            Some(b) => (
                node.transform.translation - b.anchor_vec3(),
                node.transform.rotation * b.rotation_quat().inverse(),
            ),
            None => (node.transform.translation, node.transform.rotation),
        };
        let t_scale = node.transform.scale;
        let (rx_rad, ry_rad, rz_rad) = effective_rotation.to_euler(glam::EulerRot::XYZ);
        let mut tx = effective_translation.x;
        let mut ty = effective_translation.y;
        let mut tz = effective_translation.z;
        let mut rx = rx_rad.to_degrees();
        let mut ry = ry_rad.to_degrees();
        let mut rz = rz_rad.to_degrees();
        let mut sx = t_scale.x;
        let mut sy = t_scale.y;
        let mut sz = t_scale.z;
        let node_span = node.source_span;
        let node_id = sel;
        // Snapshot of connectors taken early so the connector-list section
        // further down can render without keeping the `&node` borrow alive
        // through the wants_* mutation handlers.
        let connectors_snap: Vec<mogen_core::Connector> = node.connectors.clone();

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
                let pre_sx = sx;
                let pre_sy = sy;
                let pre_sz = sz;
                let linked = self.inspector_scale_linked;
                let mut changed_axis: Option<u8> = None;
                if ui.add(egui::DragValue::new(&mut sx).speed(0.02)).changed() {
                    changed_axis = Some(0);
                }
                if ui.add(egui::DragValue::new(&mut sy).speed(0.02)).changed() {
                    changed_axis = Some(1);
                }
                if ui.add(egui::DragValue::new(&mut sz).speed(0.02)).changed() {
                    changed_axis = Some(2);
                }
                if let Some(axis) = changed_axis {
                    if linked {
                        // Multiply the other two axes by the same ratio the
                        // dragged axis just took, falling back to uniform
                        // when the old value is ~0 — otherwise the others
                        // would stay at 0 and silently swallow the drag.
                        let (new_v, old_v) = match axis {
                            0 => (sx, pre_sx),
                            1 => (sy, pre_sy),
                            _ => (sz, pre_sz),
                        };
                        if old_v.abs() > 1.0e-6 {
                            let ratio = new_v / old_v;
                            sx = pre_sx * ratio;
                            sy = pre_sy * ratio;
                            sz = pre_sz * ratio;
                        } else {
                            sx = new_v;
                            sy = new_v;
                            sz = new_v;
                        }
                    }
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
                let link_label = if linked { "🔗" } else { "🔓" };
                let link_tip = if linked {
                    "Scale axes linked — drag any axis to scale all three (click to unlink)"
                } else {
                    "Scale axes independent (click to link)"
                };
                if ui
                    .selectable_label(linked, link_label)
                    .on_hover_text(link_tip)
                    .clicked()
                {
                    self.inspector_scale_linked = !linked;
                }
                ui.end_row();
            });

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
        }

        // CSG op switch — flip union/difference/intersect on the same node
        // without retyping. Lowering keys off `node.kind`, so we use a
        // dedicated PendingEdit::ChangeKind via a span rewrite below.
        let mut change_kind_to: Option<&'static str> = None;
        if matches!(node.kind.as_str(), "union" | "difference" | "intersect") {
            ui.add_space(8.0);
            ui.separator();
            ui.label(egui::RichText::new("CSG op").strong());
            let cur = node.kind.as_str();
            ui.horizontal(|ui| {
                for k in ["union", "difference", "intersect"] {
                    let selected = cur == k;
                    if ui.selectable_label(selected, k).clicked() && !selected {
                        change_kind_to = Some(match k {
                            "union" => "union",
                            "difference" => "difference",
                            "intersect" => "intersect",
                            _ => unreachable!(),
                        });
                    }
                }
            });
        }

        // Array / mirror modifier rows. Scalar count/axis/start_angle for
        // arrays; axis-only for mirror. Replicators are not editable in the
        // inspector when their wrapper isn't `editable`, but the wrapper
        // *itself* stays editable per `lower::layout` so this still applies.
        match node.kind.as_str() {
            "array" => {
                ui.add_space(8.0);
                ui.separator();
                ui.label(egui::RichText::new("Array").strong());
                if let Some(span) = node_span {
                    let src_view = self.files[i].source.clone();
                    let cur_count = crate::edit::get_attr(&src_view, span, "count")
                        .and_then(|s| s.parse::<f32>().ok())
                        .unwrap_or(1.0);
                    let cur_around = crate::edit::get_attr(&src_view, span, "around")
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "Y".into());
                    let cur_start = crate::edit::get_attr(&src_view, span, "start_angle")
                        .and_then(|s| s.parse::<f32>().ok())
                        .unwrap_or(0.0);
                    egui::Grid::new(("inspector_array", node_id.0))
                        .num_columns(2)
                        .spacing([6.0, 4.0])
                        .show(ui, |ui| {
                            ui.label("Count");
                            let mut count = cur_count.max(1.0);
                            if ui
                                .add(egui::DragValue::new(&mut count).speed(0.1).range(1.0..=512.0))
                                .changed()
                            {
                                edits.push(PendingEdit::SetAttrCanonical {
                                    node: node_id,
                                    attr: "count".into(),
                                    value: (count.round() as i32).to_string(),
                                    delete: Vec::new(),
                                });
                            }
                            ui.end_row();

                            ui.label("Around");
                            let mut around = cur_around.clone();
                            egui::ComboBox::from_id_salt(("inspector_array_axis", node_id.0))
                                .selected_text(around.as_str())
                                .show_ui(ui, |ui| {
                                    for axis in ["X", "Y", "Z"] {
                                        if ui
                                            .selectable_value(&mut around, axis.into(), axis)
                                            .clicked()
                                            && around.as_str() != cur_around.as_str()
                                        {
                                            edits.push(PendingEdit::SetAttrCanonical {
                                                node: node_id,
                                                attr: "around".into(),
                                                value: axis.into(),
                                                delete: Vec::new(),
                                            });
                                        }
                                    }
                                });
                            ui.end_row();

                            ui.label("Start °");
                            let mut start = cur_start;
                            if ui
                                .add(egui::DragValue::new(&mut start).speed(0.5).suffix("°"))
                                .changed()
                            {
                                edits.push(PendingEdit::SetAttrCanonical {
                                    node: node_id,
                                    attr: "start_angle".into(),
                                    value: format_inspector_scalar(start),
                                    delete: Vec::new(),
                                });
                            }
                            ui.end_row();
                        });
                }
            }
            "mirror" => {
                ui.add_space(8.0);
                ui.separator();
                ui.label(egui::RichText::new("Mirror").strong());
                if let Some(span) = node_span {
                    let src_view = self.files[i].source.clone();
                    let cur_axis = crate::edit::get_attr(&src_view, span, "axis")
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "X".into());
                    ui.horizontal(|ui| {
                        ui.label("Axis");
                        let mut axis = cur_axis.clone();
                        egui::ComboBox::from_id_salt(("inspector_mirror_axis", node_id.0))
                            .selected_text(axis.as_str())
                            .show_ui(ui, |ui| {
                                for a in ["X", "Y", "Z"] {
                                    if ui
                                        .selectable_value(&mut axis, a.into(), a)
                                        .clicked()
                                        && axis.as_str() != cur_axis.as_str()
                                    {
                                        edits.push(PendingEdit::SetAttrCanonical {
                                            node: node_id,
                                            attr: "axis".into(),
                                            value: a.into(),
                                            delete: Vec::new(),
                                        });
                                    }
                                }
                            });
                    });
                }
            }
            _ => {}
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
        // `collider=` there (lights have no AABB to enclose).
        let collider_present = node.collider.is_some();
        let collider_aabb = node.collider;
        let mut wants_set_collider = false;
        let mut wants_remove_collider = false;
        if node.light.is_none() {
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
        // edit pipeline as the transform grid. A kind switch carries a
        // `delete` list so attrs that no longer apply (`range` for
        // directional, `inner_cone` / `outer_cone` for non-spot) don't sit
        // around poisoning the next compile with a validation error.
        if let Some(light) = node.light.as_ref() {
            use mogen_core::LightKind;

            // Copy light fields into locals so the grid closure doesn't keep
            // a borrow on `self.files` — the "remove range" branch needs to
            // call `&mut self` methods (push_undo / break_undo_chain) and
            // would otherwise collide with the outer `node` borrow.
            let kind = light.kind;
            let light_color = light.color;
            let light_intensity = light.intensity;
            let light_range = light.range;
            let light_inner_deg = light.inner_cone_rad.to_degrees();
            let light_outer_deg = light.outer_cone_rad.to_degrees();
            let mut wants_remove_range = false;

            ui.add_space(8.0);
            ui.separator();
            ui.label(egui::RichText::new("Light").strong());

            egui::Grid::new("inspector_light")
                .num_columns(2)
                .spacing([6.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Kind");
                    let mut new_kind = kind;
                    egui::ComboBox::from_id_salt("light_kind")
                        .selected_text(match new_kind {
                            LightKind::Directional => "directional",
                            LightKind::Point => "point",
                            LightKind::Spot => "spot",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut new_kind,
                                LightKind::Directional,
                                "directional",
                            );
                            ui.selectable_value(&mut new_kind, LightKind::Point, "point");
                            ui.selectable_value(&mut new_kind, LightKind::Spot, "spot");
                        });
                    if new_kind != kind {
                        let mut to_delete: Vec<String> = Vec::new();
                        if matches!(new_kind, LightKind::Directional) {
                            to_delete.push("range".into());
                        }
                        if !matches!(new_kind, LightKind::Spot) {
                            to_delete.push("inner_cone".into());
                            to_delete.push("outer_cone".into());
                        }
                        let dsl = match new_kind {
                            LightKind::Directional => "directional",
                            LightKind::Point => "point",
                            LightKind::Spot => "spot",
                        };
                        edits.push(PendingEdit::SetAttrCanonical {
                            node: node_id,
                            attr: "kind".into(),
                            value: dsl.into(),
                            delete: to_delete,
                        });
                    }
                    ui.end_row();

                    ui.label("Color");
                    let mut color = light_color;
                    if ui.color_edit_button_rgb(&mut color).changed() {
                        edits.push(PendingEdit::SetAttrCanonical {
                            node: node_id,
                            attr: "color".into(),
                            value: format!(
                                "[{}, {}, {}]",
                                format_inspector_scalar(color[0]),
                                format_inspector_scalar(color[1]),
                                format_inspector_scalar(color[2]),
                            ),
                            delete: Vec::new(),
                        });
                    }
                    ui.end_row();

                    ui.label("Intensity");
                    let mut intensity = light_intensity;
                    let suffix = if matches!(kind, LightKind::Directional) {
                        " lx"
                    } else {
                        " cd"
                    };
                    if ui
                        .add(
                            egui::DragValue::new(&mut intensity)
                                .speed(0.1)
                                .suffix(suffix)
                                .range(0.0..=f32::INFINITY),
                        )
                        .changed()
                    {
                        edits.push(PendingEdit::SetAttrCanonical {
                            node: node_id,
                            attr: "intensity".into(),
                            value: format_inspector_scalar(intensity),
                            delete: Vec::new(),
                        });
                    }
                    ui.end_row();

                    if matches!(kind, LightKind::Point | LightKind::Spot) {
                        ui.label("Range");
                        match light_range {
                            Some(r) => {
                                let mut range = r;
                                ui.horizontal(|ui| {
                                    if ui
                                        .add(
                                            egui::DragValue::new(&mut range)
                                                .speed(0.1)
                                                .range(0.001..=f32::INFINITY),
                                        )
                                        .changed()
                                    {
                                        edits.push(PendingEdit::SetAttrCanonical {
                                            node: node_id,
                                            attr: "range".into(),
                                            value: format_inspector_scalar(range),
                                            delete: Vec::new(),
                                        });
                                    }
                                    if ui
                                        .small_button("✕")
                                        .on_hover_text("Remove range (unlimited)")
                                        .clicked()
                                    {
                                        // PendingEdit can only set+delete-
                                        // shadows; do the primary-attr delete
                                        // outside the closure to avoid a
                                        // double borrow on `self.files`.
                                        wants_remove_range = true;
                                    }
                                });
                            }
                            None => {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new("(unlimited)").italics().weak(),
                                    );
                                    if ui.small_button("+ set").clicked() {
                                        edits.push(PendingEdit::SetAttrCanonical {
                                            node: node_id,
                                            attr: "range".into(),
                                            value: "8".into(),
                                            delete: Vec::new(),
                                        });
                                    }
                                });
                            }
                        }
                        ui.end_row();
                    }

                    if matches!(kind, LightKind::Spot) {
                        ui.label("Inner cone");
                        let mut inner = light_inner_deg;
                        // Spec: 0 ≤ inner ≤ outer ≤ 90°. Outer cone clamps
                        // separately below; inner shares the same cap.
                        if ui
                            .add(
                                egui::DragValue::new(&mut inner)
                                    .speed(0.5)
                                    .suffix("°")
                                    .range(0.0..=90.0),
                            )
                            .changed()
                        {
                            edits.push(PendingEdit::SetAttrCanonical {
                                node: node_id,
                                attr: "inner_cone".into(),
                                value: format_inspector_scalar(inner),
                                delete: Vec::new(),
                            });
                        }
                        ui.end_row();

                        ui.label("Outer cone");
                        let mut outer = light_outer_deg;
                        if ui
                            .add(
                                egui::DragValue::new(&mut outer)
                                    .speed(0.5)
                                    .suffix("°")
                                    .range(0.0..=90.0),
                            )
                            .changed()
                        {
                            edits.push(PendingEdit::SetAttrCanonical {
                                node: node_id,
                                attr: "outer_cone".into(),
                                value: format_inspector_scalar(outer),
                                delete: Vec::new(),
                            });
                        }
                        ui.end_row();
                    }
                });

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

/// Render a Deform-row for a 0..=1 unit modifier (`noise`, `jitter`, `droop`).
/// `default` is the value used when the user clicks "+ add" on an absent attr.
#[allow(clippy::too_many_arguments)]
fn deform_unit_row(
    ui: &mut egui::Ui,
    label: &str,
    attr: &'static str,
    current: Option<f32>,
    default: f32,
    node_id: mogen_core::NodeId,
    edits: &mut Vec<crate::viewer::PendingEdit>,
    wants_remove: &mut Vec<&'static str>,
) {
    deform_scalar_row(
        ui, label, attr, current, default,
        0.0..=1.0, 0.01, "", node_id, edits, wants_remove,
    );
}

/// Render a Deform-row for an angle modifier (`bend_x`/`y`/`z`, `twist_y`),
/// authored in degrees. The lowering pass runs `.to_radians()` before applying.
#[allow(clippy::too_many_arguments)]
fn deform_angle_row(
    ui: &mut egui::Ui,
    label: &str,
    attr: &'static str,
    current: Option<f32>,
    default: f32,
    node_id: mogen_core::NodeId,
    edits: &mut Vec<crate::viewer::PendingEdit>,
    wants_remove: &mut Vec<&'static str>,
) {
    deform_scalar_row(
        ui, label, attr, current, default,
        -180.0..=180.0, 0.5, "°", node_id, edits, wants_remove,
    );
}

/// Render the `taper` row. Different range/default from the unit row because
/// taper's neutral is 1.0 (no scale change) and authors flare past 1.0 for
/// trumpet shapes — clamping to [0, 1] would silently drop that case.
fn deform_taper_row(
    ui: &mut egui::Ui,
    current: Option<f32>,
    node_id: mogen_core::NodeId,
    edits: &mut Vec<crate::viewer::PendingEdit>,
    wants_remove: &mut Vec<&'static str>,
) {
    deform_scalar_row(
        ui, "Taper", "taper", current, 0.5,
        0.0..=4.0, 0.02, "", node_id, edits, wants_remove,
    );
}

#[allow(clippy::too_many_arguments)]
fn deform_scalar_row(
    ui: &mut egui::Ui,
    label: &str,
    attr: &'static str,
    current: Option<f32>,
    default: f32,
    range: std::ops::RangeInclusive<f32>,
    speed: f32,
    suffix: &str,
    node_id: mogen_core::NodeId,
    edits: &mut Vec<crate::viewer::PendingEdit>,
    wants_remove: &mut Vec<&'static str>,
) {
    use crate::app::util::format_inspector_scalar;
    use crate::viewer::PendingEdit;
    ui.label(label);
    ui.horizontal(|ui| match current {
        Some(initial) => {
            let mut v = initial;
            let resp = ui.add(
                egui::DragValue::new(&mut v)
                    .speed(speed)
                    .suffix(suffix)
                    .range(range),
            );
            if resp.changed() {
                edits.push(PendingEdit::SetAttrCanonical {
                    node: node_id,
                    attr: attr.into(),
                    value: format_inspector_scalar(v),
                    delete: Vec::new(),
                });
            }
            if ui
                .small_button("✕")
                .on_hover_text("Remove modifier")
                .clicked()
            {
                wants_remove.push(attr);
            }
        }
        None => {
            ui.label(egui::RichText::new("(none)").italics().weak());
            if ui.small_button("+ add").clicked() {
                edits.push(PendingEdit::SetAttrCanonical {
                    node: node_id,
                    attr: attr.into(),
                    value: format_inspector_scalar(default),
                    delete: Vec::new(),
                });
            }
        }
    });
    ui.end_row();
}

/// Render the `faceted` row as a checkbox: present-and-non-zero is on, absent
/// or `0` is off. Toggling on writes `faceted=1`; toggling off removes the
/// attr entirely so it doesn't sit at `0` polluting the source.
fn deform_faceted_row(
    ui: &mut egui::Ui,
    current: Option<f32>,
    node_id: mogen_core::NodeId,
    edits: &mut Vec<crate::viewer::PendingEdit>,
    wants_remove: &mut Vec<&'static str>,
) {
    use crate::viewer::PendingEdit;
    ui.label("Faceted");
    ui.horizontal(|ui| {
        let on = current.map(|v| v != 0.0).unwrap_or(false);
        let mut new_on = on;
        ui.checkbox(&mut new_on, "")
            .on_hover_text("Hard-edge per-triangle normals (low-poly look).");
        if new_on != on {
            if new_on {
                edits.push(PendingEdit::SetAttrCanonical {
                    node: node_id,
                    attr: "faceted".into(),
                    value: "1".into(),
                    delete: Vec::new(),
                });
            } else if current.is_some() {
                wants_remove.push("faceted");
            }
        }
    });
    ui.end_row();
}

/// Render the `seed` row. Drives the random stream for `noise` / `jitter`;
/// authors typically tweak it to roll a different stochastic result while
/// keeping the modifier amounts the same.
fn deform_seed_row(
    ui: &mut egui::Ui,
    current: Option<u32>,
    node_id: mogen_core::NodeId,
    edits: &mut Vec<crate::viewer::PendingEdit>,
    wants_remove: &mut Vec<&'static str>,
) {
    use crate::viewer::PendingEdit;
    ui.label("Seed");
    ui.horizontal(|ui| match current {
        Some(initial) => {
            let mut v = initial;
            let resp = ui.add(
                egui::DragValue::new(&mut v)
                    .speed(1.0)
                    .range(0..=u32::MAX),
            );
            if resp.changed() {
                edits.push(PendingEdit::SetAttrCanonical {
                    node: node_id,
                    attr: "seed".into(),
                    value: v.to_string(),
                    delete: Vec::new(),
                });
            }
            if ui
                .small_button("✕")
                .on_hover_text("Remove seed (defaults to 1)")
                .clicked()
            {
                wants_remove.push("seed");
            }
        }
        None => {
            ui.label(egui::RichText::new("(none)").italics().weak());
            if ui.small_button("+ add").clicked() {
                edits.push(PendingEdit::SetAttrCanonical {
                    node: node_id,
                    attr: "seed".into(),
                    value: "1".into(),
                    delete: Vec::new(),
                });
            }
        }
    });
    ui.end_row();
}

/// Render scalar geometry-parameter rows for `kind`. Returns `true` when at
/// least one editable row was added, so the caller can paint a "(no editable
/// params)" hint when the kind has nothing to show. Each numeric attr falls
/// back to its primitive lowering default — see `mogen_dsl::lower::primitive`.
/// List-shaped attrs (`points`, `profile`, `path`, `holes`) are intentionally
/// skipped — they need a richer editor than a sidebar grid.
///
/// Out entries are pre-formatted `(attr_name, value_literal)` pairs so the
/// helper can emit list-shaped writes (`size=[1, 2, 3]`) without the caller
/// having to know which attrs are scalar vs. vector.
#[allow(clippy::too_many_arguments)]
pub(in crate::app) fn geom_params_for_kind(
    ui: &mut egui::Ui,
    kind: &str,
    src: &str,
    span: mogen_core::Span,
    out: &mut Vec<(&'static str, String)>,
) -> bool {
    use crate::edit::get_attr;
    fn read(src: &str, span: mogen_core::Span, attr: &str) -> Option<f32> {
        get_attr(src, span, attr).and_then(|s| s.parse::<f32>().ok())
    }
    fn read_vec3(src: &str, span: mogen_core::Span, attr: &str) -> Option<[f32; 3]> {
        let raw = get_attr(src, span, attr)?;
        let trimmed = raw.trim().trim_start_matches('[').trim_end_matches(']');
        let parts: Vec<f32> = trimmed
            .split(',')
            .filter_map(|s| s.trim().parse::<f32>().ok())
            .collect();
        match parts.len() {
            1 => Some([parts[0], parts[0], parts[0]]),
            3 => Some([parts[0], parts[1], parts[2]]),
            _ => None,
        }
    }
    fn read_vec2(src: &str, span: mogen_core::Span, attr: &str) -> Option<[f32; 2]> {
        let raw = get_attr(src, span, attr)?;
        let trimmed = raw.trim().trim_start_matches('[').trim_end_matches(']');
        let parts: Vec<f32> = trimmed
            .split(',')
            .filter_map(|s| s.trim().parse::<f32>().ok())
            .collect();
        match parts.len() {
            1 => Some([parts[0], parts[0]]),
            2 => Some([parts[0], parts[1]]),
            _ => None,
        }
    }
    fn scalar_row(
        ui: &mut egui::Ui,
        label: &str,
        attr: &'static str,
        current: f32,
        speed: f32,
        out: &mut Vec<(&'static str, String)>,
    ) {
        use crate::app::util::format_inspector_scalar;
        ui.label(label);
        let mut v = current;
        if ui.add(egui::DragValue::new(&mut v).speed(speed)).changed() {
            out.push((attr, format_inspector_scalar(v)));
        }
        ui.end_row();
    }
    fn int_row(
        ui: &mut egui::Ui,
        label: &str,
        attr: &'static str,
        current: i32,
        min: i32,
        max: i32,
        out: &mut Vec<(&'static str, String)>,
    ) {
        ui.label(label);
        let mut v = current;
        if ui
            .add(egui::DragValue::new(&mut v).speed(0.1).range(min..=max))
            .changed()
        {
            out.push((attr, v.to_string()));
        }
        ui.end_row();
    }
    use crate::app::util::format_inspector_scalar;
    let mut shown = false;
    match kind {
        "box" | "slab" | "post" | "panel" | "wedge" | "ellipsoid" | "rounded_box"
        | "chamfered_box" | "inset_box" | "wall" | "prism" | "superellipsoid" => {
            let s = read_vec3(src, span, "size").unwrap_or([1.0, 1.0, 1.0]);
            ui.label("Size");
            ui.horizontal(|ui| {
                let mut sx = s[0];
                let mut sy = s[1];
                let mut sz = s[2];
                let mut emit = false;
                if ui.add(egui::DragValue::new(&mut sx).speed(0.02)).changed() { emit = true; }
                if ui.add(egui::DragValue::new(&mut sy).speed(0.02)).changed() { emit = true; }
                if ui.add(egui::DragValue::new(&mut sz).speed(0.02)).changed() { emit = true; }
                if emit {
                    out.push((
                        "size",
                        format!(
                            "[{}, {}, {}]",
                            format_inspector_scalar(sx),
                            format_inspector_scalar(sy),
                            format_inspector_scalar(sz),
                        ),
                    ));
                }
            });
            ui.end_row();
            shown = true;
            // Kind-specific extras
            match kind {
                "rounded_box" | "chamfered_box" => {
                    scalar_row(ui, "Radius", "radius",
                        read(src, span, "radius").unwrap_or(0.1), 0.005, out);
                    if kind == "rounded_box" {
                        let segs = read(src, span, "segments").unwrap_or(4.0) as i32;
                        int_row(ui, "Segments", "segments", segs, 1, 32, out);
                    }
                }
                "inset_box" => {
                    scalar_row(ui, "Amount", "amount",
                        read(src, span, "amount").unwrap_or(0.1), 0.005, out);
                    scalar_row(ui, "Depth", "depth",
                        read(src, span, "depth").unwrap_or(0.05), 0.005, out);
                }
                "superellipsoid" => {
                    scalar_row(ui, "Equator", "ew",
                        read(src, span, "ew").unwrap_or(1.0), 0.05, out);
                    scalar_row(ui, "Meridian", "ns",
                        read(src, span, "ns").unwrap_or(1.0), 0.05, out);
                    let r = read(src, span, "rings").unwrap_or(16.0) as i32;
                    let s = read(src, span, "segments").unwrap_or(24.0) as i32;
                    int_row(ui, "Rings", "rings", r, 2, 256, out);
                    int_row(ui, "Segments", "segments", s, 3, 256, out);
                }
                "ellipsoid" => {
                    let r = read(src, span, "rings").unwrap_or(16.0) as i32;
                    let s = read(src, span, "segments").unwrap_or(24.0) as i32;
                    int_row(ui, "Rings", "rings", r, 2, 256, out);
                    int_row(ui, "Segments", "segments", s, 3, 256, out);
                }
                _ => {}
            }
        }
        "plane" | "quad" | "decal" | "leaf_card" | "curved_plane" => {
            // plane uses [x,z], quad/decal/leaf_card uses [x,y], curved_plane uses [x,z].
            let s = read_vec2(src, span, "size").unwrap_or([1.0, 1.0]);
            ui.label("Size");
            ui.horizontal(|ui| {
                let mut su = s[0];
                let mut sv = s[1];
                let mut emit = false;
                if ui.add(egui::DragValue::new(&mut su).speed(0.05)).changed() { emit = true; }
                if ui.add(egui::DragValue::new(&mut sv).speed(0.05)).changed() { emit = true; }
                if emit {
                    out.push((
                        "size",
                        format!(
                            "[{}, {}]",
                            format_inspector_scalar(su),
                            format_inspector_scalar(sv),
                        ),
                    ));
                }
            });
            ui.end_row();
            shown = true;
            if kind == "decal" {
                scalar_row(ui, "Offset", "offset",
                    read(src, span, "offset").unwrap_or(0.001), 0.0005, out);
            }
        }
        "cylinder" | "cone" | "half_cylinder" => {
            scalar_row(ui, "Radius", "radius",
                read(src, span, "radius").unwrap_or(0.5), 0.02, out);
            scalar_row(ui, "Height", "height",
                read(src, span, "height").unwrap_or(1.0), 0.02, out);
            let s = read(src, span, "segments").unwrap_or(24.0) as i32;
            int_row(ui, "Segments", "segments", s, 3, 256, out);
            shown = true;
        }
        "sphere" | "hemisphere" => {
            scalar_row(ui, "Radius", "radius",
                read(src, span, "radius").unwrap_or(0.5), 0.02, out);
            let r = read(src, span, "rings").unwrap_or(if kind == "hemisphere" { 8.0 } else { 16.0 }) as i32;
            let s = read(src, span, "segments").unwrap_or(24.0) as i32;
            int_row(ui, "Rings", "rings", r, 2, 256, out);
            int_row(ui, "Segments", "segments", s, 3, 256, out);
            shown = true;
        }
        "icosphere" => {
            scalar_row(ui, "Radius", "radius",
                read(src, span, "radius").unwrap_or(0.5), 0.02, out);
            let s = read(src, span, "subdivisions").unwrap_or(2.0) as i32;
            int_row(ui, "Subdivisions", "subdivisions", s, 0, 6, out);
            shown = true;
        }
        "capsule" => {
            scalar_row(ui, "Radius", "radius",
                read(src, span, "radius").unwrap_or(0.5), 0.02, out);
            scalar_row(ui, "Height", "height",
                read(src, span, "height").unwrap_or(1.0), 0.02, out);
            let r = read(src, span, "rings").unwrap_or(8.0) as i32;
            let s = read(src, span, "segments").unwrap_or(24.0) as i32;
            int_row(ui, "Rings", "rings", r, 2, 64, out);
            int_row(ui, "Segments", "segments", s, 3, 256, out);
            shown = true;
        }
        "torus" => {
            scalar_row(ui, "Major", "major",
                read(src, span, "major").unwrap_or(0.5), 0.02, out);
            scalar_row(ui, "Minor", "minor",
                read(src, span, "minor").unwrap_or(0.15), 0.02, out);
            let mj = read(src, span, "major_segments").unwrap_or(24.0) as i32;
            let mn = read(src, span, "minor_segments").unwrap_or(12.0) as i32;
            int_row(ui, "Major segs", "major_segments", mj, 3, 256, out);
            int_row(ui, "Minor segs", "minor_segments", mn, 3, 256, out);
            shown = true;
        }
        "torus_arc" => {
            scalar_row(ui, "Major", "major",
                read(src, span, "major").unwrap_or(0.5), 0.02, out);
            scalar_row(ui, "Minor", "minor",
                read(src, span, "minor").unwrap_or(0.15), 0.02, out);
            scalar_row(ui, "Arc°", "arc",
                read(src, span, "arc").unwrap_or(90.0), 1.0, out);
            shown = true;
        }
        "pyramid" => {
            scalar_row(ui, "Radius", "radius",
                read(src, span, "radius").unwrap_or(0.5), 0.02, out);
            scalar_row(ui, "Height", "height",
                read(src, span, "height").unwrap_or(1.0), 0.02, out);
            let s = read(src, span, "sides").unwrap_or(4.0) as i32;
            int_row(ui, "Sides", "sides", s, 3, 64, out);
            shown = true;
        }
        "disc" => {
            scalar_row(ui, "Radius", "radius",
                read(src, span, "radius").unwrap_or(0.5), 0.02, out);
            let s = read(src, span, "segments").unwrap_or(24.0) as i32;
            int_row(ui, "Segments", "segments", s, 3, 256, out);
            shown = true;
        }
        "tube" => {
            scalar_row(ui, "Outer", "outer",
                read(src, span, "outer").unwrap_or(0.5), 0.02, out);
            scalar_row(ui, "Inner", "inner",
                read(src, span, "inner").unwrap_or(0.3), 0.02, out);
            scalar_row(ui, "Height", "height",
                read(src, span, "height").unwrap_or(1.0), 0.02, out);
            let s = read(src, span, "segments").unwrap_or(24.0) as i32;
            int_row(ui, "Segments", "segments", s, 3, 256, out);
            shown = true;
        }
        "coil" => {
            scalar_row(ui, "Radius", "radius",
                read(src, span, "radius").unwrap_or(0.5), 0.02, out);
            scalar_row(ui, "Height", "height",
                read(src, span, "height").unwrap_or(1.0), 0.02, out);
            scalar_row(ui, "Turns", "turns",
                read(src, span, "turns").unwrap_or(3.0), 0.1, out);
            scalar_row(ui, "Profile r", "profile_radius",
                read(src, span, "profile_radius").unwrap_or(0.05), 0.005, out);
            shown = true;
        }
        "frustum" => {
            scalar_row(ui, "Height", "height",
                read(src, span, "height").unwrap_or(1.0), 0.02, out);
            shown = true;
        }
        _ => {}
    }
    shown
}

/// Rewrite the keyword (kind identifier) at the very start of the node
/// covered by `span`. Used by the CSG op switch in the inspector — the
/// keyword sits before the optional `"name" (...)` header, and existing
/// edit helpers don't expose a kind rewrite. Bytes-only; no AST round-trip.
fn rewrite_node_kind(src: &str, span: mogen_core::Span, new_kind: &str) -> String {
    let bytes = src.as_bytes();
    let start = span.start.min(src.len());
    // Skip leading whitespace inside the span — pest spans typically begin
    // exactly at the kind keyword, but be defensive.
    let mut i = start;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    let kind_start = i;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
        i += 1;
    }
    if i == kind_start {
        return src.to_string();
    }
    let mut out = String::with_capacity(src.len() + new_kind.len());
    out.push_str(&src[..kind_start]);
    out.push_str(new_kind);
    out.push_str(&src[i..]);
    out
}
