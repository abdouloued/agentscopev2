use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Bar, BarChart, BarGroup, Block, Borders, List, ListItem, Paragraph},
    Frame, Terminal,
};
use std::{
    io,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crate::config;
use crate::git;
use crate::judge::{JudgeResult, JudgeVerdict};
use crate::policy::{AnnotatedFile, FileVerdict, PolicyEngine};
use crate::session::load_active_session;

// ── Theme constants ───────────────────────────────────────────────────────────

const CLR_BG: Color = Color::Rgb(14, 17, 23);
#[allow(dead_code)]
const CLR_SURFACE: Color = Color::Rgb(28, 31, 38);
const CLR_BORDER: Color = Color::Rgb(42, 45, 54);
const CLR_DIM: Color = Color::Rgb(74, 78, 92);
const CLR_MUTED: Color = Color::Rgb(107, 114, 128);
const CLR_WHITE: Color = Color::Rgb(226, 232, 240);
const CLR_GREEN: Color = Color::Rgb(74, 222, 128);
const CLR_RED: Color = Color::Rgb(248, 113, 113);
const CLR_AMBER: Color = Color::Rgb(251, 191, 36);
const CLR_CYAN: Color = Color::Rgb(103, 232, 249);
const CLR_BLUE: Color = Color::Rgb(96, 165, 250);
const CLR_PURPLE: Color = Color::Rgb(192, 132, 252);

const POLL_MS: u64 = 150;

pub async fn run_watch() -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_app(&mut terminal).await;

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
    )?;
    terminal.show_cursor()?;

    result
}

// ── Judge state (shared with async task) ──────────────────────────────────────

#[derive(Clone)]
enum JudgeStatus {
    Idle,
    Running,
    Done(JudgeResult),
    Error(String),
}

/// TUI state
struct WatchState {
    flash: Option<(String, Instant)>,
    refresh_count: u64,
    show_dashboard: bool,
    started_at: Instant,
    judge_status: Arc<Mutex<JudgeStatus>>,
    /// Track line history for sparkline (last 20 samples)
    line_history: Vec<u64>,
}

impl WatchState {
    fn new() -> Self {
        Self {
            flash: None,
            refresh_count: 0,
            show_dashboard: false,
            started_at: Instant::now(),
            judge_status: Arc::new(Mutex::new(JudgeStatus::Idle)),
            line_history: Vec::new(),
        }
    }

    fn set_flash(&mut self, msg: &str) {
        self.flash = Some((msg.to_string(), Instant::now()));
    }

    fn active_flash(&self) -> Option<&str> {
        match &self.flash {
            Some((msg, when)) if when.elapsed() < Duration::from_secs(2) => Some(msg),
            _ => None,
        }
    }

    fn uptime_str(&self) -> String {
        let secs = self.started_at.elapsed().as_secs();
        let mins = secs / 60;
        let hrs = mins / 60;
        if hrs > 0 {
            format!("{}h {}m", hrs, mins % 60)
        } else if mins > 0 {
            format!("{}m {}s", mins, secs % 60)
        } else {
            format!("{}s", secs)
        }
    }

    fn record_lines(&mut self, total: u64) {
        self.line_history.push(total);
        if self.line_history.len() > 20 {
            self.line_history.remove(0);
        }
    }
}

