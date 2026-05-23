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
//! Migrations live in [`migrate`] and run inside a transaction; each step
//! bumps `schema_version`. Adding a column means appending a new step —
//! never editing an existing one.
//!
//! ## Concurrency
//!
//! The recorder spawns one writer thread on construction; record requests
//! cross an mpsc channel so call sites never block on disk I/O. Reads
//! (`query`, `summary`, `by_model`, `distinct`) open a fresh connection
//! on the calling thread — SQLite handles cross-connection visibility via
//! its built-in journal once the writer commits.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};

use super::pricing::{
    compute_cost, image_price_for_model, text_price_for_model, ImagePricing, PricingSeed,
    TextPricing, SEED,
};
use super::recorder::{Distinct, SpendRecorder};
use super::CallRecord;

/// Latest schema version this crate knows how to produce. Bumping this
/// without adding a corresponding step in [`migrate`] is a panic at
/// open time.
pub const CURRENT_SCHEMA_VERSION: i32 = 1;

/// Resolve the on-disk DB path. `~/.mogen/spend.db` by default; honours
/// `MOGEN_SPEND_DB` (full path override) and `MOGEN_CACHE_DIR` (parent
/// dir override) the same way the API-key store does.
pub fn db_path() -> Option<PathBuf> {
    super::default_db_path()
}

/// Open the SQLite database, applying any pending migrations and seeding
/// the `pricing` table on first run. The schema is idempotent — calling
/// this multiple times produces the same database.
pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = Connection::open(path)?;
    // WAL gives concurrent readers + one writer without blocking the
    // UI when the panel queries while a background record write is
    // outstanding. The mode is sticky on the file once set.
    let _: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
    conn.execute_batch("PRAGMA synchronous=NORMAL;")?;
    migrate(&conn)?;
    seed_pricing_if_empty(&conn)?;
    Ok(conn)
}

/// Apply schema migrations from `applied_version` up to
/// [`CURRENT_SCHEMA_VERSION`]. Each step bumps `schema_version` so the
/// next launch picks up where this one left off.
fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (
            version INTEGER PRIMARY KEY,
            applied_at INTEGER NOT NULL
        )",
    )?;

    let current: i32 = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_version",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    if current < 1 {
        apply_v1(conn)?;
    }
    // Future migrations chain here: `if current < 2 { apply_v2(conn)? }` and
    // so on. Never mutate an existing `apply_vN` — write a new step.

    Ok(())
}

fn apply_v1(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS calls (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            ts              INTEGER NOT NULL,
            provider        TEXT    NOT NULL,
            model           TEXT    NOT NULL,
            operation       TEXT    NOT NULL,
            prompt_tokens   INTEGER NOT NULL DEFAULT 0,
            response_tokens INTEGER NOT NULL DEFAULT 0,
            cached_tokens   INTEGER NOT NULL DEFAULT 0,
            image_count     INTEGER NOT NULL DEFAULT 0,
            cost_usd        REAL    NOT NULL DEFAULT 0,
            scene_path      TEXT,
            session_id      TEXT,
            success         INTEGER NOT NULL DEFAULT 1,
            notes           TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_calls_scene_ts ON calls(scene_path, ts);
        CREATE INDEX IF NOT EXISTS idx_calls_model_ts ON calls(model, ts);
        CREATE INDEX IF NOT EXISTS idx_calls_ts       ON calls(ts);

        CREATE TABLE IF NOT EXISTS pricing (
            id                              INTEGER PRIMARY KEY AUTOINCREMENT,
            provider                        TEXT    NOT NULL,
            model                           TEXT    NOT NULL,
            input_per_mtok_usd              REAL    NOT NULL DEFAULT 0,
            cached_input_per_mtok_usd       REAL    NOT NULL DEFAULT 0,
            output_per_mtok_usd             REAL    NOT NULL DEFAULT 0,
            image_per_unit_usd              REAL    NOT NULL DEFAULT 0,
            long_context_threshold          INTEGER NOT NULL DEFAULT 0,
            input_per_mtok_long_usd         REAL    NOT NULL DEFAULT 0,
            output_per_mtok_long_usd        REAL    NOT NULL DEFAULT 0,
            cached_input_per_mtok_long_usd  REAL    NOT NULL DEFAULT 0,
            effective_from                  INTEGER NOT NULL,
            effective_to                    INTEGER
        );
        CREATE INDEX IF NOT EXISTS idx_pricing_model ON pricing(provider, model, effective_from);
        ",
    )?;

    conn.execute(
        "INSERT INTO schema_version(version, applied_at) VALUES (?1, ?2)",
        params![1, now_unix()],
    )?;
    Ok(())
}

