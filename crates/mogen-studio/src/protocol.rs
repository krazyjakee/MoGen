//! `mogen://` URL scheme: parsing + per-OS registration.
//!
//! The studio gets handed a URL when a user clicks an `Open in MoGen
//! Studio` link on the MoGHub website (or any other surface that knows
//! the scheme). The OS launches `mogen-studio <url>`; [`parse`] turns
//! that string into a [`MogenUrl`] which the app processes after the
//! splash drains.
//!
//! Registration is opt-in via `mogen-studio --register-protocol`. The
//! deb package's `.desktop` file already declares the handler, so distro
//! installs are auto-registered; the flag covers cargo / portable /
//! macOS / Windows installs.
//!
//! Supported URL shape:
//!
//! ```text
//! mogen://moghub/<user>/<slug>[?version=<n>]
//! ```
//!
//! `user` / `slug` are percent-decoded; an absent or non-numeric
//! `version` is treated as "latest" (the default for the server's
//! `download.zip` endpoint).

#[cfg(target_os = "linux")]
use std::path::PathBuf;

/// Parsed `mogen://…` URL. Add variants here as new actions are wired up.
#[derive(Debug, Clone)]
pub enum MogenUrl {
    /// `mogen://moghub/<user>/<slug>[?version=<n>]`
    MoghubOpen {
        user: String,
        slug: String,
        version: Option<i32>,
    },
}

/// Parse a `mogen://…` URL. Returns `None` for any input that isn't a
/// well-formed mogen URL — the caller falls back to treating the argv
/// as a regular file path.
pub fn parse(input: &str) -> Option<MogenUrl> {
    let input = input.trim();
    let rest = input.strip_prefix("mogen://")?;
    // Split off the query string before path parsing so `?version=…`
    // doesn't leak into the slug.
    let (path_part, query_part) = match rest.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (rest, None),
    };

    let mut segments = path_part.split('/').filter(|s| !s.is_empty());
    let action = segments.next()?;
    match action {
        "moghub" => {
            // `moghub/<user>/<slug>` — optional `open` action prefix
            // tolerated for forward-compat (`moghub/open/<user>/<slug>`).
            let peek = segments.next()?;
            let (user, slug);
            if peek == "open" {
                user = decode_segment(segments.next()?)?;
                slug = decode_segment(segments.next()?)?;
            } else {
                user = decode_segment(peek)?;
                slug = decode_segment(segments.next()?)?;
            }
            // Reject trailing junk so typos surface instead of silently
            // dropping data.
            if segments.next().is_some() {
                return None;
            }
            let version = query_part.and_then(parse_version_query);
            // Sanity: handles + slugs are short alphanumerics on the
            // server side, but we don't enforce the exact regex here —
            // leave that to the API and produce a friendly error
            // server-side rather than a confusing parse failure.
            if user.is_empty() || slug.is_empty() {
                return None;
            }
            Some(MogenUrl::MoghubOpen {
                user,
                slug,
                version,
            })
        }
        _ => None,
    }
}

/// Pull the first `version=` value out of a URL query. Other params are
/// ignored so future additions don't break older builds.
fn parse_version_query(query: &str) -> Option<i32> {
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=')?;
        if k == "version" {
            return v.parse().ok();
        }
    }
    None
}

/// Tiny percent-decoder for path segments. Good enough for the
/// `[A-Za-z0-9_-]` handles + slugs that moghub allows; falls back to
/// the raw byte for malformed escapes so we never panic on user input.
fn decode_segment(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push(((hi << 4) | lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b);
        i += 1;
    }
    String::from_utf8(out).ok()
}

/// Outcome of [`register`] / [`unregister`]. The `note` is shown to the
/// user so they know whether anything else needs to happen (e.g. a
/// shell restart, or that macOS bundling is required).
pub struct RegisterOutcome {
    pub ok: bool,
    pub note: String,
}

/// Register the `mogen://` URL handler with the OS. Idempotent.
pub fn register() -> RegisterOutcome {
    #[cfg(target_os = "linux")]
    {
        register_linux()
    }
    #[cfg(target_os = "windows")]
    {
        register_windows()
    }
    #[cfg(target_os = "macos")]
    {
        register_macos()
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        RegisterOutcome {
            ok: false,
            note: "protocol registration is not implemented on this platform".into(),
        }
    }
}

