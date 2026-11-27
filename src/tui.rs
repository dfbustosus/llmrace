//! Live terminal race view.

use crate::bench::LaneUpdate;
use crate::provider;
use ratatui::{
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Sparkline},
    Frame,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const REDRAW_TICK: Duration = Duration::from_millis(250);
const SPARK_POINTS: usize = 40;

static RESTORE_ARMED: AtomicBool = AtomicBool::new(false);

/// Matches a lane against a (provider, variant, model) key; a `None` model
/// in the key matches any lane of that provider and variant (legacy updates).
fn lane_matches(lane: &Lane, provider: &str, variant: Option<&str>, model: Option<&str>) -> bool {
    *lane.name == *provider
        && lane.variant.as_deref() == variant
        && model.is_none_or(|m| lane.model == m)
}

fn is_quit_key(key: KeyEvent, allow_enter: bool) -> bool {
    matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
        || (allow_enter && key.code == KeyCode::Enter)
        || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
}

fn restore_once() {
    if RESTORE_ARMED.swap(false, Ordering::AcqRel) {
        ratatui::restore();
    }
}

struct TerminalGuard;

impl TerminalGuard {
    fn install() -> Self {
        RESTORE_ARMED.store(true, Ordering::Release);
        TerminalGuard
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_once();
    }
}

#[derive(Clone)]
struct Lane {
    name: Arc<str>,
    variant: Option<Arc<str>>,
    model: String,
    tokens: u64,
    cost: Option<f64>,
    elapsed_ms: f64,
    ttfb_ms: Option<f64>,
    finished: bool,
    error: Option<String>,
    best_ms: Option<f64>,
    server_ms: Option<f64>,
    ratelimit: Option<String>,
    preview: String,
    rate_history: Vec<f64>,
    last_tokens: u64,
    last_sample: Instant,
}

impl Lane {
    fn new(name: Arc<str>, variant: Option<Arc<str>>, model: String) -> Self {
        let cost =
            crate::cost::pricing(name.as_ref(), &model).map(|(pin, pout)| (pin + pout) / 2.0);
        Lane {
            name,
            variant,
            model,
            tokens: 0,
            cost,
            elapsed_ms: 0.0,
            ttfb_ms: None,
            finished: false,
            error: None,
            best_ms: None,
            server_ms: None,
            ratelimit: None,
            preview: String::new(),
            rate_history: Vec::new(),
            last_tokens: 0,
            last_sample: Instant::now(),
        }
    }
}

#[derive(Clone)]
struct RaceState {
    lanes: Vec<Lane>,
    prompt: String,
    rounds_label: String,
    started: Instant,
    all_done: bool,
}

/// Terminal user interface driving the live race.
pub struct Tui {
    state: RaceState,
}

impl Tui {
    /// Creates a TUI for the given lanes and round count.
    pub fn new(
        lanes: Vec<(Arc<str>, Option<Arc<str>>, String)>,
        prompt: String,
        runs: usize,
    ) -> Self {
        Self::with_rounds_label(lanes, prompt, format!("{runs} rounds"))
    }

    /// Creates a TUI for a time-bounded run (`--duration`): the header
    /// shows the bound instead of a misleading sentinel round count.
    pub fn with_rounds_label(
        lanes: Vec<(Arc<str>, Option<Arc<str>>, String)>,
        prompt: String,
        rounds_label: String,
    ) -> Self {
        Tui {
            state: RaceState {
                lanes: lanes
                    .into_iter()
                    .map(|(name, variant, model)| Lane::new(name, variant, model))
                    .collect(),
                prompt,
                rounds_label,
                started: Instant::now(),
                all_done: false,
            },
        }
    }

    /// Progress snapshot for one lane key: `(tokens, finished)`.
    pub fn lane_progress(
        &self,
        provider: &str,
        variant: Option<&str>,
        model: Option<&str>,
    ) -> Option<(u64, bool)> {
        self.state
            .lanes
            .iter()
            .find(|l| lane_matches(l, provider, variant, model))
            .map(|l| (l.tokens, l.finished))
    }

