//! Tools menu actions. Each entry is a pure source rewrite plumbed through
//! the same undo / dirty / recompile path as the other menu mutations.

use std::time::Instant;

use crate::edit;

use super::types::UndoKey;
use super::MogenStudioApp;

impl MogenStudioApp {
    /// Reformat the active tab's `.mog` source with predictable indentation
    /// and trailing-whitespace cleanup. No-op when the result matches the
    /// current buffer so a second press doesn't dirty the file or break the
    /// undo chain.
    pub(super) fn tidy_active(&mut self) {
        let i = self.active;
        let before = self.files[i].source.clone();
        let after = edit::tidy(&before);
        if after == before {
            self.active_mut().status = "tidy: already formatted".into();
            return;
        }

        {
            let f = &mut self.files[i];
            f.source = after;
            f.dirty = f.source != f.last_saved_source;
            f.needs_compile = true;
            f.last_edit_at = Some(Instant::now());
            f.status = "tidy: reformatted".into();
        }
        let key = UndoKey {
            surface: "menu",
            attr: Some("tidy".into()),
            node_path: Vec::new(),
        };
        self.push_undo(i, before, key);
        self.compile_active();
    }
}
