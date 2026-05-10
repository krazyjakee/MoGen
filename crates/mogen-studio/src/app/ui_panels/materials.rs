use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use eframe::egui;

use crate::app::types::{ThumbEntry, UndoKey, TEX_EXISTS_TTL};
use crate::app::util::{
    delete_material_textures, delete_texture_group, ellipsize_path, find_material_source_span,
    format_inspector_scalar, gather_texture_refs, materials_referenced_by_visible_nodes,
    origin_in_visible_set, resolve_for_check, scan_unused_textures, visible_origins,
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
        use mogen_core::{AlphaMode, MaterialShader, UvMode};

        // Per-material texture-management actions queued during the UI scan and
        // applied after the loop so we can take a fresh `&mut self` borrow
        // without fighting the material-clone iteration.
        enum TexAction {
            Regenerate(String),
            Delete(String),
            Reveal(PathBuf),
        }

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

                    // Normal / AO strength. `normal_strength` is now a render-time
                    // multiplier on the tangent-space XY components — it scales
                    // the live preview and exports as glTF's `normalTexture.scale`,
                    // and is also used as the bake-time slope when the textures
                    // pipeline derives the normal PNG.
                    ui.horizontal(|ui| {
                        let mut ns = mat.normal_strength;
                        if ui
                            .add(
                                egui::Slider::new(&mut ns, 0.0..=8.0)
                                    .text("normal strength")
                                    .fixed_decimals(2),
                            )
                            .on_hover_text(
                                "Slope multiplier on the normal map. 0 flattens \
                                 the surface to its geometric normal; larger values \
                                 exaggerate bumps. Live in the preview and exported \
                                 as glTF `normalTexture.scale`; also used as the \
                                 bake-time slope by `mogen textures`.",
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

                    // Per-material shader override. Studio-only — the export
                    // path always emits standard PBR. Currently exposes the
                    // animated water surface; new shaders join this dropdown
                    // when added.
                    ui.horizontal(|ui| {
                        ui.label("Shader");
                        let mut shader = mat.shader;
                        let shader_id = egui::Id::new(("shader", *idx, mat.name.as_str()));
                        egui::ComboBox::from_id_salt(shader_id)
                            .selected_text(match shader {
                                MaterialShader::Standard => "standard (PBR)",
                                MaterialShader::Water => "water",
                            })
                            .show_ui(ui, |ui| {
                                let mut changed = false;
                                changed |= ui
                                    .selectable_value(
                                        &mut shader,
                                        MaterialShader::Standard,
                                        "standard (PBR)",
                                    )
                                    .changed();
                                changed |= ui
                                    .selectable_value(
                                        &mut shader,
                                        MaterialShader::Water,
                                        "water",
                                    )
                                    .changed();
                                if changed {
                                    let v = match shader {
                                        MaterialShader::Standard => "\"standard\"",
                                        MaterialShader::Water => "\"water\"",
                                    };
                                    pending.push((
                                        mat.name.clone(),
                                        "shader",
                                        v.to_string(),
                                    ));
                                }
                            });
                    });

                    // Per-material textures section: albedo thumbnail,
                    // generate/delete/reveal actions, and the slot roster
                    // with ✓/✗ existence marks. All texture management for
                    // this material lives here so the relationship between
                    // a material and its PNGs is unambiguous.
                    let mat_slots: Vec<(String, &'static str, PathBuf)> = texture_slots
                        .iter()
                        .filter(|(m, _, _)| m == &mat.name)
                        .cloned()
                        .collect();
                    let albedo_path: Option<PathBuf> = mat_slots
                        .iter()
                        .find(|(_, slot, _)| *slot == "base_color")
                        .map(|(_, _, rel)| resolve_for_check(rel, source_dir.as_deref()));
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
                            if let Some(handle) = self.thumb_handle(&ctx, &abs) {
                                ui.add(
                                    egui::Image::new((handle.id(), cell))
                                        .rounding(4.0),
                                )
                                .on_hover_text(ellipsize_path(&abs, 60));
                            } else {
                                let (rect, _) =
                                    ui.allocate_exact_size(cell, egui::Sense::hover());
                                ui.painter().rect_filled(
                                    rect,
                                    4.0,
                                    ui.visuals().faint_bg_color,
                                );
                            }
                        } else {
                            let (rect, _) =
                                ui.allocate_exact_size(cell, egui::Sense::hover());
                            ui.painter().rect_filled(
                                rect,
                                4.0,
                                ui.visuals().faint_bg_color,
                            );
                            ui.painter().text(
                                rect.center(),
                                egui::Align2::CENTER_CENTER,
                                "(none)",
                                egui::FontId::proportional(11.0),
                                ui.visuals().weak_text_color(),
                            );
                        }

                        ui.vertical(|ui| {
                            let gen_label =
                                if albedo_exists { "Regenerate" } else { "Generate" };
                            let gen_tip = if albedo_exists {
                                "Re-run the textures pipeline for this material \
                                 (forces overwrite of existing PNGs)"
                            } else {
                                "Run the textures pipeline for this material — writes a \
                                 base_color PNG (plus derived normal/MR/AO) into ./textures/"
                            };
                            let gen_resp = ui
                                .add_enabled(tex_enabled, egui::Button::new(gen_label))
                                .on_hover_text(
                                    tex_disabled_reason.as_deref().unwrap_or(gen_tip),
                                );
                            if gen_resp.clicked() {
                                pending_tex_action =
                                    Some(TexAction::Regenerate(mat.name.clone()));
                            }
                            if ui
                                .add_enabled(albedo_exists, egui::Button::new("Delete"))
                                .on_hover_text(
                                    "Remove the albedo + PBR companion PNGs and clear the \
                                     *_texture attrs on this material",
                                )
                                .clicked()
                            {
                                pending_tex_action =
                                    Some(TexAction::Delete(mat.name.clone()));
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
                                    pending_tex_action = Some(TexAction::Reveal(p));
                                }
                            }
                        });
                    });

                    if !mat_slots.is_empty() {
                        for (_, slot, rel_path) in &mat_slots {
                            let resolved = resolve_for_check(rel_path, source_dir.as_deref());
                            let exists = self.cached_exists(&resolved);
                            let (mark, color) = if exists {
                                ("✓", egui::Color32::from_rgb(80, 200, 120))
                            } else {
                                ("✗", egui::Color32::from_rgb(230, 100, 100))
                            };
                            // Concrete hover text on the missing marker so
                            // the user sees *which* path the file picker
                            // looked for, not just that something is wrong.
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
                                ui.label(display)
                                    .on_hover_text(rel_path.to_string_lossy());
                            });
                        }
                    }

                    // Manual per-slot picker — Browse points the slot at an
                    // existing PNG without re-running the LLM. The LLM
                    // pipeline does not expose this knob, so authors who
                    // already have textures on disk would otherwise have to
                    // hand-edit the .mog. Rows for every slot, regardless of
                    // whether one is currently authored — Clear hides itself
                    // when the slot is empty.
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
                                            .clone()
                                            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default()),
                                    )
                                    .pick_file()
                                {
                                    let rel = source_dir
                                        .as_deref()
                                        .and_then(|base| {
                                            picked.strip_prefix(base).ok().map(|p| p.to_path_buf())
                                        })
                                        .unwrap_or_else(|| picked.clone());
                                    let value = format!("\"{}\"", rel.to_string_lossy());
                                    pending_tex_path =
                                        Some((mat.name.clone(), attr, Some(value)));
                                }
                            }
                            if authored
                                && ui
                                    .small_button("Clear")
                                    .on_hover_text("Remove this slot's path attr")
                                    .clicked()
                            {
                                pending_tex_path = Some((mat.name.clone(), attr, None));
                            }
                        });
                    }

                    // Rename + Delete actions live at the bottom of the
                    // material body. Rename uses an in-place text field
                    // committed on focus loss (mirrors the meta editor);
                    // Delete is a two-step confirm to match the clip-delete
                    // chip pattern.
                    ui.add_space(6.0);
                    ui.separator();
                    ui.label(egui::RichText::new("manage").weak());
                    let mut rename_buf = self
                        .material_name_drafts
                        .entry(mat.name.clone())
                        .or_insert_with(|| mat.name.clone())
                        .clone();
                    let rename_resp = ui.horizontal(|ui| {
                        ui.label("Rename");
                        ui.add(
                            egui::TextEdit::singleline(&mut rename_buf)
                                .desired_width(160.0)
                                .id_salt(("mat_rename", *idx)),
                        )
                    });
                    if rename_resp.inner.changed() {
                        self.material_name_drafts
                            .insert(mat.name.clone(), rename_buf.clone());
                    }
                    if rename_resp.inner.lost_focus()
                        && !rename_buf.trim().is_empty()
                        && rename_buf != mat.name
                    {
                        pending_rename_material =
                            Some((mat.name.clone(), rename_buf.trim().to_string()));
                    }

                    ui.horizontal(|ui| {
                        if ui
                            .button(egui::RichText::new("🗑 Delete material"))
                            .on_hover_text(
                                "Remove the material declaration from the source. \
                                 Nodes that reference it will fall back to default PBR \
                                 until you reassign them.",
                            )
                            .clicked()
                        {
                            pending_delete_material = Some(mat.name.clone());
                        }
                    });
                });
        }

        if wants_add_material {
            let suggested = next_material_name(&self.files[i].source);
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
            // Two-pass: rewrite the material declaration's name literal,
            // then sweep every `material="<old>"` reference across the
            // source. The reference sweep is a verbatim substring replace
            // bounded by the quote pair so we don't clobber an unrelated
            // token that happens to equal the old name.
            if let Some(span) = find_material_source_span(&self.files[i].source, &old_name) {
                let before = self.files[i].source.clone();
                let with_decl = rewrite_material_decl_name(&before, span, &new_name);
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

    /// Look up or lazily upload an albedo thumbnail for `abs`. `None` when the
    /// file isn't a readable PNG. Same cache (keyed by absolute path + mtime)
    /// the LLM panel previously used.
    fn thumb_handle(
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

/// Suggest a unique `material_<n>` name for a freshly-added material by
/// scanning the source for any existing `material "material_N" …` literal
/// and returning `material_<max+1>`. Defaults to `material_1` when no
/// numbered material is present.
fn next_material_name(src: &str) -> String {
    let prefix = "material_";
    let mut max_n: u32 = 0;
    for line in src.lines() {
        let trimmed = line.trim_start();
        let after_kw = match trimmed.strip_prefix("material ") {
            Some(s) => s,
            None => continue,
        };
        let after_quote = match after_kw.trim_start().strip_prefix('"') {
            Some(s) => s,
            None => continue,
        };
        let end = match after_quote.find('"') {
            Some(e) => e,
            None => continue,
        };
        let name = &after_quote[..end];
        if let Some(rest) = name.strip_prefix(prefix) {
            if let Ok(n) = rest.parse::<u32>() {
                if n > max_n {
                    max_n = n;
                }
            }
        }
    }
    format!("{prefix}{}", max_n + 1)
}

/// Rewrite the quoted name literal inside the `material "name" (...)`
/// declaration covered by `span`. Bytes-level so we don't disturb the
/// surrounding formatting / comments. Returns the source unchanged if the
/// span doesn't contain a quoted name (defensive — `find_material_source_span`
/// only returns spans that do).
fn rewrite_material_decl_name(src: &str, span: mogen_core::Span, new_name: &str) -> String {
    let bytes = src.as_bytes();
    let start = span.start.min(src.len());
    let end = span.end.min(src.len());
    let mut i = start;
    while i < end && bytes[i] != b'"' {
        i += 1;
    }
    if i >= end {
        return src.to_string();
    }
    let q_open = i;
    i += 1;
    while i < end && bytes[i] != b'"' {
        if bytes[i] == b'\\' && i + 1 < end {
            i += 2;
            continue;
        }
        i += 1;
    }
    if i >= end {
        return src.to_string();
    }
    let q_close = i;
    let mut out = String::with_capacity(src.len() + new_name.len());
    out.push_str(&src[..q_open + 1]);
    out.push_str(new_name);
    out.push_str(&src[q_close..]);
    out
}
