//! Studio "Spending" panel — the read side of the spend tracker (issue 60).
//!
//! Persistence lives in [`mogen_llm::spend`]; this module just renders the
//! data, supplies filters, and exports CSV. Read queries open a fresh
//! SQLite connection through the installed global recorder (the writer
//! thread holds its own connection), so the panel never contends with
//! the recorder for write locks.
//!
//! UI follows `CLAUDE.md`'s text-size convention: no `.small()` on body
//! copy. The compact chart uses `egui::Painter` directly so it adapts to
//! whichever theme is active without a separate plot crate.

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use eframe::egui;
use mogen_llm::spend::{self, CallFilter, CallRow, Distinct, ModelSummary, SpendRecorder, SummaryRow};

use crate::app::MogenStudioApp;

/// Persistent state for the Spending panel. Holds the user's filter
/// choices and the most recent query results. Refreshes on open, on
/// filter change, and when the user clicks Refresh.
pub(in crate::app) struct SpendingState {
    /// Filter combobox draft — `"All"` is the unfiltered fallback.
    pub scene_filter: String,
    pub model_filter: String,
    pub operation_filter: String,
    pub range: TimeRange,
    /// Cached query result, refreshed on filter change. The panel
    /// re-renders from these snapshots so a slow DB read doesn't stall
    /// a frame.
    pub summary: SummaryRow,
    pub by_model: Vec<ModelSummary>,
    pub recent: Vec<CallRow>,
    pub distinct: Distinct,
    /// Time-bucketed series for the chart, computed alongside `summary`.
    pub series: Vec<TimeBucket>,
    /// Bucket granularity for the chart.
    pub bucket: Granularity,
    /// Whether this scene is the only one being shown — used by the
    /// per-file pill to switch the panel into single-scene mode without
    /// hunting the dropdown.
    pub scope_to_active_scene: bool,
    last_refreshed: Option<Instant>,
    /// CSV export status line shown briefly after a successful save.
    pub last_export: Option<String>,
}

impl Default for SpendingState {
    fn default() -> Self {
        Self {
            scene_filter: "All".into(),
            model_filter: "All".into(),
            operation_filter: "All".into(),
            range: TimeRange::Last30Days,
            summary: SummaryRow::default(),
            by_model: Vec::new(),
            recent: Vec::new(),
            distinct: Distinct::default(),
            series: Vec::new(),
            bucket: Granularity::Day,
            scope_to_active_scene: false,
            last_refreshed: None,
            last_export: None,
        }
    }
}

/// Time-range filter chip. Maps to a Unix-seconds `from_ts` at refresh
/// time so the SQLite index can be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeRange {
    Today,
    Last7Days,
    Last30Days,
    Last90Days,
    AllTime,
}

