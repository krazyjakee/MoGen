use eframe::egui;

use crate::app::util::format_inspector_scalar;

/// Render the PBR slider / colour / dropdown block for a single material.
/// Every change is appended to `pending` as `(material_name, attr, value)` —
/// the caller applies them in a second pass once the borrow on the cloned
/// material slice is released. Each per-material widget ID is salted with
/// `idx` so imports that re-use a name don't collide.
pub(super) fn render(
    ui: &mut egui::Ui,
    idx: usize,
    mat: &mogen_core::Material,
    pending: &mut Vec<(String, &'static str, String)>,
) {
    use mogen_core::{AlphaMode, UvMode};

    // Colour + alpha
    ui.horizontal(|ui| {
        ui.label("Color");
        let mut rgb = [mat.base_color[0], mat.base_color[1], mat.base_color[2]];
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

    // Normal / AO strength. `normal_strength` is now a render-time multiplier
    // on the tangent-space XY components — it scales the live preview and
    // exports as glTF's `normalTexture.scale`, and is also used as the
    // bake-time slope when the textures pipeline derives the normal PNG.
    ui.horizontal(|ui| {
        let mut ns = mat.normal_strength;
        if ui
            .add(
                egui::Slider::new(&mut ns, 0.0..=8.0)
                    .text("normal strength")
                    .fixed_decimals(2),
            )
            .on_hover_text(
                "Slope multiplier on the normal map. 0 flattens the surface to \
                 its geometric normal; larger values exaggerate bumps. Live in \
                 the preview and exported as glTF `normalTexture.scale`; also \
                 used as the bake-time slope by `mogen textures`.",
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
        let mode_id = egui::Id::new(("alpha_mode", idx, mat.name.as_str()));
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
                    pending.push((mat.name.clone(), "alpha_mode", v.to_string()));
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
        let uv_id = egui::Id::new(("uv_mode", idx, mat.name.as_str()));
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
                    pending.push((mat.name.clone(), "uv_mode", v.to_string()));
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

    // Per-material shader override. Studio-only — the export path always emits
    // standard PBR. Currently exposes the animated water surface; new shaders
    // join this dropdown when added.
    ui.horizontal(|ui| {
        ui.label("Shader");
        // Studio-only preview shader. `None`/`standard` = the regular PBR path;
        // `water` is the built-in preset. User `shader "…"` declarations are
        // authored in source — this dropdown exposes the built-in options.
        let current = mat.shader_name.as_deref().unwrap_or("standard");
        let mut choice = current.to_string();
        let shader_id = egui::Id::new(("shader", idx, mat.name.as_str()));
        egui::ComboBox::from_id_salt(shader_id)
            .selected_text(match current {
                "standard" => "standard (PBR)",
                other => other,
            })
            .show_ui(ui, |ui| {
                let mut changed = false;
                changed |= ui
                    .selectable_value(&mut choice, "standard".to_string(), "standard (PBR)")
                    .changed();
                changed |= ui
                    .selectable_value(&mut choice, "water".to_string(), "water")
                    .changed();
                if changed {
                    pending.push((mat.name.clone(), "shader", format!("\"{choice}\"")));
                }
            });
    });
}
