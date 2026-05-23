//! Persistent LLM/image spend tracking.
//!
//! Every text and image call that flows through `mogen-llm` is recorded to a
//! local SQLite database at `~/.mogen/spend.db`. The schema is versioned
//! (see [`migrate`]) and the writer runs on a background thread so call
//! sites never pay any I/O latency.
//!
//! ## Wiring
//!
//! - [`SpendRecorder`] is the entry-point trait. Provider clients call
//!   [`record`] after a successful (or failed-with-usage) response; the
//!   global recorder installed by the host process (CLI or Studio) handles
//!   persistence. With no recorder installed every call is a no-op, so the
//!   library is safe to use without a database.
//! - [`SqliteRecorder`] is the default implementation. It spawns a writer
//!   thread on construction and ships records over an mpsc channel.
//! - [`install_global`] installs a recorder process-wide. Idempotent — the
//!   second installer is silently ignored so unit tests that build their own
//!   recorder don't fight the CLI's default.
//!
//! ## Pricing
//!
//! Costs are computed against rows from the `pricing` table at the time the
//! call lands. The table ships with baseline rates for every model `mogen-llm`
//! talks to (see [`pricing::SEED`]); the Studio UI can override them. Pricing
//! rows are effective-dated so editing today's price doesn't rewrite history.

pub mod pricing;
pub mod recorder;
pub mod sqlite;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

pub use pricing::{
    compute_cost, image_price_for_model, text_price_for_model, ImagePricing, PriceTier,
    TextPricing,
};
pub use recorder::{Distinct, NoopRecorder, SpendRecorder};
pub use sqlite::{
    db_path, open as open_sqlite, query_calls, summarize, CallFilter, CallRow, ModelSummary,
    SqliteRecorder, SummaryRow,
};

use crate::types::Usage;

/// Stable tag for the type of LLM work that produced a record. Carried on
/// [`CallRecord::operation`] so the Spending panel can split text-LLM vs
/// image-gen, and so per-operation aggregates ("how much did texture
/// generation cost me?") stay coherent across providers.
///
/// Lowercase, snake-case strings — they're stored as TEXT in the DB so any
/// new operation name can be added without a schema migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    /// Initial `mogen generate` / Studio "New from Prompt" turn.
    Generate,
    /// `mogen modify` / Studio Modify — incremental edit of an existing scene.
    Modify,
    /// Repair iteration inside the validator → LLM feedback loop.
    Repair,
    /// Texture-pipeline image generation (per-material albedo).
    Textures,
    /// Local PBR map derivation. Recorded for completeness even though the
    /// cost is zero — keeps the call count honest in the Spending panel.
    PbrMaps,
    /// Studio "Ask MoGen" Q&A modal.
    Ask,
    /// Studio prompt enhancer (the small "polish my prompt" button).
    Enhance,
    /// Architect / planner pre-pass before the coder turn.
    Plan,
    /// Visual-refine review pass (renders → critic → edits).
    Refine,
    /// Animation-clip generation.
    Animate,
    /// Catch-all for anything not covered above. Carries a free-form
    /// label so future flows don't need a code change in this crate to be
    /// tracked.
    Other(&'static str),
}

impl Operation {
    pub fn as_str(&self) -> &str {
        match self {
            Operation::Generate => "generate",
            Operation::Modify => "modify",
            Operation::Repair => "repair",
            Operation::Textures => "textures",
            Operation::PbrMaps => "pbr_maps",
            Operation::Ask => "ask",
            Operation::Enhance => "enhance",
            Operation::Plan => "plan",
            Operation::Refine => "refine",
            Operation::Animate => "animate",
            Operation::Other(s) => s,
        }
    }
}

