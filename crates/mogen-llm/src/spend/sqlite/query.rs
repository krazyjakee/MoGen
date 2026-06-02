//! Read-side query construction: the [`CallFilter`] WHERE builder and the
//! `query_calls` / `summarize` / `group_by_model` / `read_distinct` readers.

use rusqlite::Connection;

use crate::spend::recorder::Distinct;

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

pub(super) fn read_distinct(conn: &Connection) -> rusqlite::Result<Distinct> {
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
