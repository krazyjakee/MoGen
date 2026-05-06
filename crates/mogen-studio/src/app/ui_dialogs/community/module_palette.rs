//! Module palette (Cmd+Shift+M): typeahead `/api/registry/suggest`,
//! click a row to append a `use "@user/slug@v"` line at the end of the
//! active file's source.

use crate::app::moghub::{fetch_module_suggestions, MoghubMessage};
use crate::app::MogenStudioApp;

use super::state::ModulePaletteState;
use super::util::format_err;

impl MogenStudioApp {
    /// Open the module palette and kick an initial empty-query fetch
    /// so the user sees popular modules immediately. Called from the
    /// Community menu and from the Cmd+Shift+M shortcut.
    pub(in crate::app) fn open_module_palette(&mut self, ctx: &egui::Context) {
        self.community.module_palette = Some(ModulePaletteState {
            query: String::new(),
            results: Vec::new(),
            results_for: String::new(),
            error: None,
            needs_focus: true,
        });
        self.kick_module_suggest(ctx, String::new());
    }

    /// Render the module palette. Search field autofocuses on first
    /// paint; clicking a row appends the `use` line and closes the
    /// palette.
    pub(in crate::app) fn module_palette_dialog(&mut self, ctx: &egui::Context) {
        if self.community.module_palette.is_none() {
            return;
        }
        // Drain any in-flight registry-suggest reply.
        self.poll_module_suggest_worker();

        let mut keep_open = true;
        let mut to_insert: Option<String> = None;
        let mut new_query: Option<String> = None;
        egui::Window::new("Insert module reference")
            .open(&mut keep_open)
            .default_width(440.0)
            .resizable(true)
            .show(ctx, |ui| {
                let Some(state) = self.community.module_palette.as_mut() else {
                    return;
                };
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut state.query)
                        .hint_text("Search modules — try @alice/chair"),
                );
                if state.needs_focus {
                    resp.request_focus();
                    state.needs_focus = false;
                }
                if resp.changed() {
                    new_query = Some(state.query.clone());
                }
                if let Some(err) = &state.error {
                    ui.colored_label(egui::Color32::LIGHT_RED, err);
                }
                ui.separator();
                if state.results.is_empty() {
                    if state.query.trim().is_empty() {
                        ui.weak("Loading…");
                    } else {
                        ui.weak(
                            "No matching modules. \
                             Publish your own with Community → Publish current file…",
                        );
                    }
                } else {
                    egui::ScrollArea::vertical()
                        .max_height(360.0)
                        .auto_shrink([false; 2])
                        .show(ui, |ui| {
                            for s in &state.results {
                                let row_label = format!(
                                    "@{}/{} @{} — {}",
                                    s.user, s.slug, s.latest_version, s.title
                                );
                                if ui.button(row_label).clicked() {
                                    to_insert = Some(format!(
                                        "use \"@{}/{}@{}\"",
                                        s.user, s.slug, s.latest_version,
                                    ));
                                }
                            }
                        });
                }
            });
        if let Some(q) = new_query {
            self.kick_module_suggest(ctx, q);
        }
        if let Some(line) = to_insert {
            self.insert_module_line(line);
            self.community.module_palette = None;
        }
        if !keep_open {
            self.community.module_palette = None;
        }
    }

    fn kick_module_suggest(&mut self, ctx: &egui::Context, query: String) {
        let url = self.settings.moghub_url.clone();
        let token = self.settings.moghub_session.clone();
        self.community.pending_module_suggest =
            Some(fetch_module_suggestions(url, token, ctx.clone(), query));
    }

    fn poll_module_suggest_worker(&mut self) {
        let Some(inflight) = &self.community.pending_module_suggest else {
            return;
        };
        let Some(msg) = inflight.try_recv() else {
            return;
        };
        self.community.pending_module_suggest = None;
        if let MoghubMessage::ModuleSuggestions { query, result } = msg {
            let Some(state) = self.community.module_palette.as_mut() else {
                return;
            };
            // Drop stale results: the user has typed past the query
            // this fetch was for.
            if state.query != query {
                return;
            }
            match result {
                Ok(items) => {
                    state.results = items;
                    state.results_for = query;
                    state.error = None;
                }
                Err(e) => {
                    state.error = Some(format_err(&e));
                }
            }
        }
    }

    /// Append `line` to the active tab's source on its own line, with
    /// a trailing newline. Mirrors the web client's RegistryPalette
    /// insertion strategy — predictable for the user, doesn't poke the
    /// editor's TextEdit cursor state from outside.
    fn insert_module_line(&mut self, line: String) {
        let i = self.active;
        let Some(file) = self.files.get_mut(i) else {
            return;
        };
        if !file.source.ends_with('\n') && !file.source.is_empty() {
            file.source.push('\n');
        }
        file.source.push_str(&line);
        file.source.push('\n');
        file.dirty = file.source != file.last_saved_source;
        file.needs_compile = true;
        file.last_edit_at = Some(std::time::Instant::now());
    }
}
