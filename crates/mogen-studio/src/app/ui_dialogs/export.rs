use eframe::egui;

use crate::app::MogenStudioApp;

impl MogenStudioApp {
    /// "Build GLB" dialog. Edits an `ExportOptions` draft and, when the user
    /// clicks Build, spawns a background worker that runs the merge / export
    /// pipeline off the UI thread. While a build is in flight the toggles
    /// lock out and the modal shows a spinner + the current stage. When done,
    /// the modal displays the result and a Close button.
    pub(in crate::app) fn ui_export_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_export {
            return;
        }

        let in_flight = self.build_rx.is_some();
        let mut open = true;
        let mut do_build = false;
        let mut do_cancel = false;
        let mut do_close = false;
        let i = self.active;
        let file_has_path = self.files[i].path.is_some();
        let last_status = self.files[i].status.clone();
        let current_stage = self
            .build_stage
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default();

        egui::Window::new("Build GLB")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(460.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label("Compile the active scene to glTF 2.0 binary (.glb).");
                if !file_has_path {
                    ui.add_space(4.0);
                    ui.colored_label(
                        egui::Color32::from_rgb(230, 200, 100),
                        "untitled MOG file — output will be written to the project root \
                         as untitled.glb. Save the MOG file first to export next to it.",
                    );
                }

                ui.add_space(10.0);
                ui.heading("Options");

                let opts = &mut self.export_opts_draft;
                ui.add_enabled_ui(!in_flight, |ui| {
                    ui.checkbox(
                        &mut opts.include_animations,
                        "Include animations",
                    )
                    .on_hover_text(
                        "Emit the scene's `animations[]` array. Off = bake a static GLB.",
                    );
                    ui.checkbox(&mut opts.include_textures, "Include textures")
                        .on_hover_text(
                            "Pack texture images into the GLB binary chunk and wire them to \
                             materials. Off = materials export with only PBR numeric factors.",
                        );
                    ui.checkbox(
                        &mut opts.merge_sibling_meshes,
                        "Merge overlapping meshes (CSG union)",
                    )
                    .on_hover_text(
                        "Collapse same-material, non-skinned sibling meshes under each parent \
                         into a single CSG-unioned mesh. Removes interior geometry where shapes \
                         overlap. Slow on complex scenes. UVs are preserved through the merge \
                         when all operands in a group have them.",
                    );
                });

                ui.add_space(12.0);

                if in_flight {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        let label = if current_stage.is_empty() {
                            "working…".to_string()
                        } else {
                            format!("{current_stage}…")
                        };
                        ui.label(label);
                    });
                    ui.add_space(6.0);
                    if ui
                        .button("Cancel")
                        .on_hover_text(
                            "Stop waiting. The background worker may still finish but its \
                             output is discarded.",
                        )
                        .clicked()
                    {
                        do_cancel = true;
                    }
                } else {
                    // After a build completes, `self.files[i].status` carries
                    // the "wrote X (size)" or "export failed: …" summary —
                    // surface it so the user can see the result in-modal.
                    if last_status.starts_with("wrote ") || last_status.starts_with("export failed") {
                        ui.label(&last_status);
                        ui.add_space(6.0);
                    }
                    ui.horizontal(|ui| {
                        if ui
                            .button("Build")
                            .on_hover_text("Compile + export to .glb with the options above")
                            .clicked()
                        {
                            do_build = true;
                        }
                        if ui.button("Close").clicked() {
                            do_close = true;
                        }
                    });
                }
            });

        if !open || do_close {
            self.show_export = false;
            return;
        }
        if do_cancel {
            self.cancel_build();
            return;
        }
        if do_build {
            let ctx_clone = ctx.clone();
            self.spawn_build(ctx_clone);
        }
    }
}
