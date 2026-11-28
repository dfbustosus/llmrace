//! Report writing, metric export, history recording and CI gates.

use super::config::RunConfig;
use anyhow::{Context, Result};
use llmrace::bench::LaneSummary;
use llmrace::report;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Atomically writes `bytes` to `path` via a sibling temporary file and rename.
///
/// Creates missing parent directories, writes and flushes `.llmrace-<pid>-<seq>.tmp`
/// in the same directory, then renames it into place. Removes the temporary file
/// if writing, flushing, or renaming fails.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating directory {}", parent.display()))?;
        }
    }
    let seq = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp_name = format!(".llmrace-{}-{seq}.tmp", std::process::id());
    let tmp_path = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(tmp_name),
        _ => PathBuf::from(tmp_name),
    };
    let write_res = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)?;
        file.write_all(bytes)?;
        file.flush()?;
        Ok(())
    })();
    if let Err(err) = write_res {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err).with_context(|| format!("writing {}", path.display()));
    }
    if let Err(err) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err).with_context(|| format!("writing {}", path.display()));
    }
    Ok(())
}

/// Writes the markdown/JSON/CSV/HTML reports requested by the config.
pub(super) fn write_reports(
    summaries: &[LaneSummary],
    cfg: &RunConfig,
    regression: Option<&llmrace::regression::RegressionReport>,
) -> Result<()> {
    // Report the rounds actually measured: `--duration` uses a sentinel
    // round count internally and a graceful interrupt may stop early —
    // neither belongs in report metadata.
    let actual_runs = summaries
        .iter()
        .map(|s| s.measured_rounds())
        .max()
        .unwrap_or(cfg.bench.runs);
    let mut md = report::markdown(summaries, &cfg.prompt, actual_runs, cfg.bench.max_tokens);
    if let Some(note) = report::sampling_note(&cfg.bench.sampling) {
        report::insert_note(&mut md, &note);
    }
    if let Some(note) = report::cache_note(summaries) {
        report::insert_note(&mut md, &note);
    }
    if cfg.winner {
        let weights = llmrace::score::applied_weights(summaries, &cfg.weights);
        let groups = llmrace::score::score_groups(summaries, &weights);
        let block = llmrace::score::podiums(&groups, &weights);
        if !block.is_empty() {
            md.push_str("\n## Podium\n");
            md.push_str(&block);
        }
    }
    if cfg.histogram {
        md.push_str(&report::histogram_section(summaries));
    }
    if let Some(regression) = regression {
        md.push_str(&regression.markdown_section(cfg.max_regression, cfg.require_significance));
        if cfg.fail_on_inconclusive && regression.inconclusive(cfg.require_significance) {
            md.push_str(
                "\nThe gate fails because `--fail-on-inconclusive` requires usable evidence.\n",
            );
        }
    }
    let out_ext = if cfg.out == "-" {
        None
    } else {
        Path::new(&cfg.out)
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
    };

    if cfg.out == "-" {
        // markdown to stdout (piping into CI summaries); sibling formats
        // fall back to the conventional filenames
        println!("{md}");
    } else {
        let md_path = match out_ext.as_deref() {
            Some("json" | "csv" | "html" | "xml" | "sarif") => {
                crate::report_sibling(&cfg.out, "md")
            }
            _ => cfg.out.clone(),
        };
        write_atomic(Path::new(&md_path), md.as_bytes())?;
        cfg.status(format!("\u{1f4c4} markdown report saved to {md_path}"));
    }

    if cfg.json || out_ext.as_deref() == Some("json") {
        let request = race_request_sha256(cfg);
        let variants = race_experiment_variants(cfg);
        let variant_refs: Vec<(&str, &str)> = variants
            .iter()
            .map(|(name, prompt)| (name.as_str(), prompt.as_str()))
            .collect();
        let regression_verdict = regression.map(|r| {
            r.verdict_json_with_policy(
                cfg.max_regression,
                cfg.require_significance,
                cfg.fail_on_inconclusive,
            )
        });
        let opts = report::JsonReportOptions {
            region: cfg.region.as_deref(),
            sent: Some(report::SentPrompts {
                prompt: &cfg.bench.prompt,
                workload: &cfg.bench.workload,
                warmup: cfg.bench.warmup,
            }),
            request_sha256: Some(&request),
            sampling: Some(&cfg.bench.sampling),
            experiment: Some((&cfg.bench, &variant_refs)),
            regression: regression_verdict.as_ref(),
        };
        let j = report::render_json_report(
            summaries,
            &cfg.prompt,
            actual_runs,
            cfg.bench.max_tokens,
            &opts,
        );
        let path = crate::report_sibling(&cfg.out, "json");
        write_atomic(Path::new(&path), j.as_bytes())?;
        cfg.status(format!("\u{1f4ca} json report saved to {path}"));
    }
    if cfg.csv || out_ext.as_deref() == Some("csv") {
        let c = report::csv(summaries);
        let path = crate::report_sibling(&cfg.out, "csv");
        write_atomic(Path::new(&path), c.as_bytes())?;
        cfg.status(format!("\u{1f4c8} csv report saved to {path}"));
    }
    if cfg.junit || out_ext.as_deref() == Some("xml") {
        let x = report::junit_xml_with_gate(
            summaries,
            regression,
            cfg.require_significance,
            cfg.fail_on_inconclusive,
        );
        let path = crate::report_sibling(&cfg.out, "xml");
        write_atomic(Path::new(&path), x.as_bytes())?;
        cfg.status(format!("\u{1f9ea} junit report saved to {path}"));
    }
    if cfg.sarif || out_ext.as_deref() == Some("sarif") {
        let s = llmrace::sarif::sarif_with_policy(
            summaries,
            regression.map(|r| (r, cfg.max_regression, cfg.require_significance)),
            cfg.max_ttfb,
            cfg.fail_on_inconclusive,
        );
        let path = crate::report_sibling(&cfg.out, "sarif");
        write_atomic(Path::new(&path), s.as_bytes())?;
        cfg.status(format!("\u{1f6e1} sarif report saved to {path}"));
    }
    if cfg.html || out_ext.as_deref() == Some("html") {
        let groups = if cfg.winner {
            let weights = llmrace::score::applied_weights(summaries, &cfg.weights);
            llmrace::score::score_groups(summaries, &weights)
        } else {
            Vec::new()
        };
        let h = report::html_report_with_sampling(
            summaries,
            &cfg.prompt,
            actual_runs,
            cfg.bench.max_tokens,
            &groups,
            Some(&cfg.bench.sampling),
        );
        let path = crate::report_sibling(&cfg.out, "html");
        write_atomic(Path::new(&path), h.as_bytes())?;
        cfg.status(format!("\u{1f310} html report saved to {path}"));
    }
    Ok(())
}

