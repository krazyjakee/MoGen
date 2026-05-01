//! Spotlight-style command palette opened with Ctrl/Cmd+P. Lists recently
//! opened `.mog` files *and* a curated set of action commands (Save, Build,
//! Frame, gizmo modes, etc.), filtered by a fuzzy substring match on the
//! typed query. Arrow keys navigate, Enter activates, Esc closes.
//!
//! Recent-file activations route through `open_path`. Command activations
//! route through `dispatch_menu_action` for everything that already maps to
//! a `MenuAction`, plus a small set of viewer/find calls for actions that
//! aren't menu-bound (gizmo-mode switches, "Find in Editor", "Toggle
//! Cinema"). That reuse means new menu items pick up palette entries for
//! free if they're added to the catalog below.

use std::path::PathBuf;

use eframe::egui;

use crate::gizmo::GizmoMode;

use super::types::{MenuAction, ShortcutAction};
use super::MogenStudioApp;

/// Stable egui id for the spotlight query input. Constant because only one
/// palette is ever live at a time.
fn spotlight_input_id() -> egui::Id {
    egui::Id::new("mog_spotlight_input")
}

/// Action a spotlight row will run when activated. Most map onto an existing
/// `MenuAction` — that's the right level of indirection because the menu
/// dispatcher already handles confirmations, in-flight gating, status
/// messages, etc. The non-menu variants cover viewport / editor toggles
/// that the menu bar doesn't expose as menu items today. Not `Copy` because
/// `MenuAction::OpenPath` carries a `PathBuf`; the catalog is consumed by
/// move on activation, which is fine.
enum CommandAction {
    /// Run via `dispatch_menu_action`.
    Menu(MenuAction),
    /// Switch the viewport gizmo to a specific mode.
    Gizmo(GizmoMode),
    /// Toggle cinema-mode playback in the 3D viewer.
    ToggleCinema,
    /// Open the editor's find bar (Cmd+F equivalent).
    OpenFind,
}

/// One row in the command catalog. `label` is what the user sees; `hint` is
/// the right-aligned subtitle (typically a keyboard shortcut). The
/// `keywords` field exists so commands can match queries that don't appear
/// in the visible label — "open" finds "Find in Editor", "shortcut" finds
/// "Preferences", etc.
struct Command {
    label: &'static str,
    hint: String,
    keywords: &'static str,
    action: CommandAction,
}

