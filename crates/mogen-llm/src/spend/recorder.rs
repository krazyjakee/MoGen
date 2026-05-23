//! [`SpendRecorder`] trait + default no-op implementation.
//!
//! Recorders are object-safe so the global slot can hold an `Arc<dyn
//! SpendRecorder>` without baking the concrete type into every consumer.
//! `record` is `&self` so the SQLite implementation can dispatch through a
//! channel internally without forcing every caller to hold a mutable
//! reference.

use super::{CallFilter, CallRecord, CallRow, ModelSummary, SummaryRow};

/// Persistent sink for [`CallRecord`]s. Implementations may queue, batch,
/// or write synchronously — callers should treat `record` as fire-and-forget.
pub trait SpendRecorder: Send + Sync + 'static {
    /// Persist one call. Implementations must not block the calling
    /// thread for an unbounded time — the SQLite backend ships off to a
    /// writer thread for exactly this reason.
    fn record(&self, record: CallRecord);

    /// Query historical calls matching `filter`. Returns most-recent-first
    /// (i.e. ORDER BY ts DESC) up to `filter.limit` rows. The default
    /// implementation returns an empty list so trivial recorders (the
    /// in-memory test stub) don't have to wire a read path.
    fn query(&self, _filter: &CallFilter) -> Vec<CallRow> {
        Vec::new()
    }

    /// Summary aggregates for `filter`: total spend, call count, tokens,
    /// images. Used by the Spending panel's summary row and per-file pill.
    fn summary(&self, _filter: &CallFilter) -> SummaryRow {
        SummaryRow::default()
    }

    /// Per-model breakdown for `filter`. Driven by the Spending panel's
    /// model legend.
    fn by_model(&self, _filter: &CallFilter) -> Vec<ModelSummary> {
        Vec::new()
    }

    /// Distinct list of (scene_path, model, operation) values present in
    /// the table — drives the Spending panel's filter combo boxes. The
    /// default returns empty so trivial backends don't have to implement
    /// this.
    fn distinct(&self) -> Distinct {
        Distinct::default()
    }

    /// Flush any pending writes. The SQLite backend uses this to wait for
    /// the background writer to drain — primarily a test affordance so
    /// `record` → `query` can be asserted deterministically.
    fn flush(&self) {}
}

/// Distinct set of values for each filterable column. Powers the dropdowns
/// in the Spending panel.
#[derive(Debug, Default, Clone)]
pub struct Distinct {
    pub scenes: Vec<String>,
    pub models: Vec<String>,
    pub operations: Vec<String>,
}

/// Recorder that drops every record. Default for processes that haven't
/// opted in to tracking, and the implementation tests use to assert the
/// crate compiles without a SQLite write path.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopRecorder;

impl SpendRecorder for NoopRecorder {
    fn record(&self, _record: CallRecord) {}
}