/// One persisted call. Mirrors the `calls` table schema. Created by
/// provider call sites (or higher-level entry points) and shipped to the
/// installed [`SpendRecorder`].
#[derive(Debug, Clone)]
pub struct CallRecord {
    /// Unix timestamp (seconds). `0` means "use the current wall clock at
    /// record time" — [`SqliteRecorder`] backfills this so callers don't
    /// have to.
    pub ts: i64,
    /// Provider key, e.g. `"gemini"` / `"openai"`. Lowercased.
    pub provider: String,
    /// Model id as the provider saw it. Stored verbatim so the pricing
    /// match can fingerprint variant tags (e.g. `gemini-3.1-pro-preview`).
    pub model: String,
    /// Operation tag — see [`Operation`].
    pub operation: String,
    pub prompt_tokens: u32,
    pub response_tokens: u32,
    pub cached_tokens: u32,
    /// For image-gen calls: number of images returned. Zero for text-only
    /// calls.
    pub image_count: u32,
    /// Computed USD cost. Backfilled by [`SqliteRecorder::record`] using
    /// [`compute_cost`] when zero; passing a non-zero value here lets the
    /// caller override (e.g. for negotiated rates that don't live in the
    /// pricing table yet).
    pub cost_usd: f64,
    /// Absolute path of the scene this call was attributed to. `None` for
    /// calls that aren't tied to a specific file (e.g. a bench run, or the
    /// Studio Ask modal on an untitled buffer).
    pub scene_path: Option<String>,
    /// UUID-shaped string identifying the parent session. Lets the UI
    /// surface "today's session" vs lifetime totals. Optional — CLI
    /// invocations may use a fresh per-call id, or leave it blank.
    pub session_id: Option<String>,
    /// True when the call returned usable usage data. `false` carries a
    /// failed-with-usage record (e.g. budget-exceeded after the API
    /// reported tokens) so the meter doesn't undercount the bill.
    pub success: bool,
    /// Free-form notes (the model's error message, retry index, etc.).
    /// Surfaced in the panel's per-call detail row, never indexed.
    pub notes: Option<String>,
}

impl CallRecord {
    /// Build a text-call record from the provider response. `provider` and
    /// `model` come from the [`crate::LlmClient`] dispatcher; `ctx` carries
    /// the caller-supplied operation and scene attribution from
    /// [`crate::types::GenerateConfig`].
    pub fn from_text(
        provider: &str,
        model: &str,
        usage: &Usage,
        ctx: &CallContext,
        success: bool,
        notes: Option<String>,
    ) -> Self {
        Self {
            ts: now_unix(),
            provider: provider.to_string(),
            model: model.to_string(),
            operation: ctx.operation.to_string(),
            prompt_tokens: usage.prompt_tokens,
            response_tokens: usage.response_tokens,
            cached_tokens: usage.cached_tokens,
            image_count: 0,
            cost_usd: 0.0,
            scene_path: ctx.scene_path.clone(),
            session_id: ctx.session_id.clone(),
            success,
            notes,
        }
    }

    /// Build an image-call record. The image surfaces don't return token
    /// counts, so cost is computed per-image from [`ImagePricing`].
    pub fn from_image(
        provider: &str,
        model: &str,
        image_count: u32,
        ctx: &CallContext,
        success: bool,
        notes: Option<String>,
    ) -> Self {
        Self {
            ts: now_unix(),
            provider: provider.to_string(),
            model: model.to_string(),
            operation: ctx.operation.to_string(),
            prompt_tokens: 0,
            response_tokens: 0,
            cached_tokens: 0,
            image_count,
            cost_usd: 0.0,
            scene_path: ctx.scene_path.clone(),
            session_id: ctx.session_id.clone(),
            success,
            notes,
        }
    }
}

/// Contextual attribution carried with a [`GenerateConfig`][crate::GenerateConfig]
/// or `ImageClient::generate_image` call so the recorder can answer
/// "how much have I spent on file X?" and "how much went to texture
/// generation?".
///
/// All fields are optional; the default `("other", None, None)` lands every
/// untagged call in a single bucket without a panic.
#[derive(Debug, Clone, Default)]
pub struct CallContext {
    /// Operation tag — `"generate"`, `"repair"`, `"textures"`, etc. See
    /// [`Operation`].
    pub operation: String,
    /// Absolute path of the `.mog` (or built `.glb`) this call is for.
    pub scene_path: Option<String>,
    /// UUID-shaped string identifying the parent session.
    pub session_id: Option<String>,
}

impl CallContext {
    pub fn new(operation: Operation) -> Self {
        Self {
            operation: operation.as_str().to_string(),
            scene_path: None,
            session_id: None,
        }
    }