    /// Applies one progress update.
    pub fn update(&mut self, u: LaneUpdate) {
        if let Some(lane) = self
            .state
            .lanes
            .iter_mut()
            .find(|l| lane_matches(l, &u.provider, u.variant.as_deref(), u.model.as_deref()))
        {
            if u.finished {
                lane.finished = true;
                lane.server_ms = u.server_ms.or(lane.server_ms);
                lane.ratelimit = u.ratelimit.clone().or_else(|| lane.ratelimit.clone());
                if let Some(e) = u.error {
                    lane.error = Some(crate::util::sanitize_control_chars(&e));
                } else {
                    // A completed lane clears transient round errors seen
                    // earlier in the race (the lane ultimately succeeded).
                    lane.error = None;
                    if u.elapsed_ms > 0.0 {
                        lane.best_ms =
                            Some(lane.best_ms.map_or(u.elapsed_ms, |b| b.min(u.elapsed_ms)));
                    }
                }
            } else {
                // Transient state (streaming or retrying): show and clear
                // round errors as they come.
                lane.error = u.error.as_deref().map(crate::util::sanitize_control_chars);
                lane.tokens = u.tokens;
                lane.elapsed_ms = u.elapsed_ms;
                lane.ttfb_ms = u.ttfb_ms.or(lane.ttfb_ms);
                if let Some(tok) = &u.last_token {
                    let clean = crate::util::sanitize_control_chars(tok);
                    lane.preview.push_str(&clean);
                    let chars: Vec<char> = lane.preview.chars().collect();
                    if chars.len() > 40 {
                        lane.preview = chars[chars.len() - 40..].iter().collect();
                    }
                }
            }
        }
        self.state.all_done = self.state.lanes.iter().all(|l| l.finished);
    }

    /// Drains all pending updates, returning `(received_any, disconnected)`.
    ///
    /// When all senders drop early (e.g. cancellation or worker panic), any
    /// remaining unfinished lanes are marked finished and `all_done` is set so
    /// the UI never waits indefinitely.
    fn drain_updates(&mut self, updates: &std::sync::mpsc::Receiver<LaneUpdate>) -> (bool, bool) {
        let mut received = false;
        loop {
            match updates.try_recv() {
                Ok(u) => {
                    self.update(u);
                    received = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => return (received, false),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    for lane in &mut self.state.lanes {
                        if !lane.finished {
                            lane.finished = true;
                        }
                    }
                    self.state.all_done = true;
                    return (received, true);
                }
            }
        }
    }

    fn sample_rates(&mut self) {
        for lane in self.state.lanes.iter_mut() {
            let elapsed_s = lane.last_sample.elapsed().as_secs_f64();
            if elapsed_s < 0.1 {
                continue;
            }
            let delta = lane.tokens.saturating_sub(lane.last_tokens);
            let rate = delta as f64 / elapsed_s;
            lane.rate_history.push(rate);
            if lane.rate_history.len() > SPARK_POINTS {
                lane.rate_history.remove(0);
            }
            lane.last_tokens = lane.tokens;
            lane.last_sample = Instant::now();
        }
    }

    /// Returns `true` once every lane in the TUI has finished.
    #[must_use]
    pub fn is_all_done(&self) -> bool {
        self.state.all_done
    }

