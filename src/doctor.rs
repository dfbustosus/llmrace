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
        .filter(|spec| *spec.id != *"mock")
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
        .map_err(|e| crate::util::truncate(&e.to_string(), 90))?;
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
            let body = r.text().await.unwrap_or_default();
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
                crate::util::truncate(&e.to_string(), 60)
            ),
        },
    }
}

/// Checks an opt-in local OpenAI-compatible daemon (LM Studio on 1234,
/// llama.cpp llama-server on 8080): a `Warn` when it is not running is
/// expected — these providers only race when named explicitly.
pub async fn check_local_daemon(client: &reqwest::Client, name: &str, port: u16) -> Check {
    let resp = client
        .get(format!("http://localhost:{port}/v1/models"))
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await;
    match resp {
        Ok(r) if r.status().is_success() => Check {
            name: name.into(),
            status: Status::Pass,
            detail: format!("daemon reachable on :{port}"),
        },
        Ok(r) => Check {
            name: name.into(),
            status: Status::Warn,
            detail: format!("answered HTTP {} on :{port}", r.status()),
        },
        Err(_) => Check {
            name: name.into(),
            status: Status::Warn,
            detail: format!("not running on :{port} (fine — opt-in provider)"),
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

/// Checks history readability and size.
pub fn check_history() -> Check {
    let path = crate::history::history_path();
    if !path.exists() {
        return Check {
            name: "history".into(),
            status: Status::Warn,
            detail: format!("no history yet at {}", path.display()),
        };
    }
    match std::fs::read_to_string(&path) {
        Ok(raw) => {
            let total = raw.lines().filter(|l| !l.trim().is_empty()).count();
            let valid = crate::history::load(&path).len();
            let status = if valid == total {
                Status::Pass
            } else {
                Status::Warn
            };
            Check {
                name: "history".into(),
                status,
                detail: format!("{valid}/{total} valid record(s) in {}", path.display()),
            }
        }
        Err(e) => Check {
            name: "history".into(),
            status: Status::Fail,
            detail: format!("{} unreadable: {e}", path.display()),
        },
    }
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
