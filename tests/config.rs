use llmrace::config::Config;

const SAMPLE: &str = r#"
[profiles.fast]
providers = ["openai", "groq"]
prompt = "Reply with a single word."
runs = 1
warmup = 0
concurrency = 2

[profiles.overnight]
providers = ["openai", "anthropic", "groq", "ollama"]
runs = 20
warmup = 2
max_tokens = 1024
timeout = 300
"#;

#[test]
fn parses_profiles_and_fields() {
    let config = Config::from_toml_str(SAMPLE).expect("parse");
    assert_eq!(config.profiles.len(), 2);

    let fast = config.profile("fast").expect("fast");
    assert_eq!(fast.providers, vec!["openai", "groq"]);
    assert_eq!(fast.prompt.as_deref(), Some("Reply with a single word."));
    assert_eq!(fast.runs, Some(1));
    assert_eq!(fast.warmup, Some(0));
    assert_eq!(fast.concurrency, Some(2));
    assert_eq!(fast.model, None);

    let overnight = config.profile("overnight").expect("overnight");
    assert_eq!(overnight.runs, Some(20));
    assert_eq!(overnight.max_tokens, Some(1024));
    assert_eq!(overnight.timeout, Some(300));
    assert!(overnight.prompt.is_none());
}

#[test]
fn missing_profile_lists_available() {
    let config = Config::from_toml_str(SAMPLE).expect("parse");
    let err = config.profile("nope").expect_err("should fail");
    let msg = err.to_string();
    assert!(msg.contains("nope"));
    assert!(msg.contains("fast"));
    assert!(msg.contains("overnight"));
}

#[test]
fn malformed_toml_is_a_config_error() {
    let err = Config::from_toml_str("this is not = = toml").expect_err("should fail");
    assert!(err.to_string().contains("invalid llmrace.toml"));
}

#[test]
fn empty_config_has_no_profiles() {
    let config = Config::default();
    assert_eq!(config.available(), "");
}

#[test]
fn profile_typos_are_rejected_loudly() {
    let typo = Config::from_toml_str("[profiles.fast]\nrun = 3\n");
    let err = typo.expect_err("typo 'run' must be rejected");
    assert!(err.to_string().contains("run"), "{err}");
    assert!(
        err.to_string().contains("runs"),
        "error should suggest the right key: {err}"
    );

    let valid = Config::from_toml_str("[profiles.fast]\nruns = 3\n");
    assert!(valid.is_ok());
}

#[test]
fn profiles_accept_retries() {
    let config = Config::from_toml_str(
        r#"
        [profiles.flaky]
        providers = ["mock"]
        retries = 2
        "#,
    )
    .unwrap();
    let flaky = config.profile("flaky").unwrap();
    assert_eq!(flaky.retries, Some(2));
}

#[test]
fn explicit_flags_win_over_profile_values() {
    use std::process::Command;
    let dir = std::env::temp_dir().join("llmrace-profile-e2e");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("llmrace.toml"),
        r#"
        [profiles.p]
        providers = ["mock"]
        runs = 4
        warmup = 0
        "#,
    )
    .unwrap();

    let run = |extra: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_llmrace"))
            .current_dir(&dir)
            .arg("--profile")
            .arg("p")
            .arg("--no-tui")
            .arg("--quiet")
            .arg("--json")
            .args(extra)
            .output()
            .unwrap();
        let json_path = dir.join("llmrace-report.json");
        let json = std::fs::read_to_string(&json_path).unwrap_or_default();
        let _ = std::fs::remove_file(&json_path);
        (out, json)
    };

    // profile value applies when the flag is absent
    let (_, json) = run(&[]);
    assert!(json.contains("\"runs\": 4"), "{json}");

    // explicit short flag wins (also proves -n5-style combined parsing is moot)
    let (_, json) = run(&["-n", "1"]);
    assert!(json.contains("\"runs\": 1"), "{json}");

    // explicit long flag wins
    let (_, json) = run(&["--runs", "2"]);
    assert!(json.contains("\"runs\": 2"), "{json}");

    // equals-form also counts as explicit
    let (_, json) = run(&["--runs=3"]);
    assert!(json.contains("\"runs\": 3"), "{json}");
}

#[test]
fn explicit_max_tokens_wins_over_profile() {
    use std::process::Command;
    let dir = std::env::temp_dir().join("llmrace-profile-mt");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("llmrace.toml"),
        r#"
        [profiles.p]
        providers = ["mock"]
        runs = 1
        warmup = 0
        max_tokens = 100
        "#,
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .current_dir(&dir)
        .arg("--profile")
        .arg("p")
        .arg("--no-tui")
        .arg("--quiet")
        .arg("--json")
        .arg("--max-tokens")
        .arg("32")
        .output()
        .unwrap();
    assert!(out.status.success());
    let json = std::fs::read_to_string(dir.join("llmrace-report.json")).unwrap();
    assert!(json.contains("\"max_tokens\": 32"), "{json}");
}

#[test]
fn custom_variants_run_in_sweeps() {
    use std::process::Command;
    let dir = std::env::temp_dir().join("llmrace-custom-variants");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("llmrace.toml"),
        r#"
        [variants]
        sql = "Write one SQL join between users and orders."

        [profiles.p]
        providers = ["mock"]
        runs = 1
        warmup = 0
        "#,
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .current_dir(&dir)
        .arg("--profile")
        .arg("p")
        .arg("--no-tui")
        .arg("--quiet")
        .arg("--sweep")
        .arg("sql")
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json = std::fs::read_to_string(dir.join("llmrace-report.json")).unwrap();
    assert!(json.contains("\"variant\": \"sql\""), "{json}");
    let md = std::fs::read_to_string(dir.join("llmrace-report.md")).unwrap();
    assert!(md.contains("variant: `sql`"), "{md}");
    assert!(md.contains("custom variant"), "{md}");
}
