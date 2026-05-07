use std::time::{Duration, Instant};

use eframe::egui;
use mogen_llm::textures::TextureStage;
use mogen_llm::Provider;

use crate::app::types::{LlmEvent, LlmEventTone, LlmKind, LlmProgress};
use crate::app::MogenStudioApp;

impl MogenStudioApp {
    /// Bordered progress card shown while a Gemini call is running on the
    /// active file. Replaces the bare spinner+text with: a kind-coloured pill
    /// header, elapsed time, a stage-specific detail row (repair dots or a
    /// texture progress bar), and a short timeline of recent events.
    pub(super) fn ui_llm_progress_card(&mut self, ui: &mut egui::Ui) {
        let Some(kind) = self.active().llm_in_flight else {
            return;
        };
        let accent = kind_color(kind);
        let started_at = self.active().llm_started_at;
        let progress = self.active().llm_progress.clone();
        let max_iters = self.settings.max_repair_iters();
        let provider = self.settings.provider();
        let events: Vec<LlmEvent> = self.active().llm_events.clone();

        let card_bg = ui.visuals().faint_bg_color;
        let mut cancel_clicked = false;

        egui::Frame::none()
            .fill(card_bg)
            .stroke(egui::Stroke::new(1.0, accent))
            .rounding(egui::Rounding::same(4.0))
            .inner_margin(egui::Margin::symmetric(10.0, 8.0))
            .show(ui, |ui| {
                // ── header row: pill · stage caption · elapsed time ─────
                ui.horizontal(|ui| {
                    draw_kind_pill(ui, kind, accent);
                    ui.add_space(6.0);
                    ui.spinner();
                    ui.label(egui::RichText::new(stage_headline(&progress, kind, provider)));
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            if let Some(t0) = started_at {
                                ui.label(
                                    egui::RichText::new(format_elapsed(t0.elapsed()))
                                        .monospace()
                                        .weak(),
                                );
                            }
                        },
                    );
                });

                // ── stage detail: repair dots OR texture progress bar ───
                let repair = matches!(progress, Some(LlmProgress::Repair { .. }));
                let texture = matches!(progress, Some(LlmProgress::Texture { .. }));
                if repair || texture || kind == LlmKind::Textures {
                    ui.add_space(6.0);
                }
                if let Some(LlmProgress::Repair { iter, max, errors }) = &progress {
                    // `max` from the worker; fall back to settings if the
                    // worker reported 0 for any reason.
                    let max = if *max > 0 { *max } else { max_iters.max(1) };
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("repair loop").weak());
                        ui.add_space(4.0);
                        draw_repair_dots(ui, *iter, max, accent);
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(format!("{iter}/{max}"))
                                .monospace(),
                        );
                        ui.label(
                            egui::RichText::new(format!(
                                "· {errors} error{} to fix",
                                if *errors == 1 { "" } else { "s" }
                            ))
                            .color(ui.visuals().warn_fg_color),
                        );
                    });
                } else if let Some(LlmProgress::Texture {
                    current,
                    total,
                    material,
                    stage,
                }) = &progress
                {
                    let frac = if *total == 0 {
                        0.0
                    } else {
                        (*current as f32) / (*total as f32)
                    };
                    // Use a finite desired width — f32::INFINITY poisons the
                    // widget's interact rect and crashes egui's hit_test when
                    // comparing WidgetRect for equality (NaN != NaN).
                    let bar_width = ui.available_width().max(1.0);
                    ui.add(
                        egui::ProgressBar::new(frac.clamp(0.0, 1.0))
                            .desired_width(bar_width)
                            .fill(accent)
                            .text(format!("{current}/{total}")),
                    );
                    let verb = match stage {
                        TextureStage::Generating => "generating",
                        TextureStage::Existing => "using existing PNG for",
                        TextureStage::Deriving => "deriving PBR for",
                        TextureStage::Done => "finished",
                        TextureStage::Failed => "failed —",
                    };
                    ui.label(
                        egui::RichText::new(format!("{verb} {material}"))
                            .weak(),
                    );
                }

                // ── timeline of recent events (oldest → newest) ─────────
                let now = Instant::now();
                let visible: Vec<&LlmEvent> = events
                    .iter()
                    .rev()
                    .take(4)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                if !visible.is_empty() {
                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(2.0);
                    // Freeze each finished event's timer at the moment the
                    // next event was logged; only the most recent event
                    // keeps ticking against `now`.
                    for i in 0..visible.len() {
                        let ev = visible[i];
                        let until = visible
                            .get(i + 1)
                            .map(|next| next.at)
                            .unwrap_or(now);
                        draw_timeline_row(ui, ev, until, accent);
                    }
                }

                // ── cancel button, right-aligned at the bottom ──────────
                // Wrap in ui.horizontal so the inner right_to_left(Center)
                // layout gets a finite row height. A bare with_layout here
                // inherits the vertical Frame's max_rect (y = INFINITY inside
                // a ScrollArea), and Center vertical-align + INFINITY height
                // produces a NaN frame rect in egui's layout math.
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            if ui
                                .button("Cancel")
                                .on_hover_text(
                                    "Stop waiting and discard the result. Any \
                                     in-flight call still completes server-side \
                                     and is billed normally — Studio just \
                                     ignores the response. (Esc)",
                                )
                                .clicked()
                            {
                                cancel_clicked = true;
                            }
                        },
                    );
                });
            });

        if cancel_clicked {
            self.cancel_active_llm();
        }

        // Keep the elapsed counter ticking even when the worker is quietly
        // waiting on an HTTP response (no Progress events to trigger paints).
        ui.ctx().request_repaint_after(Duration::from_millis(200));
    }
}

