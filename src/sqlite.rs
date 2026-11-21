//! Optional SQLite storage backend for run history (feature: `sqlite`).
//!
//! Stores the same records as the JSONL default in a `runs` table with an
//! index on `(provider, timestamp_unix)`, enabling fast long-horizon window
//! queries for `--trend` and `--dashboard`.

use crate::history::HistoryRecord;
use rusqlite::Connection;

/// Opens (creating if needed) the history database and ensures the schema.
pub fn open(path: &std::path::Path) -> rusqlite::Result<Connection> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS runs (
            timestamp_unix    INTEGER NOT NULL,
            provider          TEXT NOT NULL,
            model             TEXT NOT NULL,
            variant           TEXT,
            concurrency       INTEGER NOT NULL,
            mean_total_ms     REAL NOT NULL,
            p95_total_ms      REAL NOT NULL,
            mean_ttfb_ms      REAL NOT NULL,
            mean_tokens_per_sec REAL NOT NULL,
            itl_p95_ms        REAL,
            reasoning_tokens  INTEGER,
            thinking_ms       REAL
        );
        CREATE INDEX IF NOT EXISTS idx_runs_provider_time
            ON runs (provider, timestamp_unix);",
    )?;
    // Additive migration for databases created before ITL tracking:
    // a duplicate-column error means the column already exists.
    let _ = conn.execute("ALTER TABLE runs ADD COLUMN itl_p95_ms REAL", []);
    let _ = conn.execute("ALTER TABLE runs ADD COLUMN reasoning_tokens INTEGER", []);
    let _ = conn.execute("ALTER TABLE runs ADD COLUMN thinking_ms REAL", []);
    let _ = conn.execute("ALTER TABLE runs ADD COLUMN failure_rate REAL", []);
    let _ = conn.execute(
        "ALTER TABLE runs ADD COLUMN goodput_tokens_per_sec REAL",
        [],
    );
    let _ = conn.execute("ALTER TABLE runs ADD COLUMN retry_attempts INTEGER", []);
    let _ = conn.execute("ALTER TABLE runs ADD COLUMN cost_per_run_usd REAL", []);
    Ok(conn)
}

/// Appends records inside one transaction.
pub fn save(conn: &Connection, records: &[HistoryRecord]) -> rusqlite::Result<()> {
    conn.execute_batch("BEGIN")?;
    let result = (|| {
        let mut stmt = conn.prepare(
            "INSERT INTO runs (timestamp_unix, provider, model, variant, concurrency,
                               mean_total_ms, p95_total_ms, mean_ttfb_ms, mean_tokens_per_sec,
                               itl_p95_ms, reasoning_tokens, thinking_ms,
                               failure_rate, goodput_tokens_per_sec, retry_attempts, cost_per_run_usd)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        )?;
        for r in records {
            stmt.execute(rusqlite::params![
                r.timestamp_unix as i64,
                r.provider,
                r.model,
                r.variant,
                r.concurrency as i64,
                r.mean_total_ms,
                r.p95_total_ms,
                r.mean_ttfb_ms,
                r.mean_tokens_per_sec,
                r.itl_p95_ms,
                r.reasoning_tokens.map(|v| v as i64),
                r.thinking_ms_estimate,
                r.failure_rate,
                r.goodput_tokens_per_sec,
                r.retry_attempts as i64,
                r.cost_per_run_usd,
            ])?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => conn.execute_batch("COMMIT"),
        Err(e) => {
            conn.execute_batch("ROLLBACK").ok();
            Err(e)
        }
    }
}

/// Loads all records ordered by timestamp.
pub fn load(conn: &Connection) -> rusqlite::Result<Vec<HistoryRecord>> {
    let mut stmt = conn.prepare(
        "SELECT timestamp_unix, provider, model, variant, concurrency,
                mean_total_ms, p95_total_ms, mean_ttfb_ms, mean_tokens_per_sec,
                itl_p95_ms, reasoning_tokens, thinking_ms,
                failure_rate, goodput_tokens_per_sec, retry_attempts, cost_per_run_usd
         FROM runs ORDER BY timestamp_unix, provider",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(HistoryRecord {
            timestamp_unix: row.get::<_, i64>(0)? as u64,
            provider: row.get(1)?,
            model: row.get(2)?,
            variant: row.get(3)?,
            concurrency: row.get::<_, i64>(4)? as usize,
            mean_total_ms: row.get(5)?,
            p95_total_ms: row.get(6)?,
            mean_ttfb_ms: row.get(7)?,
            mean_tokens_per_sec: row.get(8)?,
            itl_p95_ms: row.get(9)?,
            reasoning_tokens: row.get::<_, Option<i64>>(10)?.map(|v| v.max(0) as u64),
            thinking_ms_estimate: row.get(11)?,
            failure_rate: row.get(12)?,
            goodput_tokens_per_sec: row.get(13)?,
            retry_attempts: row.get::<_, Option<i64>>(14)?.unwrap_or(0).max(0) as u64,
            cost_per_run_usd: row.get(15)?,
        })
    })?;
    rows.collect()
}
