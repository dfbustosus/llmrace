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
