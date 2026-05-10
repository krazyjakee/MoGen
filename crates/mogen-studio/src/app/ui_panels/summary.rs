use std::time::Instant;

use eframe::egui;

use crate::app::types::UndoKey;
use crate::app::MogenStudioApp;

impl MogenStudioApp {
    pub(in crate::app) fn ui_summary(&mut self, ui: &mut egui::Ui) {
        use crate::edit;
        use mogen_core::NodeId;

        let i = self.active;
        let selection = self.viewer.primary_selection();
        // (counts, scope_header) — `scope_header` is `Some(name)` when stats
        // are scoped to a selected node's subtree, `None` for the global /
        // file-scoped totals.
        let (counts, scope_header) = {
            let Some(result) = &self.files[i].last_result else {
                ui.label("(no build yet)");
                return;
            };
            let Some(scene) = &result.scene else {
                ui.label("(no scene — fix errors first)");
                return;
            };
            // When a node is selected, scope the panel to that node's subtree
            // (self + descendants). The "Selected" panel above shows the
            // single-node properties; this is the rolled-up cost. Falling back
            // to global stats whenever nothing useful is selected (no
            // selection, or a stale id) keeps the panel populated.
            let subtree: Option<(std::collections::HashSet<NodeId>, String)> = selection
                .and_then(|sel| {
                    let root = scene.nodes.get(sel.0 as usize)?;
                    let mut ids: std::collections::HashSet<NodeId> = Default::default();
                    let mut stack = vec![sel];
                    while let Some(id) = stack.pop() {
                        if !ids.insert(id) {
                            continue;
                        }
                        if let Some(n) = scene.nodes.get(id.0 as usize) {
                            stack.extend(n.children.iter().copied());
                        }
                    }
                    Some((ids, root.name.clone()))
                });

            if let Some((ids, root_name)) = subtree {
                let mut tris = 0usize;
                let mut verts = 0usize;
                let mut meshes = 0usize;
                let mut mat_ids: std::collections::HashSet<u32> = Default::default();
                let mut skin_ids: std::collections::HashSet<u32> = Default::default();
                for id in &ids {
                    let Some(n) = scene.nodes.get(id.0 as usize) else {
                        continue;
                    };
                    if let Some(m) = &n.mesh {
                        tris += m.indices.len() / 3;
                        verts += m.positions.len();
                        meshes += 1;
                    }
                    if let Some(mid) = n.material {
                        mat_ids.insert(mid.0);
                    }
                    if let Some(sid) = n.skin {
                        skin_ids.insert(sid.0);
                    }
                }
                let joints = scene
                    .joints
                    .iter()
                    .filter(|j| ids.contains(&j.pivot))
                    .count();
                // A clip belongs to the subtree if any of its tracks drives a
                // node within it. Joint-driven clips inherit this through the
                // joint's pivot node, since lowering rewrites the track to
                // target the pivot directly.
                let clips = scene
                    .clips
                    .iter()
                    .filter(|c| c.tracks.iter().any(|t| ids.contains(&t.node)))
                    .count();
                (
                    (ids.len(), meshes, tris, verts, mat_ids.len(), skin_ids.len(), clips, joints),
                    Some(root_name),
                )
            } else {
                // Nothing selected (or stale id) — show whole-scene totals,
                // including geometry pulled in via `import`. Materials /
                // Animation panels still scope by origin; this section is the
                // one place the user can see the full cost of what's actually
                // being rendered.
                let mut tris = 0usize;
                let mut verts = 0usize;
                let mut meshes = 0usize;
                for n in &scene.nodes {
                    if let Some(m) = &n.mesh {
                        tris += m.indices.len() / 3;
                        verts += m.positions.len();
                        meshes += 1;
                    }
                }
                (
                    (
                        scene.nodes.len(),
                        meshes,
                        tris,
                        verts,
                        scene.materials.len(),
                        scene.skins.len(),
                        scene.clips.len(),
                        scene.joints.len(),
                    ),
                    None,
                )
            }
        };
        let (nodes, meshes, tris, verts, mats, skins, clips, joints) = counts;

        if let Some(name) = &scope_header {
            ui.colored_label(
                egui::Color32::from_rgb(170, 200, 240),
                format!("subtree of \"{name}\""),
            )
            .on_hover_text(
                "Stats are scoped to the selected node and its descendants. \
                 Deselect to see the whole-scene totals.",
            );
            ui.add_space(2.0);
        }
        ui.label(format!("nodes: {nodes}"));
        ui.label(format!("meshes: {meshes}"));
        ui.label(format!("triangles: {tris}"));
        ui.label(format!("vertices: {verts}"));
        ui.label(format!("materials: {mats}"));
        if skins > 0 {
            ui.label(format!("skins: {skins}"));
        }
        if clips > 0 {
            ui.label(format!("clips: {clips}"));
        }
        if joints > 0 {
            ui.label(format!("joints: {joints}"));
        }

        // Polygon count slider — multiplies primitive default segment/ring
        // counts at lower-time. Reads the live value out of source so it
        // stays in sync if the user edits the directive in the text editor.
        let current = edit::get_lod_scale(&self.files[i].source).unwrap_or(1.0);
        let mut draft = current;
        ui.add_space(6.0);
        let lod_hint =
            "Multiplies primitive default segment/ring counts.\n\
             Per-primitive `segments=`/`rings=` overrides still win.";
        let resp = ui
            .horizontal(|ui| {
                ui.label("LOD scale").on_hover_text(lod_hint);
                ui.add(
                    egui::Slider::new(&mut draft, 0.25..=4.0)
                        .suffix("×")
                        .logarithmic(true)
                        .max_decimals(2),
                )
                .on_hover_text(lod_hint)
            })
            .inner;
        if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
            let snapped = (draft * 100.0).round() / 100.0;
            if (snapped - current).abs() > 1e-3 {
                let before = self.files[i].source.clone();
                let new_src = edit::set_lod_scale(&before, snapped);
                {
                    let f = &mut self.files[i];
                    f.source = new_src;
                    f.dirty = f.source != f.last_saved_source;
                    f.needs_compile = true;
                    f.last_edit_at = Some(Instant::now());
                }
                // Slider release is already a discrete event (drag_stopped
                // gate above), so each release is one undoable step.
                self.break_undo_chain(i);
                self.push_undo(
                    i,
                    before,
                    UndoKey {
                        surface: "lod",
                        attr: None,
                        node_path: Vec::new(),
                    },
                );
            }
        }
    }
}
