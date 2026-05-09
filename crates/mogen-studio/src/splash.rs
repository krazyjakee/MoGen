//! Splash screen with progress bar shown while the app finishes startup.
//!
//! The image bytes are embedded at build time so the binary stays
//! self-contained — `assets/splash.png` is actually a JPEG (the `image`
//! crate's `jpeg` feature is enabled in Cargo.toml), and the format detector
//! handles either form.

use eframe::egui;

const SPLASH_BYTES: &[u8] = include_bytes!("../../../assets/splash.png");

/// Decode the embedded splash bytes into an egui texture handle. Called once
/// during loading; failure is logged and the splash falls back to a solid
/// background so startup still works on a broken asset.
pub fn upload(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let img = match image::load_from_memory(SPLASH_BYTES) {
        Ok(i) => i.to_rgba8(),
        Err(e) => {
            eprintln!("mogen-studio: failed to decode splash image: {e}");
            return None;
        }
    };
    let (w, h) = (img.width() as usize, img.height() as usize);
    let pixels = img.into_raw();
    let color_image = egui::ColorImage::from_rgba_unmultiplied([w, h], &pixels);
    Some(ctx.load_texture(
        "mogen_studio_splash",
        color_image,
        egui::TextureOptions::LINEAR,
    ))
}

/// Paint the splash UI: scaled cover-fit image with a progress bar and stage
/// label overlaid on the lower band. `progress` is clamped to `[0, 1]`.
/// `label` is the "doing X" caption shown above the bar.
pub fn draw(
    ctx: &egui::Context,
    tex: Option<&egui::TextureHandle>,
    progress: f32,
    label: &str,
) {
    let progress = progress.clamp(0.0, 1.0);

    egui::CentralPanel::default()
        .frame(egui::Frame::none().fill(egui::Color32::from_rgb(8, 8, 12)))
        .show(ctx, |ui| {
            // On HiDPI displays the eframe window comes up much larger in
            // physical pixels, which makes the splash feel oversized. Scale
            // the working rect down to half size (centered) so the splash
            // reads at a comfortable size on Retina/2x screens; the panel
            // fill takes care of the surrounding margin.
            let full = ui.max_rect();
            let rect = if ctx.pixels_per_point() >= 1.5 {
                egui::Rect::from_center_size(full.center(), full.size() * 0.5)
            } else {
                full
            };
            let painter = ui.painter();

            // Image: cover-fit (fill the rect, crop the longer axis) so the
            // window has no letterbox bars regardless of aspect ratio.
            if let Some(tex) = tex {
                let img_size = tex.size_vec2();
                let scale = (rect.width() / img_size.x).max(rect.height() / img_size.y);
                let painted_size = img_size * scale;
                let centered = egui::Rect::from_center_size(rect.center(), painted_size);
                painter.image(
                    tex.id(),
                    centered,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }

            // Soft dark gradient at the bottom so the overlay text stays
            // readable regardless of what the splash image shows there.
            let band_height = 96.0;
            let band_top = rect.bottom() - band_height;
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(rect.left(), band_top),
                    egui::pos2(rect.right(), rect.bottom()),
                ),
                0.0,
                egui::Color32::from_black_alpha(170),
            );

            // Layout the bar + label inside the band, with a comfortable
            // horizontal margin proportional to the window width.
            let margin_x = (rect.width() * 0.06).clamp(24.0, 96.0);
            let bar_height = 8.0;
            let bar_rect = egui::Rect::from_min_max(
                egui::pos2(rect.left() + margin_x, rect.bottom() - 36.0),
                egui::pos2(rect.right() - margin_x, rect.bottom() - 36.0 + bar_height),
            );

            // Track + fill. Rounded ends on both so the leading edge of the
            // fill doesn't render as a hard rectangle inside a rounded track.
            let rounding = egui::Rounding::same(bar_height * 0.5);
            painter.rect_filled(
                bar_rect,
                rounding,
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 40),
            );
            if progress > 0.0 {
                let fill_w = bar_rect.width() * progress;
                let fill_rect = egui::Rect::from_min_max(
                    bar_rect.min,
                    egui::pos2(bar_rect.min.x + fill_w, bar_rect.max.y),
                );
                painter.rect_filled(fill_rect, rounding, egui::Color32::from_rgb(232, 226, 210));
            }

            // Stage label sits just above the bar, left-aligned with it.
            let text_pos = egui::pos2(bar_rect.left(), bar_rect.top() - 10.0);
            painter.text(
                text_pos,
                egui::Align2::LEFT_BOTTOM,
                label,
                egui::FontId::proportional(14.5),
                egui::Color32::from_rgb(232, 226, 210),
            );

            // App title on the right edge of the band, same baseline as the
            // stage label, so the splash reads as a branded loader. Version
            // is appended so users can see at a glance which build is loading.
            let title = format!("MoGen Studio v{}", env!("CARGO_PKG_VERSION"));
            let title_pos = egui::pos2(bar_rect.right(), bar_rect.top() - 10.0);
            painter.text(
                title_pos,
                egui::Align2::RIGHT_BOTTOM,
                title,
                egui::FontId::proportional(14.5),
                egui::Color32::from_rgba_unmultiplied(232, 226, 210, 200),
            );
        });
}