    /// Runs the event loop until the user quits or all lanes finish.
    ///
    /// The screen is only redrawn when new updates arrived or a 250 ms tick
    /// elapsed, and terminal state is restored even when rendering fails.
    pub fn run(mut self, updates: std::sync::mpsc::Receiver<LaneUpdate>) -> anyhow::Result<Self> {
        let mut terminal = ratatui::init();
        let _guard = TerminalGuard::install();
        let mut last_draw: Option<Instant> = None;
        loop {
            let (received, disconnected) = self.drain_updates(&updates);

            if received || disconnected || last_draw.is_none_or(|t| t.elapsed() >= REDRAW_TICK) {
                self.sample_rates();
                terminal.draw(|f| self.render(f))?;
                last_draw = Some(Instant::now());
            }

            if event::poll(Duration::from_millis(50))? {
                if let Event::Key(key) = event::read()? {
                    if is_quit_key(key, false) {
                        return Ok(self);
                    }
                }
            }
            if self.state.all_done {
                let deadline = Instant::now() + Duration::from_secs(30);
                loop {
                    if event::poll(Duration::from_millis(200))? {
                        if let Event::Key(key) = event::read()? {
                            if is_quit_key(key, true) {
                                return Ok(self);
                            }
                        }
                    }
                    if Instant::now() > deadline {
                        return Ok(self);
                    }
                }
            }
        }
    }

