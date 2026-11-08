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
