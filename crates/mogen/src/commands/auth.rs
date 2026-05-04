//! `mogen auth` — Google OAuth login/status/logout.
//!
//! Targets a Google OAuth desktop client (see
//! [`mogen_llm::google_oauth::client`]) so a paid Pro account holder can use
//! `mogen generate` against `gemini-3-pro-preview` without a billing-enabled
//! API key. The bearer token authenticates against
//! `cloudcode-pa.googleapis.com/v1internal:generateContent` — a separate
//! surface from the public `generativelanguage.googleapis.com` API-key path.
//!
//! Setup: zero by default. `mogen` ships with the same public Gemini CLI
//! OAuth client the official Google Gemini CLI uses, so `mogen auth login`
//! works out of the box. Power users can drop a populated `oauth_client.json`
//! in `~/.mogen/` (or set `MOGEN_OAUTH_CLIENT`) to point `mogen` at a
//! different OAuth client (e.g. Antigravity, or a custom Cloud Console
//! desktop client).

use std::time::Duration;

use anyhow::{bail, Context, Result};
use mogen_llm::{
    all_existing_token_paths, delete_bundle, load_bundle, run_login_flow, save_bundle,
    token_store_path, token_store_write_path, LoginOptions, OAuthError,
};

/// Subcommand surface mirrored from `clap` in `main.rs`.
pub(crate) enum AuthCmd {
    Login {
        force: bool,
        no_browser: bool,
        timeout_secs: u64,
    },
    Status {
        verbose: bool,
    },
    Logout,
}

pub(crate) fn dispatch(cmd: AuthCmd) -> Result<()> {
    match cmd {
        AuthCmd::Login { force, no_browser, timeout_secs } => {
            login(force, no_browser, timeout_secs)
        }
        AuthCmd::Status { verbose } => status(verbose),
        AuthCmd::Logout => logout(),
    }
}

fn login(force: bool, no_browser: bool, timeout_secs: u64) -> Result<()> {
    // Read path may surface an existing token at a legacy location
    // (`~/.cache/mogen/`); the write path is always the canonical
    // `~/.mogen/google_auth.json`. New logins always land canonical so
    // legacy files quietly become orphans and get cleaned up by logout.
    let read_path = token_store_path()
        .context("cannot determine token store location (set MOGEN_CACHE_DIR)")?;
    let write_path = token_store_write_path()
        .context("cannot determine token store write location (set MOGEN_CACHE_DIR)")?;

    if !force {
        if let Some(existing) = load_bundle(&read_path)
            .with_context(|| format!("reading {}", read_path.display()))?
        {
            let who = existing
                .email
                .as_deref()
                .unwrap_or("(unknown account)");
            let proj = existing
                .project_id
                .as_deref()
                .unwrap_or("(no project)");
            println!(
                "Already logged in as {who} (project {proj}). Use --force to re-authenticate."
            );
            return Ok(());
        }
    }

    eprintln!(
        "mogen auth: opening Google sign-in. If sign-in fails, set \
         GEMINI_API_KEY as a fallback (see README)."
    );

    let opts = LoginOptions {
        open_browser: !no_browser,
        timeout: Duration::from_secs(timeout_secs.clamp(10, 3600)),
    };

    let outcome = match run_login_flow(opts) {
        Ok(o) => o,
        Err(err) => return Err(login_anyhow(err)),
    };

    if no_browser {
        eprintln!(
            "mogen auth: open this URL in a browser to continue:\n{}",
            outcome.authorize_url
        );
    }

    save_bundle(&write_path, &outcome.bundle)
        .with_context(|| format!("saving {}", write_path.display()))?;

    // If we saved to the canonical path but the user had a legacy token
    // at a different path, clean that up so future reads don't accidentally
    // surface stale credentials from `~/.cache/mogen/` or `%LOCALAPPDATA%`.
    if read_path != write_path && read_path.exists() {
        let _ = delete_bundle(&read_path);
    }

    let who = outcome
        .bundle
        .email
        .as_deref()
        .unwrap_or("(unknown account)");
    let proj = outcome
        .bundle
        .project_id
        .as_deref()
        .unwrap_or("(no project)");
    println!(
        "Logged in as {who}. Project: {proj}. Token stored at {}.",
        write_path.display()
    );
    Ok(())
}

