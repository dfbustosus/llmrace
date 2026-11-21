use llmrace::provider::split_model_override;
use std::process::Command;

fn tmppath(name: &str) -> String {
    std::env::temp_dir()
        .join(format!("llmrace-{name}.md"))
        .to_string_lossy()
        .into_owned()
}

#[test]
fn completions_generate_for_every_shell() {
    for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
        let out = Command::new(env!("CARGO_BIN_EXE_llmrace"))
            .arg("--completions")
            .arg(shell)
            .output()
            .expect("spawn llmrace");
        assert!(out.status.success(), "{shell} should succeed");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.len() > 200,
            "{shell} output too small: {}",
            stdout.len()
        );
    }
}

#[test]
fn completions_reject_unknown_shell() {
    let out = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .arg("--completions")
        .arg("ksh")
        .output()
        .expect("spawn llmrace");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown shell"), "{stderr}");
}

#[test]
fn man_page_is_roff() {
    let out = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .arg("--man")
        .output()
        .unwrap();
    assert!(out.status.success());
    let man = String::from_utf8_lossy(&out.stdout);
    assert!(man.contains(".TH llmrace 1"));
    assert!(man.contains(".SH NAME"));
    assert!(man.contains("baseline"));
}

#[test]
fn price_overrides_parse_and_apply() {
    let ok = llmrace::cost::parse_override("openai/gpt-4o-mini=1.25:5").unwrap();
    assert_eq!(ok, ("openai/gpt-4o-mini".into(), 1.25, 5.0));

    for bad in ["nope", "a=b", "a=1:x", "a=-1:2", "a=1:-2"] {
        assert!(llmrace::cost::parse_override(bad).is_err(), "{bad}");
    }

    llmrace::cost::set_overrides(vec![("custom/unlisted".into(), 3.0, 9.0)]);
    assert_eq!(
        llmrace::cost::pricing("custom", "unlisted"),
        Some((3.0, 9.0))
    );
    assert!(llmrace::cost::parse_override("openai/gpt-4o-mini=0.15:0.60:0.075").is_err());

    let cached = llmrace::cost::parse_price_override("openai/gpt-4o-mini=0.15:0.60:0.075").unwrap();
    assert_eq!(cached.rates.cache_read, Some(0.075));
    assert!(cached.rates.cache_write.is_none());
    let full =
        llmrace::cost::parse_price_override("anthropic/claude-sonnet-4-5=3:15:0.3:3.75:6").unwrap();
    assert_eq!(full.rates.cache_write, Some(3.75));
    assert_eq!(full.rates.cache_write_1h, Some(6.0));
    assert!(llmrace::cost::parse_price_override("a=1:2:3:4:5:6").is_err());
    assert!(llmrace::cost::parse_price_override("a=1:2::4").is_err());
    assert!(llmrace::cost::parse_price_override("a=nan:1").is_err());
}

#[test]
fn subms_ttfb_renders_as_less_than_one() {
    assert_eq!(llmrace::stats::fmt_ms(0.4), "<1");
    assert_eq!(llmrace::stats::fmt_ms(0.0), "0");
    assert_eq!(llmrace::stats::fmt_ms(14.2), "14");
}

#[test]
fn list_json_and_doctor_json_are_valid_json() {
    let list = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .args(["--list", "--json"])
        .output()
        .unwrap();
    assert!(list.status.success());
    let parsed: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    let ids: Vec<&str> = parsed
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"openai"));
    assert!(ids.contains(&"ollama"));
    assert!(ids.len() >= 14);

    let doctor = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .args(["--doctor", "--json"])
        .output()
        .unwrap();
    assert!(doctor.status.success());
    let parsed: serde_json::Value = serde_json::from_slice(&doctor.stdout).unwrap();
    let names: Vec<&str> = parsed
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"config"));
    assert!(names.contains(&"history"));
}

#[test]
fn model_overrides_parse_correctly() {
    assert_eq!(
        split_model_override("ollama=llama3.2:1b"),
        Some(("ollama".into(), "llama3.2:1b".into()))
    );
    assert_eq!(split_model_override("gpt-4o-mini"), None);
    assert_eq!(split_model_override("ollama="), None);
    assert_eq!(split_model_override("=model"), None);
}

