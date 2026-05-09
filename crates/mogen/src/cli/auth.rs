//! `mogen auth …` subcommand surface. Three credential targets live here
//! (`gemini-cli`, `antigravity`, `moghub`) plus the conversion glue that
//! maps clap-derived enums into the [`AuthCmd`] variants the
//! [`crate::commands::auth`] dispatcher expects.

use clap::Subcommand;

use crate::commands::auth::{AuthCmd, AuthTarget, LoginCmd};

/// `mogen auth <target> <verb>` — one subcommand per credential
/// `mogen` knows how to persist. Each target keeps its own login flag
/// set (`--no-browser`/`--timeout` for Google's loopback, `--server`
/// for MoGHub) so the help text never advertises a flag that does
/// nothing on the target it's listed under.
#[derive(Subcommand)]
pub(crate) enum AuthArg {
    /// Print a one-line login status for every target at a glance.
    /// Exits 0 if any target is logged in, 1 otherwise. With
    /// `--verbose`, also dumps the per-target detail block underneath.
    Status {
        #[arg(long)]
        verbose: bool,
    },
    /// Manage Google's gemini-cli OAuth bundle (text generation via
    /// Cloud Code Assist `v1internal:generateContent`). The bundled
    /// `client_id` / `client_secret` are Google's public Gemini CLI
    /// values, so login is zero-config.
    GeminiCli {
        #[command(subcommand)]
        cmd: OauthVerb,
    },
    /// Manage Google's Antigravity OAuth bundle (image generation via
    /// the Cloud Code Assist `:streamGenerateContent` surface, which
    /// rejects the gemini-cli client). Required for OAuth-driven
    /// `mogen textures`.
    Antigravity {
        #[command(subcommand)]
        cmd: OauthVerb,
    },
    /// Manage the MoGHub session token (community publishing /
    /// authenticated browsing). Loopback browser flow against
    /// `<server>/api/auth/desktop/start`. The same `~/.mogen/
    /// moghub_auth.json` file is read by Studio, so logging in once
    /// covers both surfaces.
    Moghub {
        #[command(subcommand)]
        cmd: MoghubVerb,
    },
}

/// Verbs available for both Google OAuth targets. `--no-browser` and
/// `--timeout` only make sense for the loopback flow and are absent on
/// `MoghubVerb`.
#[derive(Subcommand)]
pub(crate) enum OauthVerb {
    /// Open Google sign-in in the browser and store the resulting
    /// token bundle. Idempotent — already-logged-in is a no-op
    /// without `--force`.
    Login {
        /// Re-authenticate even if a valid token is already stored.
        #[arg(long)]
        force: bool,
        /// Don't open the system browser. Print the authorize URL
        /// instead so you can open it on another machine (useful over
        /// SSH).
        #[arg(long)]
        no_browser: bool,
        /// How long to wait (seconds) for the OAuth callback before
        /// giving up. Clamped to [10, 3600].
        #[arg(long, default_value_t = 300)]
        timeout: u64,
    },
    /// Print the email + project the bundle resolves to, plus the
    /// access-token's remaining lifetime. Exits 0 when logged in
    /// (even if the access token is expired — refresh happens on next
    /// call), 1 when not logged in.
    Status {
        /// Also print the on-disk token-store path, OAuth scopes, the
        /// chosen `cloudcode-pa` endpoint, and a note when
        /// `GEMINI_API_KEY` shadows the OAuth credentials.
        #[arg(long)]
        verbose: bool,
    },
    /// Delete the stored token bundle. Idempotent — does not call the
    /// Google revoke endpoint, so the refresh token remains valid
    /// server-side until the user revokes consent at
    /// <https://myaccount.google.com>.
    Logout,
}

/// Verbs for the MoGHub session target. `--server` lets the user
/// authenticate against a self-hosted instance; the URL round-trips
/// into the on-disk token so future `status` calls reach the same
/// host.
#[derive(Subcommand)]
pub(crate) enum MoghubVerb {
    Login {
        /// Re-authenticate even if a session is already on disk.
        #[arg(long)]
        force: bool,
        /// MoGHub instance to sign in against. Defaults to the
        /// production server.
        #[arg(long)]
        server: Option<String>,
    },
    Status {
        /// Also call `whoami` against the server to confirm the
        /// stored token is still accepted.
        #[arg(long)]
        verbose: bool,
    },
    Logout,
}

impl From<AuthArg> for AuthCmd {
    fn from(a: AuthArg) -> Self {
        match a {
            AuthArg::Status { verbose } => AuthCmd::Status { target: None, verbose },
            AuthArg::GeminiCli { cmd } => convert_oauth(AuthTarget::GeminiCli, cmd),
            AuthArg::Antigravity { cmd } => convert_oauth(AuthTarget::Antigravity, cmd),
            AuthArg::Moghub { cmd } => convert_moghub(cmd),
        }
    }
}

fn convert_oauth(target: AuthTarget, verb: OauthVerb) -> AuthCmd {
    match verb {
        OauthVerb::Login { force, no_browser, timeout } => {
            let inner = match target {
                AuthTarget::GeminiCli => LoginCmd::GeminiCli {
                    force,
                    no_browser,
                    timeout_secs: timeout,
                },
                AuthTarget::Antigravity => LoginCmd::Antigravity {
                    force,
                    no_browser,
                    timeout_secs: timeout,
                },
                AuthTarget::Moghub => unreachable!("OauthVerb only feeds OAuth targets"),
            };
            AuthCmd::Login(inner)
        }
        OauthVerb::Status { verbose } => AuthCmd::Status {
            target: Some(target),
            verbose,
        },
        OauthVerb::Logout => AuthCmd::Logout { target },
    }
}

fn convert_moghub(verb: MoghubVerb) -> AuthCmd {
    match verb {
        MoghubVerb::Login { force, server } => {
            AuthCmd::Login(LoginCmd::Moghub { force, server })
        }
        MoghubVerb::Status { verbose } => AuthCmd::Status {
            target: Some(AuthTarget::Moghub),
            verbose,
        },
        MoghubVerb::Logout => AuthCmd::Logout {
            target: AuthTarget::Moghub,
        },
    }
}