impl TimeRange {
    pub fn label(self) -> &'static str {
        match self {
            TimeRange::Today => "Today",
            TimeRange::Last7Days => "7 days",
            TimeRange::Last30Days => "30 days",
            TimeRange::Last90Days => "90 days",
            TimeRange::AllTime => "All time",
        }
    }

    /// Inclusive lower bound on `ts` (unix seconds). `None` means no
    /// lower bound (All Time).
    pub fn from_ts(self, now: i64) -> Option<i64> {
        let day = 86_400;
        match self {
            TimeRange::Today => Some(now - now.rem_euclid(day)),
            TimeRange::Last7Days => Some(now - 7 * day),
            TimeRange::Last30Days => Some(now - 30 * day),
            TimeRange::Last90Days => Some(now - 90 * day),
            TimeRange::AllTime => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Granularity {
    Day,
    Week,
}

impl Granularity {
    pub fn seconds(self) -> i64 {
        match self {
            Granularity::Day => 86_400,
            Granularity::Week => 7 * 86_400,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Granularity::Day => "Daily",
            Granularity::Week => "Weekly",
        }
    }
}

/// One time-bucket sample for the chart. Cost split by model so the
/// chart can render one line per model without re-grouping at paint
/// time.
#[derive(Debug, Clone, Default)]
pub struct TimeBucket {
    pub ts_start: i64,
    pub total_cost_usd: f64,
    /// (model, cost) pairs. The keys are stable across buckets so the
    /// panel can pick a consistent colour per model.
    pub per_model: Vec<(String, f64)>,
}

impl SpendingState {
    /// Build the [`CallFilter`] for the current panel selections. Public
    /// so the per-file pill can preview the same filter the panel will
    /// render under.
    pub fn build_filter(&self, active_scene: Option<&str>) -> CallFilter {
        let now = now_unix();
        let scene = if self.scope_to_active_scene {
            active_scene.map(|s| s.to_string())
        } else if self.scene_filter == "All" || self.scene_filter.is_empty() {
            None
        } else {
            Some(self.scene_filter.clone())
        };
        let pick = |s: &str| -> Option<String> {
            if s == "All" || s.is_empty() {
                None
            } else {
                Some(s.to_string())
            }
        };
        CallFilter {
            from_ts: self.range.from_ts(now),
            to_ts: None,
            scene_path: scene,
            model: pick(&self.model_filter),
            provider: None,
            operation: pick(&self.operation_filter),
            session_id: None,
            limit: 500,
        }
    }
}

impl MogenStudioApp {
    /// Open or reopen the Spending panel. Forces a refresh so the most
    /// recent calls are visible immediately.
    pub(in crate::app) fn open_spending_panel(&mut self) {
        self.show_spending = true;
        self.spending.scope_to_active_scene = false;
        self.refresh_spending();
    }

    /// Per-scene shortcut — opens the panel and pre-filters to the active
    /// file's scene path so the user lands on a per-file view.
    pub(in crate::app) fn open_spending_for_active(&mut self) {
        self.show_spending = true;
        self.spending.scope_to_active_scene = true;
        self.refresh_spending();
    }

    /// Run the current filter against the spend DB. Best-effort — failures
    /// leave the cached rows empty rather than panic.
    pub(in crate::app) fn refresh_spending(&mut self) {
        let Some(rec) = spend::global() else {
            self.spending.summary = SummaryRow::default();
            self.spending.by_model.clear();
            self.spending.recent.clear();
            self.spending.series.clear();
            self.spending.distinct = Distinct::default();
            return;
        };
        let active_scene = self
            .active()
            .path
            .as_ref()
            .map(|p| p.display().to_string());
        let filter = self.spending.build_filter(active_scene.as_deref());

        self.spending.summary = rec.summary(&filter);
        self.spending.by_model = rec.by_model(&filter);
        self.spending.recent = rec.query(&filter);
        self.spending.distinct = rec.distinct();
        self.spending.series = compute_time_series(
            rec.as_ref(),
            &filter,
            self.spending.bucket,
            self.spending.range,
        );
        self.spending.last_refreshed = Some(Instant::now());
    }

    /// Per-file spending pill rendered inside the inspector. Shows the
    /// active file's total spend + a small per-model breakdown tooltip;
    /// clicking opens the full Spending panel filtered to this scene.
    pub(in crate::app) fn ui_scene_spend_pill(&mut self, ui: &mut egui::Ui) {
        let Some(rec) = spend::global() else { return };
        let scene_path = match self
            .active()
            .path
            .as_ref()
            .map(|p| p.display().to_string())
        {
            Some(p) => p,
            None => return,
        };
        // One-shot per-call summary — cheap (a couple of indexed COUNTs).
        let filter = CallFilter {
            scene_path: Some(scene_path.clone()),
            limit: 0,
            ..Default::default()
        };
        let summary = rec.summary(&filter);
        let by_model = rec.by_model(&filter);
        if summary.total_calls == 0 {
            return;
        }
        let label = format!(
            "Scene spend: {}  ·  {} call{}",
            format_usd(summary.total_cost_usd),
            summary.total_calls,
            if summary.total_calls == 1 { "" } else { "s" },
        );
        let resp = ui.button(label).on_hover_ui(|ui| {
            ui.label("Per-model breakdown:");
            ui.separator();
            for m in &by_model {
                ui.label(format!(
                    "{}  ·  {}  ·  {} call{}",
                    m.model,
                    format_usd(m.total_cost_usd),
                    m.total_calls,
                    if m.total_calls == 1 { "" } else { "s" },
                ));
            }
        });
        if resp.clicked() {
            self.open_spending_for_active();
        }
    }

    /// Modal-but-non-blocking Spending window. Top-level window so it
    /// can co-exist with the rest of the inspector while the user
    /// switches between scenes / filters.
    pub(in crate::app) fn ui_spending_window(&mut self, ctx: &egui::Context) {
        if !self.show_spending {
            return;
        }
        let mut open = true;
        let mut should_refresh = false;
        let mut should_export = false;
        let mut should_clear_scope = false;
        let active_scene = self
            .active()
            .path
            .as_ref()
            .map(|p| p.display().to_string());

        egui::Window::new("Spending")
            .open(&mut open)
            .default_size([880.0, 560.0])
            .resizable(true)
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Range:");
                    for r in [
                        TimeRange::Today,
                        TimeRange::Last7Days,
                        TimeRange::Last30Days,
                        TimeRange::Last90Days,
                        TimeRange::AllTime,
                    ] {
                        if ui
                            .selectable_label(self.spending.range == r, r.label())
                            .clicked()
                        {
                            self.spending.range = r;
                            should_refresh = true;
                        }
                    }
                    ui.separator();
                    ui.label("Bucket:");
                    for g in [Granularity::Day, Granularity::Week] {
                        if ui
                            .selectable_label(self.spending.bucket == g, g.label())
                            .clicked()
                        {
                            self.spending.bucket = g;
                            should_refresh = true;
                        }
                    }
                });

                ui.horizontal_wrapped(|ui| {
                    if self.spending.scope_to_active_scene {
                        let label = active_scene
                            .as_deref()
                            .map(short_path)
                            .unwrap_or_else(|| "active scene".into());
                        ui.label(format!("Scene: {}", label));
                        if ui.button("All scenes").clicked() {
                            should_clear_scope = true;
                            should_refresh = true;
                        }
                    } else {
                        ui.label("Scene:");
                        let prev = self.spending.scene_filter.clone();
                        egui::ComboBox::from_id_salt("spend_scene")
                            .selected_text(short_path(&self.spending.scene_filter))
                            .width(220.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.spending.scene_filter,
                                    "All".into(),
                                    "All scenes",
                                );
                                for s in &self.spending.distinct.scenes {
                                    ui.selectable_value(
                                        &mut self.spending.scene_filter,
                                        s.clone(),
                                        short_path(s),
                                    );
                                }
                            });
                        if prev != self.spending.scene_filter {
                            should_refresh = true;
                        }
                    }
                    ui.separator();
                    ui.label("Model:");
                    let prev = self.spending.model_filter.clone();
                    egui::ComboBox::from_id_salt("spend_model")
                        .selected_text(&self.spending.model_filter)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.spending.model_filter,
                                "All".into(),
                                "All models",
                            );
                            for m in &self.spending.distinct.models {
                                ui.selectable_value(
                                    &mut self.spending.model_filter,
                                    m.clone(),
                                    m,
                                );
                            }
                        });
                    if prev != self.spending.model_filter {
                        should_refresh = true;
                    }
                    ui.separator();
                    ui.label("Operation:");
                    let prev = self.spending.operation_filter.clone();
                    egui::ComboBox::from_id_salt("spend_op")
                        .selected_text(&self.spending.operation_filter)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.spending.operation_filter,
                                "All".into(),
                                "All ops",
                            );
                            for op in &self.spending.distinct.operations {
                                ui.selectable_value(
                                    &mut self.spending.operation_filter,
                                    op.clone(),
                                    op,
                                );
                            }
                        });
                    if prev != self.spending.operation_filter {
                        should_refresh = true;
                    }
                    ui.separator();
                    if ui.button("Refresh").clicked() {
                        should_refresh = true;
                    }
                    if ui.button("Export CSV…").clicked() {
                        should_export = true;
                    }
                });

                ui.separator();

                // Summary row — big numbers up top so the panel reads as
                // an at-a-glance dashboard.
                let s = &self.spending.summary;
                ui.horizontal_wrapped(|ui| {
                    summary_chip(ui, "Total spend", &format_usd(s.total_cost_usd));
                    summary_chip(
                        ui,
                        "Calls",
                        &format_count(s.total_calls),
                    );
                    summary_chip(
                        ui,
                        "Prompt tok",
                        &format_count(s.total_prompt_tokens),
                    );
                    summary_chip(
                        ui,
                        "Response tok",
                        &format_count(s.total_response_tokens),
                    );
                    summary_chip(
                        ui,
                        "Cached tok",
                        &format_count(s.total_cached_tokens),
                    );
                    summary_chip(ui, "Images", &format_count(s.total_images));
                });

                ui.separator();

                // Chart + legend side by side. The chart is hand-painted
                // with `egui::Painter` so it inherits the active theme
                // for axes and gridlines without a separate plot crate.
                egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
                    paint_time_series(ui, &self.spending.series, &self.spending.by_model);

                    ui.add_space(8.0);
                    ui.collapsing("Per-model breakdown", |ui| {
                        if self.spending.by_model.is_empty() {
                            ui.label("No calls match this filter yet.");
                        } else {
                            egui::Grid::new("spend_by_model_table")
                                .num_columns(5)
                                .striped(true)
                                .show(ui, |ui| {
                                    ui.label("Model");
                                    ui.label("Provider");
                                    ui.label("Cost");
                                    ui.label("Calls");
                                    ui.label("Tokens (in+out)");
                                    ui.end_row();
                                    for m in &self.spending.by_model {
                                        ui.label(&m.model);
                                        ui.label(&m.provider);
                                        ui.label(format_usd(m.total_cost_usd));
                                        ui.label(format_count(m.total_calls));
                                        ui.label(format_count(
                                            m.total_prompt_tokens
                                                + m.total_response_tokens,
                                        ));
                                        ui.end_row();
                                    }
                                });
                        }
                    });

                    ui.add_space(6.0);
                    ui.collapsing("Recent calls", |ui| {
                        if self.spending.recent.is_empty() {
                            ui.label("No calls match this filter yet.");
                        } else {
                            egui::Grid::new("spend_recent_table")
                                .num_columns(6)
                                .striped(true)
                                .show(ui, |ui| {
                                    ui.label("Time");
                                    ui.label("Model");
                                    ui.label("Op");
                                    ui.label("Tokens");
                                    ui.label("Images");
                                    ui.label("Cost");
                                    ui.end_row();
                                    for r in &self.spending.recent {
                                        ui.label(format_ts(r.ts));
                                        ui.label(&r.model);
                                        ui.label(&r.operation);
                                        ui.label(format_tokens(r));
                                        ui.label(format_count(r.image_count as i64));
                                        let cost = format_usd(r.cost_usd);
                                        if !r.success {
                                            ui.colored_label(
                                                ui.visuals().error_fg_color,
                                                cost,
                                            )
                                            .on_hover_text(
                                                r.notes
                                                    .as_deref()
                                                    .unwrap_or("call failed"),
                                            );
                                        } else {
                                            ui.label(cost);
                                        }
                                        ui.end_row();
                                    }
                                });
                        }
                    });
                });

                if let Some(msg) = &self.spending.last_export {
                    ui.add_space(6.0);
                    ui.label(msg.clone());
                }
            });

        self.show_spending = open;
        if should_clear_scope {
            self.spending.scope_to_active_scene = false;
        }
        if should_refresh {
            self.refresh_spending();
        }
        if should_export {
            let path = rfd::FileDialog::new()
                .set_title("Export Spending CSV")
                .add_filter("CSV", &["csv"])
                .save_file();
            if let Some(p) = path {
                match export_csv(&p, &self.spending.recent) {
                    Ok(()) => {
                        self.spending.last_export = Some(format!(
                            "exported {} rows to {}",
                            self.spending.recent.len(),
                            p.display(),
                        ));
                    }
                    Err(e) => {
                        self.spending.last_export = Some(format!("export failed: {e}"));
                    }
                }
            }
        }
    }
}

