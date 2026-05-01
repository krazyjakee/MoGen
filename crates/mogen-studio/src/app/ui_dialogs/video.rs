use eframe::egui;

use crate::app::generate::{VideoCameraMode, VideoQuality};
use crate::app::MogenStudioApp;
use crate::viewer::CaptureKind;

impl MogenStudioApp {
    /// Full-screen scrim + centred progress dialog shown while a video
    /// render or its ffmpeg encode is in flight. The scrim is an `Area` at
    /// `Foreground` order that allocates a screen-sized response with
    /// `click_and_drag` sense — egui consumes those events instead of
    /// passing them through to the menus / editor / viewport behind it, so
    /// every panel beneath visibly dims and stops responding without us
    /// having to wrap each panel in `add_enabled_ui`. The dialog itself
    /// sits at `Tooltip` order so it paints above the scrim.
    pub(in crate::app) fn ui_capture_progress(&mut self, ctx: &egui::Context) {
        // Two states qualify as "video render in flight":
        //   1. Frames are still being rendered by the GL paint callback —
        //      `viewer.capture_progress()` returns the kind + counts.
        //   2. The GL frames are on disk and ffmpeg is encoding — there's
        //      no per-frame progress here, so we fall through to a
        //      spinner-only "Encoding…" view driven by `video_encode`.
        let render = self.viewer.capture_progress();
        let render_video = render
            .filter(|(kind, _, _)| *kind == CaptureKind::Video)
            .map(|(_, done, total)| (done, total));
        let encoding = self.video_encode.is_some();

        // Thumbnails finish in a single paint and don't need a modal — bail
        // out unless a video render or encode is the active step.
        if render_video.is_none() && !encoding {
            return;
        }

        // Scrim: a screen-sized clickable area at Foreground order that
        // both dims the background and swallows mouse / scroll input so the
        // app underneath is effectively disabled.
        egui::Area::new(egui::Id::new("capture_modal_scrim"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::Pos2::ZERO)
            .interactable(true)
            .show(ctx, |ui| {
                let screen = ctx.screen_rect();
                ui.allocate_response(screen.size(), egui::Sense::click_and_drag());
                ui.painter().rect_filled(
                    screen,
                    0.0,
                    egui::Color32::from_black_alpha(160),
                );
            });

        // Dialog above the scrim. Non-collapsible / non-resizable / no
        // close-button: the user has no way to dismiss a render mid-flight,
        // matching how the menu items disable themselves.
        egui::Window::new("Rendering MP4")
            .order(egui::Order::Tooltip)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .collapsible(false)
            .resizable(false)
            .title_bar(false)
            .default_width(360.0)
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(6.0);
                    ui.heading("Rendering MP4");
                    ui.add_space(6.0);
                    if let Some((done, total)) = render_video {
                        let frac = if total == 0 {
                            0.0
                        } else {
                            (done as f32 / total as f32).clamp(0.0, 1.0)
                        };
                        ui.label(format!("Capturing frames: {done} / {total}"));
                        ui.add(
                            egui::ProgressBar::new(frac)
                                .desired_width(320.0)
                                .show_percentage(),
                        );
                    } else {
                        // ffmpeg phase has no per-frame progress to
                        // report — the spinner conveys "still working" and
                        // the 250ms repaint from `poll_generate` keeps it
                        // ticking. ProgressBar::animate only animates when
                        // value < 1, so we'd need a fake fractional value
                        // there; the spinner is simpler and reads the same.
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Encoding via ffmpeg…");
                        });
                    }
                    ui.add_space(2.0);
                    ui.small("The app is paused while the MP4 is generated.");
                    ui.add_space(6.0);
                });
            });
    }

    /// "Render MP4" options modal. Lets the user pick resolution (720p / 1080p)
    /// and camera mode (rotating / static) before the actual render kicks off.
    /// Stays modal-style — anchored centre, non-collapsible — and persists the
    /// draft so the next open defaults to whatever the user chose last.
    pub(in crate::app) fn ui_video_options(&mut self, ctx: &egui::Context) {
        if !self.show_video_options {
            return;
        }
        let mut open = true;
        let mut do_render = false;
        let mut do_close = false;

        egui::Window::new("Render MP4")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(360.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label("Render a 6-second MP4 of the active scene.");
                ui.add_space(10.0);

                ui.heading("Resolution");
                ui.horizontal(|ui| {
                    ui.selectable_value(
                        &mut self.video_opts_draft.quality,
                        VideoQuality::P720,
                        VideoQuality::P720.label(),
                    );
                    ui.selectable_value(
                        &mut self.video_opts_draft.quality,
                        VideoQuality::P1080,
                        VideoQuality::P1080.label(),
                    );
                });

                ui.add_space(8.0);
                ui.heading("Camera");
                ui.horizontal(|ui| {
                    ui.selectable_value(
                        &mut self.video_opts_draft.camera,
                        VideoCameraMode::Rotating,
                        VideoCameraMode::Rotating.label(),
                    )
                    .on_hover_text("Sweep yaw a full 360° across the clip.");
                    ui.selectable_value(
                        &mut self.video_opts_draft.camera,
                        VideoCameraMode::Static,
                        VideoCameraMode::Static.label(),
                    )
                    .on_hover_text(
                        "Hold the thumbnail framing — animations still play across the clip.",
                    );
                });

                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if ui
                        .button("Render")
                        .on_hover_text("Capture frames and encode the MP4 with ffmpeg")
                        .clicked()
                    {
                        do_render = true;
                    }
                    if ui.button("Cancel").clicked() {
                        do_close = true;
                    }
                });
            });

        if !open || do_close {
            self.show_video_options = false;
            return;
        }
        if do_render {
            let opts = self.video_opts_draft;
            self.show_video_options = false;
            self.generate_video(ctx, opts);
        }
    }
}