/// Populate the `pricing` table with [`SEED`] entries if it's empty. Run
/// on every `open` so a fresh DB on a new install gets the baseline rates
/// without requiring the user to discover the Settings → AI Pricing
/// editor.
fn seed_pricing_if_empty(conn: &Connection) -> rusqlite::Result<()> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM pricing", [], |r| r.get(0))?;
    if count > 0 {
        return Ok(());
    }
    let now = now_unix();
    for row in SEED {
        let (input, cached, output, in_long, out_long, cache_long, threshold) =
            match row.text {
                Some(t) => (
                    t.input_per_mtok,
                    t.cached_input_per_mtok,
                    t.output_per_mtok,
                    t.input_per_mtok_long,
                    t.output_per_mtok_long,
                    t.cached_input_per_mtok_long,
                    t.long_context_threshold as i64,
                ),
                None => (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0),
            };
        let image = row.image.map(|i| i.per_image_usd).unwrap_or(0.0);
        conn.execute(
            "INSERT INTO pricing (
                provider, model,
                input_per_mtok_usd, cached_input_per_mtok_usd, output_per_mtok_usd,
                image_per_unit_usd,
                long_context_threshold,
                input_per_mtok_long_usd, output_per_mtok_long_usd,
                cached_input_per_mtok_long_usd,
                effective_from
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                row.provider,
                row.model,
                input,
                cached,
                output,
                image,
                threshold,
                in_long,
                out_long,
                cache_long,
                now,
            ],
        )?;
    }
    Ok(())
}

/// One message on the writer channel. Plain enum so the writer thread can
/// match on the variant without juggling generics.
enum Msg {
    Record(CallRecord),
    /// Flush + ack — the test path waits on the condvar so it can assert
    /// `record` → `query` deterministically.
    Flush(Arc<(Mutex<bool>, Condvar)>),
    Shutdown,
}

/// SQLite-backed recorder. Owns one background writer thread and a
/// channel into it. Cheap to clone — internally it's an `Arc`.
#[derive(Clone)]
pub struct SqliteRecorder {
    inner: Arc<SqliteRecorderInner>,
}

struct SqliteRecorderInner {
    path: PathBuf,
    tx: Mutex<Option<Sender<Msg>>>,
    writer: Mutex<Option<JoinHandle<()>>>,
}

impl SqliteRecorder {
    /// Open the recorder at `path`. Spawns the writer thread immediately so
    /// records arriving microseconds later have somewhere to land.
    pub fn open(path: PathBuf) -> rusqlite::Result<Self> {
        // Verify the DB can be created / migrated before we hand the path
        // to the writer thread. Surfaces "permission denied" / "no space
        // left" at construction time rather than swallowing it forever on
        // the writer thread.
        let _ = open(&path)?;
        let (tx, rx) = mpsc::channel::<Msg>();
        let writer_path = path.clone();
        let writer = thread::Builder::new()
            .name("mogen-spend-writer".into())
            .spawn(move || writer_loop(writer_path, rx))
            .map_err(|e| {
                rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("spawn spend writer: {e}"),
                )))
            })?;
        Ok(Self {
            inner: Arc::new(SqliteRecorderInner {
                path,
                tx: Mutex::new(Some(tx)),
                writer: Mutex::new(Some(writer)),
            }),
        })
    }

    /// Open the recorder at the default `~/.mogen/spend.db` path.
    pub fn open_default() -> rusqlite::Result<Self> {
        let path = db_path().ok_or_else(|| {
            rusqlite::Error::InvalidPath(PathBuf::from("could not resolve mogen home"))
        })?;
        Self::open(path)
    }

    /// Borrow the on-disk DB path. Used by the Studio panel to surface
    /// "Spending database at …" in About / Settings.
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Open a fresh read-side connection. Returned to callers that want to
    /// run analytics queries without going through the trait.
    pub fn connection(&self) -> rusqlite::Result<Connection> {
        open(&self.inner.path)
    }
}

impl Drop for SqliteRecorderInner {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.lock().unwrap().take() {
            let _ = tx.send(Msg::Shutdown);
        }
        if let Some(handle) = self.writer.lock().unwrap().take() {
            let _ = handle.join();
        }
    }
}

