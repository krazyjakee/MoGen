//! Inspector "Meta → Generate" driver: spawns a fast-model summariser that
//! fills the `meta(name, description, tags)` block from the current DSL
//! source. Mirrors the shape of the prompt-enhance flow but writes back
//! through `mogen_dsl::upsert_meta_attr` / `upsert_meta_list_attr` so the
//! edit shows up in the same span-aware undo history as a hand edit.

use std::time::Instant;

use crate::app::types::{MetaGenerateInFlight, UndoKey};
use crate::app::util::run_meta_generate;
use crate::app::MogenStudioApp;

impl MogenStudioApp {
    /// Kick off a background meta-summarisation call for the active file.
    /// No-ops with an inline error when another meta-generate is already in
    /// flight, the source is empty, or no credential is configured.
    pub(in crate::app) fn start_meta_generate(&mut self, ctx: eframe::egui::Context) {
        if self.meta_generate_in_flight.is_some() {
            return;
        }
        let file_index = self.active;
        let source = self.files[file_index].source.clone();
        if source.trim().is_empty() {
            self.meta_generate_error.insert(
                file_index,
                "meta: nothing to summarise — write or generate some DSL first".into(),
            );
            return;
        }
        let provider = self.settings.provider();
        let credential = match self.resolve_credential() {
            Some(c) => c,
            None => {
                self.meta_generate_error.insert(
                    file_index,
                    format!(
                        "meta: no {} credential — set an API key in Edit → Preferences…",
                        provider.label(),
                    ),
                );
                return;
            }
        };
        let model = self.settings.provider_fast_model();
        let endpoints = self.provider_endpoints();

        self.meta_generate_error.remove(&file_index);

        let (tx, rx) = std::sync::mpsc::channel();
        self.meta_generate_in_flight = Some(MetaGenerateInFlight { file_index, rx });

        std::thread::spawn(move || {
            let result = run_meta_generate(
                source,
                provider,
                credential,
                model,
                endpoints,
            );
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }

    /// Drain the single meta-generate in-flight slot. On success, splice
    /// `name` / `description` / `tags` into the file's `meta(...)` block in
    /// one undoable step. On failure, stash an inline error for the button's
    /// label row.
    pub(in crate::app) fn poll_meta_generate(&mut self) {
        let Some(slot) = self.meta_generate_in_flight.as_ref() else {
            return;
        };
        let result = match slot.rx.try_recv() {
            Ok(r) => r,
            Err(_) => return,
        };
        let file_index = slot.file_index;
        self.meta_generate_in_flight = None;

        let Some(file) = self.files.get(file_index) else {
            return;
        };

        match result {
            Ok(suggestion) => {
                let before = file.source.clone();
                let mut new_src = before.clone();
                if !suggestion.name.is_empty() {
                    new_src = mogen_dsl::upsert_meta_attr(&new_src, "name", &suggestion.name);
                }
                if !suggestion.description.is_empty() {
                    new_src = mogen_dsl::upsert_meta_attr(
                        &new_src,
                        "description",
                        &suggestion.description,
                    );
                }
                let tag_refs: Vec<&str> =
                    suggestion.tags.iter().map(|s| s.as_str()).collect();
                new_src = mogen_dsl::upsert_meta_list_attr(&new_src, "tags", &tag_refs);

                if new_src == before {
                    self.meta_generate_error.insert(
                        file_index,
                        "meta: model produced no changes".into(),
                    );
                    return;
                }

                {
                    let f = &mut self.files[file_index];
                    f.source = new_src;
                    f.dirty = f.source != f.last_saved_source;
                    f.needs_compile = true;
                    f.last_edit_at = Some(Instant::now());
                }
                // Clear in-progress meta drafts so the inspector re-reads the
                // newly stamped values rather than showing the user's stale
                // edits over the LLM output.
                self.meta_name_drafts.remove(&file_index);
                self.meta_desc_drafts.remove(&file_index);
                self.meta_tags_drafts.remove(&file_index);
                self.break_undo_chain(file_index);
                self.push_undo(
                    file_index,
                    before,
                    UndoKey {
                        surface: "meta",
                        attr: Some("generate".into()),
                        node_path: Vec::new(),
                    },
                );
                self.meta_generate_error.remove(&file_index);
            }
            Err(err) => {
                self.meta_generate_error.insert(file_index, err);
            }
        }
    }

    /// True when a meta-generate call is in flight — used alongside
    /// `any_in_flight` to keep the repaint heartbeat ticking.
    pub(in crate::app) fn any_meta_generate_in_flight(&self) -> bool {
        self.meta_generate_in_flight.is_some()
    }

    /// Read-only accessor for the inspector — returns the file's current
    /// meta-generate error, if any.
    pub(in crate::app) fn meta_generate_error_for(&self, file_index: usize) -> Option<&str> {
        self.meta_generate_error.get(&file_index).map(|s| s.as_str())
    }

    /// True when there's a meta-generate call routing back to `file_index`.
    pub(in crate::app) fn meta_generate_in_flight_for(&self, file_index: usize) -> bool {
        self.meta_generate_in_flight
            .as_ref()
            .map(|s| s.file_index == file_index)
            .unwrap_or(false)
    }
}