    pub fn with_scene(mut self, path: impl Into<String>) -> Self {
        let s = path.into();
        if !s.is_empty() {
            self.scene_path = Some(s);
        }
        self
    }

    pub fn with_session(mut self, session: impl Into<String>) -> Self {
        let s = session.into();
        if !s.is_empty() {
            self.session_id = Some(s);
        }
        self
    }

    /// True when no operation tag has been set — i.e. the call site is
    /// recording an untagged event. Used by the recorder to skip dispatch
    /// when the caller explicitly disabled tracking (default-constructed
    /// `CallContext::default()` yields `true` here).
    pub fn is_empty(&self) -> bool {
        self.operation.is_empty()
    }
}

/// Global recorder slot. `OnceLock` so installation is one-shot — the
/// first caller (CLI or Studio main) wins and subsequent installers no-op.
/// `None` means tracking is disabled for this process, and [`record`] is
/// a no-op.
static GLOBAL_RECORDER: OnceLock<Arc<dyn SpendRecorder>> = OnceLock::new();

/// Install the process-wide spend recorder. Idempotent: only the first
/// successful install takes; subsequent calls return `Err` with the
/// existing recorder unchanged. Pass an [`Arc<SqliteRecorder>`][SqliteRecorder]
/// for the standard SQLite-backed implementation, or any custom
/// [`SpendRecorder`] (tests use [`NoopRecorder`]).
pub fn install_global(recorder: Arc<dyn SpendRecorder>) -> Result<(), &'static str> {
    GLOBAL_RECORDER
        .set(recorder)
        .map_err(|_| "spend recorder already installed")
}

/// Borrow the installed recorder. Used by the Studio panel to query
/// historical data via the same handle the writer side uses, so a single
/// `SqliteRecorder::open` covers both read and write paths.
pub fn global() -> Option<Arc<dyn SpendRecorder>> {
    GLOBAL_RECORDER.get().cloned()
}

/// Record a call. The recorder's `record` is fire-and-forget on the
/// SQLite implementation — it queues to a writer thread and returns
/// immediately so the call site never blocks on disk I/O. With no
/// recorder installed this is a no-op.
pub fn record(record: CallRecord) {
    if record.operation.is_empty() {
        // Untagged call (default-constructed CallContext) — caller opted out.
        return;
    }
    if let Some(r) = GLOBAL_RECORDER.get() {
        r.record(record);
    }
}

/// Canonical `~/.mogen/spend.db` path. Returns `None` only when the
/// platform exposes neither `HOME`/`USERPROFILE` nor `LOCALAPPDATA`. The
/// `MOGEN_SPEND_DB` env var overrides the full path; `MOGEN_CACHE_DIR`
/// overrides the parent directory.
pub fn default_db_path() -> Option<PathBuf> {
    crate::google_oauth::client::resolve_user_path(
        "spend.db",
        "MOGEN_SPEND_DB",
        crate::google_oauth::client::PathMode::Write,
    )
}

pub(super) fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_round_trips_through_string() {
        assert_eq!(Operation::Generate.as_str(), "generate");
        assert_eq!(Operation::Textures.as_str(), "textures");
        assert_eq!(Operation::Other("wizard").as_str(), "wizard");
    }

    #[test]
    fn call_context_default_is_empty() {
        let ctx = CallContext::default();
        assert!(ctx.is_empty());
    }

    #[test]
    fn call_context_builder() {
        let ctx = CallContext::new(Operation::Repair)
            .with_scene("/tmp/chair.mog")
            .with_session("abcd");
        assert_eq!(ctx.operation, "repair");
        assert_eq!(ctx.scene_path.as_deref(), Some("/tmp/chair.mog"));
        assert_eq!(ctx.session_id.as_deref(), Some("abcd"));
    }

    #[test]
    fn record_skips_when_operation_blank() {
        // No global recorder installed in tests by default — this just
        // exercises the early-return path so a missing install can't panic.
        record(CallRecord {
            ts: 0,
            provider: "x".into(),
            model: "y".into(),
            operation: String::new(),
            prompt_tokens: 0,
            response_tokens: 0,
            cached_tokens: 0,
            image_count: 0,
            cost_usd: 0.0,
            scene_path: None,
            session_id: None,
            success: true,
            notes: None,
        });
    }
}
