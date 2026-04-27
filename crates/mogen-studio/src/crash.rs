//! Sentry/GlitchTip crash reporting bootstrap.
//!
//! The DSN points at a self-hosted GlitchTip instance, which speaks the same
//! ingestion protocol as Sentry. The returned guard must be held for the
//! lifetime of `main` — its `Drop` flushes any in-flight events before the
//! process exits.
//!
//! Honours `MOGEN_DISABLE_TELEMETRY=1` and the cross-tool `DO_NOT_TRACK=1`
//! convention so users can opt out without recompiling, plus the persisted
//! [`Settings::crash_reports_enabled`] consent flag — Sentry only initialises
//! when the user has explicitly opted in.

const DSN: &str = "https://fec046177fb54225805b1c011f783d5d@crash.daccord.gg/3";

/// True when the environment hard-disables telemetry. Independent of any
/// saved consent — env-level opt-out wins, so the first-launch prompt should
/// be skipped entirely on machines that set these.
pub fn telemetry_blocked_by_env() -> bool {
    std::env::var_os("MOGEN_DISABLE_TELEMETRY").is_some()
        || std::env::var_os("DO_NOT_TRACK").is_some()
}

/// Initialise crash reporting iff the env permits AND the user has opted in.
/// `consented` is the persisted [`Settings::crash_reports_enabled`]; `None`
/// (undecided) and `Some(false)` both skip Sentry. Saved consent only takes
/// effect from the next launch — there is no mid-session activation.
pub fn init(consented: Option<bool>) -> Option<sentry::ClientInitGuard> {
    if telemetry_blocked_by_env() {
        return None;
    }
    if consented != Some(true) {
        return None;
    }
    let environment = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    Some(sentry::init((
        DSN,
        sentry::ClientOptions {
            release: Some(format!("mogen-studio@{}", env!("CARGO_PKG_VERSION")).into()),
            environment: Some(environment.into()),
            attach_stacktrace: true,
            send_default_pii: false,
            ..Default::default()
        },
    )))
}
