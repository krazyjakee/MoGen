//! `mogen auth` — sign-in management for every credential `mogen`
//! persists under `~/.mogen/`.
//!
//! Targets, each independently logged-in/out:
//!
//! - `gemini-cli` — Google's gemini-cli OAuth desktop client. Bearer
//!   token authorises Cloud Code Assist (`cloudcode-pa.googleapis.com/
//!   v1internal:generateContent`) so a paid Pro account can run
//!   `mogen generate` against `gemini-3-pro-preview` without an API
//!   key. Token at `~/.mogen/google_auth.json`.
//! - `antigravity` — Google's Antigravity OAuth desktop client. Same
//!   surface, different consent screen — required for image
//!   generation (the gemini-cli client gets 403 on
//!   `:streamGenerateContent`). Token at
//!   `~/.mogen/antigravity_auth.json`.
//! - `moghub` — MoGHub community session UUID, returned by the
//!   loopback OAuth flow against `<moghub>/api/auth/desktop/start`.
//!   Token at `~/.mogen/moghub_auth.json`. Studio reads this same
//!   file, so logging in once via the CLI surfaces the session in
//!   Studio (and vice versa).
//!
//! Top-level `mogen auth status` (no target) prints a one-line
//! summary for each target so the user can see at a glance which
//! credentials are on disk.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use mogen_llm::google_oauth::{ProviderConfig, ANTIGRAVITY_CONFIG, GEMINI_CLI_CONFIG};
use mogen_llm::{
    all_existing_token_paths_for, delete_bundle, load_bundle, run_login_flow, save_bundle,
    token_store_path_for, token_store_write_path_for, LoginOptions, OAuthError,
};
use mogen_moghub_client::session_store as moghub_session;
use mogen_moghub_client::DEFAULT_BASE_URL as MOGHUB_DEFAULT_BASE_URL;

/// Closed set of auth targets. Each variant maps to one on-disk
/// credential file under `~/.mogen/`.
#[derive(Clone, Copy, Debug)]
pub(crate) enum AuthTarget {
    GeminiCli,
    Antigravity,
    Moghub,
}

/// Subcommand surface mirrored from `clap` in `main.rs`. `Login`'s
/// shape varies per target so it carries a sub-enum rather than a
/// flat field set.
pub(crate) enum AuthCmd {
    Login(LoginCmd),
    Status {
        /// `None` = print status for every target.
        target: Option<AuthTarget>,
        verbose: bool,
    },
    Logout {
        target: AuthTarget,
    },
}

pub(crate) enum LoginCmd {
    GeminiCli {
        force: bool,
        no_browser: bool,
        timeout_secs: u64,
    },
    Antigravity {
        force: bool,
        no_browser: bool,
        timeout_secs: u64,
    },
    Moghub {
        force: bool,
        server: Option<String>,
    },
}

pub(crate) fn dispatch(cmd: AuthCmd) -> Result<()> {
    match cmd {
        AuthCmd::Login(login) => match login {
            LoginCmd::GeminiCli { force, no_browser, timeout_secs } => {
                oauth_login(force, no_browser, timeout_secs, &GEMINI_CLI_CONFIG)
            }
            LoginCmd::Antigravity { force, no_browser, timeout_secs } => {
                oauth_login(force, no_browser, timeout_secs, &ANTIGRAVITY_CONFIG)
            }
            LoginCmd::Moghub { force, server } => moghub_login(force, server),
        },
        AuthCmd::Status { target: Some(t), verbose } => match t {
            AuthTarget::GeminiCli => oauth_status(verbose, &GEMINI_CLI_CONFIG),
            AuthTarget::Antigravity => oauth_status(verbose, &ANTIGRAVITY_CONFIG),
            AuthTarget::Moghub => moghub_status(verbose),
        },
        AuthCmd::Status { target: None, verbose } => status_all(verbose),
        AuthCmd::Logout { target } => match target {
            AuthTarget::GeminiCli => oauth_logout(&GEMINI_CLI_CONFIG),
            AuthTarget::Antigravity => oauth_logout(&ANTIGRAVITY_CONFIG),
            AuthTarget::Moghub => moghub_logout(),
        },
    }
}

// --- Google OAuth (gemini-cli + antigravity) -------------------------