/// Remove the registration written by [`register`]. Best-effort — if
/// the user never registered, returns `ok: true` with a note.
pub fn unregister() -> RegisterOutcome {
    #[cfg(target_os = "linux")]
    {
        unregister_linux()
    }
    #[cfg(target_os = "windows")]
    {
        unregister_windows()
    }
    #[cfg(target_os = "macos")]
    {
        unregister_macos()
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        RegisterOutcome {
            ok: false,
            note: "protocol unregistration is not implemented on this platform".into(),
        }
    }
}

#[cfg(target_os = "linux")]
fn linux_desktop_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("applications"))
}

#[cfg(target_os = "linux")]
fn linux_desktop_path() -> Option<PathBuf> {
    linux_desktop_dir().map(|d| d.join("mogen-studio.desktop"))
}

#[cfg(target_os = "linux")]
fn register_linux() -> RegisterOutcome {
    let Some(exe) = std::env::current_exe().ok() else {
        return RegisterOutcome {
            ok: false,
            note: "could not resolve the current executable path".into(),
        };
    };
    let Some(dir) = linux_desktop_dir() else {
        return RegisterOutcome {
            ok: false,
            note: "could not locate $XDG_DATA_HOME/applications".into(),
        };
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return RegisterOutcome {
            ok: false,
            note: format!("creating {}: {e}", dir.display()),
        };
    }
    let path = dir.join("mogen-studio.desktop");
    let exec = exe.display().to_string();
    let body = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=MoGen Studio\n\
         GenericName=3D DSL Editor\n\
         Comment=Visual editor and live preview for the MoGen .mog DSL\n\
         Exec=\"{exec}\" %U\n\
         Icon=mogen-studio\n\
         Terminal=false\n\
         Categories=Graphics;3DGraphics;Development;\n\
         MimeType=text/plain;x-scheme-handler/mogen;\n\
         Keywords=3d;gltf;glb;dsl;mogen;\n\
         StartupWMClass=mogen-studio\n",
    );
    if let Err(e) = std::fs::write(&path, body) {
        return RegisterOutcome {
            ok: false,
            note: format!("writing {}: {e}", path.display()),
        };
    }

    // Tell xdg what app handles the scheme. `xdg-mime default` writes
    // into mimeapps.list; `update-desktop-database` refreshes the cache
    // so launchers see the new entry without a logout. Both are
    // best-effort: a missing tool is non-fatal — the .desktop file is
    // still discoverable on rescan.
    let _ = std::process::Command::new("xdg-mime")
        .args(["default", "mogen-studio.desktop", "x-scheme-handler/mogen"])
        .status();
    let _ = std::process::Command::new("update-desktop-database")
        .arg(&dir)
        .status();

    RegisterOutcome {
        ok: true,
        note: format!("wrote {} and registered x-scheme-handler/mogen", path.display()),
    }
}

#[cfg(target_os = "linux")]
fn unregister_linux() -> RegisterOutcome {
    let Some(path) = linux_desktop_path() else {
        return RegisterOutcome {
            ok: true,
            note: "nothing to remove".into(),
        };
    };
    let removed = std::fs::remove_file(&path).is_ok();
    if removed {
        if let Some(dir) = path.parent() {
            let _ = std::process::Command::new("update-desktop-database")
                .arg(dir)
                .status();
        }
    }
    RegisterOutcome {
        ok: true,
        note: if removed {
            format!("removed {}", path.display())
        } else {
            "no user-level registration found (system-installed entries are managed by the package)".into()
        },
    }
}

