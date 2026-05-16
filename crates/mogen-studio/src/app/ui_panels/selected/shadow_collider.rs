use eframe::egui;

/// Outcome of the Shadow / Collider checkbox toggles. The caller maps these
/// to `PendingEdit::SetAttrCanonical` (for the "set" cases) or `delete_attr`
/// (for the "remove" cases).
#[derive(Default)]
pub(super) struct ToggleActions {
    pub set_cast_shadow_off: bool,
    pub remove_cast_shadow: bool,
    pub set_collider: bool,
    pub remove_collider: bool,
}

/// Render the Shadow + Collider sections, skipped for light nodes (the
/// validator rejects `collider=` on lights and lights have no `cast_shadow`).
pub(super) fn render(ui: &mut egui::Ui, node: &mogen_core::SceneNode) -> ToggleActions {
    let mut actions = ToggleActions::default();
    if node.light.is_some() {
        return actions;
    }

    // Shadow casting toggle — present on every editable node that isn't a
    // light. `cast_shadow` defaults to true at lower time, so the absence
    // of an attribute reads as "casts shadow"; toggling off writes
    // `cast_shadow=0` (number, matching the `faceted` convention) and
    // toggling back on deletes the attribute so the source stays clean.
    // The lowering pass propagates `false` down the subtree, so flipping
    // this on a group disables shadows for every descendant mesh.
    ui.add_space(8.0);
    ui.separator();
    ui.label(egui::RichText::new("Shadow").strong());
    let mut cast = node.cast_shadow;
    if ui
        .checkbox(&mut cast, "Cast shadow")
        .on_hover_text(
            "Whether this node (and its subtree) contributes to the \
             realtime shadow pre-pass. When off, MoGen also writes \
             `extras.cast_shadow=false` to the exported glTF node so \
             downstream importers (Godot etc.) can mirror the choice.",
        )
        .changed()
    {
        if cast {
            actions.remove_cast_shadow = true;
        } else {
            actions.set_cast_shadow_off = true;
        }
    }

    // Collider editor — single checkbox toggling `collider="aabb"` on
    // the node. Skipped for `light` nodes since the validator rejects
    // `collider=` there (lights have no AABB to enclose).
    let collider_present = node.collider.is_some();
    let collider_aabb = node.collider.as_ref().and_then(|c| c.as_aabb());
    ui.add_space(8.0);
    ui.separator();
    ui.label(egui::RichText::new("Collider").strong());
    let mut on = collider_present;
    if ui
        .checkbox(&mut on, "AABB")
        .on_hover_text(
            "Mark this node as a collider. The AABB is derived from \
             the node's subtree mesh extents at compile time and \
             written to the .glb as `extras.collider`.",
        )
        .changed()
    {
        if on {
            actions.set_collider = true;
        } else {
            actions.remove_collider = true;
        }
    }
    if let Some(aabb) = collider_aabb {
        let extent = aabb.max - aabb.min;
        ui.label(format!(
            "  size: [{:.3}, {:.3}, {:.3}]",
            extent.x, extent.y, extent.z
        ));
        let center = (aabb.min + aabb.max) * 0.5;
        ui.label(format!(
            "  center: [{:.3}, {:.3}, {:.3}]",
            center.x, center.y, center.z
        ));
    } else if collider_present {
        // Should be unreachable — the field tracks the source attr.
        // Left as a defensive label so an empty subtree (collider
        // requested but no mesh) reads as a tooltip rather than a
        // blank panel.
        ui.colored_label(
            egui::Color32::from_rgb(230, 200, 100),
            "  (no mesh in subtree — AABB skipped)",
        );
    }

    actions
}
