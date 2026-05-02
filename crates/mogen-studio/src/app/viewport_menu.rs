//! Right-click context menu for the 3D viewport. Mirrors the basics already
//! reachable from the View menu (Frame, grid + gizmo toggles) and adds two
//! Add submenus: Primitive (insert a default-parameter primitive node) and
//! Imports (insert a `use "<module>" ()` line for any `import "…"` already
//! declared in the source).
//!
//! Source mutations go through [`crate::edit::append_to_scene`] so the
//! surrounding formatting and diagnostics survive untouched.

use std::time::Instant;

use eframe::egui;

use crate::edit;

use super::types::{MenuAction, ShortcutAction, UndoKey};
use super::MogenStudioApp;

/// The catalog of primitive kinds offered in the Add → Primitive submenu,
/// each paired with the body string we splice in. Names are filled in at
/// insert time so successive inserts of the same kind don't collide
/// (`box_1`, `box_2`, …).
const PRIMITIVES: &[(&str, &str, &str)] = &[
    ("Box", "box", "(size=[1, 1, 1])"),
    ("Rounded Box", "rounded_box", "(size=[1, 1, 1], radius=0.1)"),
    ("Sphere", "sphere", "(radius=0.5)"),
    ("Icosphere", "icosphere", "(radius=0.5)"),
    ("Cylinder", "cylinder", "(radius=0.5, height=1)"),
    ("Cone", "cone", "(radius=0.5, height=1)"),
    ("Capsule", "capsule", "(radius=0.5, height=1)"),
    ("Torus", "torus", "(major=0.5, minor=0.15)"),
    ("Prism", "prism", "(size=[1, 1, 1])"),
    ("Pyramid", "pyramid", "(radius=0.5, height=1, sides=4)"),
    ("Disc", "disc", "(radius=0.5)"),
    ("Plane", "plane", "(size=[1, 1])"),
    ("Quad", "quad", "(size=[1, 1])"),
];

impl MogenStudioApp {
    /// Render the viewport context menu inside `ui`. Called from the
    /// closure passed to `Response::context_menu`. Defers actions to a
    /// post-menu dispatch step so the immediate-mode borrows here stay
    /// confined to read access on `self`.
    pub(super) fn ui_viewport_context_menu(&mut self, ui: &mut egui::Ui) {
        let mut menu_action: MenuAction = MenuAction::None;
        let mut add_primitive: Option<&'static str> = None;
        let mut add_use: Option<String> = None;

        if shortcut_text_button(ui, "Frame", ShortcutAction::Frame, "Re-fit the camera to the scene")
            .clicked()
        {
            menu_action = MenuAction::Frame;
            ui.close_menu();
        }
        let mut show_grid = self.settings.show_grid();
        if ui
            .checkbox(&mut show_grid, "Show Grid")
            .on_hover_text("Toggle the ground-plane reference grid")
            .changed()
        {
            self.settings.set_show_grid(show_grid);
            self.viewer.set_show_grid(show_grid);
            let _ = self.settings.save();
        }
        let mut show_lights = self.settings.show_light_gizmos();
        if ui
            .checkbox(&mut show_lights, "Show Light Gizmos")
            .on_hover_text("Toggle per-light indicator overlays")
            .changed()
        {
            self.settings.set_show_light_gizmos(show_lights);
            self.viewer.set_show_light_gizmos(show_lights);
            let _ = self.settings.save();
        }
        let mut show_xform = self.settings.show_transform_gizmo();
        if ui
            .checkbox(&mut show_xform, "Show Transform Gizmo")
            .on_hover_text("Toggle translate/rotate/scale handles on the selected node")
            .changed()
        {
            self.settings.set_show_transform_gizmo(show_xform);
            self.viewer.set_show_transform_gizmo(show_xform);
            let _ = self.settings.save();
        }
        let mut show_colliders = self.settings.show_colliders();
        if ui
            .checkbox(&mut show_colliders, "Show Colliders")
            .on_hover_text("Toggle the AABB collider wireframe overlay (off by default)")
            .changed()
        {
            self.settings.set_show_colliders(show_colliders);
            self.viewer.set_show_colliders(show_colliders);
            let _ = self.settings.save();
        }

        ui.separator();

        ui.menu_button("Add", |ui| {
            ui.menu_button("Primitive", |ui| {
                for (label, kind, _) in PRIMITIVES {
                    if ui
                        .button(*label)
                        .on_hover_text(format!(
                            "Append `{kind} \"…\" (…)` to the scene with default settings"
                        ))
                        .clicked()
                    {
                        add_primitive = Some(*kind);
                        ui.close_menu();
                    }
                }
            });
            let imports = edit::list_imports(&self.files[self.active].source);
            ui.menu_button("Imports", |ui| {
                if imports.is_empty() {
                    ui.add_enabled(false, egui::Button::new("(no imports)"))
                        .on_hover_text(
                            "Use File → Import… (Cmd+Shift+I) to add an `import \"…\"` line first",
                        );
                } else {
                    for entry in &imports {
                        let label = entry.module_name.clone();
                        if ui
                            .button(&label)
                            .on_hover_text(format!(
                                "Append `use \"{}\" ()` to the scene (from `import \"{}\"`)",
                                entry.module_name, entry.path
                            ))
                            .clicked()
                        {
                            add_use = Some(entry.module_name.clone());
                            ui.close_menu();
                        }
                    }
                }
            });
        });

        // Selection-aware actions land at the bottom so the menu reads
        // top-down: viewport state → add → act on selection.
        if let Some(sel) = self.viewer.primary_selection() {
            ui.separator();
            if ui
                .button("Delete")
                .on_hover_text("Remove the selected node from the source")
                .clicked()
            {
                self.viewer
                    .push_pending_edit(crate::viewer::PendingEdit::DeleteNode { node: sel });
                ui.close_menu();
            }
        }

        // Apply deferred actions after the menu closes its borrows.
        if !matches!(menu_action, MenuAction::None) {
            self.dispatch_menu_action(ui.ctx(), menu_action);
        }
        if let Some(kind) = add_primitive {
            self.add_primitive_to_scene(kind);
        }
        if let Some(module_name) = add_use {
            self.add_use_to_scene(&module_name);
        }
    }

