use std::time::Instant;

use eframe::egui;

use crate::pipeline::{compile, Stage};

use super::types::{UndoKey, COMPILE_DEBOUNCE};
use super::MogenStudioApp;

impl MogenStudioApp {
    pub(super) fn compile_active(&mut self) {
        self.compile_file(self.active);
    }

    pub(super) fn compile_file(&mut self, i: usize) {
        let r = compile(&self.files[i].source);
        if i == self.active {
            match &r.scene {
                Some(scene) if matches!(r.stage, Stage::Ok) => {
                    let base_dir = self.files[i].path.as_deref().and_then(|p| p.parent());
                    let fit = self.files[i].first_render;
                    self.viewer.set_scene(scene, base_dir, fit);
                    self.files[i].first_render = false;
                }
                _ => self.viewer.clear(),
            }
        }
        self.files[i].last_result = Some(r);
        self.files[i].needs_compile = false;
    }

    /// Trigger a compile if the active MOG file's debounce window has elapsed.
    /// Also keeps repainting while the window is open so the UI lands on the
    /// recompile naturally.
    pub(super) fn drive_compile_debounce(&mut self, ctx: &egui::Context) {
        let i = self.active;
        let f = &self.files[i];
        if !f.needs_compile {
            return;
        }
        if let Some(t) = f.last_edit_at {
            let elapsed = t.elapsed();
            if elapsed >= COMPILE_DEBOUNCE {
                self.compile_active();
            } else {
                ctx.request_repaint_after(COMPILE_DEBOUNCE - elapsed);
            }
        }
    }

    /// Drain any viewport edits the user produced this frame (gizmo drags
    /// and inspector widgets) and splice them into the active MOG file. Also
    /// relays any pending caret jump coming from the viewport selection.
    pub(super) fn drain_viewport_edits(&mut self, ctx: &egui::Context) {
        use crate::edit;
        use crate::viewer::PendingEdit;

        let edits = self.viewer.take_pending_edits();
        let trace = std::env::var_os("MOGEN_GIZMO_TRACE").is_some();
        if trace && !edits.is_empty() {
            eprintln!("[gizmo] drain got {} edit(s)", edits.len());
        }
        // Discard viewport edits while an LLM call is in flight — the worker
        // will overwrite `source` on completion, so applying them would just
        // queue work that gets thrown away. Drained above so they don't
        // accumulate; pending_caret below is harmless and still relayed.
        let edits = if self.files[self.active].llm_in_flight.is_some() {
            if trace && !edits.is_empty() {
                eprintln!("[gizmo] drain DROPPED {} edit(s) — LLM in flight", edits.len());
            }
            Vec::new()
        } else {
            edits
        };
        if !edits.is_empty() {
            let i = self.active;
            // Snapshot the source BEFORE any edit lands so the undo stack
            // can revert the whole batch (one drain = one undoable action).
            let undo_before = self.files[i].source.clone();
            let mut source = self.files[i].source.clone();
            let mut any_applied = false;
            // Last canonical attr in the batch. Used as the coalesce key so
            // a multi-frame inspector DragValue burst writing the same vec3
            // collapses into a single undo entry, while a switch from `pos`
            // to `rot` opens a new entry.
            let mut last_attr: Option<String> = None;
            for edit in edits {
                let (node, span, attr, value, delete) = match edit {
                    PendingEdit::SetAttrCanonical { node, attr, value, delete } => {
                        // Look up the node's span in the current compile
                        // result — recompile-consistent because
                        // take_pending_edits is drained before the next
                        // compile fires.
                        let Some(result) = &self.files[i].last_result else {
                            if trace {
                                eprintln!("[gizmo] drain SKIPPED: no last_result");
                            }
                            continue;
                        };
                        let Some(span) = result.node_spans.get(node.0 as usize).and_then(|s| *s)
                        else {
                            if trace {
                                eprintln!(
                                    "[gizmo] drain SKIPPED: no span for node {} (node_spans.len={})",
                                    node.0,
                                    result.node_spans.len()
                                );
                            }
                            continue;
                        };
                        (node, span, attr, value, delete)
                    }
                    PendingEdit::SetAttrAtSpan { node, span, attr, value, delete } => {
                        // Span is supplied directly (e.g. attach-redirect
                        // pointing at a connector declaration); no node
                        // table lookup needed.
                        (node, span, attr, value, delete)
                    }
                };
                // Strip shadowing attrs BEFORE setting the canonical one.
                // Doing deletes first keeps all the spans valid for the
                // final `set_attr` (set_attr only needs the node's outer
                // span, which doesn't shift when attrs inside it shrink).
                for shadow in &delete {
                    source = edit::delete_attr(&source, span, shadow);
                }
                let before = source.clone();
                source = edit::set_attr(&source, span, &attr, &value);
                if trace {
                    eprintln!(
                        "[gizmo] drain APPLIED node={} attr={} value={} delete={:?} span={:?} changed={}",
                        node.0,
                        attr,
                        value,
                        delete,
                        span,
                        before != source
                    );
                }
                last_attr = Some(attr);
                any_applied = true;
            }
            if any_applied {
                {
                    let f = &mut self.files[i];
                    f.source = source;
                    f.dirty = f.source != f.last_saved_source;
                    f.needs_compile = true;
                    f.last_edit_at = Some(Instant::now());
                }
                // Record the batch as one undo entry. The coalesce key folds
                // successive batches on the same node + same attr (within
                // the time window) into a single entry — gizmo releases are
                // already discrete (one PendingEdit per drag) but inspector
                // DragValues fire per-frame and need the merge to behave.
                let key = UndoKey {
                    surface: "viewport",
                    attr: last_attr,
                    node_path: self.current_selection_path(i),
                };
                self.push_undo(i, undo_before, key);
                // Gizmo releases are discrete events, not keystroke bursts —
                // skip the 180 ms debounce and compile immediately so the
                // viewport can pick up the rotated / translated / scaled
                // scene on the very next frame. Without this, the preview
                // clears on release and the new scene only arrives on the
                // next idle tick, which looks to the user like the gizmo
                // action was rejected and the value snapped back.
                self.compile_active();
                // Belt-and-braces: also request the debounce-boundary
                // repaint so if something does leave `needs_compile` set
                // (e.g. the caller wraps a batch of gizmo edits through
                // this path), the next window still fires.
                ctx.request_repaint_after(COMPILE_DEBOUNCE);
                if trace {
                    eprintln!("[gizmo] drain done — triggered immediate compile");
                }
            }
        }

        if let Some(offset) = self.viewer.take_pending_caret() {
            self.files[self.active].pending_caret = Some(offset);
        }
    }
}