/// Kind-specific accent colour used for the card's stroke, pill fill, repair
/// dots, and texture progress bar. Chosen so each LLM kind reads distinctly
/// even at a glance and stays legible on both the dark and light themes.
fn kind_color(k: LlmKind) -> egui::Color32 {
    match k {
        LlmKind::Generate => egui::Color32::from_rgb(110, 170, 230),
        LlmKind::Modify => egui::Color32::from_rgb(120, 210, 180),
        LlmKind::Animate => egui::Color32::from_rgb(200, 140, 220),
        LlmKind::Repair => egui::Color32::from_rgb(220, 130, 130),
        LlmKind::Textures => egui::Color32::from_rgb(230, 160, 100),
        // Amber — distinct from Textures' orange, signals "look at the
        // render" without colliding with the existing palette.
        LlmKind::Refine => egui::Color32::from_rgb(220, 180, 100),
    }
}

/// Rounded capsule tagging the card with which kind of call is running.
/// Uses manual painter calls rather than a Button so it can't be clicked by
/// mistake and so the fill colour tracks the kind accent exactly.
fn draw_kind_pill(ui: &mut egui::Ui, kind: LlmKind, accent: egui::Color32) {
    let text = kind.label().to_uppercase();
    let galley = ui.painter().layout_no_wrap(
        text,
        egui::FontId::proportional(11.0),
        egui::Color32::BLACK,
    );
    let pad = egui::vec2(8.0, 3.0);
    let desired = galley.size() + pad * 2.0;
    let (rect, _resp) = ui.allocate_exact_size(desired, egui::Sense::hover());
    let rounding = egui::Rounding::same(rect.height() * 0.5);
    ui.painter().rect_filled(rect, rounding, accent);
    let pos = rect.center() - galley.size() * 0.5;
    ui.painter().galley(pos, galley, egui::Color32::BLACK);
}

/// Render `max` dots, filling `filled` of them with `accent` and leaving the
/// rest as faint outlines. The dot just before `filled` pulses so users can
/// tell the pipeline is live even when the count hasn't ticked yet.
fn draw_repair_dots(ui: &mut egui::Ui, filled: u32, max: u32, accent: egui::Color32) {
    let dot_size = 10.0;
    let gap = 4.0;
    let count = max.max(1);
    let width = (dot_size * count as f32) + (gap * (count.saturating_sub(1)) as f32);
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(width, dot_size),
        egui::Sense::hover(),
    );
    let painter = ui.painter();
    let empty_stroke = egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color);
    // Pulse the "currently active" dot (index = filled, 0-based) using a sine
    // of time so users see the pipeline is alive between Progress events.
    let pulse = 0.5
        + 0.5
            * (ui.ctx().input(|i| i.time) as f32 * std::f32::consts::TAU * 1.5)
                .sin();
    let active_idx = filled; // about to run
    for i in 0..count {
        let cx = rect.left() + dot_size * 0.5 + (dot_size + gap) * i as f32;
        let center = egui::pos2(cx, rect.center().y);
        if i < filled {
            painter.circle_filled(center, dot_size * 0.5, accent);
        } else if i == active_idx {
            // Subtle pulse: ring + inner dot that fades between 25% and 70%.
            let alpha = (0.25 + 0.45 * pulse).clamp(0.0, 1.0);
            let c = egui::Color32::from_rgba_unmultiplied(
                accent.r(),
                accent.g(),
                accent.b(),
                (alpha * 255.0) as u8,
            );
            painter.circle_filled(center, dot_size * 0.5, c);
            painter.circle_stroke(center, dot_size * 0.5, empty_stroke);
        } else {
            painter.circle_stroke(center, dot_size * 0.5, empty_stroke);
        }
    }
}