async fn run_app<B: ratatui::backend::Backend>(terminal: &mut Terminal<B>) -> Result<()> {
    let config = config::load_or_default();
    let mut state = WatchState::new();

    loop {
        state.refresh_count += 1;

        let session = load_active_session().ok();
        let files = if let Some(ref s) = session {
            git::open_repo()
                .and_then(|repo| git::working_tree_diff(&repo))
                .ok()
                .map(|diff| {
                    let engine = PolicyEngine::from_config(&config.policy).ok();
                    engine.map(|e| {
                        let mission = s.mission.as_str();
                        e.annotate(&diff.files, mission)
                    })
                })
                .flatten()
        } else {
            None
        };

        // Record line history for sparkline
        if let Some(ref f) = files {
            let total: u64 = f
                .iter()
                .map(|af| (af.diff.additions + af.diff.deletions) as u64)
                .sum();
            state.record_lines(total);
        }

        terminal.draw(|f| ui(f, session.as_ref(), files.as_deref(), &state))?;

        if event::poll(Duration::from_millis(POLL_MS))? {
            if let Event::Key(key) = event::read()? {
                match (key.code, key.modifiers) {
                    (KeyCode::Char('q'), _)
                    | (KeyCode::Esc, _)
                    | (KeyCode::Char('c'), KeyModifiers::CONTROL) => break,

                    (KeyCode::Char('r'), _) => {
                        state.set_flash("⟳ refreshed");
                    }
                    (KeyCode::Char('d'), _) => {
                        state.show_dashboard = !state.show_dashboard;
                        let msg = if state.show_dashboard {
                            "dashboard on"
                        } else {
                            "dashboard off"
                        };
                        state.set_flash(msg);
                    }
                    // Run judge inline
                    (KeyCode::Char('j'), _) => {
                        let current = state.judge_status.lock().unwrap().clone();
                        if matches!(current, JudgeStatus::Running) {
                            state.set_flash("judge is already running…");
                        } else if let Some(ref s) = session {
                            if let Some(ref file_list) = files {
                                state.set_flash("🔍 running judge…");
                                let judge_status = state.judge_status.clone();
                                *judge_status.lock().unwrap() = JudgeStatus::Running;

                                let mission = s.mission.clone();
                                let annotated = file_list.clone();
                                let judge_config = config.judge.clone();

                                tokio::spawn(async move {
                                    let result =
                                        crate::judge::evaluate(&mission, &annotated, &judge_config)
                                            .await;

                                    let mut status = judge_status.lock().unwrap();
                                    match result {
                                        Ok(r) => *status = JudgeStatus::Done(r),
                                        Err(e) => *status = JudgeStatus::Error(e.to_string()),
                                    }
                                });
                            } else {
                                state.set_flash("no files changed yet");
                            }
                        } else {
                            state.set_flash("no active session");
                        }
                    }
                    (KeyCode::Char('?') | KeyCode::Char('h'), _) => {
                        state.set_flash("r=refresh d=dashboard j=judge c=check q=quit");
                    }
                    (KeyCode::Char('c'), _) => {
                        state.set_flash("→ run `agentscope check` in another terminal");
                    }
                    _ => {}
                }
            }
        }
    }

    Ok(())
}

fn ui(
    f: &mut Frame,
    session: Option<&crate::session::Session>,
    files: Option<&[AnnotatedFile]>,
    state: &WatchState,
) {
    let area = f.area();
    let bg = Block::default().style(Style::default().bg(CLR_BG));
    f.render_widget(bg, area);

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(3), // header
            Constraint::Min(10),   // main
            Constraint::Length(3), // status bar
        ])
        .split(area);

    render_header(f, layout[0], session);

    if state.show_dashboard {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(layout[1]);

        // Left: file list on top, bar chart on bottom
        let left_split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(cols[0]);

        render_file_list(f, left_split[0], files);
        render_bar_chart(f, left_split[1], files);

        // Right: pie chart on top, stats + judge on bottom
        let right_split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(7), // pie chart
                Constraint::Min(5),    // stats + judge
            ])
            .split(cols[1]);

        render_pie_chart(f, right_split[0], files);
        render_stats_and_judge(f, right_split[1], session, files, state);
    } else {
        render_file_list(f, layout[1], files);
    }

    render_summary_bar(f, layout[2], files, state);
}

// ── Header ────────────────────────────────────────────────────────────────────

fn render_header(f: &mut Frame, area: Rect, session: Option<&crate::session::Session>) {
    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(CLR_BORDER));

    let content = if let Some(s) = session {
        let id_short = if s.id.len() >= 12 { &s.id[..12] } else { &s.id };
        Line::from(vec![
            Span::styled(
                "agentscope  ",
                Style::default().fg(CLR_PURPLE).add_modifier(Modifier::BOLD),
            ),
            Span::styled("watch  ", Style::default().fg(CLR_DIM)),
            Span::styled(id_short, Style::default().fg(CLR_CYAN)),
            Span::styled("  ·  ", Style::default().fg(CLR_DIM)),
            Span::styled(&s.mission, Style::default().fg(CLR_WHITE)),
        ])
    } else {
        Line::from(vec![
            Span::styled(
                "agentscope  ",
                Style::default().fg(CLR_PURPLE).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "no active session — run: agentscope start \"mission\"",
                Style::default().fg(CLR_MUTED),
            ),
        ])
    };

    let para = Paragraph::new(content).block(block);
    f.render_widget(para, area);
}

