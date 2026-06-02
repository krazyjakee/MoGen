//! Pricing-table accessors: per-record cost computation, effective-row
//! lookup, and the Settings editor's list / upsert helpers.

use rusqlite::{params, Connection, OptionalExtension};

use super::now_unix;
use crate::spend::pricing::{
    compute_cost, image_price_for_model, text_price_for_model, ImagePricing, PricingSeed,
    TextPricing, SEED,
};
use crate::spend::CallRecord;

/// Look up the effective pricing row for a [`CallRecord`] and compute its
/// cost. Falls back to the static [`SEED`] table when no DB row matches,
/// so a model the user hasn't priced yet still gets a best-effort number
/// instead of silently being free.
pub(super) fn cost_for_record(conn: &Connection, record: &CallRecord) -> rusqlite::Result<f64> {
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

pub(super) fn lookup_pricing(
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

/// Build a [`PricingSeed`] from a database row. Exposed for tests that
/// want to round-trip the seed catalogue through the schema without
/// reaching into the internal structs.
#[doc(hidden)]
pub fn _seed_row_for(provider: &str, model: &str) -> Option<PricingSeed> {
    SEED.iter()
        .find(|s| s.provider == provider && s.model == model)
        .copied()
}
