//! SQLite-backed [`SpendRecorder`] implementation.
//!
//! ## Layout
//!
//! ```text
//! ~/.mogen/spend.db
//!   schema_version (version INTEGER PRIMARY KEY, applied_at INTEGER)
//!   calls          (id, ts, provider, model, operation, prompt_tokens,
//!                   response_tokens, cached_tokens, image_count, cost_usd,
//!                   scene_path, session_id, success, notes)
//!   pricing        (id, provider, model, input_per_mtok_usd,
//!                   cached_input_per_mtok_usd, output_per_mtok_usd,
//!                   image_per_unit_usd, long_context_threshold,
//!                   input_per_mtok_long_usd, output_per_mtok_long_usd,
//!                   cached_input_per_mtok_long_usd, effective_from,
//!                   effective_to)
//! ```
//!
//! Migrations live in [`schema::migrate`] and run inside a transaction; each
//! step bumps `schema_version`. Adding a column means appending a new step —
//! never editing an existing one.
//!
//! ## Concurrency
//!
//! The recorder spawns one writer thread on construction; record requests
//! cross an mpsc channel so call sites never block on disk I/O. Reads
//! (`query`, `summary`, `by_model`, `distinct`) open a fresh connection
//! on the calling thread — SQLite handles cross-connection visibility via
//! its built-in journal once the writer commits.
//!
//! The implementation is split by concern: [`schema`] (DB open + migration),
//! [`recorder`] (the async writer + trait impl), [`pricing`] (cost
//! computation + the pricing-table editor helpers), and [`query`] (read-side
//! filters + aggregates). The public surface is re-exported flat below so
//! callers keep using `sqlite::open`, `sqlite::SqliteRecorder`, etc.

mod pricing;
mod query;
mod recorder;
mod schema;

pub use pricing::{list_pricing, upsert_pricing, PricingRow, _seed_row_for};
pub use query::{group_by_model, query_calls, summarize, CallFilter, CallRow, ModelSummary, SummaryRow};
pub use recorder::SqliteRecorder;
pub use schema::{db_path, open, CURRENT_SCHEMA_VERSION};

