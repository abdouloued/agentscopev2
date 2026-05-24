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

/// TUI state for flash messages and refresh counter
struct WatchState {
    flash: Option<(String, Instant)>,
    refresh_count: u64,
}

impl WatchState {
    fn new() -> Self {
        Self {
            flash: None,
            refresh_count: 0,
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
                    // Check hint
                    (KeyCode::Char('c'), _) => {
                        state.set_flash("→ run `agentscope check` in another terminal");
                    }
                    // Help
                    (KeyCode::Char('?') | KeyCode::Char('h'), _) => {
                        state.set_flash("r=refresh  c=check  q/esc=quit  ?=help");
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
            Constraint::Min(10),   // file list
            Constraint::Length(3), // summary bar
        ])
        .split(area);

    render_header(f, layout[0], session);
    render_file_list(f, layout[1], files);
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

        Line::from(vec![
            Span::styled(format!("  {} in scope", in_scope), Style::default().fg(CLR_GREEN)),
            Span::styled("  ·  ", Style::default().fg(CLR_DIM)),
            Span::styled(format!("{} unasked", unasked), Style::default().fg(CLR_AMBER)),
            Span::styled("  ·  ", Style::default().fg(CLR_DIM)),
            Span::styled(format!("{} blocked", blocked), Style::default().fg(CLR_RED)),
            Span::styled(
                format!("    {} live  r=refresh  q=quit  ?=help", pulse),
                Style::default().fg(CLR_DIM),
            ),
        ])
    } else {
        Line::from(Span::styled(
            "  no session    q=quit  ?=help",
            Style::default().fg(CLR_DIM),
        ))
    };

    let para = Paragraph::new(line).block(block);
    f.render_widget(para, area);
}
