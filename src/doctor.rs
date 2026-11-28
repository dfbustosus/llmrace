//! Environment diagnostics: `llmrace --doctor`.
//!
//! Checks API keys, endpoint reachability (any HTTP response counts — even
//! 401 proves egress and DNS), local Ollama, config discovery and history
//! health, and prints a decision-ready table for bug reports.

use crate::config::Config;
use crate::provider::ProviderSpec;

/// Outcome of one diagnostic check.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    /// Everything expected is in place.
    Pass,
    /// Usable, but something is off (e.g. no key for an optional provider).
    Warn,
    /// Expected but broken (e.g. unreachable endpoint, unreadable history).
    Fail,
}

/// One diagnostic result row.
#[derive(Debug, Clone)]
pub struct Check {
    /// Short component name (e.g. `openai`, `history`).
    pub name: String,
    /// Outcome.
    pub status: Status,
    /// Human-readable explanation.
    pub detail: String,
}

/// Runs provider-level checks: key presence and endpoint egress.
///
/// Egress is probed with a 3-second GET; any HTTP status (including 401/404)
/// counts as reachable, since it proves DNS + TLS + routing work.
pub async fn check_providers(client: &reqwest::Client, specs: &[ProviderSpec]) -> Vec<Check> {
    // Probe every keyed provider concurrently — sequential 3s timeouts
    // could stall --doctor for half a minute when several endpoints are
    // blocked. Results keep registry order.
    let futures: Vec<_> = specs
        .iter()
        // opt-in local daemons are covered by check_local_daemon; a daemon
        // that is intentionally not running must not read as a failure here
        .filter(|spec| *spec.id != *"mock" && !spec.opt_in)
        .map(|spec| async move {
            if !spec.is_available() {
                return Check {
                    name: spec.id.to_string(),
                    status: Status::Warn,
                    detail: format!(
                        "no {} — provider skipped in auto-detect",
                        spec.api_key_env.as_deref().unwrap_or("key")
                    ),
                };
            }
            match probe(client, &spec.endpoint).await {
                Ok(()) => Check {
                    name: spec.id.to_string(),
                    status: Status::Pass,
                    detail: format!("key set, {} reachable", host(&spec.endpoint)),
                },
                Err(e) => Check {
                    name: spec.id.to_string(),
                    status: Status::Fail,
                    detail: format!("key set but {} unreachable: {e}", host(&spec.endpoint)),
                },
            }
        })
        .collect();
    futures_util::future::join_all(futures).await
}

async fn probe(client: &reqwest::Client, endpoint: &str) -> Result<(), String> {
    let resp = client
        .get(endpoint)
        .timeout(std::time::Duration::from_secs(3))
        .send()
        .await
        .map_err(|e| crate::util::truncate(&e.without_url().to_string(), 90))?;
    let _ = resp.status();
    Ok(())
}

fn host(endpoint: &str) -> String {
    endpoint
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or(endpoint)
        .to_string()
}

/// Checks the local Ollama daemon via `/api/tags`.
pub async fn check_ollama(client: &reqwest::Client) -> Check {
    let resp = client
        .get("http://localhost:11434/api/tags")
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await;
    match resp {
        Ok(r) if r.status().is_success() => {
            let body = crate::util::read_body_capped(r, 64 * 1024)
                .await
                .unwrap_or_default();
            let models = body.matches("\"name\"").count();
            Check {
                name: "ollama".into(),
                status: Status::Pass,
                detail: format!("daemon reachable, ~{} model(s) pulled", models.max(1)),
            }
        }
        Ok(r) => Check {
            name: "ollama".into(),
            status: Status::Fail,
            detail: format!("daemon answered HTTP {}", r.status()),
        },
        Err(e) => Check {
            name: "ollama".into(),
            status: Status::Warn,
            detail: format!(
                "not running ({})",
                crate::util::truncate(&e.without_url().to_string(), 60)
            ),
        },
    }
}