#[test]
fn per_provider_model_flag_targets_one_lane() {
    let out = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .args([
            "mock",
            "--no-tui",
            "-n",
            "1",
            "--warmup",
            "0",
            "-m",
            "mock=custom-model-9",
            "--json",
            "--out",
            tmppath("pm").as_str(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json_path = tmppath("pm").replace(".md", ".json");
    let raw = std::fs::read_to_string(&json_path)
        .unwrap_or_else(|_| panic!("expected json report at {json_path}"));
    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(parsed["providers"][0]["model"], "custom-model-9");
}

#[test]
fn global_model_flag_applies_to_all_lanes() {
    let out = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .args([
            "mock",
            "--no-tui",
            "-n",
            "1",
            "--warmup",
            "0",
            "-m",
            "gpt-4o-mini",
            "--json",
            "--out",
            tmppath("gm").as_str(),
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let json_path = tmppath("gm").replace(".md", ".json");
    let raw = std::fs::read_to_string(&json_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(parsed["providers"][0]["model"], "gpt-4o-mini");
}

#[test]
fn init_creates_config_and_fails_on_existing() {
    let dir = std::env::temp_dir().join(format!("llmrace-init-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .arg("--init")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(dir.join("llmrace.toml").exists());
    let config = std::fs::read_to_string(dir.join("llmrace.toml")).unwrap();
    assert!(config.contains("[profiles.fast]"));
    assert!(config.contains("[profiles.ci]"));

    let dup = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .arg("--init")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(!dup.status.success());
    assert!(String::from_utf8_lossy(&dup.stderr).contains("already exists"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn quiet_mode_suppresses_output() {
    let out = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .args([
            "mock",
            "--no-tui",
            "--quiet",
            "-n",
            "1",
            "--warmup",
            "0",
            "--out",
            tmppath("quiet").as_str(),
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("🏁"),
        "quiet must suppress banner: {stdout}"
    );
    assert!(
        !stdout.contains("✓ mock"),
        "quiet must suppress progress: {stdout}"
    );
}

#[test]
fn max_ttfb_gate_fails_and_passes() {
    let fail = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .args([
            "mock",
            "--no-tui",
            "--quiet",
            "-n",
            "1",
            "--warmup",
            "0",
            "--max-ttfb",
            "1",
        ])
        .output()
        .unwrap();
    assert!(!fail.status.success(), "TTFB ~14ms must fail a 1ms gate");
    let stderr = String::from_utf8_lossy(&fail.stderr);
    assert!(stderr.contains("TTFB GATE FAILED"), "{stderr}");
    assert!(stderr.contains("mock"), "{stderr}");

    let pass = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .args([
            "mock",
            "--no-tui",
            "--quiet",
            "-n",
            "1",
            "--warmup",
            "0",
            "--max-ttfb",
            "1000",
        ])
        .output()
        .unwrap();
    assert!(pass.status.success());
}

#[test]
fn provider_model_positional_races_named_model() {
    let out = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .arg("mock=demo-model")
        .arg("--no-tui")
        .arg("-n")
        .arg("1")
        .arg("--warmup")
        .arg("0")
        .arg("--json")
        .arg("--out")
        .arg(tmppath("positional"))
        .output()
        .expect("spawn llmrace");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json = std::fs::read_to_string(std::env::temp_dir().join("llmrace-positional.json"))
        .expect("json report written");
    assert!(json.contains("demo-model"), "{json}");
}

#[test]
fn same_provider_twice_races_two_models() {
    let out = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .arg("mock=model-a")
        .arg("mock=model-b")
        .arg("--no-tui")
        .arg("-n")
        .arg("1")
        .arg("--warmup")
        .arg("0")
        .arg("--json")
        .arg("--out")
        .arg(tmppath("twice"))
        .output()
        .expect("spawn llmrace");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json = std::fs::read_to_string(std::env::temp_dir().join("llmrace-twice.json"))
        .expect("json report written");
    assert!(json.contains("model-a"), "{json}");
    assert!(json.contains("model-b"), "{json}");
}

#[test]
fn positional_with_unknown_provider_errors() {
    let out = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .arg("bogus=model-x")
        .arg("--no-tui")
        .output()
        .expect("spawn llmrace");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown provider"), "{stderr}");
}

#[test]
fn api_keys_never_leak_into_reports() {
    // A distinctive fake key: if any output path ever echoes environment
    // values, this test catches it.
    const MARKER: &str = "sk-LEAK-MARKER-do-not-print";
    let out = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .env("OPENAI_API_KEY", MARKER)
        .arg("mock")
        .arg("--no-tui")
        .arg("-n")
        .arg("1")
        .arg("--warmup")
        .arg("0")
        .arg("--json")
        .arg("--csv")
        .arg("--html")
        .arg("--out")
        .arg(tmppath("leak"))
        .output()
        .expect("spawn llmrace");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains(MARKER), "key leaked to stdout");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains(MARKER), "key leaked to stderr");
    for ext in ["md", "json", "csv", "html"] {
        let path = std::env::temp_dir().join(format!(
            "llmrace-leak{}",
            if ext == "md" {
                ".md".to_string()
            } else {
                format!(".{ext}")
            }
        ));
        if let Ok(content) = std::fs::read_to_string(&path) {
            assert!(!content.contains(MARKER), "key leaked into {ext} report");
        }
    }
}

#[test]
fn man_page_renders() {
    let out = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .arg("--man")
        .output()
        .expect("spawn llmrace");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(".TH"),
        "roff header expected: {} bytes",
        stdout.len()
    );
    assert!(stdout.contains("llmrace"));
}

#[test]
fn doctor_json_is_structured() {
    let out = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .arg("--doctor")
        .arg("--json")
        .output()
        .expect("spawn llmrace");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    let checks = parsed.as_array().expect("array of checks");
    assert!(!checks.is_empty());
    assert!(checks
        .iter()
        .all(|c| c.get("name").is_some() && c.get("status").is_some()));
}

#[test]
fn trend_with_empty_history_renders_hint() {
    // XDG_DATA_HOME pointed at an empty temp dir — no history exists there
    let out = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .env("XDG_DATA_HOME", std::env::temp_dir().join("llmrace-empty"))
        .arg("--trend")
        .output()
        .expect("spawn llmrace");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("no history yet"), "{stdout}");
}

#[test]
fn compare_with_missing_file_errors() {
    let out = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .arg("--compare")
        .arg("/tmp/does-not-exist-a.json,/tmp/does-not-exist-b.json")
        .output()
        .expect("spawn llmrace");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("reading"), "{stderr}");
}

#[cfg(unix)]
#[test]
fn watch_mode_runs_records_and_exits_cleanly_on_sigint() {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let data_dir = std::env::temp_dir().join("llmrace-watch-test");
    let _ = std::fs::remove_dir_all(&data_dir);
    let out_path = std::env::temp_dir().join("llmrace-watch.md");

    let mut child = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .arg("mock")
        .arg("--watch")
        .arg("--every")
        .arg("1")
        .arg("-n")
        .arg("1")
        .arg("--warmup")
        .arg("0")
        .arg("--record")
        .env("XDG_DATA_HOME", &data_dir)
        .arg("--out")
        .arg(&out_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn llmrace --watch");

    // let at least one round finish, then interrupt
    std::thread::sleep(Duration::from_secs(3));
    let status = Command::new("kill")
        .arg("-INT")
        .arg(child.id().to_string())
        .status()
        .expect("send SIGINT");

    assert!(status.success(), "kill -INT should succeed");
    let deadline = Instant::now() + Duration::from_secs(25);
    let out = loop {
        if let Ok(Some(_)) = child.try_wait() {
            break child.wait_with_output().expect("collect output");
        }
        assert!(
            Instant::now() < deadline,
            "watch did not exit after SIGINT (watchdog)"
        );
        std::thread::sleep(Duration::from_millis(300));
        if Instant::now() > deadline - Duration::from_secs(20) {
            let _ = Command::new("kill")
                .arg("-9")
                .arg(child.id().to_string())
                .status();
        }
    };

    assert!(
        out.status.success(),
        "graceful exit expected, got {:?} — {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("session:"), "{stdout}");
    assert!(stdout.contains("interrupted"), "{stdout}");

    let report = std::fs::read_to_string(&out_path).expect("final report written");
    assert!(report.contains("llmrace report"), "{report}");

    let history_path = data_dir.join("llmrace/history.jsonl");
    let mut history = String::new();
    std::fs::File::open(&history_path)
        .expect("history recorded")
        .read_to_string(&mut history)
        .unwrap();
    assert!(history.contains("\"provider\":\"mock\""), "{history}");
}

#[test]
fn tools_conflicts_with_sweep() {
    let out = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .arg("mock")
        .arg("--tools")
        .arg("--sweep")
        .arg("short,json")
        .arg("--no-tui")
        .output()
        .expect("spawn llmrace");
    assert!(!out.status.success(), "conflicting flags must be rejected");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("cannot be used with"), "{stderr}");
}

#[test]
fn schema_is_valid_json_and_matches_reports() {
    let out = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .arg("--schema")
        .output()
        .expect("spawn llmrace");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let schema: serde_json::Value = serde_json::from_str(&stdout).expect("schema is valid JSON");
    assert_eq!(
        schema["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );

    // every top-level required property exists in the schema, and every
    // top-level key a real report emits is described by it
    let report = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .arg("mock")
        .arg("--no-tui")
        .arg("-n")
        .arg("1")
        .arg("--warmup")
        .arg("0")
        .arg("--json")
        .arg("--out")
        .arg(
            std::env::temp_dir()
                .join("llmrace-schema.md")
                .to_string_lossy()
                .into_owned(),
        )
        .output()
        .unwrap();
    assert!(report.status.success());
    let json_path = std::env::temp_dir().join("llmrace-schema.json");
    let generated: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json_path).unwrap()).unwrap();
    for key in generated.as_object().unwrap().keys() {
        assert!(
            schema["properties"].as_object().unwrap().contains_key(key),
            "schema is missing report key '{key}'"
        );
    }
    for key in schema["required"].as_array().unwrap() {
        assert!(
            generated
                .as_object()
                .unwrap()
                .contains_key(key.as_str().unwrap()),
            "report is missing required key '{}'",
            key
        );
    }
}

#[test]
fn ramp_runs_phases_and_writes_curve() {
    let out = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .arg("mock")
        .arg("--ramp")
        .arg("1,2")
        .arg("--no-tui")
        .arg("-n")
        .arg("1")
        .arg("--warmup")
        .arg("0")
        .arg("--json")
        .arg("--out")
        .arg(tmppath("ramp"))
        .output()
        .expect("spawn llmrace");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let md = std::fs::read_to_string(tmppath("ramp")).unwrap();
    assert!(md.contains("Saturation curve"), "{md}");
    assert!(md.contains("c=1"), "{md}");
    let json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(std::env::temp_dir().join("llmrace-ramp.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(json["mode"], "ramp");
    let ramp = json["ramp"].as_array().unwrap();
    assert_eq!(ramp[0]["provider"], "mock");
    // mock concurrency scales linearly: c=2 roughly doubles c=1
    let tps = ramp[0]["tokens_per_sec_by_concurrency"]
        .as_object()
        .unwrap();
    let one = tps["1"].as_f64().unwrap();
    let two = tps["2"].as_f64().unwrap();
    assert!(two > one * 1.8, "expected ~2x scaling: {one} -> {two}");
    assert_eq!(ramp[0]["peak_concurrency"], 2);
}

#[test]
fn ramp_conflicts_and_validation() {
    let conflict = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .arg("mock")
        .arg("--ramp")
        .arg("1,2")
        .arg("-c")
        .arg("4")
        .arg("--no-tui")
        .output()
        .unwrap();
    assert!(!conflict.status.success());

    for bad in ["0,2", "1", "1,x"] {
        let out = Command::new(env!("CARGO_BIN_EXE_llmrace"))
            .arg("mock")
            .arg("--ramp")
            .arg(bad)
            .arg("--no-tui")
            .output()
            .unwrap();
        assert!(!out.status.success(), "--ramp {bad} must be rejected");
    }
}

#[test]
fn json_report_carries_the_gate_verdict() {
    let dir = std::env::temp_dir().join(format!("llmrace-verdict-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let baseline = dir.join("baseline.md");
    let again = dir.join("again.md");
    let record = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .args([
            "mock", "--no-tui", "-n", "2", "--warmup", "0", "--quiet", "--json", "--out",
        ])
        .arg(&baseline)
        .output()
        .unwrap();
    assert!(
        record.status.success(),
        "{}",
        String::from_utf8_lossy(&record.stderr)
    );
    let recorded: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(baseline.with_extension("json")).unwrap())
            .unwrap();
    assert!(recorded.get("regression").is_none());

    let gate = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .args([
            "mock",
            "--no-tui",
            "-n",
            "2",
            "--warmup",
            "0",
            "--quiet",
            "--json",
            "--max-regression",
            "10000",
            "--baseline",
        ])
        .arg(baseline.with_extension("json"))
        .arg("--out")
        .arg(&again)
        .output()
        .unwrap();
    assert!(
        gate.status.success(),
        "{}",
        String::from_utf8_lossy(&gate.stderr)
    );
    let body: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(again.with_extension("json")).unwrap())
            .unwrap();
    assert_eq!(body["regression"]["ok"], true);
    assert_eq!(body["regression"]["request_mismatch"], false);
    assert!(body["regression"]["failures"]
        .as_array()
        .unwrap()
        .is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn baseline_gate_end_to_end_passes_on_stable_provider() {
    // mock is deterministic: a baseline recorded now gates a replay cleanly
    let baseline_path = std::env::temp_dir().join("llmrace-baseline-e2e.json");
    let md_path = std::env::temp_dir().join("llmrace-baseline-e2e.md");

    let record = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .arg("mock")
        .arg("--no-tui")
        .arg("-n")
        .arg("3")
        .arg("--warmup")
        .arg("0")
        .arg("--json")
        .arg("--out")
        .arg(md_path.to_string_lossy().into_owned())
        .output()
        .unwrap();
    assert!(record.status.success());
    let baseline_json = std::env::temp_dir().join("llmrace-baseline-e2e.json");
    std::fs::rename(&baseline_json, &baseline_path).unwrap();

    // This test exercises gate MECHANICS (record → replay → verdict), not
    // statistics: under CI load the mock's timing varies across processes,
    // and a threshold of 15% can be breached *significantly*. A threshold
    // no measurement can reach keeps the test deterministic while still
    // proving p-value rendering and the pass verdict.
    let gate = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .arg("mock")
        .arg("--no-tui")
        .arg("-n")
        .arg("3")
        .arg("--warmup")
        .arg("0")
        .arg("--baseline")
        .arg(baseline_path.to_string_lossy().into_owned())
        .arg("--max-regression")
        .arg("10000")
        .arg("--require-significance")
        .arg("--quiet")
        .arg("--out")
        .arg(md_path.to_string_lossy().into_owned())
        .output()
        .unwrap();
    assert!(
        gate.status.success(),
        "gate mechanics must pass with an unreachable threshold: {}",
        String::from_utf8_lossy(&gate.stderr)
    );
    let stdout = String::from_utf8_lossy(&gate.stdout);
    assert!(
        stdout.contains("no regressions beyond threshold"),
        "gate must print the pass verdict: {stdout}"
    );
    assert!(stdout.contains("p="), "p-values rendered: {stdout}");
}

#[cfg(unix)]
#[test]
fn watch_resolves_model_per_lane() {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let data_dir = std::env::temp_dir().join("llmrace-watch-models");
    let _ = std::fs::remove_dir_all(&data_dir);
    let out_path = std::env::temp_dir().join("llmrace-watch-models.md");

    let mut child = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .arg("mock=model-a")
        .arg("mock=model-b")
        .arg("--watch")
        .arg("--every")
        .arg("1")
        .arg("-n")
        .arg("1")
        .arg("--warmup")
        .arg("0")
        .arg("--record")
        .env("XDG_DATA_HOME", &data_dir)
        .arg("--out")
        .arg(&out_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn llmrace --watch");

    std::thread::sleep(Duration::from_secs(3));
    let _ = Command::new("kill")
        .arg("-INT")
        .arg(child.id().to_string())
        .status();
    let deadline = Instant::now() + Duration::from_secs(25);
    let out = loop {
        if let Ok(Some(_)) = child.try_wait() {
            break child.wait_with_output().expect("collect output");
        }
        assert!(Instant::now() < deadline, "watchdog: watch did not exit");
        std::thread::sleep(Duration::from_millis(300));
    };
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // both lanes must have raced (and recorded) their OWN model
    let history = std::fs::read_to_string(data_dir.join("llmrace/history.jsonl")).unwrap();
    assert!(history.contains("\"model\":\"model-a\""), "{history}");
    assert!(history.contains("\"model\":\"model-b\""), "{history}");
}

#[test]
fn out_dash_streams_markdown_to_stdout() {
    let out = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .arg("mock")
        .arg("--no-tui")
        .arg("-n")
        .arg("1")
        .arg("--warmup")
        .arg("0")
        .arg("--quiet")
        .arg("--out")
        .arg("-")
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("# 🏁 llmrace report"), "{stdout}");
    assert!(stdout.contains("| 1 | mock |"), "{stdout}");
    // no report file named '-' left behind
    assert!(!std::path::Path::new("-").exists());
}

#[test]
fn non_tty_stdout_falls_back_to_headless() {
    // cargo test pipes stdout, so the TUI is impossible here: without the
    // fallback this invocation used to garble or fail on a pipe
    let out = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .arg("mock")
        .arg("-n")
        .arg("1")
        .arg("--warmup")
        .arg("0")
        .arg("--out")
        .arg(tmppath("tty"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "piped stdout must run headless: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("headless"), "note expected: {stderr}");
}

#[test]
fn cli_contract_every_documented_flag_exists() {
    // The README stability policy declares CLI flags a contract. This test
    // is that contract, mechanically: removing or renaming any flag below
    // fails CI. Add new flags here when they are documented.
    let out = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .arg("--help")
        .output()
        .unwrap();
    let help = String::from_utf8_lossy(&out.stdout);
    let flags = [
        "--prompt",
        "--prompt-file",
        "--runs",
        "--warmup",
        "--duration",
        "--concurrency",
        "--model",
        "--max-tokens",
        "--temperature",
        "--top-p",
        "--seed",
        "--reproducible",
        "--system",
        "--timeout",
        "--retries",
        "--ramp",
        "--endpoint",
        "--provider-name",
        "--api-key-env",
        "--no-tui",
        "--json",
        "--csv",
        "--html",
        "--baseline",
        "--max-regression",
        "--require-significance",
        "--max-ttfb",
        "--out",
        "--record",
        "--storage",
        "--trend",
        "--dashboard",
        "--days",
        "--histogram",
        "--tools",
        "--push-gateway",
        "--watch",
        "--every",
        "--alert-rounds",
        "--sweep",
        "--profile",
        "--list",
        "--doctor",
        "--quiet",
        "--init",
        "--price",
        "--completions",
        "--man",
        "--schema",
        "--models",
        "--compare",
        "--merge",
        "--region",
        "--winner",
        "--weights",
        "--recommend",
        "--suite",
        "--prom-output",
        "--alert-webhook",
        "--notify-desktop",
        "--alert-test",
        "--version-json",
    ];
    let missing: Vec<&str> = flags
        .iter()
        .filter(|f| !help.contains(*f))
        .copied()
        .collect();
    assert!(
        missing.is_empty(),
        "CLI contract broken, missing: {missing:?}"
    );
}

#[test]
fn prom_output_writes_text_format_file() {
    let path = std::env::temp_dir().join("llmrace-prom-output-test.prom");
    let out = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .arg("mock")
        .arg("--no-tui")
        .arg("-n")
        .arg("1")
        .arg("--warmup")
        .arg("0")
        .arg("--quiet")
        .arg("--prom-output")
        .arg(path.to_string_lossy().into_owned())
        .output()
        .unwrap();
    assert!(out.status.success());
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(body.contains("llmrace_ttfb_ms{provider=\"mock\""), "{body}");
    assert!(body.contains("llmrace_tokens_per_second{"), "{body}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn suite_runs_multiple_races_and_writes_combined_report() {
    let dir = std::env::temp_dir();
    let suite_path = dir.join("llmrace-suite-test.toml");
    let out_path = dir.join("llmrace-suite-out.md");
    std::fs::write(
        &suite_path,
        r#"
[[suite]]
name = "quick"
description = "fast check"
providers = ["mock"]
runs = 1
warmup = 0
prompt = "hello"

[[suite]]
name = "sat"
description = "concurrent"
providers = ["mock"]
runs = 1
warmup = 0
concurrency = 4
prompt = "count"
"#,
    )
    .unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .arg("--suite")
        .arg(suite_path.to_string_lossy().into_owned())
        .arg("--quiet")
        .arg("--out")
        .arg(out_path.to_string_lossy().into_owned())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let suite_report = dir.join("llmrace-suite-out-suite.md");
    let body = std::fs::read_to_string(&suite_report).unwrap();
    assert!(body.contains("## quick"), "{body}");
    assert!(body.contains("## sat"), "{body}");
    assert!(body.contains("fast check"), "{body}");
    assert!(body.contains("concurrent"), "{body}");
    assert!(
        !body.contains("1 failed"),
        "no suite failures expected: {body}"
    );
}

#[test]
fn recommend_json_is_structured() {
    let out = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .arg("--recommend")
        .arg("--json")
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    assert_eq!(parsed["mode"], "recommend");
    assert!(parsed["providers"].as_array().is_some());
}

#[test]
fn sampling_flags_are_stamped_and_out_of_range_values_fail() {
    let path = std::env::temp_dir().join(format!("llmrace-sampling-{}.md", std::process::id()));
    let out = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .arg("mock")
        .arg("--no-tui")
        .arg("-n")
        .arg("1")
        .arg("--warmup")
        .arg("0")
        .arg("--quiet")
        .arg("--json")
        .arg("--html")
        .arg("--temperature")
        .arg("0")
        .arg("--top-p")
        .arg("1")
        .arg("--seed")
        .arg("7")
        .arg("--system")
        .arg("Be brief.")
        .arg("--out")
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json_path = path.with_extension("json");
    let body = std::fs::read_to_string(&json_path).unwrap();
    assert!(body.contains("\"temperature\": 0.0"), "{body}");
    assert!(body.contains("\"top_p\": 1.0"), "{body}");
    assert!(body.contains("\"seed\": 7"), "{body}");
    assert!(body.contains("Be brief."), "{body}");
    let md = std::fs::read_to_string(&path).unwrap();
    assert!(md.contains("**Sampling**:"), "{md}");
    assert!(md.contains("temperature 0"), "{md}");
    let html = std::fs::read_to_string(path.with_extension("html")).unwrap();
    assert!(html.contains("temperature 0"), "{html}");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&json_path);
    let _ = std::fs::remove_file(path.with_extension("html"));

    let bad = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .args([
            "mock",
            "--temperature",
            "9",
            "--no-tui",
            "-n",
            "1",
            "--warmup",
            "0",
        ])
        .output()
        .unwrap();
    assert!(!bad.status.success());
    let err = String::from_utf8_lossy(&bad.stderr);
    assert!(err.contains("temperature"), "{err}");
}

#[test]
fn reproducible_fills_only_the_sampling_controls_still_unset() {
    let dir = std::env::temp_dir().join(format!("llmrace-repro-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let report = dir.join("race.json");
    let run = |extra: &[&str]| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_llmrace"));
        cmd.current_dir(&dir).args([
            "mock", "--no-tui", "-n", "1", "--warmup", "0", "--quiet", "--json", "--out",
        ]);
        cmd.arg(&report);
        cmd.args(extra);
        let out = cmd.output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::fs::read_to_string(report.with_extension("json")).unwrap()
    };
    let pinned_body = run(&["--reproducible"]);
    assert!(
        pinned_body.contains("\"temperature\": 0.0"),
        "{pinned_body}"
    );
    assert!(pinned_body.contains("\"top_p\": 1.0"), "{pinned_body}");
    assert!(pinned_body.contains("\"seed\": 0"), "{pinned_body}");

    let plain_body = run(&[]);
    assert!(!plain_body.contains("\"temperature\""), "{plain_body}");
    let pinned_hash = serde_json::from_str::<serde_json::Value>(&pinned_body).unwrap();
    let plain_hash = serde_json::from_str::<serde_json::Value>(&plain_body).unwrap();
    assert_ne!(pinned_hash["request_sha256"], plain_hash["request_sha256"]);

    let kept_body = run(&["--reproducible", "--temperature", "0.2"]);
    assert!(kept_body.contains("\"temperature\": 0.2"), "{kept_body}");
    assert!(kept_body.contains("\"seed\": 0"), "{kept_body}");

    std::fs::write(
        dir.join("llmrace.toml"),
        "[profiles.p]\nproviders = [\"mock\"]\nruns = 1\nwarmup = 0\ntemperature = 0.4\n",
    )
    .unwrap();
    let profile_body = run(&["--profile", "p", "--reproducible"]);
    assert!(
        profile_body.contains("\"temperature\": 0.4"),
        "{profile_body}"
    );
    assert!(profile_body.contains("\"top_p\": 1.0"), "{profile_body}");
    assert!(profile_body.contains("\"seed\": 0"), "{profile_body}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn alert_test_requires_webhook() {
    let out = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .arg("--alert-test")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--alert-webhook"), "{stderr}");
}