// ── File list ─────────────────────────────────────────────────────────────────

fn render_file_list(f: &mut Frame, area: Rect, files: Option<&[AnnotatedFile]>) {
    let block = Block::default()
        .borders(Borders::NONE)
        .style(Style::default().bg(CLR_BG));

    let items: Vec<ListItem> = match files {
        None => vec![ListItem::new(Line::from(Span::styled(
            "  waiting for session…",
            Style::default().fg(CLR_DIM),
        )))],
        Some(files) if files.is_empty() => vec![
            ListItem::new(Line::from(Span::styled(
                "  no changes yet",
                Style::default().fg(CLR_DIM),
            ))),
            ListItem::new(Line::from(Span::styled(
                "  watching all files vs HEAD",
                Style::default()
                    .fg(CLR_MUTED)
                    .add_modifier(Modifier::ITALIC),
            ))),
            ListItem::new(Line::from(Span::raw(""))),
            ListItem::new(Line::from(Span::styled(
                "  tip: press d for dashboard, j to run judge",
                Style::default().fg(CLR_MUTED),
            ))),
        ],
        Some(files) => files
            .iter()
            .map(|af| {
                let (tag, tag_color, path_color) = match &af.verdict {
                    FileVerdict::InScope => ("IN SCOPE", CLR_GREEN, CLR_BLUE),
                    FileVerdict::Unasked => ("UNASKED ", CLR_AMBER, CLR_AMBER),
                    FileVerdict::Blocked { .. } => ("BLOCKED ", CLR_RED, CLR_RED),
                    FileVerdict::Clean => ("CLEAN   ", CLR_DIM, CLR_DIM),
                };

                let stats = format!(" +{} −{}", af.diff.additions, af.diff.deletions);
                let line = Line::from(vec![
                    Span::styled(
                        format!("  {} ", tag),
                        Style::default().fg(tag_color).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        af.diff.path.display().to_string(),
                        Style::default().fg(path_color),
                    ),
                    Span::styled(stats, Style::default().fg(CLR_DIM)),
                ]);
                ListItem::new(line)
            })
            .collect(),
    };

    let list = List::new(items).block(block);
    f.render_widget(list, area);
}

// ── Bar chart (verdicts) ──────────────────────────────────────────────────────

