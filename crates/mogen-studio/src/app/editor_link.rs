//! Ctrl+click navigation in the code editor.
//!
//! Hooks called from `ui_editor` after the TextEdit has rendered:
//!
//! - [`MogenStudioApp::handle_editor_link_click`] — performs hit-testing,
//!   updates the cursor icon while Ctrl is held, and dispatches the action
//!   on click. Returns `true` when a click was consumed so the caller can
//!   skip the normal caret-move side-effects.
//! - [`MogenStudioApp::open_link_target`] — apply a [`LinkTarget`] (open
//!   URL, open file, jump caret to local module declaration, scroll docs
//!   window). Pulled out of the click handler so the inspector can reuse it
//!   later if we want to surface the same actions through a context menu.
//!
//! The token resolver itself lives in `crate::docs` so the navigation logic
//! is testable without an egui context.

use std::path::PathBuf;
use std::process::Command;

use eframe::egui;

use crate::docs::{self, LinkHit, LinkTarget};

use super::MogenStudioApp;

impl MogenStudioApp {
    /// One-shot Ctrl+click handler driven from `ui_editor`. Resolves the
    /// click position to a token, pulls the right [`LinkTarget`] from
    /// `crate::docs`, and dispatches it. Returns `true` when the event was
    /// consumed (so `changed = true` doesn't get spuriously set later).
    ///
    /// Also paints the cursor as `PointingHand` whenever the user hovers a
    /// resolvable token with Ctrl held — matches every IDE's affordance for
    /// "this is a link, click to follow".
    pub(super) fn handle_editor_link_click(
        &mut self,
        ui: &egui::Ui,
        output: &egui::widgets::text_edit::TextEditOutput,
    ) -> bool {
        let resp = &output.response;
        let Some(hover_pos) = ui.input(|i| i.pointer.hover_pos()) else {
            return false;
        };
        if !resp.rect.contains(hover_pos) {
            return false;
        }

        // Modifiers: prefer `command` (works on macOS via Cmd) but also
        // accept raw Ctrl on every platform — same convention the viewport
        // already uses for its modifier checks.
        let ctrl = ui.input(|i| i.modifiers.ctrl || i.modifiers.command);
        if !ctrl {
            return false;
        }

        // Hit-test inside the galley to map the screen position to a byte
        // offset in the source. egui's CCursor is a char index; convert it
        // back to bytes so the docs resolver can slice the source directly.
        let local = hover_pos - output.galley_pos;
        let cursor = output.galley.cursor_from_pos(local);
        let char_idx = cursor.ccursor.index;
        let i = self.active;
        let source = &self.files[i].source;
        let byte_off = char_to_byte_offset(source, char_idx);

        let Some(hit) = docs::resolve_link_at(source, byte_off) else {
            return false;
        };

        // Ctrl is held over a resolvable token — show the link affordance.
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);

        // Only dispatch on the actual press; hovering with Ctrl held just
        // changes the cursor.
        let pressed = ui.input(|i| i.pointer.primary_pressed());
        if !pressed {
            return false;
        }

