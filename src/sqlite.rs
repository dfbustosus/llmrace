//! Optional SQLite storage backend for run history (feature: `sqlite`).
//!
//! Stores the same records as the JSONL default in a `runs` table with an
//! index on `(provider, timestamp_unix)`, enabling fast long-horizon window
//! queries for `--trend` and `--dashboard`.

use crate::history::HistoryRecord;
use rusqlite::Connection;
use std::collections::HashSet;
use std::time::Duration;

/// Opens (creating if needed) the history database and ensures the schema.
pub fn open(path: &std::path::Path) -> rusqlite::Result<Connection> {
    if let Some(dir) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| {
            rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
                Some(format!(
                    "failed to create database directory {}: {e}",
                    dir.display()
                )),
            )
        })?;
    }
    let conn = Connection::open(path)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         CREATE TABLE IF NOT EXISTS runs (
            timestamp_unix         INTEGER NOT NULL,
            provider               TEXT NOT NULL,
            model                  TEXT NOT NULL,
            variant                TEXT,
            concurrency            INTEGER NOT NULL,
            mean_total_ms          REAL NOT NULL,
            p95_total_ms           REAL NOT NULL,
            mean_ttfb_ms           REAL NOT NULL,
            mean_tokens_per_sec    REAL NOT NULL,
            itl_p95_ms             REAL,
            reasoning_tokens       INTEGER,
            thinking_ms            REAL,
            failure_rate           REAL,
            goodput_tokens_per_sec REAL,
            retry_attempts         INTEGER,
            cost_per_run_usd       REAL,
            task_pass_rate         REAL,
            request_sha256         TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_runs_provider_time
            ON runs (provider, timestamp_unix);",
    )?;
    // Additive migration for older databases: inspect existing columns first so
    // real SQLite errors (read-only DB, I/O errors) are never swallowed.
    let existing: HashSet<String> = {
        let mut stmt = conn.prepare("PRAGMA table_info(runs)")?;
        let names = stmt.query_map([], |row| row.get::<_, String>(1))?;
        names.collect::<rusqlite::Result<_>>()?
    };
    const MIGRATIONS: &[(&str, &str)] = &[
        ("itl_p95_ms", "REAL"),
        ("reasoning_tokens", "INTEGER"),
        ("thinking_ms", "REAL"),
        ("failure_rate", "REAL"),
        ("goodput_tokens_per_sec", "REAL"),
        ("retry_attempts", "INTEGER"),
        ("cost_per_run_usd", "REAL"),
        ("task_pass_rate", "REAL"),
        ("request_sha256", "TEXT"),
    ];
    for &(col, ty) in MIGRATIONS {
        if !existing.contains(col) {
            conn.execute(&format!("ALTER TABLE runs ADD COLUMN {col} {ty}"), [])?;
        }
    }
    Ok(conn)
}

/// Appends records inside one transaction.
pub fn save(conn: &Connection, records: &[HistoryRecord]) -> rusqlite::Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        let mut stmt = conn.prepare(
            "INSERT INTO runs (timestamp_unix, provider, model, variant, concurrency,
                               mean_total_ms, p95_total_ms, mean_ttfb_ms, mean_tokens_per_sec,
                               itl_p95_ms, reasoning_tokens, thinking_ms,
                               failure_rate, goodput_tokens_per_sec, retry_attempts, cost_per_run_usd,
                               task_pass_rate, request_sha256)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
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
                r.task_pass_rate,
                r.request_sha256,
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
                failure_rate, goodput_tokens_per_sec, retry_attempts, cost_per_run_usd,
                task_pass_rate, request_sha256
         FROM runs ORDER BY timestamp_unix, rowid",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(HistoryRecord {
            timestamp_unix: row.get::<_, i64>(0)?.max(0) as u64,
            provider: row.get(1)?,
            model: row.get(2)?,
            variant: row.get(3)?,
            concurrency: row.get::<_, i64>(4)?.max(0) as usize,
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
            task_pass_rate: row.get(16)?,
            request_sha256: row.get(17)?,
        })
    })?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn sample_record(ts: u64, provider: &str) -> HistoryRecord {
        HistoryRecord {
            timestamp_unix: ts,
            provider: provider.into(),
            model: "m1".into(),
            variant: None,
            concurrency: 2,
            mean_total_ms: 100.0,
            p95_total_ms: 120.0,
            mean_ttfb_ms: 20.0,
            mean_tokens_per_sec: 50.0,
            itl_p95_ms: Some(12.0),
            reasoning_tokens: Some(4),
            thinking_ms_estimate: Some(10.0),
            failure_rate: Some(0.0),
            goodput_tokens_per_sec: Some(50.0),
            retry_attempts: 1,
            cost_per_run_usd: Some(0.001),
            task_pass_rate: Some(1.0),
            request_sha256: Some("abc".into()),
        }
    }

    #[test]
    fn open_migrates_legacy_table_and_clamps_negative_values() {
        let dir = std::env::temp_dir().join(format!(
            "llmrace-sqlite-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("legacy.db");
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE runs (
                    timestamp_unix INTEGER NOT NULL,
                    provider TEXT NOT NULL,
                    model TEXT NOT NULL,
                    variant TEXT,
                    concurrency INTEGER NOT NULL,
                    mean_total_ms REAL NOT NULL,
                    p95_total_ms REAL NOT NULL,
                    mean_ttfb_ms REAL NOT NULL,
                    mean_tokens_per_sec REAL NOT NULL
                );
                INSERT INTO runs VALUES (-5, 'legacy', 'm0', NULL, -3, 10.0, 20.0, 5.0, 40.0);",
            )
            .unwrap();
        }

        let conn = open(&db_path).unwrap();
        save(&conn, &[sample_record(100, "z"), sample_record(100, "a")]).unwrap();
        let loaded = load(&conn).unwrap();
        assert_eq!(loaded.len(), 3);
        // Legacy row clamped to 0, followed by insertion order ("z" then "a") at ts=100.
        assert_eq!(loaded[0].provider, "legacy");
        assert_eq!(loaded[0].timestamp_unix, 0);
        assert_eq!(loaded[0].concurrency, 0);
        assert_eq!(loaded[1].provider, "z");
        assert_eq!(loaded[2].provider, "a");

        drop(conn);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
