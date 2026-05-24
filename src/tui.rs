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
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame, Terminal,
};
use std::{io, time::{Duration, Instant}};

use crate::config;
use crate::git;
use crate::policy::{FileVerdict, PolicyEngine};
use crate::session::load_active_session;

// ── Theme constants (matching the mockup palette) ─────────────────────────────

const CLR_BG:        Color = Color::Rgb(14, 17, 23);   // #0e1117
#[allow(dead_code)]
const CLR_SURFACE:   Color = Color::Rgb(28, 31, 38);   // #1c1f26
const CLR_BORDER:    Color = Color::Rgb(42, 45, 54);   // #2a2d36
const CLR_DIM:       Color = Color::Rgb(74, 78, 92);   // #4a4e5c
const CLR_MUTED:     Color = Color::Rgb(107, 114, 128); // #6b7280
const CLR_WHITE:     Color = Color::Rgb(226, 232, 240); // #e2e8f0
const CLR_GREEN:     Color = Color::Rgb(74, 222, 128);  // #4ade80
const CLR_RED:       Color = Color::Rgb(248, 113, 113); // #f87171
const CLR_AMBER:     Color = Color::Rgb(251, 191, 36);  // #fbbf24
const CLR_CYAN:      Color = Color::Rgb(103, 232, 249); // #67e8f9
const CLR_BLUE:      Color = Color::Rgb(96, 165, 250);  // #60a5fa
const CLR_PURPLE:    Color = Color::Rgb(192, 132, 252); // #c084fc

/// Polling interval — 150ms is snappy without thrashing CPU
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

/// TUI state for flash messages, refresh counter, and dashboard toggle
struct WatchState {
    flash: Option<(String, Instant)>,
    refresh_count: u64,
    show_dashboard: bool,
    started_at: Instant,
}