impl SpendRecorder for SqliteRecorder {
    fn record(&self, record: CallRecord) {
        let tx = self.inner.tx.lock().unwrap();
        if let Some(tx) = tx.as_ref() {
            let _ = tx.send(Msg::Record(record));
        }
    }

    fn query(&self, filter: &CallFilter) -> Vec<CallRow> {
        match self.connection().and_then(|c| query_calls(&c, filter)) {
            Ok(rows) => rows,
            Err(_) => Vec::new(),
        }
    }

    fn summary(&self, filter: &CallFilter) -> SummaryRow {
        self.connection()
            .and_then(|c| summarize(&c, filter))
            .unwrap_or_default()
    }

    fn by_model(&self, filter: &CallFilter) -> Vec<ModelSummary> {
        match self.connection().and_then(|c| group_by_model(&c, filter)) {
            Ok(rows) => rows,
            Err(_) => Vec::new(),
        }
    }

    fn distinct(&self) -> Distinct {
        match self.connection().and_then(|c| read_distinct(&c)) {
            Ok(d) => d,
            Err(_) => Distinct::default(),
        }
    }

    fn flush(&self) {
        let pair = Arc::new((Mutex::new(false), Condvar::new()));
        {
            let tx = self.inner.tx.lock().unwrap();
            if let Some(tx) = tx.as_ref() {
                if tx.send(Msg::Flush(pair.clone())).is_err() {
                    return;
                }
            } else {
                return;
            }
        }
        let (lock, cv) = &*pair;
        let mut done = lock.lock().unwrap();
        while !*done {
            let r = cv
                .wait_timeout(done, Duration::from_secs(2))
                .unwrap();
            done = r.0;
            if r.1.timed_out() {
                break;
            }
        }
    }
}

fn writer_loop(path: PathBuf, rx: mpsc::Receiver<Msg>) {
    let mut conn = match open(&path) {
        Ok(c) => c,
        Err(_) => return,
    };

    while let Ok(msg) = rx.recv() {
        match msg {
            Msg::Record(record) => {
                let _ = insert_record(&mut conn, record);
            }
            Msg::Flush(pair) => {
                let (lock, cv) = &*pair;
                let mut done = lock.lock().unwrap();
                *done = true;
                cv.notify_all();
            }
            Msg::Shutdown => break,
        }
    }
}

fn insert_record(conn: &mut Connection, mut record: CallRecord) -> rusqlite::Result<()> {
    if record.ts == 0 {
        record.ts = now_unix();
    }
    if record.cost_usd <= 0.0 {
        record.cost_usd = cost_for_record(conn, &record).unwrap_or(0.0);
    }
    conn.execute(
        "INSERT INTO calls (
            ts, provider, model, operation,
            prompt_tokens, response_tokens, cached_tokens, image_count,
            cost_usd, scene_path, session_id, success, notes
        ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
        params![
            record.ts,
            record.provider,
            record.model,
            record.operation,
            record.prompt_tokens,
            record.response_tokens,
            record.cached_tokens,
            record.image_count,
            record.cost_usd,
            record.scene_path,
            record.session_id,
            if record.success { 1i32 } else { 0 },
            record.notes,
        ],
    )?;
    Ok(())
}

/// Look up the effective pricing row for a [`CallRecord`] and compute its
/// cost. Falls back to the static [`SEED`] table when no DB row matches,
/// so a model the user hasn't priced yet still gets a best-effort number
/// instead of silently being free.
fn cost_for_record(conn: &Connection, record: &CallRecord) -> rusqlite::Result<f64> {
    let row = lookup_pricing(conn, &record.provider, &record.model, record.ts)?;
    if record.image_count > 0 {
        let per_image = row.map(|r| r.image_per_unit_usd).unwrap_or_else(|| {
            image_price_for_model(&record.model).per_image_usd
        });
        return Ok(per_image * record.image_count as f64);
    }
    let usage = crate::types::Usage {
        prompt_tokens: record.prompt_tokens,
        response_tokens: record.response_tokens,
        total_tokens: record.prompt_tokens + record.response_tokens,
        cached_tokens: record.cached_tokens,
    };
    let price = row
        .map(|r| TextPricing {
            input_per_mtok: r.input_per_mtok_usd,
            output_per_mtok: r.output_per_mtok_usd,
            cached_input_per_mtok: r.cached_input_per_mtok_usd,
            input_per_mtok_long: r.input_per_mtok_long_usd,
            output_per_mtok_long: r.output_per_mtok_long_usd,
            cached_input_per_mtok_long: r.cached_input_per_mtok_long_usd,
            long_context_threshold: r.long_context_threshold as u32,
        })
        .unwrap_or_else(|| text_price_for_model(&record.model));
    Ok(compute_cost(&usage, price))
}