    /// Splice a default-parameter primitive of `kind` into the topmost
    /// `scene { … }` block. Picks a unique `<kind>_<n>` name based on
    /// existing names so successive inserts don't collide. Records one
    /// undo entry and forces a recompile so the new node shows up in the
    /// viewport on the next frame.
    fn add_primitive_to_scene(&mut self, kind: &str) {
        let attrs = match PRIMITIVES.iter().find(|(_, k, _)| *k == kind) {
            Some((_, _, a)) => *a,
            None => return,
        };
        let i = self.active;
        let undo_before = self.files[i].source.clone();
        let name = edit::suggest_primitive_name(&self.files[i].source, kind);
        let body = format!("{kind} \"{name}\" {attrs}");
        let new_source = edit::append_to_scene(&self.files[i].source, &body);
        if new_source == self.files[i].source {
            return;
        }
        {
            let f = &mut self.files[i];
            f.source = new_source;
            f.dirty = f.source != f.last_saved_source;
            f.needs_compile = true;
            f.last_edit_at = Some(Instant::now());
            f.status = format!("added {kind} \"{name}\"");
        }
        let key = UndoKey {
            surface: "viewport-add",
            attr: Some(format!("primitive:{kind}")),
            node_path: Vec::new(),
        };
        self.push_undo(i, undo_before, key);
        self.compile_active();
    }

    /// Splice `use "<module_name>" ()` into the topmost scene. Records one
    /// undo entry and recompiles so the instance shows up immediately.
    fn add_use_to_scene(&mut self, module_name: &str) {
        let i = self.active;
        let undo_before = self.files[i].source.clone();
        let body = format!("use \"{module_name}\" ()");
        let new_source = edit::append_to_scene(&self.files[i].source, &body);
        if new_source == self.files[i].source {
            return;
        }
        {
            let f = &mut self.files[i];
            f.source = new_source;
            f.dirty = f.source != f.last_saved_source;
            f.needs_compile = true;
            f.last_edit_at = Some(Instant::now());
            f.status = format!("added use \"{module_name}\"");
        }
        let key = UndoKey {
            surface: "viewport-add",
            attr: Some(format!("use:{module_name}")),
            node_path: Vec::new(),
        };
        self.push_undo(i, undo_before, key);
        self.compile_active();
    }
}

/// Render a button that shows the global keyboard shortcut for `action`.
/// Mirrors `types::shortcut_menu_item` but lives here so the file menu's
/// helper doesn't need to be made `pub(super)` for two callers.
fn shortcut_text_button(
    ui: &mut egui::Ui,
    label: &str,
    action: ShortcutAction,
    tooltip: &str,
) -> egui::Response {
    let sc = action.shortcut();
    let sc_text = ui.ctx().format_shortcut(&sc);
    let resp = ui.add(egui::Button::new(label).shortcut_text(sc_text.clone()));
    if tooltip.is_empty() {
        resp.on_hover_text(sc_text)
    } else {
        resp.on_hover_text(format!("{tooltip}  ({sc_text})"))
    }
}
