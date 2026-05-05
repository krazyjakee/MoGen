//! Right-click context menu for the 3D viewport. Mirrors the basics already
//! reachable from the View menu (Frame, grid + gizmo toggles) and exposes
//! an Add submenu that inserts default-parameter scene nodes: primitives,
//! lights, CSG ops, group / solid containers, materials, and `use` lines
//! for any `import "…"` already declared in the source.
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
/// (`box_1`, `box_2`, …). Ordered by family so the long list reads as
/// boxes → spheres → cylinders → torus → flat → spline.
const PRIMITIVES: &[(&str, &str, &str)] = &[
    ("Box", "box", "(size=[1, 1, 1])"),
    ("Rounded Box", "rounded_box", "(size=[1, 1, 1], radius=0.1)"),
    ("Slab", "slab", "(size=[1, 0.1, 1])"),
    ("Post", "post", "(size=[0.1, 1, 0.1])"),
    ("Panel", "panel", "(size=[1, 1, 0.05])"),
    ("Wedge", "wedge", "(size=[1, 1, 1])"),
    ("Frustum", "frustum", "(bottom=[1, 1], top=[0.5, 0.5], height=1)"),
    ("Wall", "wall", "(size=[3, 3, 0.1])"),
    ("Sphere", "sphere", "(radius=0.5)"),
    ("Icosphere", "icosphere", "(radius=0.5)"),
    ("Ellipsoid", "ellipsoid", "(size=[1, 1, 1])"),
    ("Hemisphere", "hemisphere", "(radius=0.5)"),
    ("Superellipsoid", "superellipsoid", "(size=[1, 1, 1], ew=1, ns=1)"),
    ("Cylinder", "cylinder", "(radius=0.5, height=1)"),
    ("Half Cylinder", "half_cylinder", "(radius=0.5, height=1)"),
    ("Tube", "tube", "(outer=0.5, inner=0.4, height=1)"),
    ("Cone", "cone", "(radius=0.5, height=1)"),
    ("Capsule", "capsule", "(radius=0.5, height=1)"),
    ("Pyramid", "pyramid", "(radius=0.5, height=1, sides=4)"),
    ("Prism", "prism", "(size=[1, 1, 1])"),
    ("Disc", "disc", "(radius=0.5)"),
    ("Torus", "torus", "(major=0.5, minor=0.15)"),
    ("Torus Arc", "torus_arc", "(major=0.5, minor=0.15, arc=90)"),
    ("Plane", "plane", "(size=[1, 1, 1])"),
    ("Quad", "quad", "(size=[1, 1, 1])"),
    ("Curved Plane", "curved_plane", "(size=[1, 1], bend_u=30)"),
    ("Leaf Card", "leaf_card", "(size=[0.4, 0.5, 0.5], cards=2)"),
    (
        "Lathe",
        "lathe",
        "(profile=[[0.0, -0.5], [0.4, -0.3], [0.5, 0.0], [0.3, 0.4], [0.0, 0.5]])",
    ),
    (
        "Spline Tube",
        "spline_tube",
        "(points=[[0, 0, 0], [0, 0.5, 0.3], [0, 1, 0]], radius=0.1)",
    ),
    (
        "Spline Ribbon",
        "spline_ribbon",
        "(points=[[0, 0, 0], [0, 0.5, 0.3], [0, 1, 0]], width=0.2)",
    ),
];

/// Lights all share the `light` node kind; the variant is selected via the
/// `kind=` attribute. Defaults match the per-kind examples in `docs/dsl.md`
/// so a freshly-inserted light reads sensibly without further tweaking.
const LIGHTS: &[(&str, &str)] = &[
    (
        "Point",
        "(kind=point, pos=[0, 2, 0], color=[1, 0.9, 0.7], intensity=10, range=8)",
    ),
    (
        "Directional",
        "(kind=directional, dir=[-0.4, -1, -0.3], color=[1, 0.95, 0.85], intensity=3)",
    ),
    (
        "Spot",
        "(kind=spot, pos=[0, 3, 0], dir=[0, -1, 0], color=[1, 1, 1], intensity=20, range=10, inner_cone=20, outer_cone=35)",
    ),
];

