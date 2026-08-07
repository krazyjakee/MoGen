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
                                // Labels match the bare W/E/R hotkeys so the
                                // button text and the keyboard binding don't
                                // contradict each other.
                                for (label, mode, tip) in [
                                    ("W", GizmoMode::Translate, "Translate gizmo  (W)"),
                                    ("E", GizmoMode::Rotate, "Rotate gizmo  (E)"),
                                    ("R", GizmoMode::Scale, "Scale gizmo  (R)"),
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
                                    let mut show_colliders =
                                        self.settings.show_colliders();
                                    if ui
                                        .checkbox(
                                            &mut show_colliders,
                                            "Colliders",
                                        )
                                        .on_hover_text(
                                            "AABB collider wireframe overlay \
                                             (off by default)",
                                        )
                                        .changed()
                                    {
                                        self.settings
                                            .set_show_colliders(show_colliders);
                                        self.viewer
                                            .set_show_colliders(show_colliders);
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
                            // LOD-detail preview picker. Renders exactly the
                            // simplified geometry a `bundle_lods_and_imposter`
                            // build would ship at each stage; "Imposter…"
                            // bakes the spritesheet the export embeds. Not
                            // persisted — it's a transient preview, and
                            // disabled in cinema mode like the other editor
                            // controls.
                            ui.add_enabled_ui(!cinema_on, |ui| {
                                use crate::app::preview::{PreviewLod, PREVIEW_LODS};
                                let cur_lod = self.preview_lod;
                                let mut chosen_lod: Option<PreviewLod> = None;
                                let mut bake_imposter = false;
                                let label = format!("◇ {}", cur_lod.short());
                                ui.menu_button(label, |ui| {
                                    ui.label(egui::RichText::new("LOD preview").strong());
                                    ui.separator();
                                    for l in PREVIEW_LODS {
                                        let selected = l == cur_lod;
                                        if ui
                                            .selectable_label(selected, l.label())
                                            .clicked()
                                            && !selected
                                        {
                                            chosen_lod = Some(l);
                                            ui.close_menu();
                                        }
                                    }
                                    ui.separator();
                                    if ui
                                        .button("Imposter…")
                                        .on_hover_text(
                                            "Bake and show the scene-wide imposter \
                                             billboard spritesheet the export embeds",
                                        )
                                        .clicked()
                                    {
                                        bake_imposter = true;
                                        ui.close_menu();
                                    }
                                })
                                .response
                                .on_hover_text(
                                    "Preview the LOD stages / imposter the \
                                     `bundle_lods_and_imposter` export bundles",
                                );
                                if let Some(l) = chosen_lod {
                                    self.set_preview_lod(l);
                                }
                                if bake_imposter {
                                    self.start_imposter_preview(ctx);
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
                            // Camera-mode dropdown: Orbit / Cinema / Free cam.
                            // Cinema suppresses orbit/pan/zoom + gizmo + grid;
                            // free cam flies with WASD + right-drag look. Kept
                            // outside the disabled group so the user can always
                            // switch back out of a special mode.
                            use crate::viewer::CameraMode;
                            let cur_mode = self.viewer.camera_mode();
                            let mut chosen_mode: Option<CameraMode> = None;
                            ui.menu_button(cur_mode.label(), |ui| {
                                ui.label(egui::RichText::new("Camera").strong());
                                ui.separator();
                                for (m, tip) in [
                                    (
                                        CameraMode::Orbit,
                                        "Drag to orbit · middle/right-drag to pan · \
                                         scroll to zoom",
                                    ),
                                    (
                                        CameraMode::Cinema,
                                        "Play an automated sequence of camera shots. \
                                         Toggle Play first if you want the subject to \
                                         move while the camera pans.",
                                    ),
                                    (
                                        CameraMode::FreeCam,
                                        "Fly the camera: WASD / arrow keys to move along \
                                         the look vector, hold right-click to look around, \
                                         Shift to move faster.",
                                    ),
                                ] {
                                    let selected = m == cur_mode;
                                    if ui
                                        .selectable_label(selected, m.label())
                                        .on_hover_text(tip)
                                        .clicked()
                                        && !selected
                                    {
                                        chosen_mode = Some(m);
                                        ui.close_menu();
                                    }
                                }
                            })
                            .response
                            .on_hover_text("Viewport camera (Orbit / Cinema / Free cam)");
                            if let Some(m) = chosen_mode {
                                self.viewer.set_camera_mode(m);
                            }
                        });
                    });
            });

        // Bottom-left status strip: camera-controls hint, or the active cinema
        // shot label. Lives in its own `Area` so the top toolbar stays compact
        // and never competes with the right-hand inspector for horizontal room
        // (DCC convention — Blender/Maya put help text at the bottom).
        let cinema_on = self.viewer.is_cinema_active();
        let free_cam_on = self.viewer.camera_mode() == crate::viewer::CameraMode::FreeCam;
        let ctrl_held = ctx.input(|i| i.modifiers.ctrl || i.modifiers.command);
        // A broken shader outranks every hint below: the viewport is actively
        // showing something other than what the file asks for, and without this
        // the only symptom is a material that looks stubbornly untouched.
        let shader_err = self.viewer.shader_error();
        let status_text: Option<String> = if let Some((name, msg)) = &shader_err {
            Some(format!(
                "shader \"{name}\" failed — drawing standard PBR instead: {msg}"
            ))
        } else if cinema_on {
            self.viewer
                .cinema_shot_label()
                .map(|name| format!("now: {name}"))
        } else if free_cam_on {
            Some(
                "free cam — wasd / arrows: move · right-drag: look · \
                 scroll: speed · shift: faster · click: select"
                    .to_string(),
            )
        } else if ctrl_held {
            // Ctrl held = snap mode. Surface the actual step values so users
            // know exactly what they're snapping to before committing a drag.
            Some(format!(
                "snap: translate {}u · rotate {}° · scale {}× — release ctrl for free drag",
                crate::viewer::state::TRANSLATE_SNAP_STEP,
                crate::viewer::state::ROTATE_SNAP_STEP_DEG,
                crate::viewer::state::SCALE_SNAP_STEP,
            ))
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
                            // Non-wrapping: a wrapping label in an auto-sized
                            // Area collapses to minimum width during egui's
                            // sizing pass and the squashed size then sticks in
                            // area memory.
                            ui.add(
                                egui::Label::new(egui::RichText::new(text).weak())
                                    .wrap_mode(egui::TextWrapMode::Extend),
                            );
                        });
                });
        }

        // First-launch / empty-buffer state. Rendered centred over the
        // viewport so the user has somewhere to start instead of staring at
        // a black canvas. Suppressed when the buffer has DSL but failed to
        // render — the diagnostics panel is the relevant signal there, not a
        // "no scene loaded" nudge.
        let buffer_empty = self
            .files
            .get(self.active)
            .map(|f| f.source.trim().is_empty())
            .unwrap_or(true);
        if !self.viewer.has_scene() && buffer_empty {
            egui::Area::new(egui::Id::new("viewport_empty_state"))
                .fixed_pos(viewport_rect.center())
                .pivot(egui::Align2::CENTER_CENTER)
                .interactable(false)
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style())
                        .fill(ui.visuals().window_fill().linear_multiply(0.85))
                        .inner_margin(egui::Margin::symmetric(20.0, 16.0))
                        .show(ui, |ui| {
                            ui.vertical_centered(|ui| {
                                ui.label(
                                    egui::RichText::new("No scene loaded yet")
                                        .heading()
                                        .strong(),
                                );
                                ui.add_space(6.0);
                                ui.label(
                                    "Type DSL in the editor on the left, or use one of:",
                                );
                                ui.add_space(4.0);
                                ui.label(
                                    egui::RichText::new(
                                        "• File → Open…  to load a .mog file\n\
                                         • File → New from Prompt…  to generate one with AI\n\
                                         • Right-click the viewport  to add a primitive",
                                    )
                                    .weak(),
                                );
                            });
                        });
                });
        }
    }
}
