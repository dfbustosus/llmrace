use std::process::Command;

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
fn doctor_render_summarizes_statuses() {
    use llmrace::doctor::{render, Check, Status};
    let checks = vec![
        Check {
            name: "a".into(),
            status: Status::Pass,
            detail: "ok".into(),
        },
        Check {
            name: "b".into(),
            status: Status::Warn,
            detail: "meh".into(),
        },
        Check {
            name: "c".into(),
            status: Status::Fail,
            detail: "bad".into(),
        },
    ];
    let out = render(&checks);
    assert!(out.contains("✓ a") && out.contains("ok"));
    assert!(out.contains("! b") && out.contains("meh"));
    assert!(out.contains("✗ c") && out.contains("bad"));
    assert!(out.contains("summary: 1 fail, 1 warn, 1 pass"));
    assert!(out.contains("attach this output"));
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
fn model_overrides_parse_correctly() {
    use llmrace::provider::split_model_override;
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
            "/tmp/pm.md",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let parsed: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string("/tmp/pm.json")
            .unwrap()
            .replace('\n', ""),
    )
    .ok()
    .or_else(|| serde_json::from_slice(&out.stdout).ok())
    .expect("json report");
    let first = &parsed["providers"][0];
    assert_eq!(first["model"], "custom-model-9");
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
            "/tmp/gm.md",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let raw = std::fs::read_to_string("/tmp/gm.json").unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(parsed["providers"][0]["model"], "gpt-4o-mini");
}
