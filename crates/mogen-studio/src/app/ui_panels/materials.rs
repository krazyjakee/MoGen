use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use eframe::egui;

use crate::app::types::{UndoKey, TEX_EXISTS_TTL};
use crate::app::util::{
    delete_texture_group, ellipsize_path, find_material_source_span, format_inspector_scalar,
    gather_texture_refs, materials_referenced_by_visible_nodes, origin_in_visible_set,
    resolve_for_check, scan_unused_textures, visible_origins,
};
use crate::app::MogenStudioApp;

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
        use mogen_core::{AlphaMode, UvMode};

        let i = self.active;
        let selection = self.viewer.selection();
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
                        let mode_id = egui::Id::new(("alpha_mode", *idx, mat.name.as_str()));
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
                        let uv_id = egui::Id::new(("uv_mode", *idx, mat.name.as_str()));
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
    fn cached_exists(&mut self, path: &Path) -> bool {
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
