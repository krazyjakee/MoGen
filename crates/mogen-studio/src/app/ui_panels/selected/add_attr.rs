//! Schema-driven "Add attribute" picker.
//!
//! Several useful common attributes (`anchor`, `from`/`to`, the relative-
//! placement family, `lod`, `gap`, `role`, `tags`) have no dedicated widget,
//! so they were previously undiscoverable from the inspector — you had to
//! know the DSL and edit text. This lists the ones that are valid for the
//! selected kind (per the validator's own schema tables) and not already
//! present, and splices a sensible default via the normal span-aware
//! `SetAttrCanonical` path so the write is undoable and round-trips cleanly.
//!
//! Scope is deliberately the placement/metadata attrs without other UI:
//! transforms (grid), `mat` (picker), `collider`/`cast_shadow` (checkboxes),
//! deform knobs (Deform section) and the kind geometry params (grid) all
//! have first-class editors already and are excluded to avoid two controls
//! fighting over one attribute.

use eframe::egui;

use crate::edit::get_attr;
use crate::viewer::PendingEdit;

/// Attrs we offer, with a default literal chosen to be schema-valid so the
/// inserted line builds (the user then edits the value). Relative-placement
/// attrs take a sibling *name* we can't know, so they seed an empty string
/// and the validator/Problems section guides the user from there.
const CANDIDATES: &[(&str, &str)] = &[
    ("anchor", "center"),
    ("from", "[0, 0, 0]"),
    ("to", "[0, 0, 0]"),
    ("above", "\"\""),
    ("below", "\"\""),
    ("left_of", "\"\""),
    ("right_of", "\"\""),
    ("in_front_of", "\"\""),
    ("behind", "\"\""),
    ("gap", "0"),
    ("lod", "1"),
    ("role", "\"part\""),
    ("tags", "\"\""),
];

pub(super) fn render(
    ui: &mut egui::Ui,
    kind: &str,
    src: &str,
    span: mogen_core::Span,
    node_id: mogen_core::NodeId,
    edits: &mut Vec<PendingEdit>,
) {
    // Intersect our curated list with what the validator actually accepts for
    // this kind, then drop anything already written on the node.
    let common = mogen_validate::common_attrs_for_kind(kind);
    let specific = mogen_validate::attrs_for_kind(kind);
    let mut addable: Vec<(&str, &str)> = Vec::new();
    for &(name, default) in CANDIDATES {
        let valid = common.contains(&name) || specific.contains(&name);
        if valid && get_attr(src, span, name).is_none() {
            addable.push((name, default));
        }
    }
    if addable.is_empty() {
        return;
    }

    ui.add_space(8.0);
    ui.separator();
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Add attribute").strong());
        egui::ComboBox::from_id_salt(("inspector_add_attr", node_id.0))
            .selected_text("+ choose…")
            .show_ui(ui, |ui| {
                for (name, default) in addable {
                    if ui
                        .selectable_label(false, name)
                        .on_hover_text(format!("Insert `{name}={default}` and edit it"))
                        .clicked()
                    {
                        edits.push(PendingEdit::SetAttrCanonical {
                            node: node_id,
                            attr: name.into(),
                            value: default.into(),
                            delete: Vec::new(),
                        });
                    }
                }
            });
    });
}