fn render_bar_chart(f: &mut Frame, area: Rect, files: Option<&[AnnotatedFile]>) {
    let block = Block::default()
        .title(Span::styled(
            " Verdicts ",
            Style::default().fg(CLR_CYAN).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(CLR_BORDER))
        .style(Style::default().bg(CLR_BG));

    if let Some(files) = files {
        let in_scope = files
            .iter()
            .filter(|f| f.verdict == FileVerdict::InScope)
            .count() as u64;
        let unasked = files
            .iter()
            .filter(|f| f.verdict == FileVerdict::Unasked)
            .count() as u64;
        let blocked = files.iter().filter(|f| f.verdict.is_blocked()).count() as u64;

        let bar_group = BarGroup::default().bars(&[
            Bar::default()
                .value(in_scope)
                .label("In Scope".into())
                .style(Style::default().fg(CLR_GREEN)),
            Bar::default()
                .value(unasked)
                .label("Unasked".into())
                .style(Style::default().fg(CLR_AMBER)),
            Bar::default()
                .value(blocked)
                .label("Blocked".into())
                .style(Style::default().fg(CLR_RED)),
        ]);

        let chart = BarChart::default()
            .block(block)
            .data(bar_group)
            .bar_width(8)
            .bar_gap(2)
            .value_style(Style::default().fg(CLR_WHITE).add_modifier(Modifier::BOLD));

        f.render_widget(chart, area);
    } else {
        let para = Paragraph::new(Line::from(Span::styled(
            "  no data",
            Style::default().fg(CLR_DIM),
        )))
        .block(block);
        f.render_widget(para, area);
    }
}

// ── Pie chart (horizontal stacked bar + legend) ──────────────────────────────

fn render_pie_chart(f: &mut Frame, area: Rect, files: Option<&[AnnotatedFile]>) {
    let block = Block::default()
        .title(Span::styled(
            " Scope Distribution ",
            Style::default().fg(CLR_PURPLE).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(CLR_BORDER))
        .style(Style::default().bg(CLR_BG));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.width < 10 || inner.height < 3 {
        return;
    }

    if let Some(files) = files {
        if files.is_empty() {
            let para = Paragraph::new(Line::from(Span::styled(
                "  no changes to chart",
                Style::default().fg(CLR_DIM),
            )));
            f.render_widget(para, inner);
            return;
        }

        let total = files.len().max(1);
        let in_scope = files
            .iter()
            .filter(|f| f.verdict == FileVerdict::InScope)
            .count();
        let unasked = files
            .iter()
            .filter(|f| f.verdict == FileVerdict::Unasked)
            .count();
        let blocked = files.iter().filter(|f| f.verdict.is_blocked()).count();

        let bar_width = (inner.width as usize).saturating_sub(4);

        // Calculate proportional widths
        let g_width = (in_scope * bar_width) / total;
        let a_width = (unasked * bar_width) / total;
        let r_width = (blocked * bar_width) / total;
        // Remaining goes to the largest segment
        let remainder = bar_width.saturating_sub(g_width + a_width + r_width);
        let g_width = g_width + remainder;

        // Stacked bar
        let bar_line = Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled("█".repeat(g_width), Style::default().fg(CLR_GREEN)),
            Span::styled("█".repeat(a_width), Style::default().fg(CLR_AMBER)),
            Span::styled("█".repeat(r_width), Style::default().fg(CLR_RED)),
        ]);

        // Percentages
        let g_pct = (in_scope * 100) / total;
        let a_pct = (unasked * 100) / total;
        let b_pct = (blocked * 100) / total;

        let legend = Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled("● ", Style::default().fg(CLR_GREEN)),
            Span::styled(format!("{}% ", g_pct), Style::default().fg(CLR_GREEN)),
            Span::styled("● ", Style::default().fg(CLR_AMBER)),
            Span::styled(format!("{}% ", a_pct), Style::default().fg(CLR_AMBER)),
            Span::styled("● ", Style::default().fg(CLR_RED)),
            Span::styled(format!("{}%", b_pct), Style::default().fg(CLR_RED)),
        ]);

        let label_line = Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled("scope", Style::default().fg(CLR_DIM)),
            Span::styled("  ", Style::default()),
            Span::styled("unasked", Style::default().fg(CLR_DIM)),
            Span::styled("  ", Style::default()),
            Span::styled("blocked", Style::default().fg(CLR_DIM)),
        ]);

        let text = vec![bar_line, Line::from(Span::raw("")), legend, label_line];

        let para = Paragraph::new(text);
        f.render_widget(para, inner);
    }
}

// ── Stats + Judge panel ──────────────────────────────────────────────────────