fn summary_chip(ui: &mut egui::Ui, label: &str, value: &str) {
    let frame = egui::Frame::group(ui.style())
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .rounding(6.0)
        .inner_margin(egui::vec2(8.0, 4.0));
    frame.show(ui, |ui| {
        ui.vertical(|ui| {
            ui.label(egui::RichText::new(label).weak());
            ui.label(
                egui::RichText::new(value)
                    .strong()
                    .text_style(egui::TextStyle::Heading),
            );
        });
    });
}

/// Render a compact line chart of the time series. One line per model
/// (`top` is the by-model summary already sorted by cost descending —
/// we cap at the top six so the chart doesn't render confetti).
fn paint_time_series(
    ui: &mut egui::Ui,
    series: &[TimeBucket],
    top: &[ModelSummary],
) {
    let height = 200.0;
    let (rect, _resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width().max(120.0), height),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);

    // Background + frame match the theme's window stroke so the chart
    // sits inside the panel cleanly.
    painter.rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);
    painter.rect_stroke(
        rect,
        4.0,
        ui.visuals().widgets.noninteractive.bg_stroke,
    );

    if series.is_empty() {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "No data in range",
            egui::FontId::proportional(13.0),
            ui.visuals().widgets.noninteractive.fg_stroke.color,
        );
        return;
    }

    // Pick the top N models by cost; remainder bucketed as "other" so the
    // legend stays readable.
    let palette: &[egui::Color32] = &[
        egui::Color32::from_rgb(76, 175, 240),
        egui::Color32::from_rgb(240, 130, 80),
        egui::Color32::from_rgb(140, 200, 100),
        egui::Color32::from_rgb(200, 120, 200),
        egui::Color32::from_rgb(240, 200, 80),
        egui::Color32::from_rgb(120, 220, 200),
    ];
    let top_models: Vec<&str> = top.iter().take(palette.len()).map(|m| m.model.as_str()).collect();

    // y-axis scale: max cost across buckets (total of all models in the
    // bucket so stacked-ness isn't visually misleading).
    let mut y_max: f64 = series
        .iter()
        .map(|b| b.total_cost_usd)
        .fold(0.0_f64, f64::max);
    if y_max <= 0.0 {
        y_max = 1.0;
    }
    let margin = 18.0;
    let chart = rect.shrink2(egui::vec2(margin + 28.0, margin));

    // Gridlines: 4 horizontal lines from 0..y_max.
    let grid_stroke = egui::Stroke::new(
        1.0,
        ui.visuals().widgets.noninteractive.bg_stroke.color,
    );
    for i in 0..=4 {
        let frac = i as f32 / 4.0;
        let y = chart.bottom() - frac * chart.height();
        painter.line_segment(
            [egui::pos2(chart.left(), y), egui::pos2(chart.right(), y)],
            grid_stroke,
        );
        let value = y_max * frac as f64;
        painter.text(
            egui::pos2(chart.left() - 4.0, y),
            egui::Align2::RIGHT_CENTER,
            format_usd(value),
            egui::FontId::proportional(10.0),
            ui.visuals().weak_text_color(),
        );
    }

    // X-axis ticks: first / last bucket labels only — anything more is
    // unreadable at this size.
    if let (Some(first), Some(last)) = (series.first(), series.last()) {
        painter.text(
            egui::pos2(chart.left(), chart.bottom() + 4.0),
            egui::Align2::LEFT_TOP,
            format_ts_short(first.ts_start),
            egui::FontId::proportional(10.0),
            ui.visuals().weak_text_color(),
        );
        painter.text(
            egui::pos2(chart.right(), chart.bottom() + 4.0),
            egui::Align2::RIGHT_TOP,
            format_ts_short(last.ts_start),
            egui::FontId::proportional(10.0),
            ui.visuals().weak_text_color(),
        );
    }

    let n = series.len().max(1);
    let bucket_to_x = |i: usize| -> f32 {
        if n == 1 {
            chart.center().x
        } else {
            chart.left() + (i as f32) / (n as f32 - 1.0) * chart.width()
        }
    };
    let value_to_y = |v: f64| -> f32 {
        chart.bottom() - (v / y_max).clamp(0.0, 1.0) as f32 * chart.height()
    };

    // One polyline per top model.
    for (mi, model) in top_models.iter().enumerate() {
        let color = palette[mi];
        let points: Vec<egui::Pos2> = series
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let cost = b
                    .per_model
                    .iter()
                    .find(|(m, _)| m == model)
                    .map(|(_, c)| *c)
                    .unwrap_or(0.0);
                egui::pos2(bucket_to_x(i), value_to_y(cost))
            })
            .collect();
        if points.len() >= 2 {
            painter.add(egui::Shape::line(
                points,
                egui::Stroke::new(1.6, color),
            ));
        } else if let Some(p) = points.first() {
            painter.circle_filled(*p, 2.5, color);
        }
    }

    // Total line — bolder, in the foreground text colour. Lets the user
    // see overall spend trajectory even when individual model lines
    // bunch at the bottom.
    let total_color = ui.visuals().text_color();
    let total_pts: Vec<egui::Pos2> = series
        .iter()
        .enumerate()
        .map(|(i, b)| egui::pos2(bucket_to_x(i), value_to_y(b.total_cost_usd)))
        .collect();
    if total_pts.len() >= 2 {
        painter.add(egui::Shape::line(
            total_pts,
            egui::Stroke::new(2.0, total_color),
        ));
    }

    // Legend below the chart.
    ui.horizontal_wrapped(|ui| {
        // Total swatch.
        ui.colored_label(total_color, "▬ Total");
        for (mi, model) in top_models.iter().enumerate() {
            let color = palette[mi];
            ui.colored_label(color, format!("▬ {model}"));
        }
    });
}