impl WatchState {
    fn new() -> Self {
        Self {
            flash: None,
            refresh_count: 0,
            show_dashboard: false,
            started_at: Instant::now(),
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
}

async fn run_app<B: ratatui::backend::Backend>(terminal: &mut Terminal<B>) -> Result<()> {
    let config = config::load_or_default();
    let mut state = WatchState::new();

    loop {
        state.refresh_count += 1;

        // Refresh data each frame
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

        terminal.draw(|f| ui(f, session.as_ref(), files.as_deref(), &state))?;

        if event::poll(Duration::from_millis(POLL_MS))? {
            if let Event::Key(key) = event::read()? {
                match (key.code, key.modifiers) {
                    // Quit
                    (KeyCode::Char('q'), _)
                    | (KeyCode::Esc, _)
                    | (KeyCode::Char('c'), KeyModifiers::CONTROL) => break,
                    // Force refresh (visual confirmation)
                    (KeyCode::Char('r'), _) => {
                        state.set_flash("⟳ refreshed");
                    }
                    // Toggle dashboard
                    (KeyCode::Char('d'), _) => {
                        state.show_dashboard = !state.show_dashboard;
                        let msg = if state.show_dashboard { "dashboard on" } else { "dashboard off" };
                        state.set_flash(msg);
                    }
                    // Check hint
                    (KeyCode::Char('c'), _) => {
                        state.set_flash("→ run `agentscope check` in another terminal");
                    }
                    // Judge hint
                    (KeyCode::Char('j'), _) => {
                        state.set_flash("→ run `agentscope judge` in another terminal");
                    }
                    // Help
                    (KeyCode::Char('?') | KeyCode::Char('h'), _) => {
                        state.set_flash("r=refresh  d=dashboard  j=judge  c=check  q=quit");
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
    files: Option<&[crate::policy::AnnotatedFile]>,
    state: &WatchState,
) {
    let area = f.area();

    // Background
    let bg = Block::default().style(Style::default().bg(CLR_BG));
    f.render_widget(bg, area);

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(3),  // header
            Constraint::Min(10),   // main content
            Constraint::Length(3), // summary bar
        ])
        .split(area);

    render_header(f, layout[0], session);

    if state.show_dashboard {
        // Two-column layout: files on left, stats on right
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(60),
                Constraint::Percentage(40),
            ])
            .split(layout[1]);

        render_file_list(f, cols[0], files);
        render_dashboard_panel(f, cols[1], session, files, state);
    } else {
        render_file_list(f, layout[1], files);
    }

    render_summary_bar(f, layout[2], files, state);
}

fn render_header(f: &mut Frame, area: Rect, session: Option<&crate::session::Session>) {
    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(CLR_BORDER));

    let content = if let Some(s) = session {
        let id_short = if s.id.len() >= 12 { &s.id[..12] } else { &s.id };
        Line::from(vec![
            Span::styled("agentscope  ", Style::default().fg(CLR_PURPLE).add_modifier(Modifier::BOLD)),
            Span::styled("watch  ", Style::default().fg(CLR_DIM)),
            Span::styled(id_short, Style::default().fg(CLR_CYAN)),
            Span::styled("  ·  ", Style::default().fg(CLR_DIM)),
            Span::styled(&s.mission, Style::default().fg(CLR_WHITE)),
        ])
    } else {
        Line::from(vec![
            Span::styled("agentscope  ", Style::default().fg(CLR_PURPLE).add_modifier(Modifier::BOLD)),
            Span::styled("no active session — run: agentscope start \"mission\"", Style::default().fg(CLR_MUTED)),
        ])
    };

    let para = Paragraph::new(content).block(block);
    f.render_widget(para, area);
}

fn render_file_list(f: &mut Frame, area: Rect, files: Option<&[crate::policy::AnnotatedFile]>) {
    let block = Block::default()
        .borders(Borders::NONE)
        .style(Style::default().bg(CLR_BG));

    let items: Vec<ListItem> = match files {
        None => vec![
            ListItem::new(Line::from(Span::styled(
                "  waiting for session…",
                Style::default().fg(CLR_DIM),
            )))
        ],
        Some(files) if files.is_empty() => vec![
            ListItem::new(Line::from(Span::styled(
                "  no changes yet",
                Style::default().fg(CLR_DIM),
            ))),
            ListItem::new(Line::from(Span::styled(
                "  watching all files in working tree vs HEAD",
                Style::default().fg(CLR_MUTED).add_modifier(Modifier::ITALIC),
            ))),
            ListItem::new(Line::from(Span::raw(""))),
            ListItem::new(Line::from(Span::styled(
                "  tip: changes will appear here as soon as any file is modified",
                Style::default().fg(CLR_MUTED),
            ))),
        ],
        Some(files) => files.iter().map(|af| {
            let (tag, tag_color, path_color) = match &af.verdict {
                FileVerdict::InScope =>
                    ("  IN SCOPE ", CLR_GREEN, CLR_BLUE),
                FileVerdict::Unasked =>
                    ("  UNASKED  ", CLR_AMBER, CLR_AMBER),
                FileVerdict::Blocked { .. } =>
                    ("  BLOCKED  ", CLR_RED, CLR_RED),
                FileVerdict::Clean =>
                    ("  CLEAN    ", CLR_DIM, CLR_DIM),
            };

            let stats = format!(
                "  +{} −{}",
                af.diff.additions,
                af.diff.deletions,
            );

            let line = Line::from(vec![
                Span::styled(tag, Style::default().fg(tag_color).add_modifier(Modifier::BOLD)),
                Span::raw("  "),
                Span::styled(
                    af.diff.path.display().to_string(),
                    Style::default().fg(path_color),
                ),
                Span::styled(stats, Style::default().fg(CLR_DIM)),
            ]);

            ListItem::new(line)
        }).collect(),
    };

    let list = List::new(items).block(block);
    f.render_widget(list, area);
}

// ── Dashboard stats panel (toggled with 'd') ──────────────────────────────────

fn render_dashboard_panel(
    f: &mut Frame,
    area: Rect,
    session: Option<&crate::session::Session>,
    files: Option<&[crate::policy::AnnotatedFile]>,
    state: &WatchState,
) {
    let block = Block::default()
        .title(Span::styled(" Dashboard ", Style::default().fg(CLR_PURPLE).add_modifier(Modifier::BOLD)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(CLR_BORDER))
        .style(Style::default().bg(CLR_BG));

    let mut lines: Vec<Line> = Vec::new();

    // Session info
    if let Some(s) = session {
        lines.push(Line::from(vec![
            Span::styled("  Mission  ", Style::default().fg(CLR_DIM)),
        ]));
        // Truncate mission to fit panel
        let mission_display = if s.mission.len() > 30 {
            format!("{}…", &s.mission[..29])
        } else {
            s.mission.clone()
        };
        lines.push(Line::from(vec![
            Span::styled(format!("  {}", mission_display), Style::default().fg(CLR_WHITE)),
        ]));
        lines.push(Line::from(Span::raw("")));
    }

    // File stats
    if let Some(files) = files {
        let in_scope = files.iter().filter(|f| f.verdict == FileVerdict::InScope).count();
        let unasked = files.iter().filter(|f| f.verdict == FileVerdict::Unasked).count();
        let blocked = files.iter().filter(|f| f.verdict.is_blocked()).count();
        let total_add: usize = files.iter().map(|f| f.diff.additions).sum();
        let total_del: usize = files.iter().map(|f| f.diff.deletions).sum();

        lines.push(Line::from(vec![
            Span::styled("  ── Files ──", Style::default().fg(CLR_DIM)),
        ]));
        lines.push(Line::from(vec![
            Span::styled(format!("  {} total", files.len()), Style::default().fg(CLR_WHITE)),
        ]));
        lines.push(Line::from(vec![
            Span::styled(format!("  {} in scope", in_scope), Style::default().fg(CLR_GREEN)),
        ]));
        lines.push(Line::from(vec![
            Span::styled(format!("  {} unasked", unasked), Style::default().fg(CLR_AMBER)),
        ]));
        lines.push(Line::from(vec![
            Span::styled(format!("  {} blocked", blocked), Style::default().fg(CLR_RED)),
        ]));
        lines.push(Line::from(Span::raw("")));

        lines.push(Line::from(vec![
            Span::styled("  ── Lines ──", Style::default().fg(CLR_DIM)),
        ]));
        lines.push(Line::from(vec![
            Span::styled(format!("  +{}", total_add), Style::default().fg(CLR_GREEN)),
            Span::styled(format!("  -{}", total_del), Style::default().fg(CLR_RED)),
        ]));
        lines.push(Line::from(Span::raw("")));

        // Health bar
        let total = files.len().max(1);
        let health_pct = (in_scope * 100) / total;
        let filled = (health_pct / 10).min(10);
        let empty = 10 - filled;
        let bar_color = if blocked > 0 { CLR_RED } else if unasked > 0 { CLR_AMBER } else { CLR_GREEN };

        lines.push(Line::from(vec![
            Span::styled("  Health  ", Style::default().fg(CLR_DIM)),
            Span::styled("█".repeat(filled), Style::default().fg(bar_color)),
            Span::styled("░".repeat(empty), Style::default().fg(CLR_DIM)),
            Span::styled(format!(" {}%", health_pct), Style::default().fg(bar_color)),
        ]));
    }

    lines.push(Line::from(Span::raw("")));

    // Uptime
    lines.push(Line::from(vec![
        Span::styled("  ── Watch ──", Style::default().fg(CLR_DIM)),
    ]));
    lines.push(Line::from(vec![
        Span::styled(format!("  Uptime  {}", state.uptime_str()), Style::default().fg(CLR_MUTED)),
    ]));
    lines.push(Line::from(vec![
        Span::styled(format!("  Cycles  {}", state.refresh_count), Style::default().fg(CLR_MUTED)),
    ]));

    lines.push(Line::from(Span::raw("")));

    // Quick commands
    lines.push(Line::from(vec![
        Span::styled("  ── Commands ──", Style::default().fg(CLR_DIM)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  agentscope judge", Style::default().fg(CLR_CYAN)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  agentscope report", Style::default().fg(CLR_CYAN)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  agentscope diff", Style::default().fg(CLR_CYAN)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  agentscope hook install", Style::default().fg(CLR_CYAN)),
    ]));

    let para = Paragraph::new(lines).block(block);
    f.render_widget(para, area);
}

fn render_summary_bar(
    f: &mut Frame,
    area: Rect,
    files: Option<&[crate::policy::AnnotatedFile]>,
    state: &WatchState,
) {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(CLR_BORDER))
        .style(Style::default().bg(CLR_BG));

    let line = if let Some(flash) = state.active_flash() {
        // Show flash message (green, fades after 2s)
        Line::from(Span::styled(
            format!("  {}", flash),
            Style::default().fg(CLR_GREEN).add_modifier(Modifier::BOLD),
        ))
    } else if let Some(files) = files {
        let in_scope = files.iter().filter(|f| f.verdict == FileVerdict::InScope).count();
        let unasked = files.iter().filter(|f| f.verdict == FileVerdict::Unasked).count();
        let blocked = files.iter().filter(|f| f.verdict.is_blocked()).count();

        // Pulse indicator — blinks to show the TUI is alive
        let pulse = if state.refresh_count % 4 < 2 { "●" } else { "○" };

        let dashboard_hint = if state.show_dashboard { "d=hide" } else { "d=dashboard" };

        Line::from(vec![
            Span::styled(format!("  {} in scope", in_scope), Style::default().fg(CLR_GREEN)),
            Span::styled("  ·  ", Style::default().fg(CLR_DIM)),
            Span::styled(format!("{} unasked", unasked), Style::default().fg(CLR_AMBER)),
            Span::styled("  ·  ", Style::default().fg(CLR_DIM)),
            Span::styled(format!("{} blocked", blocked), Style::default().fg(CLR_RED)),
            Span::styled(
                format!("    {} live  r=refresh  {}  ?=help", pulse, dashboard_hint),
                Style::default().fg(CLR_DIM),
            ),
        ])
    } else {
        Line::from(Span::styled(
            "  no session    d=dashboard  q=quit  ?=help",
            Style::default().fg(CLR_DIM),
        ))
    };

    let para = Paragraph::new(line).block(block);
    f.render_widget(para, area);
}
