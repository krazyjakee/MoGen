//! "Problems" subsection — the validator diagnostics that fall inside the
//! selected node's source span, surfaced in context.
//!
//! The global diagnostics footer lists every problem for the whole file;
//! this answers the narrower, more frequent question "why is *this* node
//! misbehaving?" without making the user scan the footer and map spans back
//! to the selection by eye. Read-only: it renders text, never mutates source.

use eframe::egui;
use mogen_core::{Diagnostic, Severity, Span};

fn intersects(a: Span, b: Span) -> bool {
    a.start < b.end && b.start < a.end
}

pub(super) fn render(
    ui: &mut egui::Ui,
    diagnostics: &[Diagnostic],
    node_span: Option<Span>,
) {
    let Some(ns) = node_span else {
        return;
    };
    let mut hits: Vec<&Diagnostic> = diagnostics
        .iter()
        .filter(|d| d.span.is_some_and(|s| intersects(s, ns)))
        .collect();
    if hits.is_empty() {
        return;
    }
    // Errors first, then warnings, then info — most actionable on top.
    hits.sort_by_key(|d| match d.severity {
        Severity::Error => 0,
        Severity::Warning => 1,
        Severity::Info => 2,
    });

    ui.add_space(8.0);
    ui.separator();
    egui::CollapsingHeader::new(format!("Problems ({})", hits.len()))
        .id_salt(("inspector_node_problems", ns.start))
        .default_open(true)
        .show(ui, |ui| {
            for d in hits {
                let (color, glyph) = match d.severity {
                    Severity::Error => (egui::Color32::from_rgb(240, 120, 120), "✖"),
                    Severity::Warning => (egui::Color32::from_rgb(230, 200, 100), "⚠"),
                    Severity::Info => (egui::Color32::from_rgb(150, 190, 240), "ℹ"),
                };
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(color, glyph);
                    ui.label(egui::RichText::new(&d.code).monospace().weak());
                    ui.label(&d.message);
                })
                .response
                .on_hover_text(format!("[{}] {}", d.code, d.message));
            }
        });
}
