//! Right-sidebar **Preview / Render** section.
//!
//! Consolidates the viewer-side knobs that previously lived only in the
//! floating viewport overlay (environment, shadows, preview shader, show-grid /
//! show-lights / show-transform-gizmo / show-colliders) plus a few that lived
//! in the Edit menu (background colour, max FPS). Discoverability fix — the
//! overlay popup is great for users who already know it's there but new users
//! generally find sidebar sections first.
//!
//! Every change writes through `Settings` so it persists across restarts and
//! propagates to the viewer's GL state via the matching `viewer.set_*` calls.

use eframe::egui;

use crate::app::MogenStudioApp;

impl MogenStudioApp {
    pub(in crate::app) fn ui_preview(&mut self, ui: &mut egui::Ui) {
        use crate::preview_shader::{
            preview_shader_label, PreviewShader, PREVIEW_SHADERS,
        };
        use crate::viewer::environment::{environment_label, Environment, ENVIRONMENTS};
        use crate::viewer::shadows::SHADOW_QUALITIES;

        // Preview shader (PBR / Toon / CRT / Matcap / Wireframe).
        ui.horizontal(|ui| {
            ui.label("Shader");
            let cur = self.settings.preview_shader();
            let mut chosen: Option<PreviewShader> = None;
            egui::ComboBox::from_id_salt("preview_shader_panel")
                .selected_text(preview_shader_label(cur))
                .show_ui(ui, |ui| {
                    for s in PREVIEW_SHADERS {
                        let selected = s == cur;
                        if ui
                            .selectable_label(selected, preview_shader_label(s))
                            .clicked()
                            && !selected
                        {
                            chosen = Some(s);
                        }
                    }
                });
            if let Some(s) = chosen {
                self.settings.set_preview_shader(s);
                self.viewer.set_preview_shader(s);
                let _ = self.settings.save();
            }
        });

        // Environment (sky probe + key/fill rig fallback).
        ui.horizontal(|ui| {
            ui.label("Environment");
            let cur = self.viewer.environment();
            let mut chosen: Option<Environment> = None;
            egui::ComboBox::from_id_salt("preview_env_panel")
                .selected_text(environment_label(cur))
                .show_ui(ui, |ui| {
                    for env in ENVIRONMENTS {
                        let selected = env == cur;
                        if ui
                            .selectable_label(selected, environment_label(env))
                            .clicked()
                            && !selected
                        {
                            chosen = Some(env);
                        }
                    }
                });
            if let Some(env) = chosen {
                self.settings.set_environment(env);
                self.viewer.set_environment(env);
                let _ = self.settings.save();
            }
        });

        // Shadow quality (off / 1024 / 2048 / 4096).
        ui.horizontal(|ui| {
            ui.label("Shadows");
            let cur = self.viewer.shadows();
            let mut chosen = None;
            egui::ComboBox::from_id_salt("preview_shadow_panel")
                .selected_text(cur.label())
                .show_ui(ui, |ui| {
                    for q in SHADOW_QUALITIES {
                        let selected = q == cur;
                        if ui
                            .selectable_label(selected, q.label())
                            .clicked()
                            && !selected
                        {
                            chosen = Some(q);
                        }
                    }
                });
            if let Some(q) = chosen {
                self.settings.set_shadow_quality(q);
                self.viewer.set_shadows(q);
                let _ = self.settings.save();
            }
        });

        ui.add_space(6.0);
        ui.label(egui::RichText::new("Show").strong());

        let mut show_grid = self.settings.show_grid();
        if ui
            .checkbox(&mut show_grid, "Grid")
            .on_hover_text("Ground-plane reference grid")
            .changed()
        {
            self.settings.set_show_grid(show_grid);
            self.viewer.set_show_grid(show_grid);
            let _ = self.settings.save();
        }
        let mut show_lights = self.settings.show_light_gizmos();
        if ui
            .checkbox(&mut show_lights, "Light gizmos")
            .on_hover_text(
                "Per-light indicator overlays (point sphere, spot cone, \
                 directional arrow)",
            )
            .changed()
        {
            self.settings.set_show_light_gizmos(show_lights);
            self.viewer.set_show_light_gizmos(show_lights);
            let _ = self.settings.save();
        }
        let mut show_xform = self.settings.show_transform_gizmo();
        if ui
            .checkbox(&mut show_xform, "Transform gizmo")
            .on_hover_text("Translate / rotate / scale handles on the selected node")
            .changed()
        {
            self.settings.set_show_transform_gizmo(show_xform);
            self.viewer.set_show_transform_gizmo(show_xform);
            let _ = self.settings.save();
        }
        let mut show_colliders = self.settings.show_colliders();
        if ui
            .checkbox(&mut show_colliders, "Colliders")
            .on_hover_text(
                "Render the AABB outline of every node tagged `collider=\"aabb\"`",
            )
            .changed()
        {
            self.settings.set_show_colliders(show_colliders);
            self.viewer.set_show_colliders(show_colliders);
            let _ = self.settings.save();
        }

        ui.add_space(6.0);
        // Background colour (sRGB) — sits at the bottom because it's the
        // least-frequently-changed knob in this group.
        ui.horizontal(|ui| {
            ui.label("Background");
            let current = self.settings.viewer_bg_rgb();
            let mut srgba = egui::Color32::from_rgb(current[0], current[1], current[2]);
            if ui.color_edit_button_srgba(&mut srgba).changed() {
                let rgb = [srgba.r(), srgba.g(), srgba.b()];
                if rgb != current {
                    self.settings.set_viewer_bg_rgb(rgb);
                    let _ = self.settings.save();
                }
            }
            if ui.small_button("Reset").clicked() {
                self.settings
                    .set_viewer_bg_rgb(crate::settings::DEFAULT_VIEWER_BG_RGB);
                let _ = self.settings.save();
            }
        });

        // Max FPS — useful when the viewer is competing with another GPU
        // workload. None = uncapped (egui's default).
        ui.horizontal(|ui| {
            ui.label("Max FPS");
            let cur = self.settings.max_fps();
            let mut on = cur.is_some();
            if ui.checkbox(&mut on, "cap").changed() {
                let new = if on { Some(cur.unwrap_or(60)) } else { None };
                self.settings.set_max_fps(new);
                self.viewer.set_max_fps(new);
                let _ = self.settings.save();
            }
            if let Some(mut fps) = cur {
                if ui
                    .add(egui::DragValue::new(&mut fps).range(15..=240).speed(1.0))
                    .changed()
                {
                    self.settings.set_max_fps(Some(fps));
                    self.viewer.set_max_fps(Some(fps));
                    let _ = self.settings.save();
                }
            }
        });
    }
}