fn render_stats_and_judge(
    f: &mut Frame,
    area: Rect,
    session: Option<&crate::session::Session>,
    files: Option<&[AnnotatedFile]>,
    state: &WatchState,
) {
    let block = Block::default()
        .title(Span::styled(
            " Stats & Judge ",
            Style::default().fg(CLR_CYAN).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(CLR_BORDER))
        .style(Style::default().bg(CLR_BG));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();

    // File & line stats
    if let Some(files) = files {
        let total_add: usize = files.iter().map(|f| f.diff.additions).sum();
        let total_del: usize = files.iter().map(|f| f.diff.deletions).sum();
        let in_scope = files
            .iter()
            .filter(|f| f.verdict == FileVerdict::InScope)
            .count();

        lines.push(Line::from(vec![
            Span::styled("  Files  ", Style::default().fg(CLR_DIM)),
            Span::styled(
                format!("{}", files.len()),
                Style::default().fg(CLR_WHITE).add_modifier(Modifier::BOLD),
            ),
            Span::styled("    Lines  ", Style::default().fg(CLR_DIM)),
            Span::styled(format!("+{}", total_add), Style::default().fg(CLR_GREEN)),
            Span::styled(format!(" -{}", total_del), Style::default().fg(CLR_RED)),
        ]));

        // Health score
        let total = files.len().max(1);
        let health = (in_scope * 100) / total;
        let filled = (health / 10).min(10);
        let empty = 10 - filled;
        let bar_color = if health >= 80 {
            CLR_GREEN
        } else if health >= 50 {
            CLR_AMBER
        } else {
            CLR_RED
        };

        lines.push(Line::from(vec![
            Span::styled("  Health ", Style::default().fg(CLR_DIM)),
            Span::styled("█".repeat(filled), Style::default().fg(bar_color)),
            Span::styled("░".repeat(empty), Style::default().fg(CLR_DIM)),
            Span::styled(
                format!(" {}%", health),
                Style::default().fg(bar_color).add_modifier(Modifier::BOLD),
            ),
        ]));
    }

    // Uptime
    lines.push(Line::from(vec![
        Span::styled("  Watch  ", Style::default().fg(CLR_DIM)),
        Span::styled(state.uptime_str(), Style::default().fg(CLR_MUTED)),
        Span::styled(
            format!("  ({} cycles)", state.refresh_count),
            Style::default().fg(CLR_DIM),
        ),
    ]));

    lines.push(Line::from(Span::raw("")));

    // ── Judge result ──
    let judge_status = state.judge_status.lock().unwrap().clone();
    match judge_status {
        JudgeStatus::Idle => {
            lines.push(Line::from(vec![Span::styled(
                "  ── Judge ──",
                Style::default().fg(CLR_DIM),
            )]));
            lines.push(Line::from(vec![
                Span::styled("  Press ", Style::default().fg(CLR_DIM)),
                Span::styled(
                    "j",
                    Style::default().fg(CLR_CYAN).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" to run LLM judge", Style::default().fg(CLR_DIM)),
            ]));
        }
        JudgeStatus::Running => {
            lines.push(Line::from(vec![Span::styled(
                "  ── Judge ──",
                Style::default().fg(CLR_PURPLE),
            )]));
            let dots = ".".repeat(((state.refresh_count / 3) % 4) as usize);
            lines.push(Line::from(vec![Span::styled(
                format!("  ⏳ Analyzing{}", dots),
                Style::default().fg(CLR_AMBER),
            )]));
        }
        JudgeStatus::Done(ref result) => {
            let (verdict_str, verdict_color) = match result.verdict {
                JudgeVerdict::Matches => ("✓ MATCHES MISSION", CLR_GREEN),
                JudgeVerdict::Drift => ("✕ DRIFT DETECTED", CLR_RED),
                JudgeVerdict::Unknown => ("? UNKNOWN", CLR_MUTED),
            };

            lines.push(Line::from(vec![Span::styled(
                "  ── Judge ──",
                Style::default().fg(CLR_PURPLE),
            )]));
            lines.push(Line::from(vec![Span::styled(
                format!("  {}", verdict_str),
                Style::default()
                    .fg(verdict_color)
                    .add_modifier(Modifier::BOLD),
            )]));

            // Confidence bar
            let pct = (result.confidence * 100.0) as usize;
            let filled = (pct / 10).min(10);
            let empty = 10 - filled;
            let bar_color = if pct >= 70 {
                CLR_GREEN
            } else if pct >= 40 {
                CLR_AMBER
            } else {
                CLR_RED
            };

            lines.push(Line::from(vec![
                Span::styled("  Conf.  ", Style::default().fg(CLR_DIM)),
                Span::styled("█".repeat(filled), Style::default().fg(bar_color)),
                Span::styled("░".repeat(empty), Style::default().fg(CLR_DIM)),
                Span::styled(format!(" {}%", pct), Style::default().fg(bar_color)),
            ]));

            // Reasoning (truncated)
            let reasoning = if result.reasoning.len() > 40 {
                format!("{}…", &result.reasoning[..39])
            } else {
                result.reasoning.clone()
            };
            lines.push(Line::from(vec![Span::styled(
                format!("  \"{}\"", reasoning),
                Style::default().fg(CLR_MUTED),
            )]));

            lines.push(Line::from(vec![Span::styled(
                format!("  {} / {}", result.provider, result.model),
                Style::default().fg(CLR_DIM),
            )]));
        }
        JudgeStatus::Error(ref msg) => {
            lines.push(Line::from(vec![Span::styled(
                "  ── Judge ──",
                Style::default().fg(CLR_RED),
            )]));
            let short = if msg.len() > 35 {
                format!("{}…", &msg[..34])
            } else {
                msg.clone()
            };
            lines.push(Line::from(vec![Span::styled(
                format!("  ✕ {}", short),
                Style::default().fg(CLR_RED),
            )]));
            lines.push(Line::from(vec![Span::styled(
                "  Press j to retry",
                Style::default().fg(CLR_DIM),
            )]));
        }
    }

    // Mission reminder
    if let Some(s) = session {
        lines.push(Line::from(Span::raw("")));
        lines.push(Line::from(vec![Span::styled(
            "  ── Mission ──",
            Style::default().fg(CLR_DIM),
        )]));
        let mission = if s.mission.len() > 38 {
            format!("{}…", &s.mission[..37])
        } else {
            s.mission.clone()
        };
        lines.push(Line::from(vec![Span::styled(
            format!("  {}", mission),
            Style::default().fg(CLR_WHITE),
        )]));
    }

    let para = Paragraph::new(lines);
    f.render_widget(para, inner);
}

// ── Summary bar ──────────────────────────────────────────────────────────────

fn render_summary_bar(
    f: &mut Frame,
    area: Rect,
    files: Option<&[AnnotatedFile]>,
    state: &WatchState,
) {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(CLR_BORDER))
        .style(Style::default().bg(CLR_BG));

    let line = if let Some(flash) = state.active_flash() {
        Line::from(Span::styled(
            format!("  {}", flash),
            Style::default().fg(CLR_GREEN).add_modifier(Modifier::BOLD),
        ))
    } else if let Some(files) = files {
        let in_scope = files
            .iter()
            .filter(|f| f.verdict == FileVerdict::InScope)
            .count();
        let unasked = files
            .iter()
            .filter(|f| f.verdict == FileVerdict::Unasked)
            .count();
        let blocked = files.iter().filter(|f| f.verdict.is_blocked()).count();

        let pulse = if state.refresh_count % 4 < 2 {
            "●"
        } else {
            "○"
        };
        let dashboard_hint = if state.show_dashboard {
            "d=hide"
        } else {
            "d=dash"
        };

        // Show judge status indicator
        let judge_indicator = match *state.judge_status.lock().unwrap() {
            JudgeStatus::Idle => "",
            JudgeStatus::Running => "  ⏳judge",
            JudgeStatus::Done(ref r) => match r.verdict {
                JudgeVerdict::Matches => "  ✓judge",
                JudgeVerdict::Drift => "  ✕judge",
                JudgeVerdict::Unknown => "  ?judge",
            },
            JudgeStatus::Error(_) => "  ✕judge",
        };

        Line::from(vec![
            Span::styled(
                format!("  {} in scope", in_scope),
                Style::default().fg(CLR_GREEN),
            ),
            Span::styled("  ·  ", Style::default().fg(CLR_DIM)),
            Span::styled(
                format!("{} unasked", unasked),
                Style::default().fg(CLR_AMBER),
            ),
            Span::styled("  ·  ", Style::default().fg(CLR_DIM)),
            Span::styled(format!("{} blocked", blocked), Style::default().fg(CLR_RED)),
            Span::styled(
                format!(
                    "  {} live  j=judge  {}  ?=help{}",
                    pulse, dashboard_hint, judge_indicator
                ),
                Style::default().fg(CLR_DIM),
            ),
        ])
    } else {
        Line::from(Span::styled(
            "  no session    d=dashboard  j=judge  q=quit  ?=help",
            Style::default().fg(CLR_DIM),
        ))
    };

    let para = Paragraph::new(line).block(block);
    f.render_widget(para, area);
}