/// CSG ops require ≥ 1 (union/difference) or ≥ 2 (intersect) operands or
/// lowering errors out, so each entry seeds two primitives the user can
/// rename or replace. Bodies are multi-line — `append_to_scene` re-indents
/// the interior to match the scene's inner indent.
const CSG_OPS: &[(&str, &str, &str)] = &[
    (
        "Union",
        "union",
        "() {\n  box \"a\" (size=[1, 1, 1])\n  sphere \"b\" (pos=[0.6, 0, 0], radius=0.65)\n}",
    ),
    (
        "Difference",
        "difference",
        "() {\n  box \"a\" (size=[1, 1, 1])\n  box \"b\" (pos=[0.5, 0, 0], size=[0.7, 1.2, 0.7])\n}",
    ),
    (
        "Intersect",
        "intersect",
        "() {\n  box \"a\" (size=[1, 1, 1])\n  sphere \"b\" (radius=0.65)\n}",
    ),
];

/// Material / group / solid are single-button entries; the body strings
/// here are spliced verbatim after the auto-generated `"<kind>_N"` name.
const MATERIAL_ATTRS: &str = "(color=[0.5, 0.5, 0.5], roughness=0.8, metallic=0.0)";
const GROUP_ATTRS: &str = "() { }";
const SOLID_ATTRS: &str = "() { }";

impl MogenStudioApp {
    /// Render the viewport context menu inside `ui`. Called from the
    /// closure passed to `Response::context_menu`. Defers actions to a
    /// post-menu dispatch step so the immediate-mode borrows here stay
    /// confined to read access on `self`.
    pub(super) fn ui_viewport_context_menu(&mut self, ui: &mut egui::Ui) {
        let mut menu_action: MenuAction = MenuAction::None;
        let mut add_node: Option<(&'static str, &'static str)> = None;
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
                for (label, kind, attrs) in PRIMITIVES {
                    if ui
                        .button(*label)
                        .on_hover_text(format!(
                            "Append `{kind} \"…\" (…)` to the scene with default settings"
                        ))
                        .clicked()
                    {
                        add_node = Some((*kind, *attrs));
                        ui.close_menu();
                    }
                }
            });
            ui.menu_button("Light", |ui| {
                for (label, attrs) in LIGHTS {
                    if ui
                        .button(*label)
                        .on_hover_text(format!(
                            "Append a `{}` light to the scene with sensible defaults",
                            label.to_lowercase()
                        ))
                        .clicked()
                    {
                        add_node = Some(("light", *attrs));
                        ui.close_menu();
                    }
                }
            });
            ui.menu_button("CSG", |ui| {
                for (label, kind, attrs) in CSG_OPS {
                    if ui
                        .button(*label)
                        .on_hover_text(format!(
                            "Append a `{kind}` op seeded with two operands you can replace"
                        ))
                        .clicked()
                    {
                        add_node = Some((*kind, *attrs));
                        ui.close_menu();
                    }
                }
            });
            if ui
                .button("Group")
                .on_hover_text("Append an empty `group \"…\" () { }` container")
                .clicked()
            {
                add_node = Some(("group", GROUP_ATTRS));
                ui.close_menu();
            }
            if ui
                .button("Solid")
                .on_hover_text(
                    "Append an empty `solid \"…\" () { }` — same-material leaves merge at export",
                )
                .clicked()
            {
                add_node = Some(("solid", SOLID_ATTRS));
                ui.close_menu();
            }
            if ui
                .button("Material")
                .on_hover_text("Append a default `material \"…\" (…)` definition")
                .clicked()
            {
                add_node = Some(("material", MATERIAL_ATTRS));
                ui.close_menu();
            }
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
        if let Some((kind, attrs)) = add_node {
            self.add_node_to_scene(kind, attrs);
        }
        if let Some(module_name) = add_use {
            self.add_use_to_scene(&module_name);
        }
    }

    /// Splice `kind "<kind>_N" <attrs>` into the topmost `scene { … }`
    /// block. Used for primitives, lights, CSG ops, group/solid containers,
    /// and material definitions — anything single-statement-shaped. Picks
    /// a unique `<kind>_<n>` name from existing literals so successive
    /// inserts don't collide. Records one undo entry and forces a recompile
    /// so the new node shows up in the viewport on the next frame.
    fn add_node_to_scene(&mut self, kind: &str, attrs: &str) {
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
            attr: Some(format!("add:{kind}")),
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
