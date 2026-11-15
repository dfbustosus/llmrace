//! TUI rendering tests using ratatui's in-memory `TestBackend`.
//!
//! The live event loop needs a real terminal; `render` is exercised
//! directly against a fake backend buffer instead.

use llmrace::tui::Tui;
use ratatui::{backend::TestBackend, Terminal};
use std::sync::Arc;

fn draw(tui: &Tui, w: u16, h: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| tui.render(f)).unwrap();
    let buffer = terminal.backend().buffer();
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

fn upd(
    provider: &str,
    model: &str,
    tokens: u64,
    finished: bool,
    error: Option<&str>,
) -> llmrace::bench::LaneUpdate {
    llmrace::bench::LaneUpdate {
        provider: Arc::from(provider),
        variant: None,
        model: Some(Arc::from(model)),
        tokens,
        elapsed_ms: 250.0,
        ttfb_ms: Some(12.0),
        finished,
        error: error.map(str::to_string),
        server_ms: None,
        last_token: None,
        ratelimit: Some("2999".into()),
    }
}

#[test]
fn renders_streaming_lane() {
    let lanes = vec![(Arc::from("mock"), None, "mock-1".to_string())];
    let mut tui = Tui::new(lanes, "prompt".into(), 2);
    tui.update(upd("mock", "mock-1", 14, false, None));
    let buf = draw(&tui, 100, 12);
    assert!(buf.contains("MOCK"), "lane title: {buf}");
    assert!(buf.contains("streaming"), "{buf}");
    assert!(buf.contains("14 tok"), "{buf}");
    assert!(buf.contains("mock-1"), "{buf}");
}

#[test]
fn renders_finished_lane_with_time_and_ratelimit() {
    let lanes = vec![(Arc::from("mock"), None, "mock-1".to_string())];
    let mut tui = Tui::new(lanes, "p".into(), 2);
    tui.update(upd("mock", "mock-1", 24, true, None));
    let buf = draw(&tui, 100, 12);
    assert!(buf.contains("250 ms"), "finish time: {buf}");
    assert!(buf.contains("ratelimit"), "{buf}");
    assert!(buf.contains("2999"), "{buf}");
}

#[test]
fn renders_lane_error() {
    let lanes = vec![(Arc::from("mock"), None, "mock-1".to_string())];
    let mut tui = Tui::new(lanes, "p".into(), 2);
    tui.update(upd(
        "mock",
        "mock-1",
        0,
        true,
        Some("HTTP 401 unauthorized"),
    ));
    let buf = draw(&tui, 100, 12);
    assert!(buf.contains("HTTP 401"), "{buf}");
}

#[test]
fn duplicate_providers_show_model_in_title() {
    let lanes = vec![
        (Arc::from("openai"), None, "gpt-4o-mini".to_string()),
        (Arc::from("openai"), None, "gpt-4.1".to_string()),
    ];
    let mut tui = Tui::new(lanes, "p".into(), 2);
    tui.update(upd("openai", "gpt-4o-mini", 5, false, None));
    tui.update(upd("openai", "gpt-4.1", 9, false, None));
    let buf = draw(&tui, 120, 20);
    assert!(buf.contains("gpt-4o-mini"), "{buf}");
    assert!(buf.contains("gpt-4.1"), "{buf}");
    assert_eq!(
        tui.lane_progress("openai", None, Some("gpt-4.1")),
        Some((9, false))
    );
}

#[test]
fn transient_error_clears_on_recovery() {
    let lanes = vec![(Arc::from("mock"), None, "mock-1".to_string())];
    let mut tui = Tui::new(lanes, "p".into(), 2);
    tui.update(upd(
        "mock",
        "mock-1",
        3,
        false,
        Some("round failed, retrying"),
    ));
    let mid = draw(&tui, 100, 12);
    assert!(mid.contains("retrying"), "{mid}");
    // tokens flow again on the next round — error must clear
    tui.update(upd("mock", "mock-1", 7, false, None));
    let after = draw(&tui, 100, 12);
    assert!(!after.contains("retrying"), "{after}");
    assert!(after.contains("streaming"), "{after}");
    // lane ultimately finishes clean
    tui.update(upd("mock", "mock-1", 24, true, None));
    let done = draw(&tui, 100, 12);
    assert!(done.contains("250 ms"), "{done}");
}

#[test]
fn long_prompt_is_truncated_in_header() {
    let long = "word ".repeat(40);
    let tui = Tui::new(vec![(Arc::from("mock"), None, "m".into())], long, 1);
    let buf = draw(&tui, 90, 10);
    assert!(buf.contains('…'), "ellipsis expected: {buf}");
}

#[test]
fn long_ttfb_shows_thinking_state() {
    let lanes = vec![(Arc::from("gpt5"), None, "gpt-5-nano".to_string())];
    let mut tui = Tui::new(lanes, "p".into(), 2);
    // 2s elapsed, no first token yet
    tui.update(llmrace::bench::LaneUpdate {
        provider: Arc::from("gpt5"),
        variant: None,
        model: Some(Arc::from("gpt-5-nano")),
        tokens: 0,
        elapsed_ms: 2_000.0,
        ttfb_ms: None,
        finished: false,
        error: None,
        server_ms: None,
        last_token: None,
        ratelimit: None,
    });
    let buf = draw(&tui, 100, 12);
    assert!(buf.contains("thinking"), "{buf}");
    assert!(buf.contains("2.0s"), "{buf}");
    assert!(!buf.contains("streaming"), "{buf}");

    // short waits and tokened lanes keep the streaming label
    let mut early = Tui::new(vec![(Arc::from("m"), None, "m".into())], "p".into(), 2);
    early.update(upd("m", "m", 0, false, None)); // elapsed 250ms
    let buf2 = draw(&early, 100, 12);
    assert!(buf2.contains("streaming"), "{buf2}");
}

#[test]
fn finished_reasoning_lane_shows_brain() {
    let lanes = vec![(Arc::from("openai"), None, "gpt-5-nano".to_string())];
    let mut tui = Tui::new(lanes, "p".into(), 2);
    tui.update(upd("openai", "gpt-5-nano", 24, true, None));
    let buf = draw(&tui, 100, 12);
    assert!(buf.contains('🧠'), "reasoning lane must flag: {buf}");

    let classic = Tui::new(
        vec![(Arc::from("openai"), None, "gpt-4o-mini".to_string())],
        "p".into(),
        2,
    );
    let buf2 = draw(&classic, 100, 12);
    assert!(!buf2.contains('🧠'), "{buf2}");
}