fn oauth_login(
    force: bool,
    no_browser: bool,
    timeout_secs: u64,
    config: &'static ProviderConfig,
) -> Result<()> {
    let read_path = token_store_path_for(config)
        .context("cannot determine token store location (set MOGEN_CACHE_DIR)")?;
    let write_path = token_store_write_path_for(config)
        .context("cannot determine token store write location (set MOGEN_CACHE_DIR)")?;

    if !force {
        if let Some(existing) = load_bundle(&read_path)
            .with_context(|| format!("reading {}", read_path.display()))?
        {
            let who = existing.email.as_deref().unwrap_or("(unknown account)");
            let proj = existing.project_id.as_deref().unwrap_or("(no project)");
            println!(
                "Already logged in as {who} (project {proj}). Use --force to re-authenticate."
            );
            return Ok(());
        }
    }

    eprintln!(
        "mogen auth ({}): opening Google sign-in. If sign-in fails, set \
         GEMINI_API_KEY as a fallback (see README).",
        provider_label(config),
    );

    let opts = LoginOptions {
        open_browser: !no_browser,
        timeout: Duration::from_secs(timeout_secs.clamp(10, 3600)),
    };

    let outcome = match run_login_flow(opts, config) {
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

    if read_path != write_path && read_path.exists() {
        let _ = delete_bundle(&read_path);
    }

    let who = outcome.bundle.email.as_deref().unwrap_or("(unknown account)");
    let proj = outcome.bundle.project_id.as_deref().unwrap_or("(no project)");
    println!(
        "Logged in as {who}. Project: {proj}. Token stored at {}.",
        write_path.display()
    );
    Ok(())
}

fn oauth_status(verbose: bool, config: &'static ProviderConfig) -> Result<()> {
    let label = provider_label(config);
    let login_hint = format!("mogen auth {label} login");
    let path = match token_store_path_for(config) {
        Some(p) => p,
        None => {
            println!(
                "Not logged in to {label} (no token store path resolvable; set MOGEN_CACHE_DIR). \
                 Run '{login_hint}'."
            );
            std::process::exit(1);
        }
    };

    let bundle = match load_bundle(&path) {
        Ok(Some(b)) => b,
        Ok(None) => {
            println!("Not logged in to {label}. Run '{login_hint}'.");
            std::process::exit(1);
        }
        Err(err) => bail!("reading {}: {err}", path.display()),
    };

    let now = now_unix();
    let who = bundle.email.as_deref().unwrap_or("(unknown account)");
    let proj = bundle.project_id.as_deref().unwrap_or("(no project)");

    let line = if bundle.is_access_expired(now) {
        format!(
            "Logged in to {label} as {who}. Project {proj}. Access token expired; will refresh on next call."
        )
    } else {
        let remaining = bundle.access_expires_at_unix.saturating_sub(now);
        format!(
            "Logged in to {label} as {who}. Project {proj}. Access token expires in {}.",
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

fn oauth_logout(config: &'static ProviderConfig) -> Result<()> {
    let label = provider_label(config);
    let paths = all_existing_token_paths_for(config);
    if paths.is_empty() {
        println!("Not logged in to {label}; nothing to remove.");
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

fn provider_label(config: &ProviderConfig) -> &'static str {
    if std::ptr::eq(config, &ANTIGRAVITY_CONFIG) {
        "antigravity"
    } else {
        "gemini-cli"
    }
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
            "stored credentials were revoked. Run `mogen auth gemini-cli login --force` \
             after re-authorising in the consent screen",
        ),
        OAuthError::MissingProject => Some(
            "no Google Cloud project came back from loadCodeAssist — your \
             account may not be enrolled. Set GEMINI_API_KEY as a fallback",
        ),
        _ => None,
    };
    match hint {
        Some(h) => anyhow::anyhow!("{err}\nhint: {h}"),
        None => anyhow::anyhow!("{err}"),
    }
}

// --- MoGHub session --------------------------------------------------

fn moghub_login(force: bool, server: Option<String>) -> Result<()> {
    if !force {
        if let Some(existing) = moghub_session::read_session() {
            let stored_url = moghub_session::read_base_url()
                .unwrap_or_else(|| MOGHUB_DEFAULT_BASE_URL.to_string());
            println!(
                "Already logged in to moghub at {stored_url} (token …{}). \
                 Use --force to sign in again.",
                tail(&existing, 6),
            );
            return Ok(());
        }
    }

    let base_url = server.unwrap_or_else(|| MOGHUB_DEFAULT_BASE_URL.to_string());
    eprintln!("mogen auth (moghub): opening browser sign-in at {base_url} …");

    let token = mogen_moghub_client::oauth::run_loopback_flow(&base_url)
        .map_err(|e| anyhow::anyhow!("moghub sign-in failed: {e}"))?;

    moghub_session::save_session(&token, Some(&base_url))
        .with_context(|| "saving moghub session token")?;

    let path = moghub_session::session_path(moghub_session::PathMode::Write)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "(unknown path)".into());
    println!("Logged in to moghub at {base_url}. Session stored at {path}.");
    Ok(())
}

fn moghub_status(verbose: bool) -> Result<()> {
    let Some(token) = moghub_session::read_session() else {
        println!("Not logged in to moghub. Run 'mogen auth moghub login'.");
        std::process::exit(1);
    };
    let base_url =
        moghub_session::read_base_url().unwrap_or_else(|| MOGHUB_DEFAULT_BASE_URL.to_string());
    println!("Logged in to moghub at {base_url} (token …{}).", tail(&token, 6));

    if verbose {
        let path = moghub_session::session_path(moghub_session::PathMode::Read)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(unknown path)".into());
        println!("  session store: {path}");

        match mogen_moghub_client::MoghubClient::new(&base_url) {
            Ok(client) => match client.with_token(Some(token)).whoami() {
                Ok(whoami) => match whoami.user {
                    Some(u) => println!("  whoami: {} (id {})", u.handle, u.id),
                    None => println!("  whoami: anonymous (token rejected by server)"),
                },
                Err(err) => println!("  whoami: error talking to {base_url}: {err}"),
            },
            Err(err) => println!("  whoami: invalid base url {base_url}: {err}"),
        }
    }

    Ok(())
}

fn moghub_logout() -> Result<()> {
    let paths = moghub_session::all_existing_session_paths();
    if paths.is_empty() {
        println!("Not logged in to moghub; nothing to remove.");
        return Ok(());
    }
    moghub_session::clear_session().context("removing moghub session file(s)")?;
    for path in paths {
        println!("Removed {}.", path.display());
    }
    Ok(())
}

fn tail(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= n {
        chars.iter().collect()
    } else {
        chars[chars.len() - n..].iter().collect()
    }
}

// --- combined "all targets" status -----------------------------------

fn status_all(verbose: bool) -> Result<()> {
    // Print a one-line summary per target, then the verbose detail
    // block underneath when requested. We deliberately don't `exit(1)`
    // for any individual target — the combined view exits 0 when the
    // user has run *anything*; per-target exit codes still apply when
    // a target is named explicitly.
    let mut any_logged_in = false;

    for (label, line) in [
        ("gemini-cli", oauth_summary(&GEMINI_CLI_CONFIG)),
        ("antigravity", oauth_summary(&ANTIGRAVITY_CONFIG)),
        ("moghub", moghub_summary()),
    ] {
        let (logged_in, summary) = line;
        if logged_in {
            any_logged_in = true;
        }
        println!("{label:<12} {summary}");
        let _ = label;
    }

    if verbose {
        println!();
        let _ = oauth_status(true, &GEMINI_CLI_CONFIG);
        println!();
        let _ = oauth_status(true, &ANTIGRAVITY_CONFIG);
        println!();
        let _ = moghub_status(true);
    }

    if !any_logged_in {
        std::process::exit(1);
    }
    Ok(())
}

fn oauth_summary(config: &'static ProviderConfig) -> (bool, String) {
    let Some(path) = token_store_path_for(config) else {
        return (false, "no token store path resolvable".into());
    };
    match load_bundle(&path) {
        Ok(Some(bundle)) => {
            let who = bundle.email.as_deref().unwrap_or("(unknown account)");
            let proj = bundle.project_id.as_deref().unwrap_or("(no project)");
            (true, format!("logged in as {who} (project {proj})"))
        }
        Ok(None) => (false, "not logged in".into()),
        Err(err) => (false, format!("error reading {}: {err}", path.display())),
    }
}

fn moghub_summary() -> (bool, String) {
    match moghub_session::read_session() {
        Some(token) => {
            let url = moghub_session::read_base_url()
                .unwrap_or_else(|| MOGHUB_DEFAULT_BASE_URL.to_string());
            (true, format!("logged in at {url} (token …{})", tail(&token, 6)))
        }
        None => (false, "not logged in".into()),
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
