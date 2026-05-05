//! Studio glue around `mogen_llm::google_oauth`. Owns:
//!
//! - `start_oauth_login_for`: spawns a background thread that runs the full
//!   loopback browser flow (PKCE, callback server, token exchange,
//!   `loadCodeAssist` project discovery) for the supplied [`ProviderConfig`]
//!   (gemini-cli for text gen, Antigravity for image gen), then ships the
//!   resulting bundle back over a channel.
//! - `poll_oauth_login`: drains that channel from the UI thread, persists
//!   the bundle to the right token file, and updates a short status string.
//! - `start_oauth_logout_for`: deletes the on-disk bundle for one provider.
//! - `oauth_stored_status_for`: synchronous read of one provider's bundle
//!   into a one-line "Logged in as <email> (expires in N min)" / "Not logged
//!   in" string suitable for Preferences.
//!
//! Studio surfaces both providers in Preferences so the user never has to
//! drop to the CLI to log in for image generation. The CLI's `mogen auth
//! login [--antigravity] | status | logout` use the same library functions,
//! so Studio and CLI converge on the same token files
//! (`google_auth.json` for gemini-cli, `antigravity_auth.json` for
//! Antigravity).

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use eframe::egui;
use mogen_llm::google_oauth::{ProviderConfig, ANTIGRAVITY_CONFIG, GEMINI_CLI_CONFIG};
use mogen_llm::{
    delete_bundle, load_bundle, run_login_flow, save_bundle, token_store_path_for,
    LoginOptions, OAuthBundle, OAuthError,
};

use super::MogenStudioApp;

impl MogenStudioApp {
    /// Drain the in-flight OAuth login channel, if any. On success, persists
    /// the bundle to the provider whose login was started; on failure,
    /// stores a short error string. Either outcome clears `oauth_login_rx`
    /// and `oauth_login_provider`.
    pub(super) fn poll_oauth_login(&mut self) {
        let Some(rx) = self.oauth_login_rx.as_ref() else {
            return;
        };
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.oauth_login_rx = None;
                self.oauth_login_provider = None;
                self.oauth_status_message =
                    Some("login worker exited unexpectedly".into());
                return;
            }
        };
        let provider = self.oauth_login_provider.unwrap_or(&GEMINI_CLI_CONFIG);
        let provider_label = provider_label(provider);
        self.oauth_login_rx = None;
        self.oauth_login_provider = None;
        match result {
            Ok(bundle) => {
                let email_line = bundle
                    .email
                    .as_deref()
                    .map(|e| format!("Logged in to {provider_label} as {e}"))
                    .unwrap_or_else(|| format!("Logged in to {provider_label}"));
                match token_store_path_for(provider) {
                    Some(path) => match save_bundle(&path, &bundle) {
                        Ok(()) => {
                            self.oauth_status_message = Some(email_line);
                        }
                        Err(e) => {
                            self.oauth_status_message = Some(format!(
                                "logged in but failed to save token: {e}"
                            ));
                        }
                    },
                    None => {
                        self.oauth_status_message = Some(
                            "logged in but no writable cache dir for token"
                                .into(),
                        );
                    }
                }
            }
            Err(e) => {
                self.oauth_status_message =
                    Some(format!("{provider_label} login failed: {e}"));
            }
        }
    }

    /// Spawn the OAuth login flow on a background thread for the supplied
    /// provider config. No-ops while a login is already in flight (either
    /// provider). The flow opens the user's default browser; if that fails
    /// the worker logs to stderr like the CLI does.
    pub(super) fn start_oauth_login_for(
        &mut self,
        ctx: egui::Context,
        config: &'static ProviderConfig,
    ) {
        if self.oauth_login_rx.is_some() {
            return;
        }
        self.oauth_status_message = Some(format!(
            "opening browser… ({})",
            provider_label(config)
        ));

        let (tx, rx) = mpsc::channel();
        self.oauth_login_rx = Some(rx);
        self.oauth_login_provider = Some(config);

        thread::spawn(move || {
            let opts = LoginOptions {
                open_browser: true,
                timeout: Duration::from_secs(300),
            };
            let result: Result<OAuthBundle, OAuthError> =
                run_login_flow(opts, config).map(|outcome| outcome.bundle);
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }

    /// Delete the on-disk OAuth bundle for `config`. Idempotent — missing
    /// file is a successful no-op (matches the CLI). Updates
    /// `oauth_status_message`.
    pub(super) fn start_oauth_logout_for(&mut self, config: &'static ProviderConfig) {
        let label = provider_label(config);
        match token_store_path_for(config) {
            Some(path) => match delete_bundle(&path) {
                Ok(()) => {
                    self.oauth_status_message =
                        Some(format!("Logged out of {label}"));
                }
                Err(e) => {
                    self.oauth_status_message =
                        Some(format!("{label} logout failed: {e}"));
                }
            },
            None => {
                self.oauth_status_message =
                    Some("no cache dir; nothing to delete".into());
            }
        }
    }

    /// Returns true while the loopback server is waiting for the OAuth
    /// callback (for either provider). Used by Preferences to disable both
    /// Login buttons during the flow so the user can't start a second one
    /// while the first is mid-handshake.
    pub(super) fn oauth_login_in_flight(&self) -> bool {
        self.oauth_login_rx.is_some()
    }

    /// Returns the in-flight provider when a login is running, else `None`.
    /// Lets Preferences show the spinner only on the section that actually
    /// initiated the flow.
    pub(super) fn oauth_login_in_flight_for(
        &self,
        config: &'static ProviderConfig,
    ) -> bool {
        match self.oauth_login_provider {
            Some(p) => std::ptr::eq(p, config) && self.oauth_login_rx.is_some(),
            None => false,
        }
    }

    /// Synchronously load `config`'s stored bundle (if any) and render a
    /// one-line status string. Cheap enough to call per frame from
    /// Preferences — `load_bundle` is a small JSON file. None if no bundle
    /// exists.
    pub(super) fn oauth_stored_status_for(
        &self,
        config: &'static ProviderConfig,
    ) -> Option<String> {
        let path = token_store_path_for(config)?;
        match load_bundle(&path).ok().flatten() {
            Some(b) => {
                let now = now_unix();
                let expires = b.access_expires_at_unix;
                let remaining = expires.saturating_sub(now);
                let suffix = if expires == 0 {
                    "expiry unknown".to_string()
                } else if remaining == 0 {
                    "access token expired (will refresh on next call)"
                        .to_string()
                } else {
                    format!("token valid {}", format_remaining(remaining))
                };
                let email = b.email.as_deref().unwrap_or("(unknown email)");
                let project = b.project_id.as_deref().unwrap_or("(no project)");
                Some(format!("Logged in as {email} · {project} · {suffix}"))
            }
            None => None,
        }
    }
}

/// Short human label for a provider, used in status messages.
fn provider_label(config: &'static ProviderConfig) -> &'static str {
    if std::ptr::eq(config, &ANTIGRAVITY_CONFIG) {
        "Antigravity"
    } else {
        "Google (gemini-cli)"
    }
}

/// Seconds-of-Unix-time helper, mirrors `mogen_llm::google_oauth::token`.
fn now_unix() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Render `seconds` as "HhMm" / "Mm Ss" / "Ss". Tight enough to fit on the
/// Preferences status line.
fn format_remaining(seconds: u64) -> String {
    let h = seconds / 3600;
    let m = (seconds % 3600) / 60;
    let s = seconds % 60;
    if h > 0 {
        format!("{h}h{m:02}m")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}