/// Checks for a newer llmrace release on crates.io.
///
/// One user-initiated GET from `--doctor`; the race path never phones
/// home. `Warn` when a newer version exists or the check is offline.
pub async fn check_version(client: &reqwest::Client) -> Check {
    const CURRENT: &str = env!("CARGO_PKG_VERSION");
    let resp = client
        .get("https://crates.io/api/v1/crates/llmrace")
        // crates.io requires a user agent
        .header("user-agent", format!("llmrace/{CURRENT} (doctor check)"))
        .timeout(std::time::Duration::from_secs(3))
        .send()
        .await;
    match resp {
        Ok(r) if r.status().is_success() => {
            let latest = crate::util::read_body_capped(r, 64 * 1024)
                .await
                .ok()
                .and_then(|body| {
                    serde_json::from_str::<serde_json::Value>(&body)
                        .ok()
                        .and_then(|v| {
                            v.pointer("/crate/max_stable_version")
                                .and_then(|s| s.as_str())
                                .map(str::to_string)
                        })
                })
                .unwrap_or_default();
            if latest.is_empty() {
                return Check {
                    name: "version".into(),
                    status: Status::Warn,
                    detail: "could not parse crates.io response".into(),
                };
            }
            if latest == CURRENT {
                Check {
                    name: "version".into(),
                    status: Status::Pass,
                    detail: format!("up to date ({CURRENT})"),
                }
            } else {
                Check {
                    name: "version".into(),
                    status: Status::Warn,
                    detail: format!("llmrace {latest} is available (you are on {CURRENT})"),
                }
            }
        }
        _ => Check {
            name: "version".into(),
            status: Status::Warn,
            detail: "could not reach crates.io (offline?)".into(),
        },
    }
}

/// Checks an opt-in local OpenAI-compatible daemon (LM Studio, llama.cpp
/// llama-server): a `Warn` when it is not running is expected — these
/// providers only race when named explicitly. The endpoint comes from the
/// provider spec, so ports live in exactly one place.
pub async fn check_local_daemon(client: &reqwest::Client, name: &str, endpoint: &str) -> Check {
    // probe the sibling /models path of the chat-completions endpoint
    let models_url = endpoint.replace("/chat/completions", "/models");
    let resp = client
        .get(&models_url)
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await;
    let host_of = || {
        models_url
            .split("//")
            .nth(1)
            .and_then(|rest| rest.split('/').next())
            .unwrap_or("localhost")
            .to_string()
    };
    match resp {
        Ok(r) if r.status().is_success() => Check {
            name: name.into(),
            status: Status::Pass,
            detail: format!("daemon reachable at {}", host_of()),
        },
        Ok(r) => Check {
            name: name.into(),
            status: Status::Warn,
            detail: format!("answered HTTP {} at {}", r.status(), host_of()),
        },
        Err(_) => Check {
            name: name.into(),
            status: Status::Warn,
            detail: format!("not running at {} (fine — opt-in provider)", host_of()),
        },
    }
}

/// Checks config discovery: which file wins and how many profiles it holds.
pub fn check_config() -> Check {
    match Config::discover() {
        Ok(config) if config.profiles.is_empty() => Check {
            name: "config".into(),
            status: Status::Warn,
            detail: "no llmrace.toml found (optional)".into(),
        },
        Ok(config) => Check {
            name: "config".into(),
            status: Status::Pass,
            detail: format!(
                "{} profile(s): {}",
                config.profiles.len(),
                config.available()
            ),
        },
        Err(e) => Check {
            name: "config".into(),
            status: Status::Fail,
            detail: e.to_string(),
        },
    }
}

fn worse_status(a: Status, b: Status) -> Status {
    match (a, b) {
        (Status::Fail, _) | (_, Status::Fail) => Status::Fail,
        (Status::Warn, _) | (_, Status::Warn) => Status::Warn,
        (Status::Pass, Status::Pass) => Status::Pass,
    }
}

