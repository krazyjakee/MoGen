use eframe::egui;
use mogen_core::Severity;

use crate::app::util::offset_to_line_col;
use crate::app::MogenStudioApp;
use crate::pipeline::Stage;

impl MogenStudioApp {
    /// True when the active file has at least one error- or warning-level
    /// diagnostic. Drives the editor's footer panel visibility — info-only
    /// or clean states keep the panel hidden so the editor reclaims the
    /// space.
    pub(in crate::app) fn has_blocking_diagnostics(&self) -> bool {
        let Some(result) = &self.files[self.active].last_result else {
            return false;
        };
        result
            .diagnostics
            .iter()
            .any(|d| matches!(d.severity, Severity::Error | Severity::Warning))
    }

    /// One-line summary of the active MOG file's validator state, shown as
    /// the header for the collapsible diagnostics footer panel. Callers only
    /// need the string.
    pub(in crate::app) fn diagnostics_header_label(&self) -> String {
        let f = &self.files[self.active];
        let Some(result) = &f.last_result else {
            return "Diagnostics — (no build yet)".to_string();
        };
        if result.diagnostics.is_empty() {
            return match result.stage {
                Stage::Ok => "Diagnostics — ✓ ok".to_string(),
                other => format!("Diagnostics — {other:?}"),
            };
        }
        let mut errs = 0usize;
        let mut warns = 0usize;
        let mut infos = 0usize;
        for d in &result.diagnostics {
            match d.severity {
                Severity::Error => errs += 1,
                Severity::Warning => warns += 1,
                Severity::Info => infos += 1,
            }
        }
        let mut parts = Vec::new();
        if errs > 0 {
            parts.push(format!("{errs} error{}", if errs == 1 { "" } else { "s" }));
        }
        if warns > 0 {
            parts.push(format!("{warns} warning{}", if warns == 1 { "" } else { "s" }));
        }
        if infos > 0 {
            parts.push(format!("{infos} info"));
        }
        format!("Diagnostics — {}", parts.join(", "))
    }

    pub(in crate::app) fn ui_diagnostics(&mut self, ui: &mut egui::Ui) {
        let f = &self.files[self.active];
        let Some(result) = &f.last_result else {
            ui.label("(no build yet)");
            return;
        };
        if result.diagnostics.is_empty() {
            match result.stage {
                Stage::Ok => {
                    ui.colored_label(egui::Color32::from_rgb(80, 200, 120), "✓ ok");
                }
                _ => {
                    ui.label(format!("{:?}", result.stage));
                }
            }
            return;
        }
        for d in &result.diagnostics {
            let (color, tag) = match d.severity {
                Severity::Error => (egui::Color32::from_rgb(230, 100, 100), "error"),
                Severity::Warning => (egui::Color32::from_rgb(230, 200, 100), "warn"),
                Severity::Info => (egui::Color32::from_rgb(150, 180, 230), "info"),
            };
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(color, format!("[{tag}] {}", d.code));
                if let Some(span) = d.span {
                    let (line, col) = offset_to_line_col(&f.source, span.start);
                    ui.label(format!("{line}:{col}"));
                }
                ui.label(&d.message);
            });
        }
    }
}
