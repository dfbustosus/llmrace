use crate::bench::LaneUpdate;
use ratatui::{
    crossterm::event::{self, Event, KeyCode},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, Paragraph},
    DefaultTerminal, Frame,
};
use std::time::{Duration, Instant};

#[derive(Clone)]
struct Lane {
    name: String,
    model: String,
    tokens: u64,
    elapsed_ms: f64,
    ttfb_ms: Option<f64>,
    finished: bool,
    error: Option<String>,
    best_ms: Option<f64>,
    finish_rank: Option<usize>,
}

#[derive(Clone)]
struct RaceState {
    lanes: Vec<Lane>,
    prompt: String,
    runs: usize,
    run_idx: usize,
    started: Instant,
    all_done: bool,
}

pub struct Tui {
    state: RaceState,
    pub final_table: Option<String>,
}

impl Tui {
    pub fn new(lane_names: &[(String, String)], prompt: String, runs: usize) -> Self {
        let lanes = lane_names
            .iter()
            .map(|(name, model)| Lane {
                name: name.clone(),
                model: model.clone(),
                tokens: 0,
                elapsed_ms: 0.0,
                ttfb_ms: None,
                finished: false,
                error: None,
                best_ms: None,
                finish_rank: None,
            })
            .collect();
        Tui {
            state: RaceState {
                lanes,
                prompt,
                runs,
                run_idx: 0,
                started: Instant::now(),
                all_done: false,
            },
            final_table: None,
        }
    }

    pub fn update(&mut self, u: LaneUpdate) {
        if let Some(lane) = self.state.lanes.iter_mut().find(|l| l.name == u.provider) {
            if u.finished {
                lane.finished = true;
                if let Some(e) = u.error {
                    lane.error = Some(e);
                } else {
                    lane.best_ms = Some(
                        lane.best_ms
                            .map(|b| b.min(u.elapsed_ms))
                            .unwrap_or(u.elapsed_ms),
                    );
                }
            } else {
                lane.tokens = u.tokens;
                lane.elapsed_ms = u.elapsed_ms;
                lane.ttfb_ms = u.ttfb_ms.or(lane.ttfb_ms);
            }
        }
        self.recompute_ranks();
    }

    fn recompute_ranks(&mut self) {
        let mut order: Vec<usize> = (0..self.state.lanes.len()).collect();
        order.sort_by(|&a, &b| {
            let la = &self.state.lanes[a];
            let lb = &self.state.lanes[b];
            let ra = (la.error.is_none(), !(la.best_ms.is_some() && la.finished));
            let rb = (lb.error.is_none(), !(lb.best_ms.is_some() && lb.finished));
            ra.cmp(&rb).then(
                la.best_ms
                    .unwrap_or(f64::MAX)
                    .total_cmp(&lb.best_ms.unwrap_or(f64::MAX)),
            )
        });
        for (rank, idx) in order.iter().enumerate() {
            self.state.lanes[*idx].finish_rank = Some(rank + 1);
        }
        self.state.all_done = self.state.lanes.iter().all(|l| l.finished);
    }

    pub fn mark_run_start(&mut self) {
        self.state.run_idx += 1;
        self.state.started = Instant::now();
        for lane in self.state.lanes.iter_mut() {
            lane.finished = false;
            lane.tokens = 0;
            lane.elapsed_ms = 0.0;
            lane.ttfb_ms = None;
            lane.error = None;
        }
    }

    pub fn run(
        mut self,
        mut updates: std::sync::mpsc::Receiver<LaneUpdate>,
    ) -> anyhow::Result<Self> {
        let mut terminal = ratatui::init();
        let result = self.event_loop(&mut terminal, &mut updates);
        ratatui::restore();
        result.map(|_| self)
    }

    fn event_loop(
        &mut self,
        terminal: &mut DefaultTerminal,
        updates: &mut std::sync::mpsc::Receiver<LaneUpdate>,
    ) -> anyhow::Result<()> {
        loop {
            while let Ok(u) = updates.try_recv() {
                self.update(u);
            }
            terminal.draw(|f| self.render(f))?;

            if event::poll(Duration::from_millis(50))? {
                if let Event::Key(key) = event::read()? {
                    if key.code == KeyCode::Char('q') || key.code == KeyCode::Esc {
                        return Ok(());
                    }
                }
            }
            if self.state.all_done
                && updates.try_recv().is_err()
                && event::poll(Duration::from_millis(300))?
            {
                if let Event::Key(key) = event::read()? {
                    if key.code == KeyCode::Char('q')
                        || key.code == KeyCode::Esc
                        || key.code == KeyCode::Enter
                    {
                        return Ok(());
                    }
                }
            }
        }
    }

    fn render(&self, f: &mut Frame) {
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
                    "run {}/{} · {:>4.1}s · {}",
                    self.state.run_idx.max(1),
                    self.state.runs,
                    self.state.started.elapsed().as_secs_f32(),
                    truncate_str(self.state.prompt.trim(), 48)
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

        let max_elapsed = self
            .state
            .lanes
            .iter()
            .map(|l| l.elapsed_ms)
            .fold(1.0f64, f64::max);

        for (i, lane) in self.state.lanes.iter().enumerate() {
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(if lane.error.is_some() {
                    Color::Red
                } else if lane.finished {
                    Color::Green
                } else {
                    Color::Blue
                }))
                .title(Span::styled(
                    format!(" {} ", lane.name.to_uppercase()),
                    Style::default()
                        .fg(if lane.error.is_some() {
                            Color::Red
                        } else if lane.finished {
                            Color::Green
                        } else {
                            Color::Cyan
                        })
                        .add_modifier(Modifier::BOLD),
                ));
            let rows = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(block.inner(inner[i]));

            let status = if let Some(e) = &lane.error {
                format!(" ✗ {}", truncate_str(e, 44))
            } else if lane.finished {
                match lane.best_ms {
                    Some(ms) => format!(" 🏁 {} ms", ms as u64),
                    None => " 🏁 done".to_string(),
                }
            } else {
                " ▶ streaming…".to_string()
            };

            let meta = Line::from(vec![
                Span::styled(
                    format!("{} ", status),
                    Style::default().fg(if lane.error.is_some() {
                        Color::Red
                    } else {
                        Color::Green
                    }),
                ),
                Span::styled(
                    format!("{} tok", lane.tokens),
                    Style::default().fg(Color::White),
                ),
                if let Some(t) = lane.ttfb_ms {
                    Span::styled(
                        format!("  ·  TTFB {:.0} ms", t),
                        Style::default().fg(Color::DarkGray),
                    )
                } else {
                    Span::raw("")
                },
            ]);
            f.render_widget(Paragraph::new(meta), rows[0]);
            f.render_widget(
                Paragraph::new(Span::styled(
                    format!("`{}`", lane.model),
                    Style::default().fg(Color::DarkGray),
                )),
                rows[1],
            );

            let ratio = ((lane.elapsed_ms / max_elapsed) * 100.0).clamp(0.0, 100.0) as u16;
            let gauge = Gauge::default()
                .ratio(f64::from(ratio) / 100.0)
                .gauge_style(
                    Style::default()
                        .fg(Color::LightYellow)
                        .bg(Color::Rgb(20, 24, 36)),
                )
                .label("");
            f.render_widget(gauge.block(Block::default()), rows[2]);
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

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}