fn check_jsonl_history(path: &std::path::Path) -> (Status, String) {
    match std::fs::read_to_string(path) {
        Ok(raw) => {
            let total = raw.lines().filter(|l| !l.trim().is_empty()).count();
            let valid = crate::history::load(path).len();
            let status = if valid == total {
                Status::Pass
            } else {
                Status::Warn
            };
            (
                status,
                format!("{valid}/{total} valid record(s) in {}", path.display()),
            )
        }
        Err(e) => (Status::Fail, format!("{} unreadable: {e}", path.display())),
    }
}

fn check_sqlite_history(path: &std::path::Path) -> (Status, String) {
    #[cfg(feature = "sqlite")]
    match crate::sqlite::count_records(path) {
        Ok(n) => (Status::Pass, format!("{n} record(s) in {}", path.display())),
        Err(e) => (
            Status::Fail,
            format!(
                "{} unreadable: {}",
                path.display(),
                crate::util::truncate(&e.to_string(), 120)
            ),
        ),
    }
    #[cfg(not(feature = "sqlite"))]
    (
        Status::Warn,
        format!(
            "{} present (rebuild with --features sqlite to inspect)",
            path.display()
        ),
    )
}

fn check_history_at(jsonl: &std::path::Path, sqlite: &std::path::Path) -> Check {
    let has_jsonl = jsonl.exists();
    let has_sqlite = sqlite.exists();
    if !has_jsonl && !has_sqlite {
        return Check {
            name: "history".into(),
            status: Status::Warn,
            detail: format!("no history yet at {}", jsonl.display()),
        };
    }

    let mut status = Status::Pass;
    let mut details = Vec::new();
    if has_jsonl {
        let (s, d) = check_jsonl_history(jsonl);
        status = worse_status(status, s);
        details.push(d);
    }
    if has_sqlite {
        let (s, d) = check_sqlite_history(sqlite);
        status = worse_status(status, s);
        details.push(d);
    }
    Check {
        name: "history".into(),
        status,
        detail: details.join("; "),
    }
}

/// Checks history readability and size.
pub fn check_history() -> Check {
    check_history_at(
        &crate::history::history_path(),
        &crate::history::sqlite_history_path(),
    )
}

