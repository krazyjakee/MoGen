//! Database creation, schema migrations, and the first-run pricing seed.

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};

use super::now_unix;
use crate::spend::pricing::SEED;

/// Latest schema version this crate knows how to produce. Bumping this
/// without adding a corresponding step in [`migrate`] is a panic at
/// open time.
pub const CURRENT_SCHEMA_VERSION: i32 = 1;

/// Resolve the on-disk DB path. `~/.mogen/spend.db` by default; honours
/// `MOGEN_SPEND_DB` (full path override) and `MOGEN_CACHE_DIR` (parent
/// dir override) the same way the API-key store does.
pub fn db_path() -> Option<PathBuf> {
    crate::spend::default_db_path()
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