/// Time-bucket the matching calls. Hand-rolled in Rust (one extra
/// `query`) rather than via a SQL window so the recorder trait stays
/// minimal. Bucket counts are bounded — at most ~365 daily buckets in
/// the All-Time range — so the post-process is cheap.
fn compute_time_series(
    rec: &dyn SpendRecorder,
    filter: &CallFilter,
    bucket: Granularity,
    range: TimeRange,
) -> Vec<TimeBucket> {
    // Pull every call in range (the limit on `query` is the cap on what
    // we plot — the panel's recent-calls list shows the same rows).
    let mut full = filter.clone();
    full.limit = 5000;
    let rows = rec.query(&full);
    if rows.is_empty() {
        return Vec::new();
    }
    let now = now_unix();
    let earliest = match range.from_ts(now) {
        Some(t) => t,
        None => rows.iter().map(|r| r.ts).min().unwrap_or(now),
    };
    let bucket_secs = bucket.seconds();
    let n_buckets =
        (((now - earliest) / bucket_secs) + 1).max(1).min(400) as usize;
    let mut buckets: Vec<TimeBucket> = (0..n_buckets)
        .map(|i| TimeBucket {
            ts_start: earliest + i as i64 * bucket_secs,
            total_cost_usd: 0.0,
            per_model: Vec::new(),
        })
        .collect();
    for r in rows {
        let idx = ((r.ts - earliest) / bucket_secs).max(0) as usize;
        if idx >= buckets.len() {
            continue;
        }
        buckets[idx].total_cost_usd += r.cost_usd;
        let entry = buckets[idx]
            .per_model
            .iter_mut()
            .find(|(m, _)| m == &r.model);
        match entry {
            Some(e) => e.1 += r.cost_usd,
            None => buckets[idx].per_model.push((r.model.clone(), r.cost_usd)),
        }
    }
    buckets
}

