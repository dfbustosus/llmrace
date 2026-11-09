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