#[cfg(target_os = "windows")]
fn register_windows() -> RegisterOutcome {
    use std::process::Command;
    let Some(exe) = std::env::current_exe().ok() else {
        return RegisterOutcome {
            ok: false,
            note: "could not resolve the current executable path".into(),
        };
    };
    // HKCU avoids needing admin rights. Per
    // https://learn.microsoft.com/en-us/previous-versions/windows/internet-explorer/ie-developer/platform-apis/aa767914(v=vs.85)
    // a custom URL scheme needs at minimum:
    //   HKCU\Software\Classes\<scheme>            (default = "URL:<scheme> Protocol")
    //   HKCU\Software\Classes\<scheme>\URL Protocol = ""
    //   HKCU\Software\Classes\<scheme>\shell\open\command (default = "<exe>" "%1")
    let exe_q = format!("\\\"{}\\\" \\\"%1\\\"", exe.display());
    let cmds: [&[&str]; 4] = [
        &[
            "add", "HKCU\\Software\\Classes\\mogen", "/ve", "/d", "URL:mogen Protocol", "/f",
        ],
        &[
            "add",
            "HKCU\\Software\\Classes\\mogen",
            "/v",
            "URL Protocol",
            "/d",
            "",
            "/f",
        ],
        &[
            "add",
            "HKCU\\Software\\Classes\\mogen\\DefaultIcon",
            "/ve",
            "/d",
            &format!("\"{}\",0", exe.display()),
            "/f",
        ],
        &[
            "add",
            "HKCU\\Software\\Classes\\mogen\\shell\\open\\command",
            "/ve",
            "/d",
            &exe_q,
            "/f",
        ],
    ];
    for args in cmds {
        let status = Command::new("reg").args(args).status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => {
                return RegisterOutcome {
                    ok: false,
                    note: format!("reg.exe exited with {s}"),
                }
            }
            Err(e) => {
                return RegisterOutcome {
                    ok: false,
                    note: format!("running reg.exe: {e}"),
                }
            }
        }
    }
    RegisterOutcome {
        ok: true,
        note: "registered mogen:// under HKCU\\Software\\Classes\\mogen".into(),
    }
}

#[cfg(target_os = "windows")]
fn unregister_windows() -> RegisterOutcome {
    use std::process::Command;
    let status = Command::new("reg")
        .args(["delete", "HKCU\\Software\\Classes\\mogen", "/f"])
        .status();
    match status {
        Ok(s) if s.success() => RegisterOutcome {
            ok: true,
            note: "removed HKCU\\Software\\Classes\\mogen".into(),
        },
        _ => RegisterOutcome {
            ok: true,
            note: "nothing to remove".into(),
        },
    }
}

#[cfg(target_os = "macos")]
fn register_macos() -> RegisterOutcome {
    // macOS ties URL scheme handlers to bundle identifiers via
    // CFBundleURLTypes in the .app's Info.plist + LaunchServices's
    // database. A bare cargo binary doesn't have a bundle, so there's
    // nothing reliable to register. Document this and let the
    // packaging story (a real .app bundle, signed and notarised) own
    // the registration.
    RegisterOutcome {
        ok: false,
        note: "macOS handlers require a signed .app bundle with CFBundleURLTypes — \
               install the packaged build instead of running cargo binaries directly"
            .into(),
    }
}

#[cfg(target_os = "macos")]
fn unregister_macos() -> RegisterOutcome {
    RegisterOutcome {
        ok: true,
        note: "macOS handlers are owned by the .app bundle's Info.plist; nothing to remove".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unwrap_moghub(url: &str) -> (String, String, Option<i32>) {
        match parse(url).unwrap() {
            MogenUrl::MoghubOpen { user, slug, version } => (user, slug, version),
        }
    }

    #[test]
    fn parses_basic() {
        let (user, slug, version) = unwrap_moghub("mogen://moghub/jake/wooden-stool");
        assert_eq!(user, "jake");
        assert_eq!(slug, "wooden-stool");
        assert_eq!(version, None);
    }

    #[test]
    fn parses_open_action_prefix() {
        let (user, slug, _) = unwrap_moghub("mogen://moghub/open/jake/wooden-stool");
        assert_eq!(user, "jake");
        assert_eq!(slug, "wooden-stool");
    }

    #[test]
    fn parses_version_query() {
        let (_, _, version) = unwrap_moghub("mogen://moghub/jake/wooden-stool?version=3");
        assert_eq!(version, Some(3));
    }

    #[test]
    fn rejects_non_mogen_scheme() {
        assert!(parse("https://example.com").is_none());
        assert!(parse("/tmp/file.mog").is_none());
        assert!(parse("").is_none());
    }

    #[test]
    fn rejects_missing_slug() {
        assert!(parse("mogen://moghub/jake").is_none());
    }

    #[test]
    fn rejects_unknown_action() {
        assert!(parse("mogen://nope/jake/x").is_none());
    }

    #[test]
    fn percent_decodes_segments() {
        let (_, slug, _) = unwrap_moghub("mogen://moghub/jake/with%20space");
        assert_eq!(slug, "with space");
    }
}