/// Pushes metrics to the Prometheus pushgateway and/or writes them to a
/// file, per configuration.
pub(super) async fn export_metrics(summaries: &[LaneSummary], cfg: &RunConfig) -> Result<()> {
    if let Some(gateway) = &cfg.push_gateway {
        let redacted = llmrace::util::redact_url(gateway);
        llmrace::prom::push(&cfg.client, gateway, summaries)
            .await
            .with_context(|| format!("pushing metrics to {redacted}"))?;
        cfg.status(format!("\u{1f4e1} metrics pushed to {redacted}"));
    }
    if let Some(path) = &cfg.prom_output {
        let body = llmrace::prom::render_metrics(summaries);
        write_atomic(Path::new(path), body.as_bytes())
            .with_context(|| format!("writing Prometheus metrics to {path}"))?;
        cfg.status(format!("\u{1f4e1} Prometheus metrics saved to {path}"));
    }
    Ok(())
}

fn race_request_sha256(cfg: &RunConfig) -> String {
    let tools = cfg.bench.tools.then_some(cfg.bench.tool_task.as_str());
    report::request_sha256(
        &cfg.bench.prompt,
        &cfg.bench.workload,
        &cfg.bench.sampling,
        cfg.bench.max_tokens,
        tools,
    )
}

fn race_experiment_sha256(cfg: &RunConfig) -> String {
    report::experiment_sha256(&cfg.bench, &race_experiment_variants(cfg))
}

fn race_experiment_variants(cfg: &RunConfig) -> Vec<(String, String)> {
    cfg.sweep
        .as_deref()
        .and_then(|spec| llmrace::variants::parse_with_custom(spec, &cfg.custom_variants).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|v| (v.name, v.prompt))
        .collect()
}

/// Persists history records using the selected storage backend (`jsonl` or `sqlite`)
/// and returns the destination path.
pub(crate) fn record_history_entries(
    storage: &str,
    records: &[llmrace::history::HistoryRecord],
) -> Result<PathBuf> {
    match storage {
        "jsonl" => {
            let path = llmrace::history::history_path();
            llmrace::history::append(&path, records)
                .with_context(|| format!("recording history to {}", path.display()))?;
            Ok(path)
        }
        #[cfg(feature = "sqlite")]
        "sqlite" => {
            let path = llmrace::history::sqlite_history_path();
            let conn = llmrace::sqlite::open(&path).with_context(|| "opening sqlite history")?;
            llmrace::sqlite::save(&conn, records).with_context(|| "writing sqlite history")?;
            Ok(path)
        }
        #[cfg(not(feature = "sqlite"))]
        "sqlite" => anyhow::bail!("sqlite backend requires rebuilding with --features sqlite"),
        other => anyhow::bail!("unknown storage backend '{other}' (expected jsonl or sqlite)"),
    }
}

/// Loads history records from the selected storage backend (`jsonl` or `sqlite`).
pub(crate) fn load_history_entries(storage: &str) -> Result<Vec<llmrace::history::HistoryRecord>> {
    match storage {
        "jsonl" => Ok(llmrace::history::load(&llmrace::history::history_path())),
        #[cfg(feature = "sqlite")]
        "sqlite" => {
            let conn = llmrace::sqlite::open(&llmrace::history::sqlite_history_path())
                .with_context(|| "opening sqlite history")?;
            Ok(llmrace::sqlite::load(&conn).with_context(|| "reading sqlite history")?)
        }
        #[cfg(not(feature = "sqlite"))]
        "sqlite" => anyhow::bail!("sqlite backend requires rebuilding with --features sqlite"),
        other => anyhow::bail!("unknown storage backend '{other}' (expected jsonl or sqlite)"),
    }
}

