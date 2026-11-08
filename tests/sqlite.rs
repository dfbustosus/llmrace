#![cfg(feature = "sqlite")]

use llmrace::history::HistoryRecord;
use llmrace::sqlite;

fn rec(ts: u64, provider: &str, model: &str) -> HistoryRecord {
    HistoryRecord {
        timestamp_unix: ts,
        provider: provider.into(),
        model: model.into(),
        variant: None,
        concurrency: 1,
        mean_total_ms: 290.0,
        p95_total_ms: 300.0,
        mean_ttfb_ms: 14.0,
        mean_tokens_per_sec: 87.0,
    }
}

#[test]
fn sqlite_roundtrip_orders_by_time() {
    let dir = std::env::temp_dir().join(format!("llmrace-sqlite-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("history.sqlite3");

    {
        let conn = sqlite::open(&path).unwrap();
        sqlite::save(
            &conn,
            &[
                rec(2000, "groq", "llama-3"),
                rec(1000, "groq", "llama-3"),
                rec(1500, "openai", "gpt-4o-mini"),
            ],
        )
        .unwrap();
    }

    let conn = sqlite::open(&path).unwrap();
    let loaded = sqlite::load(&conn).unwrap();
    assert_eq!(loaded.len(), 3);
    assert_eq!(loaded[0].timestamp_unix, 1000);
    assert_eq!(loaded[1].timestamp_unix, 1500);
    assert_eq!(loaded[1].provider, "openai");
    assert_eq!(loaded[2].provider, "groq");
    assert_eq!(loaded[2].mean_tokens_per_sec, 87.0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn sqlite_schema_is_idempotent() {
    let dir = std::env::temp_dir().join(format!("llmrace-sqlite-idem-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("h.sqlite3");
    let conn = sqlite::open(&path).unwrap();
    drop(conn);
    let conn = sqlite::open(&path).unwrap();
    sqlite::save(&conn, &[rec(1, "a", "m")]).unwrap();
    assert_eq!(sqlite::load(&conn).unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}
