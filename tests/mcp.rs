#![cfg(feature = "cli")]

use std::io::Write as _;
use std::process::{Command, Stdio};

fn talk(requests: &[serde_json::Value]) -> Vec<serde_json::Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .arg("--mcp")
        .env_remove("OPENAI_API_KEY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn llmrace --mcp");
    {
        let mut stdin = child.stdin.take().unwrap();
        for request in requests {
            writeln!(stdin, "{}", request).unwrap();
        }
    }
    let out = child.wait_with_output().expect("collect mcp output");
    assert!(out.status.success(), "server exits cleanly on EOF");
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("each line is a JSON-RPC message"))
        .collect()
}

#[test]
fn failed_provider_has_the_same_gate_result_in_cli_and_mcp() {
    let dir = std::env::temp_dir().join(format!("llmrace-outage-parity-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let baseline = dir.join("baseline.json");
    std::fs::write(
        &baseline,
        r#"{"providers":[{"provider":"openai","model":"gpt-4o-mini"}]}"#,
    )
    .unwrap();
    let cli = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .env_remove("OPENAI_API_KEY")
        .args([
            "openai",
            "mock",
            "--no-tui",
            "--warmup",
            "0",
            "-n",
            "1",
            "--baseline",
        ])
        .arg(&baseline)
        .arg("--out")
        .arg(dir.join("current.json"))
        .output()
        .unwrap();
    assert!(!cli.status.success());
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("current.json")).unwrap()).unwrap();
    assert_eq!(report["regression"]["ok"], false);
    let response = talk(&[
        serde_json::json!({"jsonrpc":"2.0", "id":1, "method":"tools/call", "params":{
            "name":"llmrace_gate", "arguments":{"providers":["openai"],"baseline":baseline.to_string_lossy(), "runs":1,"warmup":0}
        }}),
    ]);
    assert_eq!(response[0]["result"]["isError"], true);
    assert!(response[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("availability"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn handshake_lists_the_four_tools() {
    let responses = talk(&[
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}}),
        serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    ]);
    assert_eq!(responses.len(), 2, "notifications get no response");
    assert_eq!(responses[0]["id"], 1);
    assert_eq!(responses[0]["result"]["serverInfo"]["name"], "llmrace");
    assert_eq!(responses[0]["result"]["protocolVersion"], "2024-11-05");
    let tools = responses[1]["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "llmrace_race",
            "llmrace_gate",
            "llmrace_doctor",
            "llmrace_list"
        ]
    );
    assert!(tools[0]["inputSchema"]["required"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("providers")));
}

#[test]
fn race_tool_runs_a_mock_race_and_returns_markdown() {
    let responses = talk(&[
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "llmrace_race", "arguments": {
            "providers": ["mock", "mock"],
            "runs": 1,
            "warmup": 0,
            "winner": true,
        }}}),
    ]);
    assert_eq!(responses.len(), 2);
    let result = &responses[1]["result"];
    assert_eq!(result["isError"], false);
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("# 🏁 llmrace report"), "{text}");
    assert!(text.contains("mock-1"), "{text}");
    assert!(text.contains("## Podium"), "{text}");
}