/// Thin wrapper over [`super::now_unix`] so the submodules can call the
/// shorter `super::now_unix()` regardless of where the canonical clock lives.
fn now_unix() -> i64 {
    super::now_unix()
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::pricing::lookup_pricing;
    use crate::spend::pricing::{ImagePricing, TextPricing};
    use crate::spend::recorder::SpendRecorder;
    use crate::spend::{CallContext, CallRecord, Operation};
    use crate::types::Usage;
    use rusqlite::{params, Connection};

    fn tmpdb() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn open_creates_schema_and_seeds_pricing() {
        let dir = tmpdb();
        let path = dir.path().join("spend.db");
        let conn = open(&path).expect("open");
        let v: i32 = conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, CURRENT_SCHEMA_VERSION);
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM pricing", [], |r| r.get(0))
            .unwrap();
        assert!(n > 0, "pricing should be seeded");
        // The Gemini Pro row should land.
        let cnt: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pricing WHERE model = 'gemini-pro-latest'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cnt, 1);
    }

    #[test]
    fn open_is_idempotent() {
        let dir = tmpdb();
        let path = dir.path().join("spend.db");
        let _ = open(&path).expect("open 1");
        let _ = open(&path).expect("open 2");
        let conn = Connection::open(&path).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "migration should not re-run");
        // Pricing seed shouldn't double up either.
        let p: i64 = conn
            .query_row("SELECT COUNT(*) FROM pricing", [], |r| r.get(0))
            .unwrap();
        assert!(p > 0);
        let pp: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pricing WHERE model = 'gemini-pro-latest'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pp, 1);
    }

    #[test]
    fn record_and_query_round_trip() {
        let dir = tmpdb();
        let path = dir.path().join("spend.db");
        let rec = SqliteRecorder::open(path).expect("open recorder");
        let ctx = CallContext::new(Operation::Generate).with_scene("/tmp/chair.mog");
        let usage = Usage {
            prompt_tokens: 1000,
            response_tokens: 500,
            total_tokens: 1500,
            cached_tokens: 0,
        };
        let r = CallRecord::from_text(
            "gemini",
            "gemini-pro-latest",
            &usage,
            &ctx,
            true,
            None,
        );
        rec.record(r);
        rec.flush();

        let filter = CallFilter {
            scene_path: Some("/tmp/chair.mog".into()),
            limit: 10,
            ..Default::default()
        };
        let rows = rec.query(&filter);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].model, "gemini-pro-latest");
        assert_eq!(rows[0].prompt_tokens, 1000);
        // Cost was backfilled from the pricing table.
        // 1000 in @ $1.25/M + 500 out @ $10/M = $0.00125 + $0.005 = $0.00625
        assert!((rows[0].cost_usd - 0.00625).abs() < 1e-6, "got {}", rows[0].cost_usd);
    }

    #[test]
    fn image_record_uses_per_image_rate() {
        let dir = tmpdb();
        let path = dir.path().join("spend.db");
        let rec = SqliteRecorder::open(path).expect("open recorder");
        let ctx = CallContext::new(Operation::Textures).with_scene("/tmp/x.mog");
        let r = CallRecord::from_image(
            "gemini",
            "gemini-2.5-flash-image",
            3,
            &ctx,
            true,
            None,
        );
        rec.record(r);
        rec.flush();
        let summary = rec.summary(&CallFilter::default());
        assert_eq!(summary.total_images, 3);
        // 3 images × $0.039 = $0.117
        assert!((summary.total_cost_usd - 0.117).abs() < 1e-6, "got {}", summary.total_cost_usd);
    }

    #[test]
    fn summary_groups_by_model() {
        let dir = tmpdb();
        let path = dir.path().join("spend.db");
        let rec = SqliteRecorder::open(path).expect("open recorder");
        let ctx = CallContext::new(Operation::Generate);
        let usage = Usage {
            prompt_tokens: 1000,
            response_tokens: 500,
            total_tokens: 1500,
            cached_tokens: 0,
        };
        for _ in 0..2 {
            rec.record(CallRecord::from_text(
                "gemini",
                "gemini-pro-latest",
                &usage,
                &ctx,
                true,
                None,
            ));
        }
        rec.record(CallRecord::from_text(
            "gemini",
            "gemini-flash-latest",
            &usage,
            &ctx,
            true,
            None,
        ));
        rec.flush();
        let by_model = rec.by_model(&CallFilter::default());
        assert_eq!(by_model.len(), 2);
        // Pro has more spend than Flash (10x output).
        assert_eq!(by_model[0].model, "gemini-pro-latest");
        assert_eq!(by_model[0].total_calls, 2);
    }

    #[test]
    fn pricing_supersession_preserves_history() {
        let dir = tmpdb();
        let path = dir.path().join("spend.db");
        let mut conn = open(&path).expect("open");
        // Drop a record at the seeded Pro rate.
        let now = now_unix();
        conn.execute(
            "INSERT INTO calls (ts, provider, model, operation, prompt_tokens, response_tokens, cost_usd, success)
             VALUES (?1, 'gemini', 'gemini-pro-latest', 'generate', 1000, 500, 0.00625, 1)",
            params![now - 3600],
        ).unwrap();
        // Edit the pricing for this model.
        upsert_pricing(
            &mut conn,
            "gemini",
            "gemini-pro-latest",
            TextPricing::flat(99.0, 99.0, 99.0),
            ImagePricing { per_image_usd: 0.0 },
        )
        .unwrap();
        // Historical row is untouched.
        let cost: f64 = conn
            .query_row(
                "SELECT cost_usd FROM calls WHERE ts = ?1",
                params![now - 3600],
                |r| r.get(0),
            )
            .unwrap();
        assert!((cost - 0.00625).abs() < 1e-9);
        // The active row for the model is the new one.
        let row = lookup_pricing(&conn, "gemini", "gemini-pro-latest", now).unwrap();
        assert!(row.is_some());
        let row = row.unwrap();
        assert!((row.input_per_mtok_usd - 99.0).abs() < 1e-9);
        // The pricing table has both rows.
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pricing WHERE provider='gemini' AND model='gemini-pro-latest'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn distinct_lists_unique_values() {
        let dir = tmpdb();
        let path = dir.path().join("spend.db");
        let rec = SqliteRecorder::open(path).expect("open recorder");
        let usage = Usage::default();
        rec.record(CallRecord::from_text(
            "gemini",
            "gemini-pro-latest",
            &usage,
            &CallContext::new(Operation::Generate).with_scene("/a.mog"),
            true,
            None,
        ));
        rec.record(CallRecord::from_text(
            "openai",
            "gpt-5",
            &usage,
            &CallContext::new(Operation::Repair).with_scene("/b.mog"),
            true,
            None,
        ));
        rec.flush();
        let d = rec.distinct();
        assert_eq!(d.scenes, vec!["/a.mog".to_string(), "/b.mog".to_string()]);
        assert_eq!(d.models.len(), 2);
        assert!(d.operations.contains(&"generate".to_string()));
        assert!(d.operations.contains(&"repair".to_string()));
    }
}
