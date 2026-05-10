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

        // Meta editor — `name`, `description`. Maps to top-of-file
        // `meta(...)` block via `mogen_dsl::upsert_meta_attr`, which is
        // text-level and tolerates partial drafts.
        ui.add_space(8.0);
        ui.separator();
        ui.label(egui::RichText::new("Meta").strong());
        let cur_name = mogen_dsl::read_meta_attr(&self.files[i].source, "name")
            .unwrap_or_default();
        let cur_desc = mogen_dsl::read_meta_attr(&self.files[i].source, "description")
            .unwrap_or_default();
        let cur_seed = mogen_dsl::read_meta_attr(&self.files[i].source, "seed")
            .unwrap_or_default();
        let id = self.active;
        // Initialise the draft from source on first paint; thereafter the
        // user's keystrokes own the buffer until they commit (focus loss).
        if !self.meta_name_drafts.contains_key(&id) {
            self.meta_name_drafts.insert(id, cur_name.clone());
        }
        let mut name_str = self.meta_name_drafts.get(&id).cloned().unwrap_or_default();
        let name_resp = ui.horizontal(|ui| {
            ui.label("Name");
            ui.add(
                egui::TextEdit::singleline(&mut name_str)
                    .desired_width(f32::INFINITY)
                    .id_salt(("meta_name", i)),
            )
        });
        if name_resp.inner.changed() {
            self.meta_name_drafts.insert(id, name_str.clone());
        }
        if name_resp.inner.lost_focus() && name_str != cur_name {
            let before = self.files[i].source.clone();
            let new_src = mogen_dsl::upsert_meta_attr(&before, "name", &name_str);
            if new_src != before {
                {
                    let f = &mut self.files[i];
                    f.source = new_src;
                    f.dirty = f.source != f.last_saved_source;
                    f.needs_compile = true;
                    f.last_edit_at = Some(Instant::now());
                }
                self.break_undo_chain(i);
                self.push_undo(
                    i,
                    before,
                    UndoKey {
                        surface: "meta",
                        attr: Some("name".into()),
                        node_path: Vec::new(),
                    },
                );
            }
        }

        if !self.meta_desc_drafts.contains_key(&id) {
            self.meta_desc_drafts.insert(id, cur_desc.clone());
        }
        let mut desc_str = self.meta_desc_drafts.get(&id).cloned().unwrap_or_default();
        let desc_resp = ui.horizontal(|ui| {
            ui.label("Desc");
            ui.add(
                egui::TextEdit::singleline(&mut desc_str)
                    .desired_width(f32::INFINITY)
                    .hint_text("optional one-line summary")
                    .id_salt(("meta_desc", i)),
            )
        });
        if desc_resp.inner.changed() {
            self.meta_desc_drafts.insert(id, desc_str.clone());
        }
        if desc_resp.inner.lost_focus() && desc_str != cur_desc {
            let before = self.files[i].source.clone();
            let new_src = mogen_dsl::upsert_meta_attr(&before, "description", &desc_str);
            if new_src != before {
                {
                    let f = &mut self.files[i];
                    f.source = new_src;
                    f.dirty = f.source != f.last_saved_source;
                    f.needs_compile = true;
                    f.last_edit_at = Some(Instant::now());
                }
                self.break_undo_chain(i);
                self.push_undo(
                    i,
                    before,
                    UndoKey {
                        surface: "meta",
                        attr: Some("description".into()),
                        node_path: Vec::new(),
                    },
                );
            }
        }

        // Seed shown read-only because LLM workflows stamp it; expose a
        // "Clear" button for users who want a fresh roll on the next run.
        ui.horizontal(|ui| {
            ui.label("Seed");
            if cur_seed.is_empty() {
                ui.label(egui::RichText::new("(none)").italics().weak());
            } else {
                ui.label(egui::RichText::new(&cur_seed).monospace())
                    .on_hover_text(
                        "Stamped by `generate` / `modify` so rebuilds are \
                         reproducible. Clear to roll a fresh seed on the \
                         next LLM run.",
                    );
                if ui.small_button("Clear").clicked() {
                    let before = self.files[i].source.clone();
                    let new_src = mogen_dsl::upsert_meta_attr(&before, "seed", "");
                    if new_src != before {
                        {
                            let f = &mut self.files[i];
                            f.source = new_src;
                            f.dirty = f.source != f.last_saved_source;
                            f.needs_compile = true;
                            f.last_edit_at = Some(Instant::now());
                        }
                        self.break_undo_chain(i);
                        self.push_undo(
                            i,
                            before,
                            UndoKey {
                                surface: "meta",
                                attr: Some("seed".into()),
                                node_path: Vec::new(),
                            },
                        );
                    }
                }
            }
        });

        // Export options — same toggles as the File → Build GLB modal,
        // hoisted here so the user can see/save the per-file state without
        // opening the dialog. Persisted on `FileState.export_opts`; the
        // build pipeline reads them on every export.
        ui.add_space(8.0);
        ui.separator();
        ui.label(egui::RichText::new("Export").strong());
        let opts = &mut self.files[i].export_opts;
        ui.checkbox(&mut opts.include_animations, "Include animations")
            .on_hover_text(
                "Emit the scene's `animations[]` array. Off = bake a static GLB.",
            );
        ui.checkbox(&mut opts.include_textures, "Include textures")
            .on_hover_text(
                "Pack texture images into the GLB binary chunk and wire them to \
                 materials. Off = materials export with only PBR numeric factors.",
            );
        ui.checkbox(
            &mut opts.merge_sibling_meshes,
            "Merge overlapping meshes",
        )
        .on_hover_text(
            "Collapse same-material, non-skinned sibling meshes under each parent \
             into a single CSG-unioned mesh. Slow on complex scenes.",
        );
    }
}