/// One line in the card's activity log. Shows a coloured leading bullet,
/// the stage message, and the duration of the step on the right
/// (`until - ev.at`). For finished events the caller passes the timestamp
/// of the next event so the timer freezes; for the in-flight event the
/// caller passes `now` so it keeps ticking.
fn draw_timeline_row(
    ui: &mut egui::Ui,
    ev: &LlmEvent,
    until: Instant,
    accent: egui::Color32,
) {
    let bullet_color = match ev.tone {
        LlmEventTone::Info => ui.visuals().widgets.noninteractive.fg_stroke.color,
        LlmEventTone::Repair => ui.visuals().warn_fg_color,
        LlmEventTone::Texture => accent,
        LlmEventTone::Done => egui::Color32::from_rgb(120, 200, 140),
    };
    ui.horizontal(|ui| {
        // Bullet.
        let (dot_rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
        ui.painter()
            .circle_filled(dot_rect.center(), 3.0, bullet_color);
        ui.add(
            egui::Label::new(egui::RichText::new(&ev.text)).truncate(),
        );
        ui.with_layout(
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                let age = until.saturating_duration_since(ev.at);
                ui.label(
                    egui::RichText::new(format_age(age))
                        .weak()
                        .monospace(),
                );
            },
        );
    });
}

/// Headline string shown next to the spinner. Prefers the most recent
/// progress event; falls back to a kind-appropriate "starting…" when the
/// worker hasn't emitted anything yet.
fn stage_headline(p: &Option<LlmProgress>, kind: LlmKind, provider: Provider) -> String {
    let provider_name = provider.display_name();
    match p {
        Some(LlmProgress::Status(s)) => s.clone(),
        Some(LlmProgress::Repair { iter, max, errors }) => format!(
            "repair {iter}/{max} — {errors} error{} → re-calling {provider_name}",
            if *errors == 1 { "" } else { "s" }
        ),
        Some(LlmProgress::Texture {
            current,
            total,
            material,
            stage,
        }) => {
            let verb = match stage {
                TextureStage::Generating => "generating",
                TextureStage::Existing => "using existing PNG for",
                TextureStage::Deriving => "deriving PBR for",
                TextureStage::Done => "finished",
                TextureStage::Failed => "failed —",
            };
            format!("{current}/{total} — {verb} {material}")
        }
        None => match kind {
            LlmKind::Generate
            | LlmKind::Modify
            | LlmKind::Animate
            | LlmKind::Repair
            | LlmKind::Refine => format!("waiting for {provider_name}…"),
            LlmKind::Textures => "preparing texture plan…".into(),
        },
    }
}

/// Elapsed time in `0.3s` / `12.4s` / `1m 05s` style. Tuned so the header
/// never wraps and the unit is easy to scan.
fn format_elapsed(d: Duration) -> String {
    let secs = d.as_secs_f64();
    if secs < 10.0 {
        format!("{:.1}s", secs)
    } else if secs < 60.0 {
        format!("{:.0}s", secs)
    } else {
        let m = (secs / 60.0).floor() as u64;
        let s = (secs % 60.0) as u64;
        format!("{m}m {s:02}s")
    }
}

/// Shorter relative-age format for timeline entries ("0.4s", "3s", "47s", "2m").
/// Sub-second precision under 10s so very fast steps don't all collapse to "now".
fn format_age(d: Duration) -> String {
    let secs_f = d.as_secs_f64();
    if secs_f < 10.0 {
        format!("{:.1}s", secs_f)
    } else if secs_f < 60.0 {
        format!("{:.0}s", secs_f)
    } else {
        let m = (secs_f / 60.0).floor() as u64;
        format!("{m}m")
    }
}
