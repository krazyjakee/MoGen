//! Sentry/GlitchTip crash reporting bootstrap.
//!
//! The DSN points at a self-hosted GlitchTip instance, which speaks the same
//! ingestion protocol as Sentry. The returned guard must be held for the
//! lifetime of `main` — its `Drop` flushes any in-flight events before the
//! process exits.
//!
//! Honours `MOGEN_DISABLE_TELEMETRY=1` and the cross-tool `DO_NOT_TRACK=1`
//! convention so users can opt out without recompiling.

const DSN: &str = "https://fec046177fb54225805b1c011f783d5d@crash.daccord.gg/3";

pub fn init() -> Option<sentry::ClientInitGuard> {
    if std::env::var_os("MOGEN_DISABLE_TELEMETRY").is_some()
        || std::env::var_os("DO_NOT_TRACK").is_some()
    {
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