fn status(verbose: bool) -> Result<()> {
    let path = match token_store_path() {
        Some(p) => p,
        None => {
            println!(
                "Not logged in (no token store path resolvable; set MOGEN_CACHE_DIR). Run 'mogen auth login'."
            );
            std::process::exit(1);
        }
    };

    let bundle = match load_bundle(&path) {
        Ok(Some(b)) => b,
        Ok(None) => {
            println!("Not logged in. Run 'mogen auth login'.");
            std::process::exit(1);
        }
        Err(err) => {
            bail!("reading {}: {err}", path.display());
        }
    };

    let now = now_unix();
    let who = bundle.email.as_deref().unwrap_or("(unknown account)");
    let proj = bundle.project_id.as_deref().unwrap_or("(no project)");

    let line = if bundle.is_access_expired(now) {
        format!(
            "Logged in as {who}. Project {proj}. Access token expired; will refresh on next call."
        )
    } else {
        let remaining = bundle.access_expires_at_unix.saturating_sub(now);
        format!(
            "Logged in as {who}. Project {proj}. Access token expires in {}.",
            human_duration(remaining)
        )
    };
    println!("{line}");

    if verbose {
        println!("  token store: {}", path.display());
        if let Some(scope) = bundle.scope.as_deref() {
            println!("  scope: {scope}");
        }
        if let Some(endpoint) = bundle.endpoint_base.as_deref() {
            println!("  endpoint: {endpoint}");
        }
        if let Some(managed) = bundle.managed_project_id.as_deref() {
            println!("  managed project: {managed}");
        }
        if !std::env::var("GEMINI_API_KEY").unwrap_or_default().trim().is_empty() {
            println!(
                "  note: GEMINI_API_KEY is set — the resolver will use the API key, \
                 not OAuth, for Gemini calls."
            );
        }
    }

    Ok(())
}

fn logout() -> Result<()> {
    // Walk every path the resolver knows about so logout cleans up
    // canonical (`~/.mogen/`) AND legacy (`~/.cache/mogen/`,
    // `%LOCALAPPDATA%\mogen\`) tokens — otherwise a stale legacy file
    // would still authenticate after a "logout".
    let paths = all_existing_token_paths();
    if paths.is_empty() {
        println!("Not logged in; nothing to remove.");
        return Ok(());
    }
    let mut first_err: Option<anyhow::Error> = None;
    for path in &paths {
        match delete_bundle(path) {
            Ok(()) => println!("Removed {}.", path.display()),
            Err(err) => {
                let wrapped = anyhow::Error::new(err)
                    .context(format!("removing {}", path.display()));
                if first_err.is_none() {
                    first_err = Some(wrapped);
                } else {
                    eprintln!("warning: failed to remove {}", path.display());
                }
            }
        }
    }
    if let Some(err) = first_err {
        return Err(err);
    }
    Ok(())
}

fn login_anyhow(err: OAuthError) -> anyhow::Error {
    let hint = match &err {
        OAuthError::PortInUse => Some(
            "another `mogen auth login` may be running, or another process \
             is bound to localhost:51121",
        ),
        OAuthError::Timeout => Some(
            "no callback received within the timeout. Try again, or pass \
             --no-browser to copy the URL onto another machine",
        ),
        OAuthError::Revoked => Some(
            "stored credentials were revoked. Run `mogen auth login --force` \
             after re-authorising in the consent screen",
        ),
        OAuthError::MissingProject => Some(
            "no Google Cloud project came back from loadCodeAssist — your \
             account may not be enrolled. Set GEMINI_API_KEY as a fallback",
        ),
        OAuthError::MissingClientSecrets { .. } => Some(
            "your `oauth_client.json` override is unreadable; either fix the \
             file or remove it to fall back to the bundled Gemini CLI client \
             (see README \"Sign in with a paid Gemini account\")",
        ),
        _ => None,
    };
    match hint {
        Some(h) => anyhow::anyhow!("{err}\nhint: {h}"),
        None => anyhow::anyhow!("{err}"),
    }
}

fn now_unix() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn human_duration(secs: u64) -> String {
    if secs >= 3600 {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        format!("{h}h {m:02}m")
    } else if secs >= 60 {
        let m = secs / 60;
        let s = secs % 60;
        format!("{m}m {s:02}s")
    } else {
        format!("{secs}s")
    }
}

