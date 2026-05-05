//! `mogen login` — drive the loopback OAuth flow against MoGHub and
//! persist the resulting session UUID into shared storage so subsequent
//! `mogen publish` calls (and Studio sign-in) see the same session.
//!
//! Re-uses [`mogen_moghub_client::oauth::run_loopback_flow`] — the same
//! function Studio uses for its in-app sign-in, so behaviour stays in
//! lockstep across surfaces.

use anyhow::{anyhow, Result};
use mogen_moghub_client::{oauth, MoghubClient, MoghubError};

use crate::commands::auth::{clear_session, load_session, store_session, StorageTier};

pub(crate) struct LoginArgs {
    /// MoGHub base URL. Falls back to `MOGHUB_URL` env, then the
    /// client's compile-time default (production).
    pub(crate) base_url: Option<String>,
    /// Force a fresh sign-in even if a token is already stored.
    pub(crate) force: bool,
    /// Sign out: remove the stored token from every storage tier and
    /// exit. Skips the OAuth flow entirely.
    pub(crate) logout: bool,
}

pub(crate) fn login(args: LoginArgs) -> Result<()> {
    if args.logout {
        clear_session()?;
        println!("Signed out of MoGHub.");
        return Ok(());
    }

    let base_url = pick_base_url(args.base_url.as_deref());

    if !args.force {
        if let Some(existing) = load_session() {
            // If the existing token still validates, short-circuit and
            // tell the user who they are. Otherwise keep going and run
            // the OAuth flow as if `--force` had been passed.
            if let Some(handle) = whoami_handle(&base_url, &existing) {
                println!("Already signed in as @{handle}.");
                return Ok(());
            }
        }
    }

    println!("Opening browser to authenticate at {base_url}…");
    let token = oauth::run_loopback_flow(&base_url)
        .map_err(|e| anyhow!("OAuth flow failed: {e}"))?;
    let tier = store_session(&token)?;

    let handle = whoami_handle(&base_url, &token).unwrap_or_else(|| "<unknown>".into());
    match tier {
        StorageTier::Keyring => {
            println!("Signed in as @{handle}. Token stored in the OS keyring.");
        }
        StorageTier::File(path) => {
            println!(
                "Signed in as @{handle}. Token stored at {} \
                 (no keyring backend available).",
                path.display()
            );
        }
    }
    Ok(())
}

fn pick_base_url(explicit: Option<&str>) -> String {
    if let Some(s) = explicit {
        return s.to_string();
    }
    if let Ok(env) = std::env::var("MOGHUB_URL") {
        let env = env.trim();
        if !env.is_empty() {
            return env.to_string();
        }
    }
    mogen_moghub_client::DEFAULT_BASE_URL.to_string()
}

fn whoami_handle(base_url: &str, token: &str) -> Option<String> {
    let client = MoghubClient::new(base_url).ok()?.with_token(Some(token.to_string()));
    match client.whoami() {
        Ok(w) => w.user.map(|u| u.handle),
        Err(MoghubError::Unauthorized) => None,
        Err(_) => None,
    }
}
