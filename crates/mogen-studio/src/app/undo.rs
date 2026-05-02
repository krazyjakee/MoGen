use std::time::Instant;

use super::types::{
    UndoEntry, UndoKey, UNDO_COALESCE_WINDOW, UNDO_STACK_CAP,
};
use super::MogenStudioApp;

impl MogenStudioApp {
    /// Record one undo entry for tab `i`. Caller passes the source as it was
    /// BEFORE the edit; the post-edit source is read from `files[i].source`.
    /// Successive pushes that match `key` within `UNDO_COALESCE_WINDOW` merge
    /// into the existing head entry instead of stacking — so a multi-frame
    /// inspector DragValue burst becomes one undoable action.
    pub(super) fn push_undo(
        &mut self,
        i: usize,
        before: String,
        key: UndoKey,
    ) {
        if i >= self.files.len() {
            return;
        }
        if before == self.files[i].source {
            // No-op edit (e.g. set_attr that wrote the same value back). Don't
            // dirty the undo stack with empty entries.
            return;
        }

        let now = Instant::now();
        let selection = self.current_selection_path(i);

        let coalesce = match (&self.files[i].undo.last_key, self.files[i].undo.last_push_at) {
            (Some(prev), Some(t)) => prev == &key && now.duration_since(t) <= UNDO_COALESCE_WINDOW,
            _ => false,
        };

        if coalesce {
            // Snapshot the post-edit source before grabbing a mutable handle
            // on the stack — back_mut + immutable read can't co-exist on
            // the same FileState.
            let after = self.files[i].source.clone();
            if let Some(head) = self.files[i].undo.past.back_mut() {
                // Merge: leave the original `before` alone, just update
                // `after` to the latest source. Selection might have moved
                // mid-burst (rare for a transform edit) — track it on after.
                head.after = after;
                head.selection_after = selection.clone();
            }
            self.files[i].undo.last_push_at = Some(now);
            return;
        }

        let entry = UndoEntry {
            before,
            after: self.files[i].source.clone(),
            selection_before: selection.clone(),
            selection_after: selection,
        };

        let stack = &mut self.files[i].undo;
        stack.future.clear();
        stack.past.push_back(entry);
        while stack.past.len() > UNDO_STACK_CAP {
            stack.past.pop_front();
        }
        stack.last_push_at = Some(now);
        stack.last_key = Some(key);
    }

    /// Force the next push to start a fresh entry rather than merge into the
    /// previous one. Called from boundary events (Undo/Redo apply, tab
    /// activation, code-editor typing) so unrelated edits never coalesce.
    pub(super) fn break_undo_chain(&mut self, i: usize) {
        if let Some(f) = self.files.get_mut(i) {
            f.undo.last_push_at = None;
            f.undo.last_key = None;
        }
    }

    /// Pop the head of the active tab's undo stack and revert its source.
    /// Returns whether anything was applied so callers can update the status
    /// line on a no-op press.
    pub(super) fn undo_active(&mut self) -> bool {
        let i = self.active;
        let Some(entry) = self.files[i].undo.past.pop_back() else {
            return false;
        };
        let target = entry.before.clone();
        let sel = entry.selection_before.clone();
        self.files[i].undo.future.push(entry);
        self.apply_undo_entry(i, target, sel);
        self.break_undo_chain(i);
        true
    }

    /// Symmetric to `undo_active`: pop the head of the redo stack and re-apply
    /// it. The redo stack is cleared by every fresh push, so this only ever
    /// returns true after at least one undo.
    pub(super) fn redo_active(&mut self) -> bool {
        let i = self.active;
        let Some(entry) = self.files[i].undo.future.pop() else {
            return false;
        };
        let target = entry.after.clone();
        let sel = entry.selection_after.clone();
        self.files[i].undo.past.push_back(entry);
        self.apply_undo_entry(i, target, sel);
        self.break_undo_chain(i);
        true
    }

    /// Common tail for undo and redo: write the target source, recompute
    /// dirty/needs_compile, restore the selection path on the viewer, and
    /// trigger the same recompile pipeline a fresh edit would.
    fn apply_undo_entry(
        &mut self,
        i: usize,
        target_source: String,
        sel_paths: Vec<Vec<String>>,
    ) {
        {
            let f = &mut self.files[i];
            f.source = target_source;
            f.dirty = f.source != f.last_saved_source;
            f.needs_compile = true;
            f.last_edit_at = Some(Instant::now());
        }
        if i == self.active {
            // `set_selected_paths` clears the live `selected` NodeIds so the
            // inspector doesn't render against stale indices for the one
            // frame between here and the recompile that resolves the paths.
            self.viewer.set_selected_paths(sel_paths);
            self.compile_active();
        }
    }

    /// Stable name-paths of the currently selected nodes, sourced from the
    /// viewer for the active tab. Empty vec for non-active tabs (we don't
    /// carry per-tab selection in Phase 1).
    pub(super) fn current_selection_path(&self, i: usize) -> Vec<Vec<String>> {
        if i == self.active {
            self.viewer.all_selected_paths()
        } else {
            Vec::new()
        }
    }
}