#[test]
fn race_tool_rejects_empty_providers_and_unknown_methods_error() {
    let responses = talk(&[
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "llmrace_race", "arguments": {"providers": []}}}),
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "bogus/method"}),
        serde_json::json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "llmrace_race", "arguments": {"providers": ["nosuchprovider"]}}}),
    ]);
    assert_eq!(responses[0]["result"]["isError"], true);
    assert!(responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("must not be empty"));
    assert_eq!(responses[1]["error"]["code"], -32601);
    assert_eq!(responses[2]["result"]["isError"], true);
    assert!(responses[2]["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("unknown provider"));
}

#[test]
fn gate_tool_verdict_matches_the_cli_exit_rule() {
    let dir = std::env::temp_dir().join(format!("llmrace-mcp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let baseline_md = dir.join("baseline.md");
    let record = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .args([
            "mock", "--no-tui", "-n", "2", "--warmup", "0", "--quiet", "--json", "--out",
        ])
        .arg(&baseline_md)
        .output()
        .unwrap();
    assert!(record.status.success());
    let baseline = baseline_md.with_extension("json");

    let responses = talk(&[
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "llmrace_gate", "arguments": {
            "providers": ["mock"],
            "baseline": baseline.to_string_lossy(),
            "runs": 2,
            "warmup": 0,
            "max_regression": 10000.0,
        }}}),
    ]);
    let result = &responses[1]["result"];
    assert_eq!(result["isError"], false);
    let text = result["content"][0]["text"].as_str().unwrap();
    let verdict: serde_json::Value = serde_json::from_str(
        text.split('\n')
            .take_while(|line| !line.starts_with("## Regression"))
            .collect::<Vec<_>>()
            .join("\n")
            .trim(),
    )
    .expect("the gate answer starts with the JSON verdict");
    assert_eq!(verdict["ok"], true);
    assert_eq!(verdict["verdict"]["request_mismatch"], false);
    assert!(text.contains("## Regression"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mcp_rejects_invalid_parameters() {
    let dir = std::env::temp_dir().join(format!("llmrace-mcp-invalid-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let baseline = dir.join("baseline.json");
    std::fs::write(
        &baseline,
        r#"{"providers":[{"provider":"mock","model":"mock-1"}]}"#,
    )
    .unwrap();

    let responses = talk(&[
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "llmrace_race", "arguments": {"providers": ["mock"], "runs": 0}}}),
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "llmrace_race", "arguments": {"providers": ["mock"], "runs": -1}}}),
        serde_json::json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "llmrace_race", "arguments": {"providers": ["mock"], "runs": 999999}}}),
        serde_json::json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": {"name": "llmrace_race", "arguments": {"providers": ["mock"], "warmup_mode": "bogus"}}}),
        serde_json::json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call",
        "params": {"name": "llmrace_gate", "arguments": {
            "providers": ["mock"],
            "baseline": baseline.to_string_lossy(),
            "max_regression": "bad",
        }}}),
    ]);
    assert_eq!(responses.len(), 5);
    for (idx, expected_hint) in [
        (0, "runs"),
        (1, "runs"),
        (2, "runs"),
        (3, "warmup"),
        (4, "max_regression"),
    ] {
        let result = &responses[idx]["result"];
        assert_eq!(result["isError"], true, "case {idx}: {result}");
        let msg = result["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_ascii_lowercase();
        assert!(
            msg.contains(expected_hint),
            "case {idx} should mention '{expected_hint}': {msg}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn race_tool_supports_reproducible_probe_and_concurrency() {
    let responses = talk(&[
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "llmrace_race", "arguments": {
            "providers": ["mock"],
            "runs": 1,
            "warmup": 1,
            "reproducible": true,
            "warmup_mode": "probe",
            "concurrency": 2,
        }}}),
    ]);
    assert_eq!(responses.len(), 2);
    let result = &responses[1]["result"];
    assert_eq!(result["isError"], false, "{result}");
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("# 🏁 llmrace report"), "{text}");
    assert!(text.contains("mock-1"), "{text}");
    assert!(
        text.contains("- **Sampling**: temperature 0, top_p 1, seed 0"),
        "reproducible sampling note should appear in report: {text}"
    );
}

#[test]
fn race_tool_accepts_inline_response_schema_and_rejects_conflicts() {
    let responses = talk(&[
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "llmrace_race", "arguments": {
            "providers": ["mock"],
            "runs": 1,
            "warmup": 0,
            "response_schema": {"type": "object", "required": ["city"]},
        }}}),
        serde_json::json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {"name": "llmrace_race", "arguments": {
            "providers": ["mock"],
            "runs": 1,
            "warmup": 0,
            "response_schema": "/tmp/foo.json",
        }}}),
        serde_json::json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
        "params": {"name": "llmrace_race", "arguments": {
            "providers": ["mock"],
            "runs": 1,
            "warmup": 0,
            "json_output": true,
            "response_schema": {"type": "object", "required": ["city"]},
        }}}),
    ]);
    assert_eq!(responses.len(), 4);

    let ok = &responses[1]["result"];
    assert_eq!(ok["isError"], false, "{ok}");
    let text = ok["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("# 🏁 llmrace report"), "{text}");
    assert!(
        text.contains("- **Sampling**: json schema"),
        "sampling note should include json schema: {text}"
    );

    let non_object = &responses[2]["result"];
    assert_eq!(non_object["isError"], true, "{non_object}");

    let conflict = &responses[3]["result"];
    assert_eq!(conflict["isError"], true, "{conflict}");
}