/// One row from the `pricing` table. Public so the Studio Settings →
/// AI Pricing editor can render and edit it directly.
#[derive(Debug, Clone)]
pub struct PricingRow {
    pub id: i64,
    pub provider: String,
    pub model: String,
    pub input_per_mtok_usd: f64,
    pub cached_input_per_mtok_usd: f64,
    pub output_per_mtok_usd: f64,
    pub image_per_unit_usd: f64,
    pub long_context_threshold: i64,
    pub input_per_mtok_long_usd: f64,
    pub output_per_mtok_long_usd: f64,
    pub cached_input_per_mtok_long_usd: f64,
    pub effective_from: i64,
    pub effective_to: Option<i64>,
}

fn lookup_pricing(
    conn: &Connection,
    provider: &str,
    model: &str,
    ts: i64,
) -> rusqlite::Result<Option<PricingRow>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, provider, model,
                input_per_mtok_usd, cached_input_per_mtok_usd, output_per_mtok_usd,
                image_per_unit_usd,
                long_context_threshold,
                input_per_mtok_long_usd, output_per_mtok_long_usd,
                cached_input_per_mtok_long_usd,
                effective_from, effective_to
         FROM pricing
         WHERE provider = ?1 AND model = ?2
           AND effective_from <= ?3
           AND (effective_to IS NULL OR effective_to > ?3)
         ORDER BY effective_from DESC
         LIMIT 1",
    )?;
    let row = stmt
        .query_row(params![provider, model, ts], |row| {
            Ok(PricingRow {
                id: row.get(0)?,
                provider: row.get(1)?,
                model: row.get(2)?,
                input_per_mtok_usd: row.get(3)?,
                cached_input_per_mtok_usd: row.get(4)?,
                output_per_mtok_usd: row.get(5)?,
                image_per_unit_usd: row.get(6)?,
                long_context_threshold: row.get(7)?,
                input_per_mtok_long_usd: row.get(8)?,
                output_per_mtok_long_usd: row.get(9)?,
                cached_input_per_mtok_long_usd: row.get(10)?,
                effective_from: row.get(11)?,
                effective_to: row.get(12)?,
            })
        })
        .optional()?;
    Ok(row)
}