/// Build the action catalog. Each call clones any String hints (cheap — the
/// list is < 30 entries) so the result owns everything and isn't tied to
/// the borrow on `ctx`. Shortcut hints are formatted via egui so they reflect
/// the live platform binding (Cmd vs Ctrl).
fn command_catalog(ctx: &egui::Context) -> Vec<Command> {
    fn sc(ctx: &egui::Context, action: ShortcutAction) -> String {
        ctx.format_shortcut(&action.shortcut())
    }
    vec![
        // ---- File / tabs --------------------------------------------------
        Command {
            label: "New Tab",
            hint: sc(ctx, ShortcutAction::NewUntitled),
            keywords: "file create empty",
            action: CommandAction::Menu(MenuAction::NewUntitled),
        },
        Command {
            label: "New from Prompt…",
            hint: sc(ctx, ShortcutAction::OpenNewPromptModal),
            keywords: "generate llm gemini ai create scene",
            action: CommandAction::Menu(MenuAction::OpenNewPromptModal),
        },
        Command {
            label: "Open File…",
            hint: sc(ctx, ShortcutAction::OpenDialog),
            keywords: "load mog disk picker",
            action: CommandAction::Menu(MenuAction::OpenDialog),
        },
        Command {
            label: "Import .mog…",
            hint: String::new(),
            keywords: "include external file",
            action: CommandAction::Menu(MenuAction::ImportDsl),
        },
        Command {
            label: "Save",
            hint: sc(ctx, ShortcutAction::Save),
            keywords: "write disk persist",
            action: CommandAction::Menu(MenuAction::Save),
        },
        Command {
            label: "Save As…",
            hint: sc(ctx, ShortcutAction::SaveAs),
            keywords: "rename copy",
            action: CommandAction::Menu(MenuAction::SaveAs),
        },
        Command {
            label: "Close Tab",
            hint: sc(ctx, ShortcutAction::CloseActive),
            keywords: "shut tab discard",
            action: CommandAction::Menu(MenuAction::CloseActive),
        },
        Command {
            label: "Reopen Closed Tab",
            hint: sc(ctx, ShortcutAction::ReopenClosed),
            keywords: "restore last closed undo",
            action: CommandAction::Menu(MenuAction::ReopenClosed),
        },
        // ---- Build / validate --------------------------------------------
        Command {
            label: "Build GLB…",
            hint: sc(ctx, ShortcutAction::Build),
            keywords: "export gltf compile output",
            action: CommandAction::Menu(MenuAction::Build),
        },
        Command {
            label: "Re-check",
            hint: sc(ctx, ShortcutAction::Recheck),
            keywords: "validate parse diagnostics",
            action: CommandAction::Menu(MenuAction::Recheck),
        },
        // ---- View / viewport ---------------------------------------------
        Command {
            label: "Frame Selected",
            hint: sc(ctx, ShortcutAction::Frame),
            keywords: "fit camera focus zoom",
            action: CommandAction::Menu(MenuAction::Frame),
        },
        Command {
            label: "Toggle Cinema Mode",
            hint: String::new(),
            keywords: "camera shots presentation orbit auto",
            action: CommandAction::ToggleCinema,
        },
        Command {
            label: "Gizmo: Translate",
            hint: "W".into(),
            keywords: "move position xyz",
            action: CommandAction::Gizmo(GizmoMode::Translate),
        },
        Command {
            label: "Gizmo: Rotate",
            hint: "E".into(),
            keywords: "spin orient axis",
            action: CommandAction::Gizmo(GizmoMode::Rotate),
        },
        Command {
            label: "Gizmo: Scale",
            hint: "R".into(),
            keywords: "size resize stretch",
            action: CommandAction::Gizmo(GizmoMode::Scale),
        },
        // ---- Editor ------------------------------------------------------
        Command {
            label: "Find in Editor",
            hint: ctx.format_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::COMMAND,
                egui::Key::F,
            )),
            keywords: "search query text match",
            action: CommandAction::OpenFind,
        },
        // ---- Capture -----------------------------------------------------
        Command {
            label: "Generate Thumbnail",
            hint: String::new(),
            keywords: "screenshot png image render",
            action: CommandAction::Menu(MenuAction::GenerateThumbnail),
        },
        Command {
            label: "Generate Video",
            hint: String::new(),
            keywords: "mp4 movie capture cinema",
            action: CommandAction::Menu(MenuAction::GenerateVideo),
        },
        // ---- App ---------------------------------------------------------
        Command {
            label: "Preferences…",
            hint: sc(ctx, ShortcutAction::OpenOptions),
            keywords: "options settings api key theme",
            action: CommandAction::Menu(MenuAction::OpenOptions),
        },
        Command {
            label: "About MoGen Studio",
            hint: String::new(),
            keywords: "version help info",
            action: CommandAction::Menu(MenuAction::OpenAbout),
        },
    ]
}

/// One result row to render. Files and commands share the same scrollable
/// list so a single Up/Down/Enter flow walks across both — the tag tells the
/// renderer which layout to use and the activate handler which path to take.
enum SpotlightItem {
    File(PathBuf),
    Command(Command),
}

impl MogenStudioApp {
    /// Open the spotlight palette. Re-entry is a no-op except for re-grabbing
    /// focus on the input — convenient when the user mashes Cmd+P.
    pub(super) fn open_spotlight(&mut self) {
        if !self.spotlight.open {
            self.spotlight.open = true;
            self.spotlight.query.clear();
            self.spotlight.selected = 0;
        }
        self.spotlight.focus_pending = true;
    }

    pub(super) fn close_spotlight(&mut self) {
        self.spotlight.open = false;
    }

