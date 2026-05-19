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

#[cfg(test)]
mod tests {
    use super::*;

    fn sp(start: usize, end: usize) -> Span {
        Span { start, end }
    }

    #[test]
    fn overlapping_spans_intersect() {
        assert!(intersects(sp(0, 10), sp(5, 15)));
        assert!(intersects(sp(5, 15), sp(0, 10)));
    }

    #[test]
    fn nested_span_intersects() {
        assert!(intersects(sp(0, 20), sp(5, 10)));
        assert!(intersects(sp(5, 10), sp(0, 20)));
    }

    #[test]
    fn adjacent_spans_do_not_intersect() {
        // End is exclusive: [0,5) and [5,10) share no byte.
        assert!(!intersects(sp(0, 5), sp(5, 10)));
        assert!(!intersects(sp(5, 10), sp(0, 5)));
    }

    #[test]
    fn disjoint_spans_do_not_intersect() {
        assert!(!intersects(sp(0, 5), sp(6, 10)));
        assert!(!intersects(sp(6, 10), sp(0, 5)));
    }

    #[test]
    fn single_byte_span_intersects_containing_span() {
        assert!(intersects(sp(3, 4), sp(0, 10)));
        assert!(intersects(sp(0, 10), sp(3, 4)));
    }
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