/// Renders checks as a terminal table with a summary line.
pub fn render(checks: &[Check]) -> String {
    let mut out = String::from("🩺 llmrace doctor\n");
    out.push_str(&"-".repeat(64));
    out.push('\n');
    for c in checks {
        let mark = match c.status {
            Status::Pass => "✓",
            Status::Warn => "!",
            Status::Fail => "✗",
        };
        out.push_str(&format!(" {mark} {:<12} {}\n", c.name, c.detail));
    }
    let fails = checks.iter().filter(|c| c.status == Status::Fail).count();
    let warns = checks.iter().filter(|c| c.status == Status::Warn).count();
    out.push_str(&"-".repeat(64));
    out.push('\n');
    out.push_str(&format!(
        "summary: {} fail, {warns} warn, {} pass\n",
        fails,
        checks.len() - fails - warns
    ));
    out.push_str("attach this output when filing bugs\n");
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::history::HistoryRecord;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn unique_temp_dir(label: &str) -> PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "llmrace-doctor-{label}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample_record() -> HistoryRecord {
        HistoryRecord {
            timestamp_unix: 1_700_000_000,
            provider: "mock".into(),
            model: "mock-1".into(),
            variant: None,
            concurrency: 1,
            mean_total_ms: 100.0,
            p95_total_ms: 110.0,
            mean_ttfb_ms: 10.0,
            mean_tokens_per_sec: 50.0,
            itl_p95_ms: None,
            reasoning_tokens: None,
            thinking_ms_estimate: None,
            failure_rate: None,
            goodput_tokens_per_sec: None,
            retry_attempts: 0,
            cost_per_run_usd: None,
            task_pass_rate: None,
            request_sha256: None,
        }
    }

    #[test]
    fn check_history_at_warns_when_neither_exists_without_creating_files() {
        let dir = unique_temp_dir("neither");
        let nested = dir.join("missing");
        let jsonl = nested.join("history.jsonl");
        let sqlite = nested.join("history.sqlite3");

        let check = check_history_at(&jsonl, &sqlite);
        assert_eq!(check.name, "history");
        assert_eq!(check.status, Status::Warn);
        assert_eq!(
            check.detail,
            format!("no history yet at {}", jsonl.display())
        );
        assert!(!nested.exists());
        assert!(!jsonl.exists());
        assert!(!sqlite.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_history_at_checks_jsonl_only_without_creating_sqlite() {
        let dir = unique_temp_dir("jsonl-only");
        let jsonl = dir.join("history.jsonl");
        let sqlite = dir.join("history.sqlite3");
        crate::history::append(&jsonl, &[sample_record()]).unwrap();

        let check = check_history_at(&jsonl, &sqlite);
        assert_eq!(check.status, Status::Pass);
        assert_eq!(
            check.detail,
            format!("1/1 valid record(s) in {}", jsonl.display())
        );
        assert!(!sqlite.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_history_at_handles_sqlite_only() {
        let dir = unique_temp_dir("sqlite-only");
        let jsonl = dir.join("history.jsonl");
        let sqlite = dir.join("history.sqlite3");

        #[cfg(feature = "sqlite")]
        {
            let conn = crate::sqlite::open(&sqlite).unwrap();
            crate::sqlite::save(&conn, &[sample_record(), sample_record()]).unwrap();
        }
        #[cfg(not(feature = "sqlite"))]
        std::fs::write(&sqlite, b"placeholder").unwrap();

        let check = check_history_at(&jsonl, &sqlite);
        #[cfg(feature = "sqlite")]
        {
            assert_eq!(check.status, Status::Pass);
            assert_eq!(check.detail, format!("2 record(s) in {}", sqlite.display()));
        }
        #[cfg(not(feature = "sqlite"))]
        {
            assert_eq!(check.status, Status::Warn);
            assert_eq!(
                check.detail,
                format!(
                    "{} present (rebuild with --features sqlite to inspect)",
                    sqlite.display()
                )
            );
        }
        assert!(!jsonl.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_history_at_combines_jsonl_and_sqlite() {
        let dir = unique_temp_dir("both");
        let jsonl = dir.join("history.jsonl");
        let sqlite = dir.join("history.sqlite3");
        crate::history::append(&jsonl, &[sample_record()]).unwrap();

        #[cfg(feature = "sqlite")]
        {
            let conn = crate::sqlite::open(&sqlite).unwrap();
            crate::sqlite::save(&conn, &[sample_record()]).unwrap();
        }
        #[cfg(not(feature = "sqlite"))]
        std::fs::write(&sqlite, b"placeholder").unwrap();

        let check = check_history_at(&jsonl, &sqlite);
        assert!(
            check.detail.contains("1/1 valid record(s)"),
            "{}",
            check.detail
        );
        assert!(check.detail.contains("; "), "{}", check.detail);
        #[cfg(feature = "sqlite")]
        {
            assert_eq!(check.status, Status::Pass);
            assert!(check.detail.contains("1 record(s) in"), "{}", check.detail);
        }
        #[cfg(not(feature = "sqlite"))]
        {
            assert_eq!(check.status, Status::Warn);
            assert!(
                check
                    .detail
                    .contains("rebuild with --features sqlite to inspect"),
                "{}",
                check.detail
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_history_at_reports_corrupt_sqlite() {
        let dir = unique_temp_dir("corrupt-sqlite");
        let jsonl = dir.join("history.jsonl");
        let sqlite = dir.join("history.sqlite3");
        crate::history::append(&jsonl, &[sample_record()]).unwrap();
        std::fs::write(&sqlite, b"corrupt garbage bytes").unwrap();

        let check = check_history_at(&jsonl, &sqlite);
        #[cfg(feature = "sqlite")]
        {
            assert_eq!(check.status, Status::Fail);
            assert!(
                check
                    .detail
                    .contains(&format!("{} unreadable:", sqlite.display())),
                "{}",
                check.detail
            );
        }
        #[cfg(not(feature = "sqlite"))]
        {
            assert_eq!(check.status, Status::Warn);
            assert!(
                check
                    .detail
                    .contains("rebuild with --features sqlite to inspect"),
                "{}",
                check.detail
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