    /// Catch Cmd+P / Ctrl+P before any text widget sees it. Always consumes
    /// the binding (no focus gating) so the palette is reachable from inside
    /// the editor too — that's the whole point of a "type-from-anywhere"
    /// command palette.
    pub(super) fn dispatch_spotlight_shortcuts(&mut self, ctx: &egui::Context) {
        let sc = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::P);
        if ctx.input_mut(|i| i.consume_shortcut(&sc)) {
            self.open_spotlight();
        }
    }

    /// Rank and filter the combined (commands + recent files) list against
    /// the current query. With an empty query we list recent files first
    /// (the primary "open" use case), then commands in catalog order — a
    /// keyboard-only user can still scroll through every action without
    /// typing anything. With a query, both pools filter through the same
    /// subsequence test and rank together so the best label/path/keyword
    /// match floats to the top.
    fn spotlight_results(&self, ctx: &egui::Context) -> Vec<SpotlightItem> {
        let q = self.spotlight.query.trim().to_lowercase();
        let recent: Vec<PathBuf> = self
            .settings
            .recent_files
            .iter()
            .map(PathBuf::from)
            .collect();
        let catalog = command_catalog(ctx);

        if q.is_empty() {
            let mut out: Vec<SpotlightItem> =
                recent.into_iter().map(SpotlightItem::File).collect();
            out.extend(catalog.into_iter().map(SpotlightItem::Command));
            return out;
        }

        // Score each candidate: lower is better. The bonus columns are
        // ordered so a label match always outranks a path-only match for the
        // same query, regardless of the underlying string lengths.
        let mut scored: Vec<(usize, SpotlightItem)> = Vec::new();

        for path in recent {
            let path_lower = path.to_string_lossy().to_lowercase();
            let basename_lower = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_lowercase();
            if !subsequence_match(&path_lower, &q) {
                continue;
            }
            // Files: basename match (0) ranks above path-only match (1).
            let basename_bonus = if basename_lower.contains(&q) { 0 } else { 1 };
            let score = basename_bonus * 10_000 + path_lower.len();
            scored.push((score, SpotlightItem::File(path)));
        }

        for cmd in catalog {
            let label_lower = cmd.label.to_lowercase();
            let kw_lower = cmd.keywords.to_lowercase();
            let combined = format!("{label_lower} {kw_lower}");
            if !subsequence_match(&combined, &q) {
                continue;
            }
            // Commands: label match (0) > keyword-only match (1). The +1 in
            // the label tier nudges commands behind file basename matches of
            // the same length, so typing "humanoid" still surfaces a
            // humanoid_full.mog file before any future command containing the
            // word.
            let label_bonus = if label_lower.contains(&q) { 1 } else { 2 };
            let score = label_bonus * 10_000 + cmd.label.len();
            scored.push((score, SpotlightItem::Command(cmd)));
        }

        scored.sort_by_key(|(s, _)| *s);
        scored.into_iter().map(|(_, item)| item).collect()
    }

    pub(super) fn ui_spotlight(&mut self, ctx: &egui::Context) {
        if !self.spotlight.open {
            return;
        }

        let results = self.spotlight_results(ctx);
        let max_idx = results.len().saturating_sub(1);
        if self.spotlight.selected > max_idx {
            self.spotlight.selected = max_idx;
        }

        let mut close_after = false;
        let mut activate_idx: Option<usize> = None;
        let mut open_window = true;

        // Up/Down/Enter handled at the ctx level so they work whether or not
        // the input still owns focus (e.g. the user clicked into the list).
        // Esc closes regardless of focus too.
        if !results.is_empty() {
            ctx.input_mut(|i| {
                if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                    self.spotlight.selected = (self.spotlight.selected + 1) % results.len();
                }
                if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                    self.spotlight.selected = if self.spotlight.selected == 0 {
                        results.len() - 1
                    } else {
                        self.spotlight.selected - 1
                    };
                }
                if i.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
                    activate_idx = Some(self.spotlight.selected);
                }
            });
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            close_after = true;
        }

        egui::Window::new("Open File")
            .id(egui::Id::new("spotlight_modal"))
            .open(&mut open_window)
            .order(egui::Order::Tooltip)
            .collapsible(false)
            .resizable(false)
            .title_bar(false)
            .anchor(egui::Align2::CENTER_TOP, [0.0, 80.0])
            .default_width(540.0)
            .show(ctx, |ui| {
                let input_id = spotlight_input_id();
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.spotlight.query)
                        .id(input_id)
                        .desired_width(f32::INFINITY)
                        .hint_text("Type a command or recent file…"),
                );
                if self.spotlight.focus_pending {
                    ui.memory_mut(|m| m.request_focus(input_id));
                    self.spotlight.focus_pending = false;
                }
                if resp.changed() {
                    // Re-rank from the top whenever the query changes so the
                    // best match is always preselected and Enter is one step.
                    self.spotlight.selected = 0;
                }

                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);

                if results.is_empty() {
                    ui.label(egui::RichText::new("No matches.").weak());
                    return;
                }

                egui::ScrollArea::vertical()
                    .max_height(380.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for (idx, item) in results.iter().enumerate() {
                            let selected = idx == self.spotlight.selected;
                            let row = render_row(ui, item, selected);
                            if row.clicked() {
                                activate_idx = Some(idx);
                            }
                            if row.hovered() {
                                self.spotlight.selected = idx;
                            }
                        }
                    });
            });

        if let Some(idx) = activate_idx {
            if let Some(item) = results.into_iter().nth(idx) {
                self.activate_spotlight(ctx, item);
                close_after = true;
            }
        }
        if !open_window || close_after {
            self.close_spotlight();
        }
    }

    /// Activate one row. Closing the palette is the caller's job — keeping
    /// it separate means the unit test in this file can drive activation
    /// without standing up the full window.
    fn activate_spotlight(&mut self, ctx: &egui::Context, item: SpotlightItem) {
        match item {
            SpotlightItem::File(path) => self.open_path(&path),
            SpotlightItem::Command(cmd) => self.run_command_action(ctx, cmd.action),
        }
    }

    fn run_command_action(&mut self, ctx: &egui::Context, action: CommandAction) {
        match action {
            CommandAction::Menu(m) => self.dispatch_menu_action(ctx, m),
            CommandAction::Gizmo(mode) => self.viewer.set_gizmo_mode(mode),
            CommandAction::ToggleCinema => {
                let on = self.viewer.is_cinema_active();
                self.viewer.set_cinema_active(!on);
            }
            CommandAction::OpenFind => self.open_find(ctx),
        }
    }
}