    /// Renders one frame of the race view.
    ///
    /// Public so embedders and tests can draw against any
    /// [`ratatui`] backend (e.g. `TestBackend`).
    pub fn render(&self, f: &mut Frame) {
        use ratatui::layout::{Constraint, Layout};

        let chunks = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(f.area());

        let title = Paragraph::new(Line::from(vec![
            Span::styled(
                " 🏁 llmrace ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                format!(
                    "{} · {:>4.1}s · {}",
                    self.state.rounds_label,
                    self.state.started.elapsed().as_secs_f32(),
                    crate::util::truncate(self.state.prompt.trim(), 48)
                ),
                Style::default().fg(Color::DarkGray),
            ),
        ]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        );
        f.render_widget(title, chunks[0]);

        let lane_height = 5;
        let inner = Layout::vertical(
            self.state
                .lanes
                .iter()
                .map(|_| Constraint::Length(lane_height)),
        )
        .split(chunks[1]);

        for (i, lane) in self.state.lanes.iter().enumerate() {
            let accent = if lane.error.is_some() {
                Color::Red
            } else if lane.finished {
                Color::Green
            } else {
                Color::Cyan
            };
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(accent))
                .title(Span::styled(
                    match lane.variant.as_deref() {
                        Some(v) => format!(" {} [{v}] ", lane.name.to_uppercase()),
                        None => format!(" {} ", lane.name.to_uppercase()),
                    },
                    Style::default().fg(accent).add_modifier(Modifier::BOLD),
                ))
                .title(Span::styled(
                    if self
                        .state
                        .lanes
                        .iter()
                        .filter(|l| *l.name == *lane.name)
                        .count()
                        > 1
                    {
                        format!(" {} ", crate::util::truncate(&lane.model, 28))
                    } else {
                        String::new()
                    },
                    Style::default().fg(Color::DarkGray),
                ));
            let rows = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(2),
            ])
            .split(block.inner(inner[i]));

            let status = if let Some(e) = &lane.error {
                format!(" ✗ {}", crate::util::truncate(e, 42))
            } else if lane.finished {
                // reasoning models flag their hidden thinking, matching
                // the 🧠 annotation in the result tables
                let brain = if provider::is_reasoning_model(&lane.model) {
                    " 🧠"
                } else {
                    ""
                };
                match lane.best_ms {
                    Some(ms) => format!(" 🏁 {:.0} ms{brain}", ms),
                    None => format!(" 🏁 done{brain}"),
                }
            } else if lane.ttfb_ms.is_none() && lane.elapsed_ms > 800.0 {
                // No first token yet after a noticeable wait: the model is
                // thinking or the request is queueing — "streaming" would
                // be a lie.
                format!(" ⏳ thinking… {:.1}s", lane.elapsed_ms / 1000.0)
            } else {
                " ▶ streaming…".to_string()
            };

            let cost_note = lane
                .cost
                .filter(|c| lane.finished && *c > 0.0)
                .map(|c| format!("  ~${c:.2}/M tok"));
            let meta = Line::from(vec![
                Span::styled(
                    status,
                    Style::default().fg(if lane.error.is_some() {
                        Color::Red
                    } else {
                        Color::Green
                    }),
                ),
                Span::styled(
                    format!("  {} tok", lane.tokens),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    cost_note.unwrap_or_default(),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(
                    format!(
                        "  ·  TTFB {} ms",
                        lane.ttfb_ms
                            .map(|t| format!("{t:.0}"))
                            .unwrap_or("…".into())
                    ),
                    Style::default().fg(Color::DarkGray),
                ),
            ]);
            f.render_widget(Paragraph::new(meta), rows[0]);
            let line2 = if lane.finished {
                match &lane.ratelimit {
                    Some(rl) => format!("`{}`  ·  ratelimit: {} left", lane.model, rl),
                    None => format!("`{}`", lane.model),
                }
            } else if !lane.preview.is_empty() {
                format!("▊ {}", lane.preview)
            } else {
                format!("`{}`", lane.model)
            };
            f.render_widget(
                Paragraph::new(Span::styled(line2, Style::default().fg(Color::DarkGray))),
                rows[1],
            );

            let bars: Vec<u64> = lane.rate_history.iter().map(|r| r.round() as u64).collect();
            let spark = Sparkline::default()
                .data(&bars)
                .style(Style::default().fg(Color::LightYellow));
            f.render_widget(spark, rows[2]);
            f.render_widget(block, inner[i]);
        }

        let footer = Paragraph::new(Line::from(vec![
            Span::styled("q/Esc", Style::default().fg(Color::Yellow)),
            Span::styled(
                " quit · report written on exit",
                Style::default().fg(Color::DarkGray),
            ),
        ]));
        f.render_widget(footer, chunks[2]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_update(provider: &str, finished: bool) -> LaneUpdate {
        LaneUpdate {
            provider: Arc::from(provider),
            variant: None,
            model: Some(Arc::from("m1")),
            tokens: 12,
            elapsed_ms: 42.0,
            ttfb_ms: Some(10.0),
            finished,
            error: None,
            server_ms: None,
            last_token: None,
            ratelimit: None,
        }
    }

    #[test]
    fn disconnected_channel_marks_unfinished_lanes_done() {
        let mut tui = Tui::new(
            vec![
                (Arc::from("a"), None, "m1".into()),
                (Arc::from("b"), None, "m1".into()),
            ],
            "hi".into(),
            1,
        );
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send(sample_update("a", true));
        drop(tx);

        let (received, disconnected) = tui.drain_updates(&rx);
        assert!(received);
        assert!(disconnected);
        assert!(tui.state.all_done);
        assert_eq!(tui.lane_progress("a", None, Some("m1")), Some((0, true)));
        assert_eq!(tui.lane_progress("b", None, Some("m1")), Some((0, true)));
    }

    #[test]
    fn update_sanitizes_control_chars_in_tokens_and_errors() {
        let mut tui = Tui::new(vec![(Arc::from("a"), None, "m1".into())], "hi".into(), 1);

        let mut u = sample_update("a", false);
        u.last_token = Some("hi\x1b[31m\nworld\x07\t!".into());
        u.error = Some("bad\x1b]0;title\x07\r\nerr".into());
        tui.update(u);

        let lane = &tui.state.lanes[0];
        assert!(!lane.preview.chars().any(char::is_control));
        assert_eq!(lane.preview, "hi[31m world !");
        assert_eq!(lane.error.as_deref(), Some("bad]0;title  err"));

        let mut fin = sample_update("a", true);
        fin.error = Some("fatal\x1b[2J\x07boom".into());
        tui.update(fin);
        assert_eq!(tui.state.lanes[0].error.as_deref(), Some("fatal[2Jboom"));
    }

    #[test]
    fn quit_key_handles_ctrl_c_q_esc_and_optional_enter() {
        assert!(is_quit_key(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            false
        ));
        assert!(!is_quit_key(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
            false
        ));
        assert!(is_quit_key(
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
            false
        ));
        assert!(is_quit_key(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            false
        ));
        assert!(!is_quit_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            false
        ));
        assert!(is_quit_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            true
        ));
    }
}