/// List every effective pricing row right now. Used by Settings →
/// AI Pricing to render the editor table.
pub fn list_pricing(conn: &Connection) -> rusqlite::Result<Vec<PricingRow>> {
    let now = now_unix();
    let mut stmt = conn.prepare(
        "SELECT id, provider, model,
                input_per_mtok_usd, cached_input_per_mtok_usd, output_per_mtok_usd,
                image_per_unit_usd,
                long_context_threshold,
                input_per_mtok_long_usd, output_per_mtok_long_usd,
                cached_input_per_mtok_long_usd,
                effective_from, effective_to
         FROM pricing
         WHERE effective_from <= ?1 AND (effective_to IS NULL OR effective_to > ?1)
         ORDER BY provider, model",
    )?;
    let rows = stmt
        .query_map([now], |row| {
            Ok(PricingRow {
                id: row.get(0)?,
                provider: row.get(1)?,
                model: row.get(2)?,
                input_per_mtok_usd: row.get(3)?,
                cached_input_per_mtok_usd: row.get(4)?,
                output_per_mtok_usd: row.get(5)?,
                image_per_unit_usd: row.get(6)?,
                long_context_threshold: row.get(7)?,
                input_per_mtok_long_usd: row.get(8)?,
                output_per_mtok_long_usd: row.get(9)?,
                cached_input_per_mtok_long_usd: row.get(10)?,
                effective_from: row.get(11)?,
                effective_to: row.get(12)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Supersede the current effective row for `(provider, model)` and insert
/// a new one with the supplied rates effective from `now`. The previous
/// row's `effective_to` is stamped with the same `now` so historical
/// records keep billing at the old rate.
///
/// Used by Settings → AI Pricing. Wrap in a transaction so a crash mid-
/// update can't leave both rows simultaneously effective.
#[allow(clippy::too_many_arguments)]
pub fn upsert_pricing(
    conn: &mut Connection,
    provider: &str,
    model: &str,
    text: TextPricing,
    image: ImagePricing,
) -> rusqlite::Result<()> {
    let now = now_unix();
    let tx = conn.transaction()?;
    tx.execute(
        "UPDATE pricing
            SET effective_to = ?1
          WHERE provider = ?2 AND model = ?3 AND effective_to IS NULL",
        params![now, provider, model],
    )?;
    tx.execute(
        "INSERT INTO pricing (
            provider, model,
            input_per_mtok_usd, cached_input_per_mtok_usd, output_per_mtok_usd,
            image_per_unit_usd,
            long_context_threshold,
            input_per_mtok_long_usd, output_per_mtok_long_usd,
            cached_input_per_mtok_long_usd,
            effective_from
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            provider,
            model,
            text.input_per_mtok,
            text.cached_input_per_mtok,
            text.output_per_mtok,
            image.per_image_usd,
            text.long_context_threshold as i64,
            text.input_per_mtok_long,
            text.output_per_mtok_long,
            text.cached_input_per_mtok_long,
            now,
        ],
    )?;
    tx.commit()?;
    Ok(())
}

/// Filter applied to call queries. `None` fields mean "any value". Used
/// by [`SpendRecorder::query`] / `summary` / `by_model`.
#[derive(Debug, Default, Clone)]
pub struct CallFilter {
    /// Inclusive lower bound on `ts` (unix seconds).
    pub from_ts: Option<i64>,
    /// Exclusive upper bound on `ts`.
    pub to_ts: Option<i64>,
    pub scene_path: Option<String>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub operation: Option<String>,
    pub session_id: Option<String>,
    /// Cap on the number of rows returned by `query`. Other endpoints
    /// ignore this — they aggregate. Defaults to 1000 when zero.
    pub limit: u32,
}

/// One row from `SELECT * FROM calls`. Public so the Studio panel can
/// render the recent-calls list directly.
#[derive(Debug, Clone)]
pub struct CallRow {
    pub id: i64,
    pub ts: i64,
    pub provider: String,
    pub model: String,
    pub operation: String,
    pub prompt_tokens: u32,
    pub response_tokens: u32,
    pub cached_tokens: u32,
    pub image_count: u32,
    pub cost_usd: f64,
    pub scene_path: Option<String>,
    pub session_id: Option<String>,
    pub success: bool,
    pub notes: Option<String>,
}

/// Aggregate over the `calls` matching a filter.
#[derive(Debug, Default, Clone)]
pub struct SummaryRow {
    pub total_cost_usd: f64,
    pub total_calls: i64,
    pub total_prompt_tokens: i64,
    pub total_response_tokens: i64,
    pub total_cached_tokens: i64,
    pub total_images: i64,
}

/// Per-model aggregate. One row per distinct `model` value matching the
/// filter; sorted by `total_cost_usd` descending.
#[derive(Debug, Clone)]
pub struct ModelSummary {
    pub model: String,
    pub provider: String,
    pub total_cost_usd: f64,
    pub total_calls: i64,
    pub total_prompt_tokens: i64,
    pub total_response_tokens: i64,
    pub total_images: i64,
}

fn build_where(filter: &CallFilter) -> (String, Vec<rusqlite::types::Value>) {
    use rusqlite::types::Value;
    let mut parts: Vec<&str> = Vec::new();
    let mut args: Vec<Value> = Vec::new();
    if let Some(v) = filter.from_ts {
        parts.push("ts >= ?");
        args.push(Value::Integer(v));
    }
    if let Some(v) = filter.to_ts {
        parts.push("ts < ?");
        args.push(Value::Integer(v));
    }
    if let Some(v) = filter.scene_path.as_deref() {
        parts.push("scene_path = ?");
        args.push(Value::Text(v.to_string()));
    }
    if let Some(v) = filter.model.as_deref() {
        parts.push("model = ?");
        args.push(Value::Text(v.to_string()));
    }
    if let Some(v) = filter.provider.as_deref() {
        parts.push("provider = ?");
        args.push(Value::Text(v.to_string()));
    }
    if let Some(v) = filter.operation.as_deref() {
        parts.push("operation = ?");
        args.push(Value::Text(v.to_string()));
    }
    if let Some(v) = filter.session_id.as_deref() {
        parts.push("session_id = ?");
        args.push(Value::Text(v.to_string()));
    }
    let clause = if parts.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", parts.join(" AND "))
    };
    (clause, args)
}

/// Read most-recent rows from `calls` matching `filter`, newest first.
pub fn query_calls(conn: &Connection, filter: &CallFilter) -> rusqlite::Result<Vec<CallRow>> {
    let (where_clause, args) = build_where(filter);
    let limit = if filter.limit == 0 { 1000 } else { filter.limit };
    let sql = format!(
        "SELECT id, ts, provider, model, operation,
                prompt_tokens, response_tokens, cached_tokens, image_count,
                cost_usd, scene_path, session_id, success, notes
         FROM calls{}
         ORDER BY ts DESC, id DESC
         LIMIT {}",
        where_clause, limit,
    );
    let mut stmt = conn.prepare(&sql)?;
    let args = rusqlite::params_from_iter(args.iter());
    let rows = stmt
        .query_map(args, |row| {
            let success_i: i32 = row.get(12)?;
            Ok(CallRow {
                id: row.get(0)?,
                ts: row.get(1)?,
                provider: row.get(2)?,
                model: row.get(3)?,
                operation: row.get(4)?,
                prompt_tokens: row.get::<_, i64>(5)? as u32,
                response_tokens: row.get::<_, i64>(6)? as u32,
                cached_tokens: row.get::<_, i64>(7)? as u32,
                image_count: row.get::<_, i64>(8)? as u32,
                cost_usd: row.get(9)?,
                scene_path: row.get(10)?,
                session_id: row.get(11)?,
                success: success_i != 0,
                notes: row.get(13)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Aggregate cost/tokens/images for `filter`.
pub fn summarize(conn: &Connection, filter: &CallFilter) -> rusqlite::Result<SummaryRow> {
    let (where_clause, args) = build_where(filter);
    let sql = format!(
        "SELECT
            COALESCE(SUM(cost_usd), 0),
            COUNT(*),
            COALESCE(SUM(prompt_tokens), 0),
            COALESCE(SUM(response_tokens), 0),
            COALESCE(SUM(cached_tokens), 0),
            COALESCE(SUM(image_count), 0)
         FROM calls{}",
        where_clause
    );
    let mut stmt = conn.prepare(&sql)?;
    let args = rusqlite::params_from_iter(args.iter());
    let row = stmt.query_row(args, |row| {
        Ok(SummaryRow {
            total_cost_usd: row.get(0)?,
            total_calls: row.get(1)?,
            total_prompt_tokens: row.get(2)?,
            total_response_tokens: row.get(3)?,
            total_cached_tokens: row.get(4)?,
            total_images: row.get(5)?,
        })
    })?;
    Ok(row)
}

/// Per-model aggregate, sorted by total cost descending.
pub fn group_by_model(
    conn: &Connection,
    filter: &CallFilter,
) -> rusqlite::Result<Vec<ModelSummary>> {
    let (where_clause, args) = build_where(filter);
    let sql = format!(
        "SELECT model, provider,
                COALESCE(SUM(cost_usd), 0),
                COUNT(*),
                COALESCE(SUM(prompt_tokens), 0),
                COALESCE(SUM(response_tokens), 0),
                COALESCE(SUM(image_count), 0)
         FROM calls{}
         GROUP BY model, provider
         ORDER BY 3 DESC",
        where_clause
    );
    let mut stmt = conn.prepare(&sql)?;
    let args = rusqlite::params_from_iter(args.iter());
    let rows = stmt
        .query_map(args, |row| {
            Ok(ModelSummary {
                model: row.get(0)?,
                provider: row.get(1)?,
                total_cost_usd: row.get(2)?,
                total_calls: row.get(3)?,
                total_prompt_tokens: row.get(4)?,
                total_response_tokens: row.get(5)?,
                total_images: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn read_distinct(conn: &Connection) -> rusqlite::Result<Distinct> {
    fn collect(conn: &Connection, sql: &str) -> rusqlite::Result<Vec<String>> {
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
    Ok(Distinct {
        scenes: collect(
            conn,
            "SELECT DISTINCT scene_path FROM calls WHERE scene_path IS NOT NULL AND scene_path <> '' ORDER BY scene_path",
        )?,
        models: collect(
            conn,
            "SELECT DISTINCT model FROM calls WHERE model <> '' ORDER BY model",
        )?,
        operations: collect(
            conn,
            "SELECT DISTINCT operation FROM calls WHERE operation <> '' ORDER BY operation",
        )?,
    })
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Build a [`PricingSeed`] from a database row. Exposed for tests that
/// want to round-trip the seed catalogue through the schema without
/// reaching into the internal structs.
#[doc(hidden)]
pub fn _seed_row_for(provider: &str, model: &str) -> Option<PricingSeed> {
    SEED.iter()
        .find(|s| s.provider == provider && s.model == model)
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spend::{CallContext, Operation};
    use crate::types::Usage;

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
