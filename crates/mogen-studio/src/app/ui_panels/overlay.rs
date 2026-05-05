use eframe::egui;

use crate::app::types::ShortcutAction;
use crate::app::MogenStudioApp;

impl MogenStudioApp {
    /// Floating overlay buttons drawn on top of the viewport. Keeps the
    /// camera controls within the user's eye line instead of forcing a trip
    /// to the toolbar.
    pub(in crate::app) fn ui_viewport_overlay(&mut self, ctx: &egui::Context, viewport_rect: egui::Rect) {
        use crate::gizmo::GizmoMode;
        use crate::preview_shader::{
            preview_shader_label, preview_shader_short_label, PreviewShader, PREVIEW_SHADERS,
        };
        use crate::viewer::environment::{
            environment_label, environment_short_label, Environment, ENVIRONMENTS,
        };
        use crate::viewer::shadows::{ShadowQuality, SHADOW_QUALITIES};
        egui::Area::new(egui::Id::new("viewport_overlay"))
            .fixed_pos(viewport_rect.left_top() + egui::vec2(8.0, 8.0))
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
                                // Hotkeys are bare W/E/R (Godot / Unity
                                // convention) — surfaced in the tooltip so
                                // users can discover them without docs.
                                for (label, mode, tip) in [
                                    ("T", GizmoMode::Translate, "Translate gizmo  (W)"),
                                    ("R", GizmoMode::Rotate, "Rotate gizmo  (E)"),
                                    ("S", GizmoMode::Scale, "Scale gizmo  (R)"),
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
                            // Visibility toggles for viewport overlays. Kept
                            // inside a popup so the bar stays compact; each
                            // checkbox mirrors the matching `View` menu entry
                            // and persists through Settings so the choice
                            // survives a restart.
                            ui.add_enabled_ui(!cinema_on, |ui| {
                                ui.menu_button("Show", |ui| {
                                    let mut show_grid = self.settings.show_grid();
                                    if ui
                                        .checkbox(&mut show_grid, "Grid")
                                        .on_hover_text(
                                            "Ground-plane reference grid",
                                        )
                                        .changed()
                                    {
                                        self.settings.set_show_grid(show_grid);
                                        self.viewer.set_show_grid(show_grid);
                                        let _ = self.settings.save();
                                    }
                                    let mut show_lights =
                                        self.settings.show_light_gizmos();
                                    if ui
                                        .checkbox(&mut show_lights, "Light gizmos")
                                        .on_hover_text(
                                            "Per-light indicator overlays \
                                             (point sphere, spot cone, \
                                             directional arrow)",
                                        )
                                        .changed()
                                    {
                                        self.settings
                                            .set_show_light_gizmos(show_lights);
                                        self.viewer
                                            .set_show_light_gizmos(show_lights);
                                        let _ = self.settings.save();
                                    }
                                    let mut show_xform =
                                        self.settings.show_transform_gizmo();
                                    if ui
                                        .checkbox(
                                            &mut show_xform,
                                            "Transform gizmo",
                                        )
                                        .on_hover_text(
                                            "Translate / rotate / scale handles \
                                             on the selected node",
                                        )
                                        .changed()
                                    {
                                        self.settings
                                            .set_show_transform_gizmo(show_xform);
                                        self.viewer
                                            .set_show_transform_gizmo(show_xform);
                                        let _ = self.settings.save();
                                    }
                                })
                                .response
                                .on_hover_text("Toggle viewport overlays");
                            });
                            ui.separator();
                            // Preview-shader picker. Mirrors View → Shader so
                            // PBR / Toon / CRT / Matcap / Wireframe is one
                            // click away without leaving the viewport. Like
                            // environment + shadows below, it's persisted in
                            // settings and disabled in cinema mode so the
                            // presentation pass stays clean.
                            ui.add_enabled_ui(!cinema_on, |ui| {
                                let cur_shader = self.settings.preview_shader();
                                let mut chosen_shader: Option<PreviewShader> = None;
                                let label =
                                    format!("◑ {}", preview_shader_short_label(cur_shader));
                                ui.menu_button(label, |ui| {
                                    ui.label(egui::RichText::new("Shader").strong());
                                    ui.separator();
                                    for s in PREVIEW_SHADERS {
                                        let selected = s == cur_shader;
                                        if ui
                                            .selectable_label(selected, preview_shader_label(s))
                                            .clicked()
                                            && !selected
                                        {
                                            chosen_shader = Some(s);
                                            ui.close_menu();
                                        }
                                    }
                                })
                                .response
                                .on_hover_text(
                                    "Viewport preview shader (PBR, Toon, CRT, Matcap, Wireframe)",
                                );
                                if let Some(s) = chosen_shader {
                                    self.settings.set_preview_shader(s);
                                    self.viewer.set_preview_shader(s);
                                    let _ = self.settings.save();
                                }
                            });
                            ui.separator();
                            // Environment-lighting preset picker. Drives the
                            // analytic sky probe and the fallback key/fill
                            // rig the shader uses when the scene declares no
                            // `light` nodes. Persisted in settings so the
                            // chosen preset survives restart. Disabled in
                            // cinema mode for the same reason as the gizmo
                            // group above — cinema is meant for clean
                            // presentation, not editor controls.
                            ui.add_enabled_ui(!cinema_on, |ui| {
                                let cur_env = self.viewer.environment();
                                let mut chosen_env: Option<Environment> = None;
                                let label = format!("☀ {}", environment_short_label(cur_env));
                                ui.menu_button(label, |ui| {
                                    ui.label(
                                        egui::RichText::new("Environment").strong(),
                                    );
                                    ui.separator();
                                    for env in ENVIRONMENTS {
                                        let selected = env == cur_env;
                                        if ui
                                            .selectable_label(
                                                selected,
                                                environment_label(env),
                                            )
                                            .clicked()
                                            && !selected
                                        {
                                            chosen_env = Some(env);
                                            ui.close_menu();
                                        }
                                    }
                                })
                                .response
                                .on_hover_text(
                                    "World-lighting preset (sky probe + key/fill rig)",
                                );
                                if let Some(env) = chosen_env {
                                    self.settings.set_environment(env);
                                    self.viewer.set_environment(env);
                                    let _ = self.settings.save();
                                }
                            });
                            ui.separator();
                            // Shadow-quality picker. Off skips the depth
                            // pre-pass entirely; the higher tiers reallocate
                            // the depth atlas + cube textures and bump the
                            // per-frame caster cap. Disabled in cinema mode
                            // alongside the rest of the editor controls.
                            ui.add_enabled_ui(!cinema_on, |ui| {
                                let cur_q = self.viewer.shadows();
                                let mut chosen_q: Option<ShadowQuality> = None;
                                let label = format!("◐ {}", cur_q.short_label());
                                ui.menu_button(label, |ui| {
                                    ui.label(
                                        egui::RichText::new("Shadows").strong(),
                                    );
                                    ui.separator();
                                    for q in SHADOW_QUALITIES {
                                        let selected = q == cur_q;
                                        if ui
                                            .selectable_label(selected, q.label())
                                            .clicked()
                                            && !selected
                                        {
                                            chosen_q = Some(q);
                                            ui.close_menu();
                                        }
                                    }
                                })
                                .response
                                .on_hover_text(
                                    "Realtime shadow-mapping quality \
                                     (off / 1024 / 2048 / 4096)",
                                );
                                if let Some(q) = chosen_q {
                                    self.settings.set_shadow_quality(q);
                                    self.viewer.set_shadows(q);
                                    let _ = self.settings.save();
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
                        });
                    });
            });

        // Bottom-left status strip: camera-controls hint, or the active cinema
        // shot label. Lives in its own `Area` so the top toolbar stays compact
        // and never competes with the right-hand inspector for horizontal room
        // (DCC convention — Blender/Maya put help text at the bottom).
        let cinema_on = self.viewer.is_cinema_active();
        let status_text: Option<String> = if cinema_on {
            self.viewer
                .cinema_shot_label()
                .map(|name| format!("now: {name}"))
        } else {
            Some(
                "click: select · shift/cmd+click: add · del: delete selected · \
                 esc: clear · drag: orbit · shift+drag/middle/right: pan · \
                 scroll: zoom · ctrl: snap"
                    .to_string(),
            )
        };
        if let Some(text) = status_text {
            egui::Area::new(egui::Id::new("viewport_status"))
                .fixed_pos(viewport_rect.left_bottom() + egui::vec2(8.0, -8.0))
                .pivot(egui::Align2::LEFT_BOTTOM)
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style())
                        .fill(ui.visuals().window_fill().linear_multiply(0.85))
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new(text).weak());
                        });
                });
        }
    }
}