/// Render one row. Files show as `name    dir`; commands show as `label …
/// hint` with the hint right-aligned and dimmed. The dimmed-hint pattern
/// mirrors how egui's own menu items display shortcut text.
fn render_row(ui: &mut egui::Ui, item: &SpotlightItem, selected: bool) -> egui::Response {
    match item {
        SpotlightItem::File(path) => {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            let dir = path
                .parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            let label = if dir.is_empty() {
                egui::RichText::new(&name).strong()
            } else {
                egui::RichText::new(format!("{name}    {dir}")).strong()
            };
            ui.selectable_label(selected, label)
        }
        SpotlightItem::Command(cmd) => {
            // Render label + right-aligned hint inside one selectable row so
            // hover / click hit-test the whole strip. We do this by
            // `selectable_label`-ing a multi-segment LayoutJob, which lets a
            // single response cover both pieces of text.
            let mut job = egui::text::LayoutJob::default();
            let visuals = ui.style().visuals.clone();
            job.append(
                cmd.label,
                0.0,
                egui::TextFormat {
                    color: visuals.text_color(),
                    ..Default::default()
                },
            );
            if !cmd.hint.is_empty() {
                job.append(
                    "    ",
                    0.0,
                    egui::TextFormat {
                        color: visuals.weak_text_color(),
                        ..Default::default()
                    },
                );
                job.append(
                    &cmd.hint,
                    0.0,
                    egui::TextFormat {
                        color: visuals.weak_text_color(),
                        ..Default::default()
                    },
                );
            }
            ui.selectable_label(selected, job)
        }
    }
}

/// Case-insensitive subsequence test: every character of `needle` appears in
/// `haystack` in order, but not necessarily contiguously. Both inputs are
/// expected to already be lowercased by the caller. An empty needle matches
/// anything.
fn subsequence_match(haystack: &str, needle: &str) -> bool {
    let mut it = haystack.chars();
    for c in needle.chars() {
        let found = it.by_ref().any(|h| h == c);
        if !found {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::subsequence_match;

    #[test]
    fn subsequence_basic() {
        assert!(subsequence_match("humanoid_full", "huf"));
        assert!(subsequence_match("humanoid_full", "ufl"));
        assert!(!subsequence_match("humanoid_full", "xyz"));
        assert!(subsequence_match("anything", ""));
    }
}