        self.open_link_target(ui.ctx(), hit);
        true
    }

    /// Apply a resolved [`LinkHit`]. Split out so the click handler stays
    /// thin and so we can dispatch the same action from a future right-
    /// click "Go to definition" menu item without duplicating the routing.
    pub(super) fn open_link_target(&mut self, ctx: &egui::Context, hit: LinkHit) {
        let i = self.active;
        match hit.target {
            LinkTarget::Url(url) => {
                ctx.open_url(egui::OpenUrl::new_tab(url.clone()));
                self.files[i].status = format!("opened {url}");
            }
            LinkTarget::File(rel) => {
                let resolved = self.resolve_link_path(&rel);
                let is_mog = resolved
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("mog"))
                    .unwrap_or(false);
                if is_mog {
                    if resolved.is_file() {
                        self.open_path(&resolved);
                    } else {
                        self.files[i].status =
                            format!("can't open {} — file not found", resolved.display());
                    }
                } else if resolved.exists() {
                    match open_in_os(&resolved) {
                        Ok(()) => {
                            self.files[i].status = format!("opened {}", resolved.display());
                        }
                        Err(e) => {
                            self.files[i].status =
                                format!("open failed: {} ({e})", resolved.display());
                        }
                    }
                } else {
                    self.files[i].status =
                        format!("can't open {} — file not found", resolved.display());
                }
            }
            LinkTarget::Module(name) => {
                // Prefer jumping to the local declaration when one exists;
                // fall back to the stdlib docs page so a Ctrl+click on
                // `use "leg"` always lands somewhere useful.
                let src = &self.files[i].source;
                if let Some(decl_off) = docs::find_module_decl(src, &name) {
                    self.files[i].pending_caret = Some(decl_off);
                    self.files[i].status = format!("jump → module \"{name}\"");
                } else if docs::STDLIB_MODULE_NAMES.contains(&name.as_str()) {
                    self.open_docs_at("modules", Some(docs::heading_slug(&name)));
                    self.files[i].status =
                        format!("docs → modules.md#{name} (stdlib module)");
                } else {
                    self.files[i].status =
                        format!("no `module \"{name}\"` declaration in this file");
                }
            }
            LinkTarget::Docs(anchor) => {
                let page = anchor.page;
                let slug = anchor.slug.clone();
                self.open_docs_at(page, slug.clone());
                self.files[i].status = match slug {
                    Some(s) => format!("docs → {page}.md#{s}"),
                    None => format!("docs → {page}.md"),
                };
            }
        }
    }

    /// Resolve a path string from the source against the active file's
    /// directory. Absolute paths win as-is; relative paths are joined with
    /// the directory of the .mog file (or the project root when the file
    /// is untitled).
    fn resolve_link_path(&self, raw: &str) -> PathBuf {
        let p = PathBuf::from(raw);
        if p.is_absolute() {
            return p;
        }
        let base = self.files[self.active]
            .path
            .as_deref()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| self.project_root.clone());
        base.join(p)
    }
}

/// Convert a character index into a byte offset in `s`. egui's hit-testing
/// returns char positions but the docs resolver needs to slice into bytes,
/// so this bridges the two. Saturates at the end of the string when the
/// caller asks for an index past the last char.
fn char_to_byte_offset(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(s.len())
}

/// Hand a path off to the OS so its default app opens it. Used for non-mog
/// files referenced from materials (PNG textures, exported .glb assets,
/// etc.). MoGen doesn't ship with image / model viewers, so delegating is
/// the only sensible thing to do.
fn open_in_os(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg(path).status().map(|_| ())
    }
    #[cfg(target_os = "windows")]
    {
        // `cmd /C start "" path` keeps the title argument quoted so paths
        // with spaces aren't misinterpreted as the window title.
        Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(path)
            .status()
            .map(|_| ())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Command::new("xdg-open").arg(path).status().map(|_| ())
    }
}

/// Open the OS file manager at `path`'s parent directory, with `path`
/// itself selected/highlighted when the platform supports it.
///
/// macOS uses `open -R`; Windows uses `explorer /select,`; Linux speaks the
/// `org.freedesktop.FileManager1.ShowItems` DBus interface (honoured by
/// Nautilus, Nemo, Dolphin, Caja, Thunar, …) and falls back to
/// `xdg-open <parent>` if DBus or the file manager isn't available — that
/// loses the selection but still gets the user to the right folder.
pub(in crate::app) fn reveal_in_os(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg("-R").arg(path).status().map(|_| ())
    }
    #[cfg(target_os = "windows")]
    {
        // `explorer /select,<path>` opens the parent folder with the file
        // highlighted. The comma is part of the syntax, not a separator.
        let mut arg = std::ffi::OsString::from("/select,");
        arg.push(path);
        Command::new("explorer").arg(arg).status().map(|_| ())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let uri = path_to_file_uri(path);
        let dbus = Command::new("dbus-send")
            .args([
                "--session",
                "--print-reply",
                "--dest=org.freedesktop.FileManager1",
                "--type=method_call",
                "/org/freedesktop/FileManager1",
                "org.freedesktop.FileManager1.ShowItems",
            ])
            .arg(format!("array:string:{uri}"))
            .arg("string:")
            .output();
        if let Ok(out) = dbus {
            if out.status.success() {
                return Ok(());
            }
        }
        let parent = path.parent().unwrap_or(path);
        Command::new("xdg-open").arg(parent).status().map(|_| ())
    }
}

/// Encode an absolute path as a `file://` URI, percent-encoding any byte
/// that isn't an unreserved URI character. Used to pass paths to
/// `org.freedesktop.FileManager1.ShowItems`, which expects RFC-3986 URIs.
#[cfg(all(unix, not(target_os = "macos")))]
fn path_to_file_uri(path: &std::path::Path) -> String {
    let s = path.to_string_lossy();
    let mut out = String::from("file://");
    for b in s.as_bytes() {
        let c = *b as char;
        if c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '.' | '-' | '~') {
            out.push(c);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}
