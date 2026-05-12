use eframe::egui;

use super::use_wrap::resolve_use_wrap_target;
use crate::app::MogenStudioApp;

/// The result of the early-return guards run at the top of the inspector.
/// `Continue` means the inspector keeps rendering the editor surface.
/// `Return` means an explanatory label was painted and the caller should bail.
/// `ReturnWithWrap` means the user clicked the "wrap `use` in a group" button —
/// the caller applies the source rewrite.
pub(super) enum GuardOutcome {
    Continue,
    Return,
    ReturnWithWrap {
        use_span: mogen_core::Span,
        group_name: String,
    },
}

/// Run the three early-return guards (non-editable, imported-via-`use`,
/// relative-placed). Returns whether the rest of the inspector should render.
pub(super) fn evaluate(
    ui: &mut egui::Ui,
    app: &MogenStudioApp,
    scene: &mogen_core::SceneGraph,
    sel: mogen_core::NodeId,
    node: &mogen_core::SceneNode,
) -> GuardOutcome {
    if !node.editable {
        ui.add_space(6.0);
        ui.colored_label(
            egui::Color32::from_rgb(230, 200, 100),
            "Derived from array/mirror/CSG — edit the parent in the text.",
        );
        return GuardOutcome::Return;
    }
    if node.use_id.is_some() && !crate::viewer::is_import_wrapper(scene, sel) {
        // Selection landed on an imported-module node. `replace_selection`
        // normally redirects picks to the nearest user-authored wrapper,
        // so this only fires when there is no wrapper to redirect to
        // (e.g. `scene { use "desk" }` with the `use` directly under
        // `scene`). Surface the constraint instead of offering a
        // transform grid that would write back into the imported file.
        //
        // The wrapper of `use "X" (pos=...)` for an imported file is
        // exempt: its source span is the `use` line in the active
        // source, so the inspector's transform grid writes back
        // through `set_attr` cleanly.
        let active_source = app.files[app.active].source.clone();
        let wrap_target = resolve_use_wrap_target(scene, sel, node, &active_source);
        ui.add_space(6.0);
        ui.colored_label(
            egui::Color32::from_rgb(230, 200, 100),
            "Imported via `use` — wrap the `use` in a group to edit its \
             transform here.",
        );
        let (button, hover) = match &wrap_target {
            Some(_) => (
                egui::Button::new("Wrap `use` in a group"),
                "Splice a `group \"<name>\" { … }` around the matching `use` \
                 line in the source so its transform becomes editable here.",
            ),
            None => (
                egui::Button::new("Wrap `use` in a group"),
                "Couldn't locate the originating `use` line in the active \
                 source — wrap it manually by editing the text.",
            ),
        };
        let wrap_clicked = ui
            .add_enabled(wrap_target.is_some(), button)
            .on_hover_text(hover)
            .clicked();
        if wrap_clicked {
            if let Some((use_span, group_name)) = wrap_target {
                return GuardOutcome::ReturnWithWrap { use_span, group_name };
            }
        }
        return GuardOutcome::Return;
    }
    if node.relative_placed {
        // The viewport gizmo refuses these for the same reason: a layout
        // pass (attach / pack) recomputes their translation every compile,
        // so a `pos=` writeback would silently snap back. Keep the two
        // input paths consistent rather than offering a transform grid
        // that secretly does nothing.
        ui.add_space(6.0);
        ui.colored_label(
            egui::Color32::from_rgb(230, 200, 100),
            "Placed by attach/pack — translation is recomputed each compile. \
             Detach or edit the layout spec to free this node.",
        );
        return GuardOutcome::Return;
    }
    GuardOutcome::Continue
}