/// Appends lanes to the persistent history (jsonl or sqlite backend).
pub(super) fn record_history(summaries: &[LaneSummary], cfg: &RunConfig) -> Result<()> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let request = race_request_sha256(cfg);
    let records: Vec<llmrace::history::HistoryRecord> = summaries
        .iter()
        .map(|s| {
            let mut record = llmrace::history::record_from_summary(s, now);
            record.request_sha256 = Some(request.clone());
            record
        })
        .collect();
    let path = record_history_entries(&cfg.storage, &records)?;
    cfg.status(format!(
        "\u{1f5c2} recorded {} lane(s) to {}",
        records.len(),
        path.display()
    ));
    Ok(())
}

/// `--max-ttfb` gate: returns `false` when any provider exceeded the
/// threshold (the process should exit non-zero).
pub(super) fn ttfb_gate(summaries: &[LaneSummary], cfg: &RunConfig) -> bool {
    let Some(max_ttfb) = cfg.max_ttfb else {
        return true;
    };
    if summaries.iter().any(|s| !s.successful()) {
        eprintln!("TTFB gate failed: a requested lane has no successful measurements");
        return false;
    }
    let offenders = llmrace::gates::ttfb_offenders(summaries, max_ttfb);
    let verdict = llmrace::gates::render_ttfb_verdict(&offenders, max_ttfb, cfg.quiet);
    if verdict.failed() {
        eprintln!("\n{}", verdict.message());
        return false;
    }
    if !verdict.message().is_empty() {
        cfg.status(verdict.message());
    }
    true
}

/// Reads the baseline once. `Ok(None)` means this run has no `--baseline`.
/// A missing or unreadable file is an error, reported after the race
/// report has already been written.
pub(super) fn load_regression(
    summaries: &[LaneSummary],
    cfg: &RunConfig,
) -> Result<Option<llmrace::regression::RegressionReport>> {
    let Some(baseline_path) = &cfg.baseline else {
        return Ok(None);
    };
    let raw = std::fs::read_to_string(baseline_path)
        .with_context(|| format!("reading baseline {baseline_path}"))?;
    let baseline = llmrace::regression::BaselineReport::from_json_str(&raw)
        .with_context(|| format!("parsing baseline {baseline_path}"))?;
    let request = race_request_sha256(cfg);
    Ok(Some(llmrace::regression::evaluate_experiment(
        summaries,
        &baseline,
        cfg.max_regression,
        Some(&request),
        Some(&race_experiment_sha256(cfg)),
    )))
}

/// `--baseline` regression gate: returns `false` on regression.
pub(super) fn regression_gate(
    regression: Option<&llmrace::regression::RegressionReport>,
    cfg: &RunConfig,
) -> bool {
    let Some(regression) = regression else {
        return true;
    };
    cfg.status(format!(
        "\n{}",
        regression.terminal_table(cfg.max_regression)
    ));
    if regression.inconclusive(cfg.require_significance) {
        cfg.status("inconclusive: a breached metric has no usable significance test; this is not evidence of equivalence");
    } else if cfg.require_significance
        && regression.any_regressed()
        && !regression.any_regressed_significant()
    {
        cfg.status("\u{2705} threshold breached but no regression is statistically significant — treating as noise (--require-significance)");
    }
    !regression.gate_failed_with_policy(cfg.require_significance, cfg.fail_on_inconclusive)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_atomic_creates_parents_replaces_and_leaves_no_temp_files() {
        let dir =
            std::env::temp_dir().join(format!("llmrace-write-atomic-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("report.md");

        write_atomic(&path, b"first").expect("initial atomic write");
        assert_eq!(std::fs::read(&path).expect("read first"), b"first");

        write_atomic(&path, b"second").expect("overwrite atomic write");
        assert_eq!(std::fs::read(&path).expect("read second"), b"second");

        let leftover_tmp: Vec<_> = std::fs::read_dir(dir.join("nested"))
            .expect("read nested dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(
            leftover_tmp.is_empty(),
            "leftover tmp files: {leftover_tmp:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn storage_backend_rejects_unknown_and_explains_missing_sqlite_feature() {
        let err = load_history_entries("postgres").expect_err("unknown backend must fail");
        assert!(err.to_string().contains("unknown storage backend"), "{err}");

        #[cfg(not(feature = "sqlite"))]
        {
            let err = load_history_entries("sqlite").expect_err("sqlite without feature must fail");
            assert!(
                err.to_string()
                    .contains("sqlite backend requires rebuilding with --features sqlite"),
                "{err}"
            );
            let err =
                record_history_entries("sqlite", &[]).expect_err("sqlite write without feature");
            assert!(
                err.to_string()
                    .contains("sqlite backend requires rebuilding with --features sqlite"),
                "{err}"
            );
        }
    }
}