/// Dump `rows` to `path` as CSV. The header column order matches the
/// SQLite schema so a downstream `sqlite3 .import` round-trips without
/// a manual mapping step.
fn export_csv(path: &Path, rows: &[CallRow]) -> std::io::Result<()> {
    let mut f = File::create(path)?;
    writeln!(
        f,
        "ts,provider,model,operation,prompt_tokens,response_tokens,cached_tokens,image_count,cost_usd,scene_path,session_id,success,notes"
    )?;
    for r in rows {
        writeln!(
            f,
            "{},{},{},{},{},{},{},{},{:.6},{},{},{},{}",
            r.ts,
            csv_field(&r.provider),
            csv_field(&r.model),
            csv_field(&r.operation),
            r.prompt_tokens,
            r.response_tokens,
            r.cached_tokens,
            r.image_count,
            r.cost_usd,
            csv_field(r.scene_path.as_deref().unwrap_or("")),
            csv_field(r.session_id.as_deref().unwrap_or("")),
            if r.success { 1 } else { 0 },
            csv_field(r.notes.as_deref().unwrap_or("")),
        )?;
    }
    Ok(())
}

fn csv_field(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        let escaped = s.replace('"', "\"\"");
        format!("\"{escaped}\"")
    } else {
        s.to_string()
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Format a USD amount. Two decimals for ≥ $0.01, four for finer-grained
/// totals so "1 token" doesn't round to "$0.00".
pub fn format_usd(v: f64) -> String {
    if v >= 0.01 {
        format!("${v:.2}")
    } else if v > 0.0 {
        format!("${v:.4}")
    } else {
        "$0.00".to_string()
    }
}

fn format_count(n: i64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn format_tokens(r: &CallRow) -> String {
    if r.cached_tokens > 0 {
        format!(
            "{}+{} ({}c)",
            r.prompt_tokens, r.response_tokens, r.cached_tokens
        )
    } else {
        format!("{}+{}", r.prompt_tokens, r.response_tokens)
    }
}

fn format_ts(ts: i64) -> String {
    let now = now_unix();
    let dt = now - ts;
    if dt < 60 {
        "now".to_string()
    } else if dt < 3600 {
        format!("{}m ago", dt / 60)
    } else if dt < 86_400 {
        format!("{}h ago", dt / 3600)
    } else {
        format!("{}d ago", dt / 86_400)
    }
}

fn format_ts_short(ts: i64) -> String {
    let now = now_unix();
    let dt = now - ts;
    if dt < 86_400 {
        "today".to_string()
    } else {
        format!("{}d", dt / 86_400)
    }
}

/// Display the last path component (plus its parent) so the combobox
/// stays narrow. Falls back to the full path when shortening would lose
/// uniqueness.
fn short_path(p: &str) -> String {
    if p == "All" {
        return "All scenes".to_string();
    }
    let pb = std::path::Path::new(p);
    match (pb.parent().and_then(|p| p.file_name()), pb.file_name()) {
        (Some(parent), Some(file)) => {
            format!(
                "{}/{}",
                parent.to_string_lossy(),
                file.to_string_lossy()
            )
        }
        (_, Some(file)) => file.to_string_lossy().to_string(),
        _ => p.to_string(),
    }
}
