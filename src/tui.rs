use anyhow::Result;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers, MouseButton,
        MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Bar, BarChart, BarGroup, Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
    Frame, Terminal,
};
use std::{
    cell::Cell,
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crate::agents::{self, ActiveMission, AgentContext};
use crate::config;
use crate::git::{self, DiffContentLine, DiffLineKind};
use crate::judge::{JudgeResult, JudgeVerdict};
use crate::models;
use crate::policy::{AnnotatedFile, FileVerdict, PolicyEngine};
use crate::session::load_active_session;
use crate::theme::Theme;

const POLL_MS: u64 = 150;

pub fn is_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal()
}

pub async fn run_watch() -> Result<()> {
    if !is_tty() {
        anyhow::bail!(
            "agentscope watch requires a real terminal (TTY).\n\
             You appear to be running inside a sandboxed or piped environment.\n\n\
             Alternatives:\n  \
             agentscope monitor      — plain-text polling monitor (works everywhere)\n  \
             agentscope check        — one-shot policy check\n  \
             agentscope diff --problems — quick diff, problems only"
        );
    }
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Set up a file-system watcher for instant dirty detection.
    // The watcher is kept alive in this scope; events fire into a channel.
    let (fs_tx, fs_rx) = std::sync::mpsc::channel::<()>();
    let _fs_watcher: Option<notify::RecommendedWatcher> = {
        use notify::Watcher as _;
        let tx = fs_tx.clone();
        let mut w = notify::RecommendedWatcher::new(
            move |_| {
                let _ = tx.send(());
            },
            notify::Config::default(),
        )
        .ok();
        if let Some(ref mut watcher) = w {
            let _ = watcher.watch(std::path::Path::new("."), notify::RecursiveMode::Recursive);
        }
        w
    };

    let result = run_app(&mut terminal, fs_rx).await;

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InputMode {
    Normal,
    Command,
    Chat,
}

#[derive(Clone, Debug)]
struct TuiChatMessage {
    sender: String,
    content: String,
    pending: bool,
}

#[derive(Clone)]
struct DiffView {
    path: PathBuf,
    lines: Vec<DiffContentLine>,
    scroll: usize,
}

#[derive(Debug, PartialEq, Eq)]
enum TuiCommand {
    Diff(Option<PathBuf>),
    Status,
    Judge,
    JudgeProvider(Option<String>),
    JudgeModel(Option<String>),
    OllamaModels,
    OllamaModel(Option<String>),
    Check,
    Problems,
    Agents,
    Agent(Option<String>),
    Mission,
    RefreshAgents,
    Dashboard,
    Live,
    Allow(Option<String>),
    Block(Option<String>),
    Theme(Option<String>),
    Clear,
    ClearChat,
    Help,
    Quit,
    // Chat
    Chat,
    NewChat(Option<String>),
    Chats,
    DeleteChat(Option<String>),
    ChatSessions(Option<String>),
    ChatLatest(Option<String>),
    Ask(String),
    Explain(Option<String>),
    ChatReport,
    ChatFilter(Option<String>),
    ChatContext,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CommandSpec {
    name: &'static str,
    args: &'static str,
    description: &'static str,
}

const COMMAND_SPECS: &[CommandSpec] = &[
    CommandSpec {
        name: "/status",
        args: "",
        description: "Refresh session and file summary",
    },
    CommandSpec {
        name: "/diff",
        args: "[file]",
        description: "Open colored diff for selected or named file",
    },
    CommandSpec {
        name: "/check",
        args: "",
        description: "Summarize policy status in the activity log",
    },
    CommandSpec {
        name: "/judge",
        args: "",
        description: "Run the configured LLM judge",
    },
    CommandSpec {
        name: "/judge-provider",
        args: "[claude|codex|ollama]",
        description: "List or switch judge provider",
    },
    CommandSpec {
        name: "/judge-model",
        args: "[model]",
        description: "List or set judge model",
    },
    CommandSpec {
        name: "/judge-models",
        args: "[model]",
        description: "Alias for /judge-model",
    },
    CommandSpec {
        name: "/ollama-models",
        args: "",
        description: "List installed Ollama models",
    },
    CommandSpec {
        name: "/ollama-model",
        args: "[model]",
        description: "Set installed Ollama model",
    },
    CommandSpec {
        name: "/problems",
        args: "",
        description: "Toggle blocked/unasked filter",
    },
    CommandSpec {
        name: "/agents",
        args: "",
        description: "Show active/stale detected agent missions",
    },
    CommandSpec {
        name: "/agent",
        args: "[name]",
        description: "Filter view to one agent",
    },
    CommandSpec {
        name: "/mission",
        args: "",
        description: "Show full active mission context",
    },
    CommandSpec {
        name: "/refresh-agents",
        args: "",
        description: "Re-detect agent missions now",
    },
    CommandSpec {
        name: "/dashboard",
        args: "",
        description: "Toggle dashboard view",
    },
    CommandSpec {
        name: "/live",
        args: "",
        description: "Open live file-change monitor",
    },
    CommandSpec {
        name: "/allow",
        args: "[file|glob]",
        description: "Persist an allow override in agentscope.yaml",
    },
    CommandSpec {
        name: "/block",
        args: "[file|glob]",
        description: "Persist a blocked pattern in agentscope.yaml",
    },
    CommandSpec {
        name: "/theme",
        args: "[agentscope|codex|claude|openclaw|high-contrast]",
        description: "List or switch the TUI theme",
    },
    CommandSpec {
        name: "/clear",
        args: "",
        description: "Clear the activity log",
    },
    CommandSpec {
        name: "/clear-chat",
        args: "",
        description: "Clear visible chat messages",
    },
    CommandSpec {
        name: "/help",
        args: "",
        description: "Show command help",
    },
    CommandSpec {
        name: "/quit",
        args: "",
        description: "Exit watch mode",
    },
    CommandSpec {
        name: "/chat",
        args: "",
        description: "Toggle the chat panel (c key shortcut)",
    },
    CommandSpec {
        name: "/new-chat",
        args: "[title]",
        description: "Create a new persistent chat session",
    },
    CommandSpec {
        name: "/chats",
        args: "",
        description: "List saved chat sessions in the activity log",
    },
    CommandSpec {
        name: "/delete-chat",
        args: "[id]",
        description: "Archive current or named chat session",
    },
    CommandSpec {
        name: "/sessions",
        args: "[agent]",
        description: "List local agent sessions (Codex, Claude, …)",
    },
    CommandSpec {
        name: "/latest",
        args: "[agent]",
        description: "Show the latest agent session for each agent",
    },
    CommandSpec {
        name: "/ask",
        args: "<question>",
        description: "Ask a quick question without switching to chat",
    },
    CommandSpec {
        name: "/explain",
        args: "[selected|file]",
        description: "Explain a selected file in Chat",
    },
    CommandSpec {
        name: "/report",
        args: "",
        description: "Post a scope report in Chat",
    },
    CommandSpec {
        name: "/filter",
        args: "[suspicious|all]",
        description: "Filter review files from Chat",
    },
    CommandSpec {
        name: "/chat-context",
        args: "",
        description: "Show Chat's visible context",
    },
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum AppMode {
    Review,
    Chat,
    Dashboard,
    Sessions,
    Live,
}

/// TUI state
struct WatchState {
    flash: Option<(String, Instant)>,
    refresh_count: u64,
    mode: AppMode,
    input_mode: InputMode,
    input_buf: String,
    command_selected: usize,
    selected_file: usize,
    file_scroll: usize,
    agent_filter: Option<String>,
    problems_only: bool,
    diff_view: Option<DiffView>,
    output_log: Vec<(String, Color)>,
    active_missions: Vec<ActiveMission>,
    ignored_contexts: Vec<AgentContext>,
    ollama_models: Vec<String>,
    judge_provider_label: String,
    judge_model: String,
    theme: Theme,
    started_at: Instant,
    judge_status: Arc<Mutex<JudgeStatus>>,
    /// Track line history for sparkline (last 20 samples)
    line_history: Vec<u64>,
    /// Left pane width as a percentage (20-80, default 58); adjusted with `[`/`]`
    split_pct: u16,
    /// True while the user is dragging the vertical divider.
    /// Only meaningful when mouse capture is enabled.
    resize_drag: bool,
    /// Cached screen column of the vertical divider (set each frame for drag detection)
    divider_col: u16,
    /// In-memory chat message history for the current TUI session
    chat_messages: Vec<TuiChatMessage>,
    /// Scroll offset for the chat transcript
    chat_scroll: usize,
    /// Receives the async LLM reply; polled each tick
    chat_pending: Arc<Mutex<Option<String>>>,
    /// Active persistent chat session ULID (None = ephemeral)
    chat_session_id: Option<String>,
    /// Selected row index in Sessions mode
    selected_session: usize,
    /// True while the help overlay is shown
    show_help: bool,
    /// Inner content rect of the file list, updated each frame for click-to-select
    file_list_area: Cell<Rect>,
    /// Inner content rect of the chat transcript, updated each frame for double-click copy
    chat_transcript_area: Cell<Rect>,
    /// True when the TUI owns mouse events; false lets the terminal do native text selection
    mouse_capture: bool,
    /// Last left-click coordinates/time for simple double-click detection
    last_click: Option<(Instant, u16, u16)>,
    /// Tracks when each file was last observed to change (for freshness badges)
    file_freshness: HashMap<PathBuf, Instant>,
    /// Previous file add/delete counts for change detection
    prev_file_stats: HashMap<PathBuf, (usize, usize)>,
    /// Path of the file whose inline diff is loaded below
    inline_diff_path: Option<PathBuf>,
    /// Inline diff lines for the currently selected file (auto-loaded)
    inline_diff_lines: Vec<DiffContentLine>,
    /// Receiver for file-system watcher events (instant dirty detection)
    fs_event_rx: Option<std::sync::mpsc::Receiver<()>>,
    /// Count of filesystem events observed in this watch session
    live_event_count: u64,
    /// Timestamp of the most recent filesystem event
    last_fs_event: Option<Instant>,
}

impl WatchState {
    fn new(theme_name: &str) -> Self {
        Self {
            flash: None,
            refresh_count: 0,
            mode: AppMode::Review,
            input_mode: InputMode::Normal,
            input_buf: String::new(),
            command_selected: 0,
            selected_file: 0,
            file_scroll: 0,
            agent_filter: None,
            problems_only: false,
            diff_view: None,
            output_log: Vec::new(),
            active_missions: Vec::new(),
            ignored_contexts: Vec::new(),
            ollama_models: Vec::new(),
            judge_provider_label: "ollama".into(),
            judge_model: String::new(),
            theme: Theme::by_name(theme_name),
            started_at: Instant::now(),
            judge_status: Arc::new(Mutex::new(JudgeStatus::Idle)),
            line_history: Vec::new(),
            split_pct: 58,
            resize_drag: false,
            divider_col: 0,
            chat_messages: Vec::new(),
            chat_scroll: 0,
            chat_pending: Arc::new(Mutex::new(None)),
            chat_session_id: None,
            selected_session: 0,
            show_help: false,
            file_list_area: Cell::new(Rect::default()),
            chat_transcript_area: Cell::new(Rect::default()),
            mouse_capture: true,
            last_click: None,
            file_freshness: HashMap::new(),
            prev_file_stats: HashMap::new(),
            inline_diff_path: None,
            inline_diff_lines: Vec::new(),
            fs_event_rx: None,
            live_event_count: 0,
            last_fs_event: None,
        }
    }

    fn set_flash(&mut self, msg: &str) {
        self.flash = Some((msg.to_string(), Instant::now()));
    }

    fn push_log(&mut self, msg: impl Into<String>, color: Color) {
        self.output_log.push((msg.into(), color));
        if self.output_log.len() > 64 {
            self.output_log.remove(0);
        }
    }

    fn clamp_selection(&mut self, len: usize) {
        if len == 0 {
            self.selected_file = 0;
            self.file_scroll = 0;
        } else if self.selected_file >= len {
            self.selected_file = len - 1;
        }
    }

    fn scroll_file_selection(&mut self, delta: isize, len: usize) {
        if len == 0 {
            self.selected_file = 0;
            self.file_scroll = 0;
            return;
        }
        if delta.is_negative() {
            self.selected_file = self.selected_file.saturating_sub(delta.unsigned_abs());
        } else {
            self.selected_file = (self.selected_file + delta as usize).min(len - 1);
        }
        self.ensure_selected_file_visible();
    }

    fn ensure_selected_file_visible(&mut self) {
        let height = self.file_list_area.get().height.max(1) as usize;
        if self.selected_file < self.file_scroll {
            self.file_scroll = self.selected_file;
        } else if self.selected_file >= self.file_scroll + height {
            self.file_scroll = self.selected_file.saturating_sub(height.saturating_sub(1));
        }
    }

    fn clamp_command_selection(&mut self) {
        let count = command_value_suggestions(&self.input_buf, self)
            .map(|values| values.len())
            .unwrap_or_else(|| command_suggestions(&self.input_buf).len());
        if count == 0 {
            self.command_selected = 0;
        } else if self.command_selected >= count {
            self.command_selected = count - 1;
        }
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

async fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    fs_rx: std::sync::mpsc::Receiver<()>,
) -> Result<()> {
    let mut config = config::load_or_default();
    let mut state = WatchState::new(&config.tui.theme);
    state.fs_event_rx = Some(fs_rx);
    sync_judge_display(&config, &mut state);

    loop {
        state.refresh_count += 1;

        // ── File-system watcher events ────────────────────────────────────────
        let mut fs_events = 0u64;
        if let Some(rx) = state.fs_event_rx.as_ref() {
            while rx.try_recv().is_ok() {
                fs_events += 1;
            }
        }
        if fs_events > 0 {
            state.live_event_count = state.live_event_count.saturating_add(fs_events);
            state.last_fs_event = Some(Instant::now());
            state.set_flash(&format!("live: {} file event(s)", fs_events));
        }

        // ── Poll pending chat response ─────────────────────────────────────────
        {
            let mut pending = state.chat_pending.lock().unwrap();
            if let Some(text) = pending.take() {
                if let Some(msg) = state.chat_messages.iter_mut().rev().find(|m| m.pending) {
                    msg.content = text.clone();
                    msg.pending = false;
                    if let Some(ref chat_id) = state.chat_session_id.clone() {
                        let _ = crate::chat::append_message(chat_id, "assistant", &text);
                    }
                }
                // Auto-scroll to bottom on new reply
                let total = state.chat_messages.len();
                if total > 5 {
                    state.chat_scroll = total.saturating_sub(5);
                }
            }
        }

        sync_judge_display(&config, &mut state);
        if state.refresh_count == 1 || state.refresh_count % 40 == 0 {
            refresh_agent_missions(&config, &mut state);
        }

        let session = load_active_session().ok();
        let mission_pairs = active_mission_pairs(&state);
        let fallback_mission = session
            .as_ref()
            .map(|s| vec![(s.agent.clone(), s.mission.clone())])
            .unwrap_or_default();
        let effective_missions = if mission_pairs.is_empty() {
            fallback_mission.as_slice()
        } else {
            mission_pairs.as_slice()
        };
        let baseline = session.as_ref().map(|s| s.git_baseline.as_str());
        let files = if !effective_missions.is_empty() || session.is_some() {
            git::open_repo()
                .and_then(|repo| git::working_tree_diff_from(&repo, baseline))
                .ok()
                .and_then(|diff| {
                    let engine = PolicyEngine::from_config(&config.policy).ok();
                    engine.map(|e| e.annotate_with_missions(&diff.files, effective_missions))
                })
        } else {
            None
        };
        let visible_files = sorted_visible_files(files.as_deref(), state.problems_only);
        state.clamp_selection(visible_files.len());
        state.ensure_selected_file_visible();

        // ── File freshness tracking ───────────────────────────────────────────
        // Detect when a file's stats changed and record the timestamp so we can
        // show a "just now / 5s ago" badge in the file list.
        for af in &visible_files {
            let key = af.diff.path.clone();
            let stats = (af.diff.additions, af.diff.deletions);
            if state.prev_file_stats.get(&key) != Some(&stats) {
                state.file_freshness.insert(key.clone(), Instant::now());
                state.prev_file_stats.insert(key, stats);
            }
        }
        // Evict stale paths (deleted files) after 60s
        state
            .file_freshness
            .retain(|_, when| when.elapsed() < Duration::from_secs(60));

        // ── Auto-load inline diff for selected file ──────────────────────────
        let selected_path = visible_files
            .get(state.selected_file)
            .map(|f| f.diff.path.clone());
        if selected_path != state.inline_diff_path {
            state.inline_diff_path = selected_path.clone();
            if let Some(ref path) = selected_path {
                state.inline_diff_lines = git::open_repo()
                    .and_then(|repo| git::file_diff_content(&repo, path))
                    .unwrap_or_default();
            } else {
                state.inline_diff_lines.clear();
            }
        }

        // Record line history for sparkline
        if let Some(ref f) = files {
            let total: u64 = f
                .iter()
                .map(|af| (af.diff.additions + af.diff.deletions) as u64)
                .sum();
            state.record_lines(total);
        }

        terminal.draw(|f| ui(f, session.as_ref(), files.as_deref(), &state))?;

        // Cache the divider column for mouse drag detection (non-dashboard view only)
        if state.mode == AppMode::Review {
            if let Ok(size) = terminal.size() {
                state.divider_col = 1 + size.width.saturating_sub(2) * state.split_pct / 100;
            }
        }

        if event::poll(Duration::from_millis(POLL_MS))? {
            let ev = event::read()?;

            // ── Mouse events ──────────────────────────────────────────────────────────
            if let Event::Mouse(mouse) = &ev {
                if !state.mouse_capture {
                    // Mouse capture is off — let the terminal do native text selection
                    continue;
                }
                match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        if state.input_mode == InputMode::Command {
                            state.command_selected = state.command_selected.saturating_sub(1);
                        } else if state.diff_view.is_some() {
                            if let Some(ref mut v) = state.diff_view {
                                v.scroll = v.scroll.saturating_sub(3);
                            }
                        } else if state.mode == AppMode::Chat {
                            state.chat_scroll = state.chat_scroll.saturating_sub(1);
                        } else {
                            state.scroll_file_selection(-1, visible_files.len());
                        }
                    }
                    MouseEventKind::ScrollDown => {
                        if state.input_mode == InputMode::Command {
                            state.command_selected = state.command_selected.saturating_add(1);
                            state.clamp_command_selection();
                        } else if state.diff_view.is_some() {
                            if let Some(ref mut v) = state.diff_view {
                                v.scroll = v.scroll.saturating_add(3);
                            }
                        } else if state.mode == AppMode::Chat {
                            let max = state.chat_messages.len().saturating_sub(1);
                            if state.chat_scroll < max {
                                state.chat_scroll += 1;
                            }
                        } else {
                            state.scroll_file_selection(1, visible_files.len());
                        }
                    }
                    MouseEventKind::Down(MouseButton::Left) if state.show_help => {
                        state.show_help = false;
                    }
                    MouseEventKind::Down(MouseButton::Left)
                        if state.input_mode == InputMode::Normal
                            && state.diff_view.is_none()
                            && state.mode == AppMode::Review
                            && (mouse.column as i32 - state.divider_col as i32).abs() <= 2 =>
                    {
                        state.resize_drag = true;
                    }
                    MouseEventKind::Down(MouseButton::Left) => {
                        let now = Instant::now();
                        let double_click = state
                            .last_click
                            .map(|(when, col, row)| {
                                when.elapsed() <= Duration::from_millis(450)
                                    && col.abs_diff(mouse.column) <= 1
                                    && row.abs_diff(mouse.row) <= 1
                            })
                            .unwrap_or(false);
                        state.last_click = Some((now, mouse.column, mouse.row));
                        if double_click {
                            copy_mouse_target(&mut state, mouse.column, mouse.row, &visible_files);
                            continue;
                        }

                        // Click inside file list → select that row
                        let fla = state.file_list_area.get();
                        if matches!(state.mode, AppMode::Review | AppMode::Live)
                            && state.diff_view.is_none()
                            && mouse.row >= fla.y
                            && mouse.row < fla.y + fla.height
                            && mouse.column >= fla.x
                            && mouse.column < fla.x + fla.width
                        {
                            let row = state.file_scroll + (mouse.row - fla.y) as usize;
                            let n = visible_files.len();
                            if n > 0 {
                                state.selected_file = row.min(n - 1);
                                state.ensure_selected_file_visible();
                            }
                        }
                    }
                    MouseEventKind::Down(MouseButton::Right) => {
                        // Chat mode: right-click copies the message line to clipboard
                        if state.mode == AppMode::Chat {
                            let area = state.chat_transcript_area.get();
                            if mouse.row >= area.y
                                && mouse.row < area.y + area.height
                                && mouse.column >= area.x
                                && mouse.column < area.x + area.width
                            {
                                let lines = chat_plain_lines(&state, area.width);
                                let idx = state.chat_scroll + (mouse.row - area.y) as usize;
                                if let Some(line) = lines.get(idx) {
                                    let text = line.trim().to_string();
                                    if !text.is_empty() {
                                        match copy_to_clipboard(&text) {
                                            Ok(()) => state.set_flash("copied to clipboard"),
                                            Err(e) => state.push_log(
                                                format!("copy failed: {}", e),
                                                state.theme.danger,
                                            ),
                                        }
                                    }
                                }
                            }
                        }
                        // Review mode: right-click opens diff
                        let fla = state.file_list_area.get();
                        if matches!(state.mode, AppMode::Review | AppMode::Live)
                            && state.diff_view.is_none()
                            && mouse.row >= fla.y
                            && mouse.row < fla.y + fla.height
                            && mouse.column >= fla.x
                            && mouse.column < fla.x + fla.width
                        {
                            let row = state.file_scroll + (mouse.row - fla.y) as usize;
                            let n = visible_files.len();
                            if n > 0 {
                                state.selected_file = row.min(n - 1);
                                state.ensure_selected_file_visible();
                            }
                            if let Some(af) = visible_files.get(state.selected_file) {
                                let path = af.diff.path.clone();
                                open_diff_for_path(&mut state, &path);
                            }
                        }
                    }
                    MouseEventKind::Drag(MouseButton::Left) if state.resize_drag => {
                        if let Ok(size) = terminal.size() {
                            let main_w = size.width.saturating_sub(2).max(1) as u32;
                            let col = mouse.column.saturating_sub(1) as u32;
                            let pct = (col * 100 / main_w) as u16;
                            state.split_pct = pct.clamp(20, 80);
                        }
                    }
                    MouseEventKind::Drag(MouseButton::Left) => {}
                    MouseEventKind::Up(_) => {
                        state.resize_drag = false;
                    }
                    _ => {}
                }
            }

            // ── Keyboard events ───────────────────────────────────────────────────────
            if let Event::Key(key) = ev {
                if state.input_mode == InputMode::Command {
                    match key.code {
                        KeyCode::Esc => {
                            state.input_mode = InputMode::Normal;
                            state.input_buf.clear();
                            state.command_selected = 0;
                        }
                        KeyCode::Backspace => {
                            state.input_buf.pop();
                        }
                        KeyCode::Enter => {
                            let has_value_selection =
                                command_value_suggestions(&state.input_buf, &state)
                                    .map(|values| !values.is_empty())
                                    .unwrap_or(false);
                            let input =
                                if command_is_incomplete(&state.input_buf) || has_value_selection {
                                    autocomplete_command_input(&state.input_buf, &state)
                                } else {
                                    state.input_buf.clone()
                                };
                            state.input_mode = InputMode::Normal;
                            state.input_buf.clear();
                            state.command_selected = 0;
                            match parse_tui_command(&input) {
                                Ok(TuiCommand::Quit) => break,
                                Ok(command) => {
                                    handle_tui_command(
                                        command,
                                        &mut config,
                                        &mut state,
                                        session.as_ref(),
                                        files.as_deref(),
                                    )
                                    .await;
                                }
                                Err(err) => {
                                    let color = state.theme.danger;
                                    state.push_log(format!("command error: {}", err), color);
                                }
                            }
                        }
                        KeyCode::Tab => {
                            state.input_buf = autocomplete_command_input(&state.input_buf, &state);
                            state.command_selected = 0;
                        }
                        KeyCode::Up => {
                            state.command_selected = state.command_selected.saturating_sub(1);
                        }
                        KeyCode::Down => {
                            state.command_selected = state.command_selected.saturating_add(1);
                            state.clamp_command_selection();
                        }
                        KeyCode::Char(c) => {
                            state.input_buf.push(c);
                            state.command_selected = 0;
                            if state.input_buf.trim_start().starts_with("/ollama-model ")
                                && state.ollama_models.is_empty()
                            {
                                refresh_ollama_models(&config, &mut state).await;
                            }
                        }
                        _ => {}
                    }
                    continue;
                }

                if state.diff_view.is_some() {
                    match key.code {
                        KeyCode::Esc => state.diff_view = None,
                        KeyCode::Up => {
                            if let Some(view) = state.diff_view.as_mut() {
                                view.scroll = view.scroll.saturating_sub(1);
                            }
                        }
                        KeyCode::Down => {
                            if let Some(view) = state.diff_view.as_mut() {
                                view.scroll = view.scroll.saturating_add(1);
                            }
                        }
                        KeyCode::PageUp => {
                            if let Some(view) = state.diff_view.as_mut() {
                                view.scroll = view.scroll.saturating_sub(10);
                            }
                        }
                        KeyCode::PageDown => {
                            if let Some(view) = state.diff_view.as_mut() {
                                view.scroll = view.scroll.saturating_add(10);
                            }
                        }
                        _ => {}
                    }
                    continue;
                }

                // ── Chat compose mode ──────────────────────────────────────────────────
                if state.input_mode == InputMode::Chat {
                    match key.code {
                        KeyCode::Esc => {
                            state.input_mode = InputMode::Normal;
                            state.input_buf.clear();
                        }
                        KeyCode::Backspace => {
                            state.input_buf.pop();
                        }
                        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            state.input_buf.clear();
                        }
                        KeyCode::Enter => {
                            let text = state.input_buf.trim().to_string();
                            if !text.is_empty() {
                                state.input_buf.clear();
                                send_chat_message(
                                    &text,
                                    &config,
                                    &mut state,
                                    session.as_ref(),
                                    files.as_deref(),
                                );
                            }
                        }
                        KeyCode::Char(c) => {
                            if c == '/' && state.input_buf.is_empty() {
                                state.input_mode = InputMode::Command;
                                state.input_buf = "/".into();
                                state.command_selected = 0;
                            } else {
                                state.input_buf.push(c);
                            }
                        }
                        _ => {}
                    }
                    continue;
                }

                match (key.code, key.modifiers) {
                    (KeyCode::Char('q'), _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => break,

                    (KeyCode::Esc, _) => {
                        if state.show_help {
                            state.show_help = false;
                        } else if state.mode == AppMode::Chat {
                            state.set_flash("chat");
                        } else {
                            state.problems_only = false;
                            state.set_flash("filter cleared");
                        }
                    }
                    (KeyCode::Char('/'), _) if state.mode == AppMode::Chat => {
                        state.show_help = false;
                        state.input_mode = InputMode::Command;
                        state.input_buf = "/".into();
                        state.command_selected = 0;
                    }
                    (KeyCode::Char('/'), _) => {
                        state.show_help = false;
                        state.input_mode = InputMode::Command;
                        state.input_buf = "/".into();
                        state.command_selected = 0;
                    }
                    (KeyCode::Up, _) => {
                        if state.mode == AppMode::Chat {
                            state.chat_scroll = state.chat_scroll.saturating_sub(1);
                        } else if state.mode == AppMode::Sessions {
                            state.selected_session = state.selected_session.saturating_sub(1);
                        } else {
                            state.scroll_file_selection(-1, visible_files.len());
                        }
                    }
                    (KeyCode::Down, _) => {
                        if state.mode == AppMode::Chat {
                            let max = state.chat_messages.len().saturating_sub(1);
                            if state.chat_scroll < max {
                                state.chat_scroll += 1;
                            }
                        } else if state.mode == AppMode::Sessions {
                            let max_session = state.active_missions.len().saturating_sub(1);
                            if state.selected_session < max_session {
                                state.selected_session += 1;
                            }
                        } else {
                            state.scroll_file_selection(1, visible_files.len());
                        }
                    }
                    (KeyCode::Enter, _) => {
                        if state.mode == AppMode::Chat {
                            state.input_mode = InputMode::Chat;
                            state.input_buf.clear();
                        } else if state.mode == AppMode::Sessions {
                            // Inspect: switch to Review filtered by selected agent
                            if let Some(m) = state.active_missions.get(state.selected_session) {
                                let agent = m.agent.clone();
                                let agent_label = agent.to_uppercase();
                                state.agent_filter = Some(agent);
                                state.mode = AppMode::Review;
                                state.set_flash(&format!("review — filtered to {}", agent_label));
                            }
                        } else if let Some(file) = visible_files.get(state.selected_file) {
                            open_diff_for_path(&mut state, &file.diff.path);
                        }
                    }

                    // Mode switching: 1=Review, 2=Chat, 3=Dashboard, 4=Sessions, 5=Live
                    (KeyCode::Char('1'), _) => {
                        state.mode = AppMode::Review;
                        state.set_flash("review");
                    }
                    (KeyCode::Char('2'), _) => {
                        state.mode = AppMode::Chat;
                        state.set_flash("chat  i=compose  ↑↓=scroll");
                    }
                    (KeyCode::Char('3'), _) => {
                        state.mode = AppMode::Dashboard;
                        state.set_flash("dashboard");
                    }
                    (KeyCode::Char('4'), _) => {
                        state.mode = AppMode::Sessions;
                        state.set_flash("sessions");
                    }
                    (KeyCode::Char('5'), _) => {
                        state.mode = AppMode::Live;
                        state.set_flash("live changes");
                    }

                    (KeyCode::Char('r'), _) => {
                        state.set_flash("⟳ refreshed");
                    }
                    // Run judge inline
                    (KeyCode::Char('j'), _) => {
                        start_judge(&config, &mut state, session.as_ref(), files.as_deref());
                    }
                    // Cycle theme
                    (KeyCode::Char('t'), _) => {
                        let next = Theme::next_name(state.theme.name);
                        state.theme = Theme::by_name(next);
                        state.set_flash(&format!("theme: {}", next));
                    }
                    (KeyCode::Char('m'), _) => {
                        state.mouse_capture = !state.mouse_capture;
                        if state.mouse_capture {
                            let _ = execute!(io::stdout(), EnableMouseCapture);
                            state.set_flash("mouse: panel scroll on");
                        } else {
                            let _ = execute!(io::stdout(), DisableMouseCapture);
                            state.set_flash("mouse: text selection on");
                        }
                    }
                    (KeyCode::Char('?') | KeyCode::Char('h'), _) => {
                        state.show_help = !state.show_help;
                    }
                    // Enter chat compose mode when in Chat mode
                    (KeyCode::Char('i'), _) if state.mode == AppMode::Chat => {
                        state.input_mode = InputMode::Chat;
                        state.input_buf.clear();
                    }
                    // Sessions mode: n = new mission hint
                    (KeyCode::Char('n'), _) if state.mode == AppMode::Sessions => {
                        state.push_log(
                            "to start a mission: agentscope start \"<goal>\" --agent <agent>",
                            state.theme.accent,
                        );
                        state.set_flash("run agentscope start to create a mission");
                    }
                    // Pane resize: [ = shrink file list, ] = grow file list
                    (KeyCode::Char('['), _) => {
                        state.split_pct = state.split_pct.saturating_sub(3).max(20);
                    }
                    (KeyCode::Char(']'), _) => {
                        state.split_pct = (state.split_pct + 3).min(80);
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
    let theme = &state.theme;
    let bg = Block::default().style(Style::default().bg(theme.bg));
    f.render_widget(bg, area);

    // Chat gets the full content area — no header bar needed there.
    let chat_mode = state.mode == AppMode::Chat;
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints(if chat_mode {
            vec![Constraint::Min(10), Constraint::Length(3)]
        } else {
            vec![
                Constraint::Length(3), // header
                Constraint::Min(10),   // main
                Constraint::Length(3), // status bar
            ]
        })
        .split(area);

    // layout indices differ by mode: chat=[content, bar], others=[header, content, bar]
    let (content_idx, bar_idx) = if chat_mode { (0, 1) } else { (1, 2) };

    if !chat_mode {
        render_header(f, layout[0], session, files, state);
    }

    match state.mode {
        AppMode::Dashboard => {
            render_dashboard(f, layout[content_idx], session, files, state);
        }
        AppMode::Live => {
            render_live_mode(f, layout[content_idx], files, state);
        }
        AppMode::Chat => {
            render_chat_pane(f, layout[content_idx], session, files, state);
        }
        AppMode::Sessions => {
            render_sessions(f, layout[content_idx], files, state);
        }
        AppMode::Review => {
            let narrow = area.width < 120;
            let compact = area.height < 30;
            let review_area = if compact {
                layout[content_idx]
            } else {
                let main = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(6),
                        Constraint::Min(8),
                        Constraint::Length(8),
                    ])
                    .split(layout[content_idx]);
                render_agent_missions(f, main[0], state);
                render_activity_log(f, main[2], state);
                main[1]
            };
            if narrow {
                // Narrow: stack file list above detail panel
                let review = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
                    .split(review_area);
                render_file_list(f, review[0], files, state);
                render_file_detail(f, review[1], files, state);
            } else {
                let review = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([
                        Constraint::Percentage(state.split_pct),
                        Constraint::Percentage(100 - state.split_pct),
                    ])
                    .split(review_area);
                render_file_list(f, review[0], files, state);
                render_file_detail(f, review[1], files, state);
            }
        }
    }

    render_summary_bar(f, layout[bar_idx], files, state);
    if state.input_mode == InputMode::Command {
        render_command_menu(f, area, state);
    }
    if let Some(view) = &state.diff_view {
        render_diff_overlay(f, area, view, theme);
    }
    if state.show_help {
        render_help_overlay(f, area, theme);
    }
}

// ── Header ────────────────────────────────────────────────────────────────────

fn render_header(
    f: &mut Frame,
    area: Rect,
    session: Option<&crate::session::Session>,
    files: Option<&[AnnotatedFile]>,
    state: &WatchState,
) {
    let theme = &state.theme;
    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(theme.border));

    let mode_label = match state.mode {
        AppMode::Review => "review",
        AppMode::Chat => "chat",
        AppMode::Dashboard => "dashboard",
        AppMode::Sessions => "sessions",
        AppMode::Live => "live",
    };

    // Pulsing LIVE dot: alternates filled/dim every ~500ms (3 ticks at 150ms)
    let live_dot = if (state.refresh_count / 3) % 2 == 0 {
        "● "
    } else {
        "○ "
    };

    // Compute file counts
    let (n_expected, n_suspicious, n_blocked) = if let Some(fs) = files {
        let e = fs.iter().filter(|f| f.verdict.is_accepted()).count();
        let s = fs
            .iter()
            .filter(|f| f.verdict == FileVerdict::Unasked)
            .count();
        let b = fs.iter().filter(|f| f.verdict.is_blocked()).count();
        (e, s, b)
    } else {
        (0, 0, 0)
    };

    let n_agents = state.active_missions.len();

    let uptime = format!("up {}", state.uptime_str());
    let mission_max = area
        .width
        .saturating_sub(7 + 5 + uptime.chars().count() as u16 + 3) as usize;

    // Build mission snippet from active missions or session fallback
    let mission_snippet = if let Some(m) = state.active_missions.first() {
        truncate(&m.mission, mission_max)
    } else if let Some(s) = session {
        truncate(&s.mission, mission_max)
    } else {
        "no active session".into()
    };

    let status_line = Line::from(vec![
        Span::styled(
            "agentscope",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ·  ", Style::default().fg(theme.text_subtle)),
        Span::styled(
            live_dot,
            Style::default()
                .fg(theme.success)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("live  ·  ", Style::default().fg(theme.text_subtle)),
        Span::styled(mode_label, Style::default().fg(theme.text_muted)),
        Span::styled("  ·  ", Style::default().fg(theme.text_subtle)),
        Span::styled(
            format!("{} active", n_agents),
            Style::default().fg(theme.text_muted),
        ),
        Span::styled("  ·  ", Style::default().fg(theme.text_subtle)),
        Span::styled(
            format!("{} exp", n_expected),
            Style::default().fg(theme.expected),
        ),
        Span::raw("  "),
        Span::styled(
            format!("{} susp", n_suspicious),
            Style::default().fg(theme.suspicious),
        ),
        Span::raw("  "),
        Span::styled(
            format!("{} block", n_blocked),
            Style::default().fg(theme.blocked),
        ),
    ]);

    let mission_line = Line::from(vec![
        Span::styled("mission ", Style::default().fg(theme.text_subtle)),
        Span::styled(mission_snippet, Style::default().fg(theme.text_muted)),
        Span::styled("  ·  ", Style::default().fg(theme.text_subtle)),
        Span::styled(uptime, Style::default().fg(theme.text_subtle)),
    ]);

    let para = Paragraph::new(vec![status_line, mission_line]).block(block);
    f.render_widget(para, area);
}

// ── File list ─────────────────────────────────────────────────────────────────

fn render_file_list(
    f: &mut Frame,
    area: Rect,
    files: Option<&[AnnotatedFile]>,
    state: &WatchState,
) {
    let theme = &state.theme;
    let block = Block::default()
        .title(Span::styled(
            " Changes ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().bg(theme.bg));

    let inner = block.inner(area);
    let items: Vec<ListItem> = match files {
        None => vec![ListItem::new(Line::from(Span::styled(
            "  waiting for session…",
            Style::default().fg(theme.text_subtle),
        )))],
        Some([]) => vec![
            ListItem::new(Line::from(Span::styled(
                "  ✓  no changes detected",
                Style::default().fg(theme.expected),
            ))),
            ListItem::new(Line::from(Span::styled(
                "  watching all files vs HEAD",
                Style::default()
                    .fg(theme.text_muted)
                    .add_modifier(Modifier::ITALIC),
            ))),
            ListItem::new(Line::from(Span::raw(""))),
            ListItem::new(Line::from(Span::styled(
                "  tip: j=judge  3=dashboard  ?=help",
                Style::default().fg(theme.text_subtle),
            ))),
        ],
        Some(files) => {
            let sorted = sorted_visible_files(Some(files), state.problems_only);

            sorted
                .iter()
                .enumerate()
                .skip(state.file_scroll)
                .take(inner.height as usize)
                .map(|(idx, af)| {
                    let selected = idx == state.selected_file;

                    let (badge, badge_color) = verdict_badge(&af.verdict, theme);
                    let path_color = if selected {
                        theme.text
                    } else {
                        theme.text_muted
                    };
                    let stats_color = theme.text_subtle;

                    let agent_str = matched_agent_tag(&af.matched_agents);
                    let agent_col = agent_str
                        .as_deref()
                        .map(|tag| agent_color_for_tag(tag, theme))
                        .unwrap_or(theme.text_muted);
                    let marker = if selected { "▶" } else { " " };

                    let stats = format!(" +{} -{}", af.diff.additions, af.diff.deletions);

                    // Freshness badge: show age since last change
                    let fresh = state.file_freshness.get(&af.diff.path);
                    let (fresh_str, fresh_color) = if let Some(when) = fresh {
                        let age = when.elapsed().as_secs();
                        if age < 5 {
                            ("●", theme.success)
                        } else if age < 30 {
                            ("~", theme.warning)
                        } else {
                            ("", theme.text_subtle)
                        }
                    } else {
                        ("", theme.text_subtle)
                    };

                    let line = Line::from(vec![
                        Span::styled(format!("{} ", marker), Style::default().fg(theme.accent)),
                        Span::styled(
                            format!("{:<10}", badge),
                            Style::default()
                                .fg(badge_color)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            af.diff.path.display().to_string(),
                            Style::default().fg(path_color),
                        ),
                        Span::styled(stats, Style::default().fg(stats_color)),
                        Span::styled(
                            agent_str
                                .as_deref()
                                .map(|tag| format!("  {}", tag))
                                .unwrap_or_default(),
                            Style::default().fg(agent_col),
                        ),
                        Span::styled(
                            format!(" {}", fresh_str),
                            Style::default()
                                .fg(fresh_color)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]);

                    let row_style = if selected {
                        Style::default().bg(theme.selection_bg)
                    } else {
                        Style::default()
                    };
                    ListItem::new(line).style(row_style)
                })
                .collect()
        }
    };

    state.file_list_area.set(inner);
    let list = List::new(items).block(block);
    f.render_widget(list, area);
}

fn render_agent_missions(f: &mut Frame, area: Rect, state: &WatchState) {
    let theme = &state.theme;
    let block = Block::default()
        .title(Span::styled(
            " Missions ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().bg(theme.bg));

    let lines = if state.active_missions.is_empty() {
        vec![Line::from(Span::styled(
            "  no agent missions detected — run: agentscope start \"mission\"",
            Style::default().fg(theme.text_subtle),
        ))]
    } else {
        state
            .active_missions
            .iter()
            .take(area.height.saturating_sub(1) as usize)
            .map(|mission| {
                let lbl = agent_label(&mission.agent);
                let agent_col = agent_color_for_tag(lbl, theme);
                Line::from(vec![
                    Span::styled(
                        format!("  {:<9}", lbl),
                        Style::default().fg(agent_col).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!(" {:.0}%  ", mission.confidence * 100.0),
                        Style::default().fg(theme.text_subtle),
                    ),
                    Span::styled(
                        format!("{:<6}  ", format_age(mission.age_seconds)),
                        Style::default().fg(theme.text_muted),
                    ),
                    Span::styled(
                        truncate(&mission.mission, area.width.saturating_sub(28) as usize),
                        Style::default().fg(theme.text),
                    ),
                ])
            })
            .collect()
    };
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_file_detail(
    f: &mut Frame,
    area: Rect,
    files: Option<&[AnnotatedFile]>,
    state: &WatchState,
) {
    let theme = &state.theme;

    // Split area: compact verdict info on top, inline diff below
    let min_verdict_height = 10u16;
    let split = if area.height > min_verdict_height + 4 {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(min_verdict_height), Constraint::Min(4)])
            .split(area)
    } else {
        // Too short — give everything to verdict
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(100)])
            .split(area)
    };

    let verdict_area = split[0];

    let block = Block::default()
        .title(Span::styled(
            " Decision ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().bg(theme.bg));

    let sorted_files = sorted_visible_files(files, state.problems_only);
    let selected = sorted_files.get(state.selected_file).copied();

    let lines = if let Some(file) = selected {
        let (badge, badge_color) = verdict_badge(&file.verdict, theme);
        let agent_str = matched_agent_tag(&file.matched_agents).unwrap_or_else(|| "none".into());
        let agent_col = if file.matched_agents.is_empty() {
            theme.text_subtle
        } else {
            agent_color_for_tag(&agent_str, theme)
        };

        let why_text = match &file.verdict {
            FileVerdict::Blocked { policy } => format!("matched blocked policy: {}", policy),
            FileVerdict::Allowed => "explicitly allowed by policy".into(),
            FileVerdict::InScope => "matched the active mission scope".into(),
            FileVerdict::Unasked => "not covered by any active mission".into(),
            FileVerdict::Clean => "no significant changes detected".into(),
        };

        vec![
            Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(
                    format!("{:<10}", badge),
                    Style::default()
                        .fg(badge_color)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    truncate(
                        &file.diff.path.display().to_string(),
                        verdict_area.width.saturating_sub(16) as usize,
                    ),
                    Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::styled("  +", Style::default().fg(theme.diff_add)),
                Span::styled(
                    format!("{:<4}", file.diff.additions),
                    Style::default().fg(theme.diff_add),
                ),
                Span::styled(" -", Style::default().fg(theme.diff_remove)),
                Span::styled(
                    format!("{}", file.diff.deletions),
                    Style::default().fg(theme.diff_remove),
                ),
                Span::styled("  agent: ", Style::default().fg(theme.text_subtle)),
                Span::styled(agent_str, Style::default().fg(agent_col)),
            ]),
            Line::from(Span::raw("")),
            Line::from(Span::styled(
                "  Why",
                Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                format!("  {}", why_text),
                Style::default().fg(theme.text_muted),
            )),
            Line::from(Span::raw("")),
            Line::from(vec![
                Span::styled(
                    "  Actions",
                    Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "  enter=full diff  /allow  /block",
                    Style::default().fg(theme.text_subtle),
                ),
            ]),
        ]
    } else {
        vec![
            Line::from(Span::raw("")),
            Line::from(Span::styled(
                "  select a file to review",
                Style::default().fg(theme.text_subtle),
            )),
            Line::from(Span::raw("")),
            Line::from(Span::styled(
                "  ↑↓ navigate  enter=diff",
                Style::default().fg(theme.text_muted),
            )),
        ]
    };

    f.render_widget(Paragraph::new(lines).block(block), verdict_area);

    // ── Inline diff preview (only when area allows it) ────────────────────────
    if split.len() > 1 {
        render_inline_diff(f, split[1], state, theme);
    }
}

fn render_live_mode(
    f: &mut Frame,
    area: Rect,
    files: Option<&[AnnotatedFile]>,
    state: &WatchState,
) {
    let theme = &state.theme;
    let outer = Block::default()
        .title(Span::styled(
            " Live Changes ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().bg(theme.bg));
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Length(9),
            Constraint::Min(8),
        ])
        .split(inner);
    state.file_list_area.set(sections[1]);

    let sorted = sorted_visible_files(files, state.problems_only);
    let total_files = sorted.len();
    let total_add: usize = sorted.iter().map(|f| f.diff.additions).sum();
    let total_del: usize = sorted.iter().map(|f| f.diff.deletions).sum();
    let last_event = state
        .last_fs_event
        .map(|when| format!("{}s ago", when.elapsed().as_secs()))
        .unwrap_or_else(|| "waiting".into());

    let status = vec![
        Line::from(vec![
            Span::styled("  watcher ", Style::default().fg(theme.text_subtle)),
            Span::styled("● ", Style::default().fg(theme.success)),
            Span::styled(
                format!("{} events", state.live_event_count),
                Style::default().fg(theme.text),
            ),
            Span::styled("   last ", Style::default().fg(theme.text_subtle)),
            Span::styled(last_event, Style::default().fg(theme.text_muted)),
            Span::styled("   files ", Style::default().fg(theme.text_subtle)),
            Span::styled(total_files.to_string(), Style::default().fg(theme.accent)),
        ]),
        Line::from(vec![
            Span::styled("  lines   ", Style::default().fg(theme.text_subtle)),
            Span::styled(
                format!("+{}", total_add),
                Style::default().fg(theme.diff_add),
            ),
            Span::styled("  ", Style::default()),
            Span::styled(
                format!("-{}", total_del),
                Style::default().fg(theme.diff_remove),
            ),
            Span::styled(
                "   ↑↓/wheel select  enter=diff  1=review",
                Style::default().fg(theme.text_subtle),
            ),
        ]),
    ];
    f.render_widget(Paragraph::new(status), sections[0]);

    let file_lines: Vec<Line> = if sorted.is_empty() {
        vec![Line::from(Span::styled(
            "  no live changes yet",
            Style::default().fg(theme.text_subtle),
        ))]
    } else {
        sorted
            .iter()
            .enumerate()
            .skip(state.file_scroll)
            .take(sections[1].height as usize)
            .map(|(idx, file)| {
                let selected = idx == state.selected_file;
                let (badge, badge_color) = verdict_badge(&file.verdict, theme);
                let agent = matched_agent_tag(&file.matched_agents);
                let age = state
                    .file_freshness
                    .get(&file.diff.path)
                    .map(|when| format!("{}s", when.elapsed().as_secs()))
                    .unwrap_or_else(|| "--".into());
                Line::from(vec![
                    Span::styled(
                        if selected { "  ▶ " } else { "    " },
                        Style::default().fg(theme.accent),
                    ),
                    Span::styled(
                        format!("{:<10}", badge),
                        Style::default()
                            .fg(badge_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        truncate(
                            &file.diff.path.display().to_string(),
                            sections[1].width.saturating_sub(42) as usize,
                        ),
                        Style::default().fg(if selected {
                            theme.text
                        } else {
                            theme.text_muted
                        }),
                    ),
                    Span::styled(
                        format!("  +{} -{}", file.diff.additions, file.diff.deletions),
                        Style::default().fg(theme.text_subtle),
                    ),
                    Span::styled(
                        agent
                            .as_deref()
                            .map(|tag| format!("  {}", tag))
                            .unwrap_or_default(),
                        Style::default().fg(theme.accent),
                    ),
                    Span::styled(format!("  {}", age), Style::default().fg(theme.text_subtle)),
                ])
            })
            .collect()
    };
    f.render_widget(Paragraph::new(file_lines), sections[1]);

    let diff_block = Block::default()
        .title(Span::styled(
            " Selected Diff With Lines ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::TOP)
        .border_style(Style::default().fg(theme.border));
    let diff_inner = diff_block.inner(sections[2]);
    f.render_widget(diff_block, sections[2]);

    let diff_lines: Vec<Line> = state
        .inline_diff_lines
        .iter()
        .take(diff_inner.height as usize)
        .map(|line| diff_line_to_ratatui(line, theme, diff_inner.width))
        .collect();
    let diff_lines = if diff_lines.is_empty() {
        vec![Line::from(Span::styled(
            "  select a changed file to see line-numbered diff",
            Style::default().fg(theme.text_subtle),
        ))]
    } else {
        diff_lines
    };
    f.render_widget(Paragraph::new(diff_lines), diff_inner);
}

/// Render the inline diff preview for the currently selected file.
fn render_inline_diff(f: &mut Frame, area: Rect, state: &WatchState, theme: &Theme) {
    let block = Block::default()
        .title(Span::styled(
            " Live Diff ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().bg(theme.bg));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if state.inline_diff_lines.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  no diff available",
                Style::default().fg(theme.text_subtle),
            ))),
            inner,
        );
        return;
    }

    let visible = inner.height as usize;
    let lines: Vec<Line> = state
        .inline_diff_lines
        .iter()
        .take(visible)
        .map(|dl| diff_line_to_ratatui(dl, theme, inner.width))
        .collect();

    f.render_widget(Paragraph::new(lines), inner);
}

// ── Bar chart (verdicts) ──────────────────────────────────────────────────────

// ── Dashboard mode ───────────────────────────────────────────────────────────

fn render_dashboard(
    f: &mut Frame,
    area: Rect,
    session: Option<&crate::session::Session>,
    files: Option<&[AnnotatedFile]>,
    state: &WatchState,
) {
    let theme = &state.theme;

    let block = Block::default()
        .title(Span::styled(
            " Dashboard ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().bg(theme.bg));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height < 6 {
        return;
    }

    // Compute stats once
    let (n_total, n_expected, n_suspicious, n_blocked, n_ignored, total_add, total_del) =
        if let Some(fs) = files {
            let e = fs.iter().filter(|f| f.verdict.is_accepted()).count();
            let s = fs
                .iter()
                .filter(|f| f.verdict == FileVerdict::Unasked)
                .count();
            let b = fs.iter().filter(|f| f.verdict.is_blocked()).count();
            let i = fs
                .iter()
                .filter(|f| f.verdict == FileVerdict::Clean)
                .count();
            let add: usize = fs.iter().map(|f| f.diff.additions).sum();
            let del: usize = fs.iter().map(|f| f.diff.deletions).sum();
            (fs.len(), e, s, b, i, add, del)
        } else {
            (0, 0, 0, 0, 0, 0, 0)
        };

    let total_nonzero = n_total.max(1);
    let health_pct = (n_expected * 100) / total_nonzero;
    let bar_w = (inner.width.saturating_sub(12) as usize).max(10);

    let mut lines: Vec<Line> = Vec::new();

    // ── Section: Mission health ───────────────────────────────────────────────
    lines.push(Line::from(Span::styled(
        "  Mission health",
        Style::default()
            .fg(theme.text_subtle)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::raw("")));

    let mission_line = if let Some(m) = state.active_missions.first() {
        truncate(&m.mission, inner.width.saturating_sub(14) as usize)
    } else if let Some(s) = session {
        truncate(&s.mission, inner.width.saturating_sub(14) as usize)
    } else {
        "no active mission".into()
    };
    lines.push(Line::from(vec![
        Span::styled("  Mission  ", Style::default().fg(theme.text_subtle)),
        Span::styled(mission_line, Style::default().fg(theme.text_muted)),
    ]));

    // Health bar
    let health_color = if health_pct >= 80 {
        theme.success
    } else if health_pct >= 50 {
        theme.warning
    } else {
        theme.danger
    };
    let filled = ((health_pct * bar_w) / 100).min(bar_w);
    let empty = bar_w - filled;
    lines.push(Line::from(vec![
        Span::styled("  Health   ", Style::default().fg(theme.text_subtle)),
        Span::styled("█".repeat(filled), Style::default().fg(health_color)),
        Span::styled("░".repeat(empty), Style::default().fg(theme.border)),
        Span::styled(
            format!("  {}%", health_pct),
            Style::default()
                .fg(health_color)
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    // File/line stats row
    lines.push(Line::from(vec![
        Span::styled("  Files    ", Style::default().fg(theme.text_subtle)),
        Span::styled(
            format!("{} total", n_total),
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
        Span::styled("    Lines  ", Style::default().fg(theme.text_subtle)),
        Span::styled(
            format!("+{}", total_add),
            Style::default().fg(theme.diff_add),
        ),
        Span::styled(
            format!("  \u{2212}{}", total_del),
            Style::default().fg(theme.diff_remove),
        ),
        Span::styled(
            format!("    Watch  {}", state.uptime_str()),
            Style::default().fg(theme.text_subtle),
        ),
    ]));

    lines.push(Line::from(Span::raw("")));

    // ── Section: Scope distribution ───────────────────────────────────────────
    lines.push(Line::from(Span::styled(
        "  Scope distribution",
        Style::default()
            .fg(theme.text_subtle)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::raw("")));

    if n_total == 0 {
        lines.push(Line::from(Span::styled(
            "  no changes tracked yet",
            Style::default().fg(theme.text_subtle),
        )));
    } else {
        let e_w = (n_expected * bar_w) / total_nonzero;
        let s_w = (n_suspicious * bar_w) / total_nonzero;
        let b_w = (n_blocked * bar_w) / total_nonzero;
        let i_w = bar_w.saturating_sub(e_w + s_w + b_w);

        lines.push(Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled("█".repeat(e_w), Style::default().fg(theme.expected)),
            Span::styled("█".repeat(s_w), Style::default().fg(theme.suspicious)),
            Span::styled("█".repeat(b_w), Style::default().fg(theme.blocked)),
            Span::styled("█".repeat(i_w), Style::default().fg(theme.ignored)),
        ]));

        let e_pct = (n_expected * 100) / total_nonzero;
        let s_pct = (n_suspicious * 100) / total_nonzero;
        let b_pct = (n_blocked * 100) / total_nonzero;
        let i_pct = (n_ignored * 100) / total_nonzero;

        lines.push(Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled("● ", Style::default().fg(theme.expected)),
            Span::styled(
                format!("expected {}% ({})  ", e_pct, n_expected),
                Style::default().fg(theme.expected),
            ),
            Span::styled("● ", Style::default().fg(theme.suspicious)),
            Span::styled(
                format!("suspicious {}% ({})  ", s_pct, n_suspicious),
                Style::default().fg(theme.suspicious),
            ),
            Span::styled("● ", Style::default().fg(theme.blocked)),
            Span::styled(
                format!("blocked {}% ({})  ", b_pct, n_blocked),
                Style::default().fg(theme.blocked),
            ),
            Span::styled("● ", Style::default().fg(theme.ignored)),
            Span::styled(
                format!("ignored {}% ({})", i_pct, n_ignored),
                Style::default().fg(theme.ignored),
            ),
        ]));
    }

    lines.push(Line::from(Span::raw("")));

    // ── Section: Agents ───────────────────────────────────────────────────────
    lines.push(Line::from(Span::styled(
        "  Agents",
        Style::default()
            .fg(theme.text_subtle)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::raw("")));

    if state.active_missions.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no active agent sessions detected",
            Style::default().fg(theme.text_subtle),
        )));
    } else {
        for m in &state.active_missions {
            let agent_upper = m.agent.to_uppercase();
            let agent_color = agent_color_for_tag(&agent_upper, theme);
            let conf_pct = (m.confidence * 100.0) as usize;
            let conf_filled = (conf_pct / 10).min(10);
            let conf_bar = format!(
                "{}{}",
                "█".repeat(conf_filled),
                "░".repeat(10 - conf_filled)
            );
            let age_str = format_age(m.age_seconds);

            // Per-agent file counts
            let (a_exp, a_sus, a_blk) = if let Some(fs) = files {
                let tag = agent_upper.clone();
                let exp = fs
                    .iter()
                    .filter(|f| {
                        f.verdict.is_accepted()
                            && f.matched_agents
                                .iter()
                                .any(|a| a.to_uppercase().contains(&tag))
                    })
                    .count();
                let sus = fs
                    .iter()
                    .filter(|f| {
                        f.verdict == FileVerdict::Unasked
                            && f.matched_agents
                                .iter()
                                .any(|a| a.to_uppercase().contains(&tag))
                    })
                    .count();
                let blk = fs
                    .iter()
                    .filter(|f| {
                        f.verdict.is_blocked()
                            && f.matched_agents
                                .iter()
                                .any(|a| a.to_uppercase().contains(&tag))
                    })
                    .count();
                (exp, sus, blk)
            } else {
                (0, 0, 0)
            };

            let mission_snip = truncate(&m.mission, inner.width.saturating_sub(50) as usize);

            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {:<8}", agent_upper),
                    Style::default()
                        .fg(agent_color)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(conf_bar, Style::default().fg(agent_color)),
                Span::styled(format!(" {}%", conf_pct), Style::default().fg(agent_color)),
                Span::styled(
                    format!("  age {:<8}", age_str),
                    Style::default().fg(theme.text_subtle),
                ),
                Span::styled(
                    format!("{} expected  ", a_exp),
                    Style::default().fg(theme.expected),
                ),
                Span::styled(
                    format!("{} suspicious  ", a_sus),
                    Style::default().fg(theme.suspicious),
                ),
                Span::styled(
                    format!("{} blocked", a_blk),
                    Style::default().fg(theme.blocked),
                ),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(mission_snip, Style::default().fg(theme.text_muted)),
            ]));
        }
    }

    if !state.ignored_contexts.is_empty() {
        lines.push(Line::from(Span::styled(
            format!(
                "  {} stale / low-confidence session(s) ignored",
                state.ignored_contexts.len()
            ),
            Style::default().fg(theme.text_subtle),
        )));
    }

    lines.push(Line::from(Span::raw("")));

    // ── Section: Judge ────────────────────────────────────────────────────────
    lines.push(Line::from(Span::styled(
        "  Judge",
        Style::default()
            .fg(theme.text_subtle)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::raw("")));

    let judge_status = state.judge_status.lock().unwrap().clone();
    let (judge_state_str, judge_state_color) = match &judge_status {
        JudgeStatus::Idle => ("idle", theme.text_muted),
        JudgeStatus::Running => ("running…", theme.warning),
        JudgeStatus::Done { .. } => ("done", theme.success),
        JudgeStatus::Error(_) => ("error", theme.danger),
    };

    lines.push(Line::from(vec![
        Span::styled("  Provider  ", Style::default().fg(theme.text_subtle)),
        Span::styled(
            format!("{} / {}", state.judge_provider_label, state.judge_model),
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
        Span::styled("    Status  ", Style::default().fg(theme.text_subtle)),
        Span::styled(
            judge_state_str,
            Style::default()
                .fg(judge_state_color)
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    if let JudgeStatus::Done(ref result) = judge_status {
        let v_color = match result.verdict {
            crate::judge::JudgeVerdict::Matches => theme.success,
            crate::judge::JudgeVerdict::Drift => theme.warning,
            crate::judge::JudgeVerdict::Unknown => theme.danger,
        };
        let health_label = format!("{:.0}%", result.confidence * 100.0);
        lines.push(Line::from(vec![
            Span::styled("  Verdict   ", Style::default().fg(theme.text_subtle)),
            Span::styled(
                result.verdict.label(),
                Style::default().fg(v_color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  confidence {}", health_label),
                Style::default().fg(v_color),
            ),
        ]));
        if !result.reasoning.is_empty() {
            let reason_snip = truncate(&result.reasoning, inner.width.saturating_sub(14) as usize);
            lines.push(Line::from(vec![
                Span::styled("  Reason    ", Style::default().fg(theme.text_subtle)),
                Span::styled(
                    reason_snip,
                    Style::default()
                        .fg(theme.text_muted)
                        .add_modifier(Modifier::ITALIC),
                ),
            ]));
        }
    }

    lines.push(Line::from(vec![
        Span::styled(
            "  Press r to run judge",
            Style::default().fg(theme.text_subtle),
        ),
        Span::styled(
            "    R=refresh stats    1=review  2=chat  4=sessions",
            Style::default().fg(theme.text_subtle),
        ),
    ]));

    f.render_widget(Paragraph::new(lines), inner);
}

// ── Helper: format age_seconds into human-readable string ────────────────────
#[allow(dead_code)]
fn render_bar_chart(f: &mut Frame, area: Rect, files: Option<&[AnnotatedFile]>, theme: &Theme) {
    let block = Block::default()
        .title(Span::styled(
            " Verdicts ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().bg(theme.bg));

    if let Some(files) = files {
        let in_scope = files.iter().filter(|f| f.verdict.is_accepted()).count() as u64;
        let unasked = files
            .iter()
            .filter(|f| f.verdict == FileVerdict::Unasked)
            .count() as u64;
        let blocked = files.iter().filter(|f| f.verdict.is_blocked()).count() as u64;

        let bar_group = BarGroup::default().bars(&[
            Bar::default()
                .value(in_scope)
                .label("In Scope".into())
                .style(Style::default().fg(theme.expected)),
            Bar::default()
                .value(unasked)
                .label("Unasked".into())
                .style(Style::default().fg(theme.suspicious)),
            Bar::default()
                .value(blocked)
                .label("Blocked".into())
                .style(Style::default().fg(theme.blocked)),
        ]);

        let chart = BarChart::default()
            .block(block)
            .data(bar_group)
            .bar_width(8)
            .bar_gap(2)
            .value_style(Style::default().fg(theme.text).add_modifier(Modifier::BOLD));

        f.render_widget(chart, area);
    } else {
        let para = Paragraph::new(Line::from(Span::styled(
            "  no data",
            Style::default().fg(theme.text_subtle),
        )))
        .block(block);
        f.render_widget(para, area);
    }
}

// ── Pie chart (horizontal stacked bar + legend) ──────────────────────────────

#[allow(dead_code)]
fn render_pie_chart(f: &mut Frame, area: Rect, files: Option<&[AnnotatedFile]>, theme: &Theme) {
    let block = Block::default()
        .title(Span::styled(
            " Scope Distribution ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().bg(theme.bg));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.width < 10 || inner.height < 3 {
        return;
    }

    if let Some(files) = files {
        if files.is_empty() {
            let para = Paragraph::new(Line::from(Span::styled(
                "  no changes to chart",
                Style::default().fg(theme.text_subtle),
            )));
            f.render_widget(para, inner);
            return;
        }

        let total = files.len().max(1);
        let in_scope = files.iter().filter(|f| f.verdict.is_accepted()).count();
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
            Span::styled("█".repeat(g_width), Style::default().fg(theme.expected)),
            Span::styled("█".repeat(a_width), Style::default().fg(theme.suspicious)),
            Span::styled("█".repeat(r_width), Style::default().fg(theme.blocked)),
        ]);

        // Percentages
        let g_pct = (in_scope * 100) / total;
        let a_pct = (unasked * 100) / total;
        let b_pct = (blocked * 100) / total;

        let legend = Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled("● ", Style::default().fg(theme.expected)),
            Span::styled(format!("{}% ", g_pct), Style::default().fg(theme.expected)),
            Span::styled("● ", Style::default().fg(theme.suspicious)),
            Span::styled(
                format!("{}% ", a_pct),
                Style::default().fg(theme.suspicious),
            ),
            Span::styled("● ", Style::default().fg(theme.blocked)),
            Span::styled(format!("{}%", b_pct), Style::default().fg(theme.blocked)),
        ]);

        let label_line = Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled("scope", Style::default().fg(theme.text_subtle)),
            Span::styled("  ", Style::default()),
            Span::styled("unasked", Style::default().fg(theme.text_subtle)),
            Span::styled("  ", Style::default()),
            Span::styled("blocked", Style::default().fg(theme.text_subtle)),
        ]);

        let text = vec![bar_line, Line::from(Span::raw("")), legend, label_line];

        let para = Paragraph::new(text);
        f.render_widget(para, inner);
    }
}

// ── Stats + Judge panel ──────────────────────────────────────────────────────

#[allow(dead_code)]
fn render_stats_and_judge(
    f: &mut Frame,
    area: Rect,
    session: Option<&crate::session::Session>,
    files: Option<&[AnnotatedFile]>,
    state: &WatchState,
) {
    let theme = &state.theme;
    let block = Block::default()
        .title(Span::styled(
            " Stats & Judge ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().bg(theme.bg));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();

    // File & line stats
    if let Some(files) = files {
        let total_add: usize = files.iter().map(|f| f.diff.additions).sum();
        let total_del: usize = files.iter().map(|f| f.diff.deletions).sum();
        let in_scope = files.iter().filter(|f| f.verdict.is_accepted()).count();

        lines.push(Line::from(vec![
            Span::styled("  Files  ", Style::default().fg(theme.text_subtle)),
            Span::styled(
                format!("{}", files.len()),
                Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
            ),
            Span::styled("    Lines  ", Style::default().fg(theme.text_subtle)),
            Span::styled(
                format!("+{}", total_add),
                Style::default().fg(theme.success),
            ),
            Span::styled(
                format!(" \u{2212}{}", total_del),
                Style::default().fg(theme.danger),
            ),
        ]));

        // Health score
        let total = files.len().max(1);
        let health = (in_scope * 100) / total;
        let filled = (health / 10).min(10);
        let empty = 10 - filled;
        let bar_color = if health >= 80 {
            theme.success
        } else if health >= 50 {
            theme.warning
        } else {
            theme.danger
        };

        lines.push(Line::from(vec![
            Span::styled("  Health ", Style::default().fg(theme.text_subtle)),
            Span::styled("█".repeat(filled), Style::default().fg(bar_color)),
            Span::styled("░".repeat(empty), Style::default().fg(theme.text_subtle)),
            Span::styled(
                format!(" {}%", health),
                Style::default().fg(bar_color).add_modifier(Modifier::BOLD),
            ),
        ]));
    }

    // Uptime
    lines.push(Line::from(vec![
        Span::styled("  Watch  ", Style::default().fg(theme.text_subtle)),
        Span::styled(state.uptime_str(), Style::default().fg(theme.text_muted)),
        Span::styled(
            format!("  ({} cycles)", state.refresh_count),
            Style::default().fg(theme.text_subtle),
        ),
    ]));

    lines.push(Line::from(Span::raw("")));

    // ── Judge result ──
    let judge_status = state.judge_status.lock().unwrap().clone();
    match judge_status {
        JudgeStatus::Idle => {
            lines.push(Line::from(vec![Span::styled(
                "  ── Judge ──",
                Style::default().fg(theme.text_subtle),
            )]));
            lines.push(Line::from(vec![
                Span::styled("  Press ", Style::default().fg(theme.text_subtle)),
                Span::styled(
                    "j",
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" to run LLM judge", Style::default().fg(theme.text_subtle)),
            ]));
        }
        JudgeStatus::Running => {
            lines.push(Line::from(vec![Span::styled(
                "  ── Judge ──",
                Style::default().fg(theme.accent),
            )]));
            let dots = ".".repeat(((state.refresh_count / 3) % 4) as usize);
            lines.push(Line::from(vec![Span::styled(
                format!("  ⏳ Analyzing{}", dots),
                Style::default().fg(theme.warning),
            )]));
        }
        JudgeStatus::Done(ref result) => {
            let (verdict_str, verdict_color) = match result.verdict {
                JudgeVerdict::Matches => ("✓ MATCHES MISSION", theme.success),
                JudgeVerdict::Drift => ("✕ DRIFT DETECTED", theme.danger),
                JudgeVerdict::Unknown => ("? UNKNOWN", theme.text_muted),
            };

            lines.push(Line::from(vec![Span::styled(
                "  ── Judge ──",
                Style::default().fg(theme.accent),
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
                theme.success
            } else if pct >= 40 {
                theme.warning
            } else {
                theme.danger
            };

            lines.push(Line::from(vec![
                Span::styled("  Conf.  ", Style::default().fg(theme.text_subtle)),
                Span::styled("█".repeat(filled), Style::default().fg(bar_color)),
                Span::styled("░".repeat(empty), Style::default().fg(theme.text_subtle)),
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
                Style::default().fg(theme.text_muted),
            )]));

            lines.push(Line::from(vec![Span::styled(
                format!("  {} / {}", result.provider, result.model),
                Style::default().fg(theme.text_subtle),
            )]));
        }
        JudgeStatus::Error(ref msg) => {
            lines.push(Line::from(vec![Span::styled(
                "  ── Judge ──",
                Style::default().fg(theme.danger),
            )]));
            let short = if msg.len() > 35 {
                format!("{}…", &msg[..34])
            } else {
                msg.clone()
            };
            lines.push(Line::from(vec![Span::styled(
                format!("  ✕ {}", short),
                Style::default().fg(theme.danger),
            )]));
            lines.push(Line::from(vec![Span::styled(
                "  Press j to retry",
                Style::default().fg(theme.text_subtle),
            )]));
        }
    }

    // Mission reminder
    if let Some(s) = session {
        lines.push(Line::from(Span::raw("")));
        lines.push(Line::from(vec![Span::styled(
            "  ── Mission ──",
            Style::default().fg(theme.text_subtle),
        )]));
        let mission = truncate(&s.mission, inner.width.saturating_sub(4) as usize);
        lines.push(Line::from(vec![Span::styled(
            format!("  {}", mission),
            Style::default().fg(theme.text),
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
    let theme = &state.theme;
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().bg(theme.bg));

    // Command mode: show the command prompt
    if state.input_mode == InputMode::Command {
        let line = Line::from(vec![
            Span::styled(
                "  > ",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(&state.input_buf, Style::default().fg(theme.text)),
            Span::styled("▌", Style::default().fg(theme.accent)),
        ]);
        f.render_widget(Paragraph::new(line).block(block), area);
        return;
    }

    // Flash message takes priority
    if let Some(flash) = state.active_flash() {
        let line = Line::from(Span::styled(
            format!("  {}", flash),
            Style::default()
                .fg(theme.success)
                .add_modifier(Modifier::BOLD),
        ));
        f.render_widget(Paragraph::new(line).block(block), area);
        return;
    }

    // Build scope counts (left side — always shown)
    let (n_scope, n_sus, n_blk) = if let Some(fs) = files {
        let e = fs.iter().filter(|f| f.verdict.is_accepted()).count();
        let s = fs
            .iter()
            .filter(|f| f.verdict == FileVerdict::Unasked)
            .count();
        let b = fs.iter().filter(|f| f.verdict.is_blocked()).count();
        (e, s, b)
    } else {
        (0, 0, 0)
    };

    // Judge indicator
    let judge_str: &str = match *state.judge_status.lock().unwrap() {
        JudgeStatus::Idle => "",
        JudgeStatus::Running => "  ⏳",
        JudgeStatus::Done(ref r) => match r.verdict {
            JudgeVerdict::Matches => "  ✓judge",
            JudgeVerdict::Drift => "  ✕drift",
            JudgeVerdict::Unknown => "  ?judge",
        },
        JudgeStatus::Error(_) => "  ✕err",
    };

    // Mode-specific hint (right side)
    let mode_hint: &str = match state.mode {
        AppMode::Review => "↑↓/wheel=scroll list  enter=diff  m=select text  []=resize  ?=help",
        AppMode::Chat => "typing stays active  /=commands  /new-chat  /clear-chat  /chats",
        AppMode::Dashboard => "r=judge  R=refresh  j=judge  ?=help",
        AppMode::Sessions => "↑↓=select  enter=inspect  n=new  ?=help",
        AppMode::Live => "live file changes  ↑↓/wheel=select  enter=diff  1=review",
    };

    // Mode tabs (highlight active)
    let tab_spans: Vec<Span> = [
        ("1=review", AppMode::Review),
        ("2=chat", AppMode::Chat),
        ("3=dash", AppMode::Dashboard),
        ("4=sessions", AppMode::Sessions),
        ("5=live", AppMode::Live),
    ]
    .iter()
    .flat_map(|(label, mode)| {
        let style = if *mode == state.mode {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.text_subtle)
        };
        [
            Span::styled(label.to_string(), style),
            Span::styled("  ", Style::default()),
        ]
    })
    .collect();

    let mut spans = vec![Span::styled("  ", Style::default())];
    spans.extend(tab_spans);
    spans.push(Span::styled("·  ", Style::default().fg(theme.border)));

    if files.is_some() {
        spans.push(Span::styled(
            format!("{} exp  ", n_scope),
            Style::default().fg(theme.expected),
        ));
        spans.push(Span::styled(
            format!("{} susp  ", n_sus),
            Style::default().fg(theme.suspicious),
        ));
        spans.push(Span::styled(
            format!("{} blk", n_blk),
            Style::default().fg(theme.blocked),
        ));
        spans.push(Span::styled(
            judge_str,
            Style::default().fg(theme.text_muted),
        ));
    }

    if area.width >= 96 {
        spans.push(Span::styled("  ·  ", Style::default().fg(theme.border)));
        spans.push(Span::styled(
            mode_hint,
            Style::default().fg(theme.text_subtle),
        ));
    }
    if area.width >= 128 {
        spans.push(Span::styled("  ·  ", Style::default().fg(theme.border)));
        spans.push(Span::styled(
            format!("model={}/{}", state.judge_provider_label, state.judge_model),
            Style::default().fg(theme.text_subtle),
        ));
        spans.push(Span::styled("  ·  ", Style::default().fg(theme.border)));
        spans.push(Span::styled(
            if state.mouse_capture {
                "mouse=panels"
            } else {
                "mouse=text"
            },
            Style::default().fg(theme.text_subtle),
        ));
    }

    f.render_widget(Paragraph::new(Line::from(spans)).block(block), area);
}

fn render_sessions(f: &mut Frame, area: Rect, files: Option<&[AnnotatedFile]>, state: &WatchState) {
    let theme = &state.theme;

    let block = Block::default()
        .title(Span::styled(
            " Sessions ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().bg(theme.bg));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height < 4 {
        return;
    }

    let mut lines: Vec<Line> = Vec::new();

    // ── Active missions ───────────────────────────────────────────────────────
    lines.push(Line::from(Span::styled(
        "  Active missions",
        Style::default()
            .fg(theme.text_subtle)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::raw("")));

    if state.active_missions.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no active agent sessions detected",
            Style::default().fg(theme.text_muted),
        )));
        lines.push(Line::from(Span::styled(
            "  press n to see how to start a mission",
            Style::default().fg(theme.text_subtle),
        )));
    } else {
        for (idx, m) in state.active_missions.iter().enumerate() {
            let selected = idx == state.selected_session;
            let agent_upper = m.agent.to_uppercase();
            let agent_color = agent_color_for_tag(&agent_upper, theme);
            let conf_pct = (m.confidence * 100.0) as usize;

            // Per-agent file counts
            let (n_exp, n_sus, n_blk) = if let Some(fs) = files {
                let tag = agent_upper.clone();
                let e = fs
                    .iter()
                    .filter(|f| {
                        f.verdict.is_accepted()
                            && f.matched_agents
                                .iter()
                                .any(|a| a.to_uppercase().contains(&tag))
                    })
                    .count();
                let s = fs
                    .iter()
                    .filter(|f| {
                        f.verdict == FileVerdict::Unasked
                            && f.matched_agents
                                .iter()
                                .any(|a| a.to_uppercase().contains(&tag))
                    })
                    .count();
                let b = fs
                    .iter()
                    .filter(|f| {
                        f.verdict.is_blocked()
                            && f.matched_agents
                                .iter()
                                .any(|a| a.to_uppercase().contains(&tag))
                    })
                    .count();
                (e, s, b)
            } else {
                (0, 0, 0)
            };

            let age_str = format_age(m.age_seconds);
            let mission_snip = truncate(&m.mission, inner.width.saturating_sub(48) as usize);

            let row_style = if selected {
                Style::default()
                    .bg(theme.selection_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            let selector = if selected { "▶ " } else { "  " };

            lines.push(Line::from(vec![
                Span::styled(
                    format!("{}{:<8}", selector, agent_upper),
                    row_style.fg(agent_color),
                ),
                Span::styled(format!("{:>3}%  ", conf_pct), row_style.fg(agent_color)),
                Span::styled(format!("{:<8}", age_str), row_style.fg(theme.text_subtle)),
                Span::styled(mission_snip, row_style.fg(theme.text)),
            ]));

            if selected {
                // Detail row for selected session
                lines.push(Line::from(vec![
                    Span::styled("           files  ", Style::default().fg(theme.text_subtle)),
                    Span::styled(
                        format!("{} expected  ", n_exp),
                        Style::default().fg(theme.expected),
                    ),
                    Span::styled(
                        format!("{} suspicious  ", n_sus),
                        Style::default().fg(theme.suspicious),
                    ),
                    Span::styled(
                        format!("{} blocked", n_blk),
                        Style::default().fg(theme.blocked),
                    ),
                ]));
                if let Some(ref src) = m.source_path {
                    lines.push(Line::from(vec![
                        Span::styled(
                            "           source  ",
                            Style::default().fg(theme.text_subtle),
                        ),
                        Span::styled(
                            src.display().to_string(),
                            Style::default().fg(theme.text_muted),
                        ),
                    ]));
                }
                lines.push(Line::from(Span::styled(
                    "           enter=inspect in Review  1=review  n=new mission",
                    Style::default().fg(theme.text_subtle),
                )));
            }

            lines.push(Line::from(Span::raw("")));
        }
    }

    // ── Stale / ignored ───────────────────────────────────────────────────────
    if !state.ignored_contexts.is_empty() {
        lines.push(Line::from(Span::styled(
            "  Stale / ignored",
            Style::default()
                .fg(theme.text_subtle)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::raw("")));

        for ctx in &state.ignored_contexts {
            let agent_upper = ctx.agent.to_uppercase();
            let conf_pct = (ctx.confidence * 100.0) as usize;
            let mission_txt = ctx
                .mission
                .as_deref()
                .unwrap_or("(no mission recorded)")
                .to_string();
            let mission_snip = truncate(&mission_txt, inner.width.saturating_sub(36) as usize);
            let age_str = ctx
                .timestamp
                .as_deref()
                .unwrap_or("?")
                .chars()
                .take(16)
                .collect::<String>();

            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {:<8}", agent_upper),
                    Style::default().fg(theme.text_subtle),
                ),
                Span::styled(
                    format!("{:>3}%  ", conf_pct),
                    Style::default().fg(theme.text_subtle),
                ),
                Span::styled(
                    format!("{:<18}", age_str),
                    Style::default().fg(theme.text_subtle),
                ),
                Span::styled(mission_snip, Style::default().fg(theme.text_subtle)),
            ]));

            if !ctx.notes.is_empty() {
                let note = &ctx.notes[0];
                lines.push(Line::from(Span::styled(
                    format!(
                        "           {}",
                        truncate(note, inner.width.saturating_sub(14) as usize)
                    ),
                    Style::default()
                        .fg(theme.text_subtle)
                        .add_modifier(Modifier::ITALIC),
                )));
            }
        }
    } else if !state.active_missions.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no stale sessions",
            Style::default().fg(theme.text_subtle),
        )));
    }

    // Render all lines
    let visible_h = inner.height as usize;
    let visible: Vec<Line> = lines.into_iter().take(visible_h).collect();
    f.render_widget(Paragraph::new(visible), inner);
}

fn render_activity_log(f: &mut Frame, area: Rect, state: &WatchState) {
    let theme = &state.theme;
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().bg(theme.bg));
    let max_lines = area.height.saturating_sub(1).max(1) as usize;
    let visible = state
        .output_log
        .iter()
        .rev()
        .take(max_lines)
        .collect::<Vec<_>>();
    let lines = if visible.is_empty() {
        vec![Line::from(Span::styled(
            "  type / for commands, ? for help",
            Style::default().fg(theme.text_subtle),
        ))]
    } else {
        visible
            .into_iter()
            .rev()
            .map(|(msg, color)| {
                Line::from(Span::styled(
                    format!("  {}", msg),
                    Style::default().fg(*color),
                ))
            })
            .collect()
    };
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_command_menu(f: &mut Frame, area: Rect, state: &WatchState) {
    let theme = &state.theme;
    let height = 13.min(area.height.saturating_sub(4)).max(6);
    let y = area.y + area.height.saturating_sub(height + 3);
    let width = area.width.saturating_sub(4).max(40);
    let popup = Rect::new(area.x + 2, y, width, height);
    f.render_widget(Clear, popup);

    let block = Block::default()
        .title(Span::styled(
            " Slash Commands  Tab=complete  Enter=run  Esc=cancel ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().bg(theme.bg));

    let suggestions = command_suggestions(&state.input_buf);
    let lines = command_menu_lines(&suggestions, &state.input_buf, state, theme);
    f.render_widget(Paragraph::new(lines).block(block), popup);
}

fn render_help_overlay(f: &mut Frame, area: Rect, theme: &Theme) {
    let width = 58u16.min(area.width.saturating_sub(4));
    let height = 24u16.min(area.height.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let popup = Rect::new(x, y, width, height);
    f.render_widget(Clear, popup);

    let block = Block::default()
        .title(Span::styled(
            " Help  esc=close ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .style(Style::default().bg(theme.bg));

    let dim = Style::default().fg(theme.text_subtle);
    let key = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);
    let txt = Style::default().fg(theme.text_muted);

    macro_rules! kv {
        ($k:expr, $v:expr) => {
            Line::from(vec![
                Span::styled(format!("  {:12}", $k), key),
                Span::styled($v, txt),
            ])
        };
    }

    let lines: Vec<Line> = vec![
        Line::from(Span::styled("  ── Modes ─────────────────────", dim)),
        kv!("1", "Review — inspect changes"),
        kv!("2", "Chat   — ask the AI"),
        kv!("3", "Dash   — scope stats"),
        kv!("4", "Sessions — active agents"),
        kv!("5", "Live   — file events + line diffs"),
        Line::from(""),
        Line::from(Span::styled("  ── Review ────────────────────", dim)),
        kv!("↑↓ / j k", "navigate file list"),
        kv!("enter", "open diff overlay"),
        kv!("a / b", "allow / block file"),
        kv!("r", "revert file"),
        kv!("/ ", "filter / search"),
        kv!("[  ]", "resize split pane"),
        Line::from(""),
        Line::from(Span::styled("  ── Global ─────────────────────", dim)),
        kv!("t", "cycle theme"),
        kv!("j", "run judge"),
        kv!("m", "toggle mouse (off = native text select)"),
        kv!("q", "quit"),
        kv!("?  h", "toggle this overlay"),
        Line::from(""),
        Line::from(Span::styled(
            "  shift+drag = native terminal text selection",
            dim,
        )),
    ];

    let para = Paragraph::new(lines).block(block);
    f.render_widget(para, popup);
}

/// Render a single diff line into a ratatui Line with a line-number gutter.
fn diff_line_to_ratatui<'a>(dl: &'a DiffContentLine, theme: &Theme, width: u16) -> Line<'a> {
    let color = match dl.kind {
        DiffLineKind::Add => theme.diff_add,
        DiffLineKind::Delete => theme.diff_remove,
        DiffLineKind::Header => theme.accent,
        DiffLineKind::Context => theme.text_muted,
    };

    match dl.kind {
        DiffLineKind::Header => {
            // Hunk header: no line number, dimmed style
            Line::from(Span::styled(
                dl.content.clone(),
                Style::default().fg(theme.accent),
            ))
        }
        _ => {
            // Build gutter: right-aligned line number or blank
            let lineno = dl.new_lineno.or(dl.old_lineno);
            let gutter = match lineno {
                Some(n) => format!("{:>4} │", n),
                None => "     │".to_string(),
            };
            let gutter_color = match dl.kind {
                DiffLineKind::Add => theme.diff_add,
                DiffLineKind::Delete => theme.diff_remove,
                _ => theme.text_subtle,
            };
            // Content without the leading +/- (we show it via gutter color)
            let content = dl.content.clone();
            let max_content = (width as usize).saturating_sub(gutter.len() + 1);
            let display = if content.len() > max_content {
                format!("{}…", &content[..max_content.saturating_sub(1)])
            } else {
                content
            };
            Line::from(vec![
                Span::styled(gutter, Style::default().fg(gutter_color)),
                Span::styled(display, Style::default().fg(color)),
            ])
        }
    }
}

fn render_diff_overlay(f: &mut Frame, area: Rect, view: &DiffView, theme: &Theme) {
    let width = area.width.saturating_mul(9) / 10;
    let height = area.height.saturating_mul(4) / 5;
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let popup = Rect::new(x, y, width.max(20), height.max(8));
    f.render_widget(Clear, popup);

    let inner_height = popup.height.saturating_sub(2) as usize;
    let max_scroll = view.lines.len().saturating_sub(inner_height);
    let scroll = view.scroll.min(max_scroll);

    // Scroll position indicator (e.g. "23/156")
    let scroll_info = if view.lines.len() > inner_height {
        format!(" {}/{} ", scroll + inner_height, view.lines.len())
    } else {
        String::new()
    };

    let file_name = view
        .path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("?");

    let block = Block::default()
        .title(Line::from(vec![
            Span::styled(
                format!(" {} ", file_name),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{}  ", view.path.display()),
                Style::default().fg(theme.text_muted),
            ),
        ]))
        .title_bottom(Line::from(vec![
            Span::styled(
                " Esc=close  ↑↓/PgUp/PgDn=scroll ",
                Style::default().fg(theme.text_subtle),
            ),
            Span::styled(scroll_info, Style::default().fg(theme.text_muted)),
        ]))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().bg(theme.bg));

    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let lines: Vec<Line> = view
        .lines
        .iter()
        .skip(scroll)
        .take(inner_height)
        .map(|dl| diff_line_to_ratatui(dl, theme, inner.width))
        .collect();

    f.render_widget(Paragraph::new(lines), inner);
}

fn command_menu_lines<'a>(
    suggestions: &'a [CommandSpec],
    input: &str,
    state: &WatchState,
    theme: &Theme,
) -> Vec<Line<'a>> {
    if let Some(values) = command_value_suggestions(input, state) {
        let mut lines = vec![Line::from(vec![
            Span::styled("  values  ", Style::default().fg(theme.text_subtle)),
            Span::styled("current command: ", Style::default().fg(theme.text_muted)),
            Span::styled(input.to_string(), Style::default().fg(theme.text)),
        ])];
        for (idx, value) in values.iter().take(10).enumerate() {
            let marker = if idx == state.command_selected {
                "▶"
            } else {
                " "
            };
            lines.push(Line::from(vec![
                Span::styled(format!("  {} ", marker), Style::default().fg(theme.accent)),
                Span::styled(value.clone(), Style::default().fg(theme.accent)),
                Span::styled(
                    "  Enter=apply  Tab=complete",
                    Style::default().fg(theme.text_muted),
                ),
            ]));
        }
        return lines;
    }

    suggestions
        .iter()
        .take(10)
        .enumerate()
        .map(|(idx, spec)| {
            let marker = if idx == state.command_selected {
                "▶"
            } else {
                " "
            };
            Line::from(vec![
                Span::styled(format!("  {} ", marker), Style::default().fg(theme.accent)),
                Span::styled(spec.name, Style::default().fg(theme.accent)),
                Span::styled(
                    format!(" {:<31}", spec.args),
                    Style::default().fg(theme.text_muted),
                ),
                Span::styled(spec.description, Style::default().fg(theme.text)),
            ])
        })
        .collect()
}

fn filtered_files(files: Option<&[AnnotatedFile]>, problems_only: bool) -> Vec<&AnnotatedFile> {
    files
        .unwrap_or(&[])
        .iter()
        .filter(|file| {
            !problems_only || file.verdict.is_blocked() || file.verdict == FileVerdict::Unasked
        })
        .collect()
}

fn sorted_visible_files(
    files: Option<&[AnnotatedFile]>,
    problems_only: bool,
) -> Vec<&AnnotatedFile> {
    let mut sorted = filtered_files(files, problems_only);
    sorted.sort_by(|a, b| {
        verdict_sort_key(&a.verdict)
            .cmp(&verdict_sort_key(&b.verdict))
            .then_with(|| {
                let da = a.diff.additions + a.diff.deletions;
                let db = b.diff.additions + b.diff.deletions;
                db.cmp(&da)
            })
            .then_with(|| a.diff.path.cmp(&b.diff.path))
    });
    sorted
}

fn parse_tui_command(input: &str) -> Result<TuiCommand, String> {
    let trimmed = input.trim();
    if !trimmed.starts_with('/') {
        return Err("commands must start with /".into());
    }
    let body = trimmed.trim_start_matches('/');
    let mut parts = body.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("").trim();
    let arg = parts.next().map(str::trim).filter(|s| !s.is_empty());
    match name {
        "diff" => Ok(TuiCommand::Diff(arg.map(PathBuf::from))),
        "status" => Ok(TuiCommand::Status),
        "judge" => Ok(TuiCommand::Judge),
        "judge-provider" => Ok(TuiCommand::JudgeProvider(arg.map(ToString::to_string))),
        "judge-model" | "judge-models" => Ok(TuiCommand::JudgeModel(arg.map(ToString::to_string))),
        "ollama-models" => Ok(TuiCommand::OllamaModels),
        "ollama-model" => Ok(TuiCommand::OllamaModel(arg.map(ToString::to_string))),
        "check" => Ok(TuiCommand::Check),
        "problems" => Ok(TuiCommand::Problems),
        "agents" => Ok(TuiCommand::Agents),
        "agent" => Ok(TuiCommand::Agent(arg.map(ToString::to_string))),
        "mission" => Ok(TuiCommand::Mission),
        "refresh-agents" => Ok(TuiCommand::RefreshAgents),
        "dashboard" => Ok(TuiCommand::Dashboard),
        "live" => Ok(TuiCommand::Live),
        "allow" => Ok(TuiCommand::Allow(arg.map(ToString::to_string))),
        "block" => Ok(TuiCommand::Block(arg.map(ToString::to_string))),
        "theme" => Ok(TuiCommand::Theme(arg.map(ToString::to_string))),
        "clear" => Ok(TuiCommand::Clear),
        "clear-chat" => Ok(TuiCommand::ClearChat),
        "help" => Ok(TuiCommand::Help),
        "quit" | "exit" => Ok(TuiCommand::Quit),
        "chat" => Ok(TuiCommand::Chat),
        "new-chat" => Ok(TuiCommand::NewChat(arg.map(ToString::to_string))),
        "chats" => Ok(TuiCommand::Chats),
        "delete-chat" => Ok(TuiCommand::DeleteChat(arg.map(ToString::to_string))),
        "sessions" => Ok(TuiCommand::ChatSessions(arg.map(ToString::to_string))),
        "latest" => Ok(TuiCommand::ChatLatest(arg.map(ToString::to_string))),
        "ask" => Ok(TuiCommand::Ask(
            arg.map(ToString::to_string).unwrap_or_default(),
        )),
        "explain" => Ok(TuiCommand::Explain(arg.map(ToString::to_string))),
        "report" => Ok(TuiCommand::ChatReport),
        "filter" => Ok(TuiCommand::ChatFilter(arg.map(ToString::to_string))),
        "chat-context" => Ok(TuiCommand::ChatContext),
        "" => Err("empty command".into()),
        other => Err(format!("unknown command /{}", other)),
    }
}

fn command_suggestions(input: &str) -> Vec<CommandSpec> {
    let trimmed = input.trim_start();
    let command_prefix = trimmed.split_whitespace().next().unwrap_or(trimmed).trim();
    if command_prefix == "/" || command_prefix.is_empty() {
        return COMMAND_SPECS.to_vec();
    }
    COMMAND_SPECS
        .iter()
        .copied()
        .filter(|spec| spec.name.starts_with(command_prefix))
        .collect()
}

fn command_value_suggestions(input: &str, state: &WatchState) -> Option<Vec<String>> {
    let trimmed = input.trim_start();
    let value_prefix = |command: &str| {
        trimmed
            .strip_prefix(command)
            .and_then(|rest| rest.strip_prefix(' '))
            .map(str::trim)
    };
    if let Some(arg) = value_prefix("/theme") {
        return Some(
            config::tui_theme_names()
                .iter()
                .filter(|name| arg.is_empty() || name.starts_with(arg))
                .map(|name| name.to_string())
                .collect(),
        );
    }
    if let Some(arg) = value_prefix("/judge-provider") {
        return Some(
            ["claude", "openai", "gemini", "openrouter", "ollama"]
                .into_iter()
                .filter(|name| arg.is_empty() || name.starts_with(arg))
                .map(String::from)
                .collect(),
        );
    }
    if let Some(arg) = value_prefix("/agent") {
        return Some(
            state
                .active_missions
                .iter()
                .map(|mission| mission.agent.clone())
                .filter(|name| arg.is_empty() || name.starts_with(arg))
                .collect(),
        );
    }
    if let Some(arg) = value_prefix("/sessions").or_else(|| value_prefix("/latest")) {
        return Some(
            agents::supported_agents()
                .into_iter()
                .filter(|name| arg.is_empty() || name.starts_with(arg))
                .map(String::from)
                .collect(),
        );
    }
    if let Some(arg) = value_prefix("/filter") {
        return Some(
            ["suspicious", "all"]
                .into_iter()
                .filter(|name| arg.is_empty() || name.starts_with(arg))
                .map(String::from)
                .collect(),
        );
    }
    if let Some(arg) = value_prefix("/explain") {
        let mut values = vec!["selected".to_string()];
        values.extend(
            sorted_visible_files(None, false)
                .into_iter()
                .map(|file| file.diff.path.display().to_string()),
        );
        return Some(
            values
                .into_iter()
                .filter(|name| arg.is_empty() || name.starts_with(arg))
                .collect(),
        );
    }
    if let Some(arg) = value_prefix("/ollama-model") {
        return Some(
            state
                .ollama_models
                .iter()
                .filter(|name| arg.is_empty() || name.starts_with(arg))
                .cloned()
                .collect(),
        );
    }
    None
}

fn autocomplete_command_input(input: &str, state: &WatchState) -> String {
    if let Some(values) = command_value_suggestions(input, state) {
        if let Some(value) = values.get(state.command_selected.min(values.len().saturating_sub(1)))
        {
            let command = input.split_whitespace().next().unwrap_or("");
            return format!("{} {}", command, value);
        }
        return input.to_string();
    }
    let suggestions = command_suggestions(input);
    if let Some(spec) = suggestions.get(
        state
            .command_selected
            .min(suggestions.len().saturating_sub(1)),
    ) {
        let needs_space = !spec.args.is_empty();
        return if needs_space {
            format!("{} ", spec.name)
        } else {
            spec.name.to_string()
        };
    }
    input.to_string()
}

#[cfg(test)]
fn autocomplete_first(input: &str) -> String {
    let state = WatchState::new("agentscope");
    autocomplete_command_input(input, &state)
}

fn command_is_incomplete(input: &str) -> bool {
    let trimmed = input.trim();
    if trimmed == "/" {
        return true;
    }
    let first = trimmed.split_whitespace().next().unwrap_or("");
    !COMMAND_SPECS.iter().any(|spec| spec.name == first)
}

fn judge_provider_label(provider: &config::JudgeProvider) -> &'static str {
    match provider {
        config::JudgeProvider::Ollama => "ollama",
        config::JudgeProvider::Claude => "claude",
        config::JudgeProvider::Openai => "openai",
        config::JudgeProvider::Gemini => "gemini",
        config::JudgeProvider::Openrouter => "openrouter",
        config::JudgeProvider::None => "none",
    }
}

fn parse_judge_provider(value: &str) -> Result<config::JudgeProvider, String> {
    match value {
        "claude" => Ok(config::JudgeProvider::Claude),
        "codex" | "openai" => Ok(config::JudgeProvider::Openai),
        "ollama" => Ok(config::JudgeProvider::Ollama),
        "gemini" => Ok(config::JudgeProvider::Gemini),
        "openrouter" => Ok(config::JudgeProvider::Openrouter),
        "none" => Ok(config::JudgeProvider::None),
        other => Err(format!("unknown judge provider `{}`", other)),
    }
}

#[allow(dead_code)]
fn command_help_lines() -> &'static [&'static str] {
    &[
        "commands:",
        "review  /status  /diff  /judge  /allow  /block  /filter",
        "agents  /agents  /mission  /sessions  /latest  /refresh-agents",
        "judge   /judge-provider  /judge-model  /ollama-model  /theme",
        "chat    /chat  /new-chat  /clear-chat  /chats  /delete-chat  /ask",
        "views   /dashboard  /live",
    ]
}

fn active_mission_pairs(state: &WatchState) -> Vec<(String, String)> {
    state
        .active_missions
        .iter()
        .filter(|mission| {
            state
                .agent_filter
                .as_ref()
                .map(|filter| &mission.agent == filter)
                .unwrap_or(true)
        })
        .map(|mission| (mission.agent.clone(), mission.mission.clone()))
        .collect()
}

fn aggregate_mission_text(state: &WatchState, session: Option<&crate::session::Session>) -> String {
    let pairs = active_mission_pairs(state);
    if !pairs.is_empty() {
        return pairs
            .into_iter()
            .map(|(agent, mission)| format!("{}: {}", agent, mission))
            .collect::<Vec<_>>()
            .join("\n");
    }
    session
        .map(|s| s.mission.clone())
        .unwrap_or_else(|| "No active mission".into())
}

fn refresh_agent_missions(config: &config::Config, state: &mut WatchState) {
    match agents::active_missions(config) {
        Ok((active, ignored)) => {
            state.active_missions = active;
            state.ignored_contexts = ignored;
        }
        Err(err) => {
            state.push_log(format!("agent detect error: {}", err), state.theme.danger);
        }
    }
}

fn sync_judge_display(config: &config::Config, state: &mut WatchState) {
    state.judge_provider_label = judge_provider_label(&config.judge.provider).into();
    state.judge_model = config.judge.model.clone();
}

fn match_tag(agents: &[String]) -> String {
    match agents.len() {
        0 => "UNMATCHED".into(),
        1 => agent_label(&agents[0]).to_string(),
        _ => "MULTI".into(),
    }
}

fn matched_agent_tag(agents: &[String]) -> Option<String> {
    match agents.len() {
        0 => None,
        1 => Some(agent_label(&agents[0]).to_string()),
        _ => Some("MULTI".into()),
    }
}

fn agent_label(agent: &str) -> &'static str {
    match agent {
        "claude-code" => "CLAUDE",
        "codex" => "CODEX",
        "codex-app" => "CODEX APP",
        "gemini-cli" => "GEMINI",
        "copilot-cli" => "COPILOT",
        "cursor" => "CURSOR",
        "antigravity" => "ANTIGRAVITY",
        "opencode" => "OPENCODE",
        "openclaw" => "OPENCLAW",
        "hermes" => "HERMES",
        _ => "AGENT",
    }
}

fn format_age(age_seconds: Option<i64>) -> String {
    match age_seconds {
        Some(age) if age < 60 => format!("{}s", age),
        Some(age) if age < 3600 => format!("{}m", age / 60),
        Some(age) if age < 86_400 => format!("{}h", age / 3600),
        Some(age) => format!("{}d", age / 86_400),
        None => "?".into(),
    }
}

fn truncate(text: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if text.chars().count() <= max {
        return text.to_string();
    }
    let keep = max.saturating_sub(1);
    format!("{}…", text.chars().take(keep).collect::<String>())
}

#[allow(dead_code)]
fn verdict_color(verdict: &FileVerdict, theme: &Theme) -> Color {
    match verdict {
        FileVerdict::Allowed | FileVerdict::InScope => theme.expected,
        FileVerdict::Unasked => theme.suspicious,
        FileVerdict::Blocked { .. } => theme.blocked,
        FileVerdict::Clean => theme.ignored,
    }
}

fn verdict_badge(verdict: &FileVerdict, theme: &Theme) -> (&'static str, Color) {
    match verdict {
        FileVerdict::Allowed | FileVerdict::InScope => ("EXPECTED", theme.expected),
        FileVerdict::Unasked => ("SUSPICIOUS", theme.suspicious),
        FileVerdict::Blocked { .. } => ("BLOCKED", theme.blocked),
        FileVerdict::Clean => ("IGNORED", theme.ignored),
    }
}

fn verdict_sort_key(verdict: &FileVerdict) -> u8 {
    match verdict {
        FileVerdict::Blocked { .. } => 0,
        FileVerdict::Unasked => 1,
        FileVerdict::Allowed | FileVerdict::InScope => 2,
        FileVerdict::Clean => 3,
    }
}

fn agent_color_for_tag(tag: &str, theme: &Theme) -> Color {
    match tag {
        "CLAUDE" => theme.agent_claude,
        "CODEX" | "CODEX APP" => theme.agent_codex,
        "GEMINI" => theme.accent,
        "OPENCLAW" | "HERMES" => theme.accent,
        "COPILOT" | "CURSOR" | "ANTIGRAVITY" => theme.text,
        "OPENCODE" => theme.suspicious,
        _ => theme.text_muted,
    }
}

fn provider_default_model(provider: &config::JudgeProvider, current: &str) -> String {
    match provider {
        config::JudgeProvider::Ollama => current.to_string(),
        config::JudgeProvider::Claude if current.is_empty() => "claude-haiku-4-5-20251001".into(),
        config::JudgeProvider::Openai if current.is_empty() => "gpt-4o-mini".into(),
        config::JudgeProvider::Gemini if current.is_empty() => "gemini-2.0-flash-lite".into(),
        config::JudgeProvider::Openrouter if current.is_empty() => "openai/gpt-4o-mini".into(),
        _ => current.to_string(),
    }
}

async fn refresh_ollama_models(config: &config::Config, state: &mut WatchState) {
    match models::fetch_ollama_model_names(&config.judge.endpoint).await {
        Ok(models) => {
            state.ollama_models = models;
            state.push_log(
                format!("ollama: {} installed model(s)", state.ollama_models.len()),
                state.theme.accent,
            );
        }
        Err(err) => {
            state.push_log(format!("ollama error: {}", err), state.theme.danger);
        }
    }
}

async fn handle_tui_command(
    command: TuiCommand,
    config: &mut config::Config,
    state: &mut WatchState,
    session: Option<&crate::session::Session>,
    files: Option<&[AnnotatedFile]>,
) {
    match command {
        TuiCommand::Diff(path) => {
            let target = path.or_else(|| {
                filtered_files(files, state.problems_only)
                    .get(state.selected_file)
                    .map(|file| file.diff.path.clone())
            });
            if let Some(path) = target {
                open_diff_for_path(state, &path);
            } else {
                let color = state.theme.warning;
                state.push_log("no changed file selected for /diff", color);
            }
        }
        TuiCommand::Status => {
            let color = state.theme.accent;
            let msg = if let Some(files) = files {
                let allowed = files
                    .iter()
                    .filter(|f| f.verdict == FileVerdict::Allowed)
                    .count();
                let in_scope = files.iter().filter(|f| f.verdict.is_accepted()).count();
                let unasked = files
                    .iter()
                    .filter(|f| f.verdict == FileVerdict::Unasked)
                    .count();
                let blocked = files.iter().filter(|f| f.verdict.is_blocked()).count();
                format!(
                    "status: {} files · {} allowed · {} in scope · {} unasked · {} blocked",
                    files.len(),
                    allowed,
                    in_scope,
                    unasked,
                    blocked
                )
            } else {
                "status: no active session".into()
            };
            state.push_log(msg, color);
        }
        TuiCommand::Judge => start_judge(config, state, session, files),
        TuiCommand::JudgeProvider(provider) => match provider {
            None => {
                state.push_log(
                    format!(
                        "judge provider: {} / {}",
                        judge_provider_label(&config.judge.provider),
                        config.judge.model
                    ),
                    state.theme.accent,
                );
                state.push_log(
                    "providers: claude  openai  gemini  openrouter  ollama",
                    state.theme.text_muted,
                );
            }
            Some(provider) => match parse_judge_provider(&provider) {
                Ok(parsed) => {
                    config.judge.provider = parsed;
                    config.judge.model =
                        provider_default_model(&config.judge.provider, &config.judge.model);
                    match config::save(config) {
                        Ok(()) => {
                            if let Some(s) = session {
                                let _ = crate::session::append_session_activity(
                                    "judge_provider_change",
                                    s,
                                );
                            }
                            state.push_log(
                                format!(
                                    "judge provider: {}",
                                    judge_provider_label(&config.judge.provider)
                                ),
                                state.theme.success,
                            );
                        }
                        Err(err) => state.push_log(
                            format!("judge provider write failed: {}", err),
                            state.theme.danger,
                        ),
                    }
                }
                Err(err) => state.push_log(err, state.theme.danger),
            },
        },
        TuiCommand::JudgeModel(model) => match model {
            None => {
                state.push_log(
                    format!(
                        "judge model: {} / {}",
                        judge_provider_label(&config.judge.provider),
                        config.judge.model
                    ),
                    state.theme.accent,
                );
                if config.judge.provider == config::JudgeProvider::Ollama {
                    state.push_log(
                        "use /ollama-models to list installed models",
                        state.theme.text_muted,
                    );
                }
            }
            Some(model) => {
                config.judge.model = model.clone();
                match config::save(config) {
                    Ok(()) => {
                        if let Some(s) = session {
                            let _ =
                                crate::session::append_session_activity("judge_model_change", s);
                        }
                        state.push_log(format!("judge model: {}", model), state.theme.success);
                    }
                    Err(err) => state.push_log(
                        format!("judge model write failed: {}", err),
                        state.theme.danger,
                    ),
                }
            }
        },
        TuiCommand::OllamaModels => {
            refresh_ollama_models(config, state).await;
            if !state.ollama_models.is_empty() {
                // Re-enter command mode with /ollama-model pre-filled so the user can
                // arrow up/down through the model list and press Enter to select one.
                state.input_mode = InputMode::Command;
                state.input_buf = "/ollama-model ".into();
                state.command_selected = 0;
            } else {
                state.push_log(
                    "no ollama models found — is ollama running?",
                    state.theme.danger,
                );
            }
        }
        TuiCommand::OllamaModel(model) => match model {
            None => {
                refresh_ollama_models(config, state).await;
                state.push_log("type /ollama-model <name>", state.theme.text_muted);
            }
            Some(model) => {
                if state.ollama_models.is_empty() {
                    refresh_ollama_models(config, state).await;
                }
                if !state.ollama_models.is_empty()
                    && !state.ollama_models.iter().any(|known| known == &model)
                {
                    state.push_log(
                        format!("ollama model `{}` is not installed", model),
                        state.theme.danger,
                    );
                } else {
                    config.judge.provider = config::JudgeProvider::Ollama;
                    config.judge.model = model.clone();
                    match config::save(config) {
                        Ok(()) => {
                            if let Some(s) = session {
                                let _ = crate::session::append_session_activity(
                                    "judge_model_change",
                                    s,
                                );
                            }
                            state.push_log(format!("ollama model: {}", model), state.theme.success);
                        }
                        Err(err) => state.push_log(
                            format!("ollama model write failed: {}", err),
                            state.theme.danger,
                        ),
                    }
                }
            }
        },
        TuiCommand::Check => {
            let color = state.theme.accent;
            if let Some(files) = files {
                let blocked = files.iter().filter(|f| f.verdict.is_blocked()).count();
                let unasked = files
                    .iter()
                    .filter(|f| f.verdict == FileVerdict::Unasked)
                    .count();
                if blocked > 0 {
                    state.push_log(
                        format!("check: BLOCKED with {} blocked file(s)", blocked),
                        state.theme.danger,
                    );
                } else if unasked > 0 {
                    state.push_log(
                        format!("check: warning, {} unasked file(s)", unasked),
                        state.theme.warning,
                    );
                } else {
                    state.push_log("check: no blocked or unasked files", state.theme.success);
                }
            } else {
                state.push_log("check: no active session", color);
            }
        }
        TuiCommand::Problems => {
            state.problems_only = !state.problems_only;
            let color = state.theme.warning;
            state.push_log(
                if state.problems_only {
                    "filter: showing blocked and unasked files"
                } else {
                    "filter: showing all changed files"
                },
                color,
            );
        }
        TuiCommand::Agents => {
            state.push_log(
                format!(
                    "agents: {} active, {} stale/ignored",
                    state.active_missions.len(),
                    state.ignored_contexts.len()
                ),
                state.theme.accent,
            );
            for mission in state.active_missions.clone() {
                state.push_log(
                    format!(
                        "{} {:.0}% {}",
                        mission.agent,
                        mission.confidence * 100.0,
                        truncate(&mission.mission, 80)
                    ),
                    state.theme.text_muted,
                );
            }
        }
        TuiCommand::Agent(agent) => {
            state.agent_filter = agent;
            state.push_log(
                state
                    .agent_filter
                    .as_ref()
                    .map(|agent| format!("agent filter: {}", agent))
                    .unwrap_or_else(|| "agent filter cleared".into()),
                state.theme.accent,
            );
        }
        TuiCommand::Mission => {
            for line in aggregate_mission_text(state, session).lines().take(6) {
                state.push_log(line.to_string(), state.theme.text_muted);
            }
        }
        TuiCommand::RefreshAgents => {
            refresh_agent_missions(config, state);
            state.push_log("agents: refreshed", state.theme.accent);
        }
        TuiCommand::Dashboard => {
            state.mode = AppMode::Dashboard;
            state.set_flash("dashboard");
        }
        TuiCommand::Live => {
            state.mode = AppMode::Live;
            state.set_flash("live changes");
        }
        TuiCommand::Allow(pattern) => {
            persist_policy_pattern(config, state, session, files, pattern, true);
        }
        TuiCommand::Block(pattern) => {
            persist_policy_pattern(config, state, session, files, pattern, false);
        }
        TuiCommand::Theme(theme) => match theme {
            None => {
                state.push_log(
                    format!(
                        "themes: {} (current: {})",
                        config::tui_theme_names().join(", "),
                        state.theme.name
                    ),
                    state.theme.accent,
                );
            }
            Some(name) => {
                let previous = config.tui.theme.clone();
                match config::set_tui_theme(config, &name).and_then(|_| config::save(config)) {
                    Ok(()) => {
                        state.theme = Theme::by_name(&name);
                        state.push_log(format!("theme: switched to {}", name), state.theme.success);
                        if let Some(s) = session {
                            let _ = crate::session::append_session_activity("tui_theme_change", s);
                        }
                    }
                    Err(err) => {
                        config.tui.theme = previous;
                        state.push_log(format!("theme error: {}", err), state.theme.danger);
                    }
                }
            }
        },
        TuiCommand::Clear => {
            if state.mode == AppMode::Chat {
                clear_visible_chat(state);
            } else {
                state.output_log.clear();
            }
        }
        TuiCommand::ClearChat => {
            clear_visible_chat(state);
        }
        TuiCommand::Help => {
            if state.mode == AppMode::Chat {
                post_chat_local_command("/help", state, session, files);
            } else {
                state.show_help = true;
                state.set_flash("help");
            }
        }
        TuiCommand::Quit => {}
        TuiCommand::Chat => {
            state.mode = AppMode::Chat;
            state.set_flash("chat  i=compose  ↑↓=scroll");
        }
        TuiCommand::NewChat(title) => {
            let title = title.unwrap_or_else(|| "untitled".into());
            match crate::chat::create_chat(Some(title), config) {
                Ok(meta) => {
                    state.chat_session_id = Some(meta.id.clone());
                    state.chat_messages.clear();
                    state.chat_scroll = 0;
                    state.mode = AppMode::Chat;
                    let id_short = &meta.id[..8.min(meta.id.len())];
                    state.push_log(
                        format!("chat: new session {} — {}", id_short, meta.title),
                        state.theme.success,
                    );
                }
                Err(err) => {
                    state.push_log(format!("chat create error: {}", err), state.theme.danger);
                }
            }
        }
        TuiCommand::Chats => match crate::chat::list_chats(false) {
            Ok(chats) => {
                state.push_log(
                    format!("chats: {} session(s)", chats.len()),
                    state.theme.accent,
                );
                for chat in chats.iter().take(6) {
                    let id_short = &chat.id[..8.min(chat.id.len())];
                    state.push_log(
                        format!(
                            "  {} {}  {}",
                            id_short,
                            truncate(&chat.title, 24),
                            truncate(&chat.last_message_preview, 38)
                        ),
                        state.theme.text_muted,
                    );
                }
            }
            Err(err) => {
                state.push_log(format!("chats error: {}", err), state.theme.danger);
            }
        },
        TuiCommand::DeleteChat(chat_id) => {
            let target = chat_id.or_else(|| state.chat_session_id.clone());
            match target {
                Some(chat_id) => match crate::chat::soft_delete_chat(&chat_id) {
                    Ok(()) => {
                        if state.chat_session_id.as_deref() == Some(chat_id.as_str()) {
                            state.chat_session_id = None;
                            state.chat_messages.clear();
                            state.chat_scroll = 0;
                        }
                        state.mode = AppMode::Chat;
                        state.push_log(format!("chat: archived {}", chat_id), state.theme.warning);
                    }
                    Err(err) => {
                        state.push_log(format!("chat delete error: {}", err), state.theme.danger);
                    }
                },
                None => {
                    state.push_log("chat: no active chat id to delete", state.theme.warning);
                }
            }
        }
        TuiCommand::ChatSessions(agent) => {
            let cfg = config.clone();
            match crate::assistant_sessions::index_sessions(&cfg) {
                Ok(sessions) => {
                    let filtered =
                        crate::assistant_sessions::filter_sessions(sessions, agent.as_deref())
                            .unwrap_or_default();
                    state.push_log(
                        format!("sessions: {} found", filtered.len()),
                        state.theme.accent,
                    );
                    for s in filtered.iter().take(5) {
                        state.push_log(
                            format!(
                                "  {}  {}  {}",
                                s.agent,
                                s.modified_at,
                                s.mission.as_deref().unwrap_or(&s.preview)
                            ),
                            state.theme.text_muted,
                        );
                    }
                }
                Err(err) => {
                    state.push_log(format!("sessions error: {}", err), state.theme.danger);
                }
            }
        }
        TuiCommand::ChatLatest(agent) => {
            let cfg = config.clone();
            match crate::assistant_sessions::latest_session(&cfg, agent.as_deref()) {
                Ok(s) => {
                    state.push_log(
                        format!("latest {}: {}", s.agent, s.modified_at),
                        state.theme.accent,
                    );
                    if let Some(mission) = &s.mission {
                        state.push_log(
                            format!("  mission: {}", truncate(mission, 80)),
                            state.theme.text_muted,
                        );
                    }
                    state.push_log(
                        format!("  preview: {}", truncate(&s.preview, 80)),
                        state.theme.text_muted,
                    );
                }
                Err(err) => {
                    state.push_log(format!("latest error: {}", err), state.theme.danger);
                }
            }
        }
        TuiCommand::Ask(question) => {
            if question.is_empty() {
                state.push_log("usage: /ask <your question>", state.theme.warning);
            } else {
                state.mode = AppMode::Chat;
                send_chat_message(&question, config, state, session, files);
            }
        }
        TuiCommand::Explain(target) => {
            state.mode = AppMode::Chat;
            let input = target
                .map(|target| format!("/explain {}", target))
                .unwrap_or_else(|| "/explain selected".into());
            post_chat_local_command(&input, state, session, files);
        }
        TuiCommand::ChatReport => {
            state.mode = AppMode::Chat;
            post_chat_local_command("/report", state, session, files);
        }
        TuiCommand::ChatFilter(filter) => {
            state.mode = AppMode::Chat;
            let input = filter
                .map(|filter| format!("/filter {}", filter))
                .unwrap_or_else(|| "/filter".into());
            post_chat_local_command(&input, state, session, files);
        }
        TuiCommand::ChatContext => {
            state.mode = AppMode::Chat;
            post_chat_context(state, session, files);
        }
    }
}

fn persist_policy_pattern(
    config: &mut config::Config,
    state: &mut WatchState,
    session: Option<&crate::session::Session>,
    files: Option<&[AnnotatedFile]>,
    pattern: Option<String>,
    allow: bool,
) {
    let target = pattern
        .or_else(|| {
            sorted_visible_files(files, state.problems_only)
                .get(state.selected_file)
                .map(|file| file.diff.path.display().to_string())
        })
        .or_else(|| {
            state
                .diff_view
                .as_ref()
                .map(|view| view.path.display().to_string())
        });
    let Some(pattern) = target else {
        state.push_log(
            "policy command needs a selected file or explicit pattern",
            state.theme.warning,
        );
        return;
    };

    let changed = if allow {
        config::add_policy_allow(config, &pattern)
    } else {
        config::add_policy_block(config, &pattern)
    };
    if !changed {
        state.push_log(
            format!("policy: {} already exists", pattern),
            state.theme.text_muted,
        );
        return;
    }

    match config::save(config) {
        Ok(()) => {
            let event = if allow {
                "policy_allow"
            } else {
                "policy_block"
            };
            if let Some(s) = session {
                let _ = crate::session::append_session_activity(event, s);
            }
            state.push_log(
                format!(
                    "policy: {} {}",
                    if allow { "allowed" } else { "blocked" },
                    pattern
                ),
                if allow {
                    state.theme.success
                } else {
                    state.theme.danger
                },
            );
        }
        Err(err) => {
            if allow {
                config.policy.allow.retain(|value| value != &pattern);
            } else {
                config.policy.blocked.retain(|value| value != &pattern);
            }
            state.push_log(format!("policy write failed: {}", err), state.theme.danger);
        }
    }
}

fn open_diff_for_path(state: &mut WatchState, path: &Path) {
    match git::open_repo().and_then(|repo| git::file_diff_content(&repo, path)) {
        Ok(lines) => {
            state.diff_view = Some(DiffView {
                path: path.to_path_buf(),
                lines,
                scroll: 0,
            });
        }
        Err(err) => {
            state.push_log(
                format!("diff error for {}: {}", path.display(), err),
                state.theme.danger,
            );
        }
    }
}

fn copy_mouse_target(
    state: &mut WatchState,
    column: u16,
    row: u16,
    visible_files: &[&AnnotatedFile],
) {
    if let Some(text) = mouse_target_text(state, column, row, visible_files) {
        match copy_to_clipboard(&text) {
            Ok(()) => state.set_flash("copied to clipboard"),
            Err(err) => state.push_log(format!("copy failed: {}", err), state.theme.danger),
        }
    }
}

fn mouse_target_text(
    state: &WatchState,
    column: u16,
    row: u16,
    visible_files: &[&AnnotatedFile],
) -> Option<String> {
    if matches!(state.mode, AppMode::Review | AppMode::Live) {
        let area = state.file_list_area.get();
        if row >= area.y
            && row < area.y + area.height
            && column >= area.x
            && column < area.x + area.width
        {
            let idx = state.file_scroll + (row - area.y) as usize;
            return visible_files.get(idx).map(|file| {
                format!(
                    "{} {} +{} -{} {}",
                    file.verdict.label(),
                    file.diff.path.display(),
                    file.diff.additions,
                    file.diff.deletions,
                    match_tag(&file.matched_agents)
                )
            });
        }
    }

    if state.mode == AppMode::Chat {
        let area = state.chat_transcript_area.get();
        if row >= area.y
            && row < area.y + area.height
            && column >= area.x
            && column < area.x + area.width
        {
            let lines = chat_plain_lines(state, area.width);
            let idx = state.chat_scroll + (row - area.y) as usize;
            return lines.get(idx).map(|line| line.trim().to_string());
        }
    }
    None
}

fn copy_to_clipboard(text: &str) -> Result<()> {
    let mut clipboard = arboard::Clipboard::new()?;
    clipboard.set_text(text.to_string())?;
    Ok(())
}

fn clear_visible_chat(state: &mut WatchState) {
    state.chat_messages.clear();
    state.chat_scroll = 0;
    state.input_mode = InputMode::Chat;
    state.input_buf.clear();
    state.set_flash("chat cleared");
}

fn try_answer_chat_locally(
    text: &str,
    config: &config::Config,
    scope_answer: &str,
) -> Option<String> {
    let lower = text.to_lowercase();

    // Self-description / install / feature questions answered from built-in knowledge
    let asks_self = lower.contains("what are you")
        || lower.contains("what is agentscope")
        || lower.contains("how do i install")
        || lower.contains("how to install")
        || lower.contains("how do i use")
        || lower.contains("how to use")
        || lower.contains("what can you do")
        || lower.contains("your feature")
        || lower.contains("what feature")
        || lower.contains("tell me about yourself")
        || lower.contains("how does agentscope work")
        || lower.contains("what do you do");
    if asks_self {
        return Some(chat_capability_context(config));
    }

    let asks_sessions = lower.contains("session")
        || lower.contains("mission")
        || lower.contains("codex")
        || lower.contains("claude")
        || lower.contains("gemini")
        || lower.contains("copilot")
        || lower.contains("cursor")
        || lower.contains("antigravity")
        || lower.contains("opencode")
        || lower.contains("openclaw")
        || lower.contains("hermes");
    if asks_sessions
        && (lower.contains("how many") || lower.contains("list") || lower.contains("latest"))
    {
        return Some(local_session_answer(config, &lower));
    }
    if lower.contains("skill") || lower.contains("plugin") || lower.contains("command") {
        return Some(chat_capability_context(config));
    }
    if lower.contains("what changed") || lower.contains("scope") || lower.contains("suspicious") {
        return Some(scope_answer.to_string());
    }
    None
}

async fn answer_chat_message_async(
    text: String,
    config: config::Config,
    mission: String,
    scope_stats: String,
    selected_context: String,
    scope_answer: String,
    history: String,
) -> String {
    if let Some(response) = try_answer_chat_locally(&text, &config, &scope_answer) {
        return response;
    }

    let capabilities = chat_capability_context(&config);
    let session_context = assistant_session_context(&config);
    let prompt = format!(
        "You are the read-only chat assistant inside AgentScope, a coding-agent cockpit.\n\
         Stay practical and answer from the provided AgentScope context first. If the user asks about sessions, skills, plugins, themes, commands, policy, scope, or changed files, use the facts below and do not invent counts.\n\
         You can explain and inspect; you cannot modify files, drive Codex/Claude/Gemini/Copilot, or install services from this chat.\n\
         Keep answers concise and conversational. Do not repeat a branded sender label in your prose.\n\
         Format for a terminal: use short paragraphs and newline-separated bullets. Do not pack bullets into one paragraph.\n\n\
         Active mission: \"{mission}\"\n\
         Scope snapshot: {scope_stats}\n\
         Selected file: {selected_context}\n\n\
         AgentScope capabilities:\n{capabilities}\n\n\
         Local assistant session index:\n{session_context}\n\n\
         Conversation:\n{history}",
        mission = mission,
        scope_stats = scope_stats,
        selected_context = selected_context,
        capabilities = capabilities,
        session_context = session_context,
        history = history,
    );

    match crate::judge::chat(&prompt, &config.judge).await {
        Ok(text) => text,
        Err(err) => format!("error: {}", err),
    }
}

fn local_session_answer(config: &config::Config, lower: &str) -> String {
    let agent = [
        "claude",
        "codex-app",
        "codex",
        "gemini",
        "copilot",
        "cursor",
        "antigravity",
        "opencode",
        "openclaw",
        "hermes",
    ]
    .into_iter()
    .find(|name| lower.contains(name));
    match crate::assistant_sessions::index_sessions(config)
        .and_then(|sessions| crate::assistant_sessions::filter_sessions(sessions, agent))
    {
        Ok(sessions) if sessions.is_empty() => agent
            .map(|agent| {
                format!(
                    "I found 0 {} sessions in the configured local sources.",
                    agent
                )
            })
            .unwrap_or_else(|| {
                "I found 0 indexed assistant sessions in the configured local sources.".into()
            }),
        Ok(sessions) if lower.contains("latest") => {
            let s = &sessions[0];
            format!(
                "Latest {} session: {}. Mission: {}. Source: {}",
                s.agent,
                s.modified_at,
                s.mission.as_deref().unwrap_or("not inferred"),
                s.path.display()
            )
        }
        Ok(sessions) => {
            let mut lines = vec![format!(
                "I found {} {}session(s).",
                sessions.len(),
                agent.map(|agent| format!("{} ", agent)).unwrap_or_default()
            )];
            for s in sessions.iter().take(6) {
                lines.push(format!(
                    "- {} {}: {}",
                    s.agent,
                    s.modified_at,
                    s.mission.as_deref().unwrap_or(&s.preview)
                ));
            }
            lines.join("\n")
        }
        Err(err) => format!("I could not index assistant sessions: {}", err),
    }
}

fn local_scope_answer(
    state: &WatchState,
    session: Option<&crate::session::Session>,
    files: Option<&[AnnotatedFile]>,
) -> String {
    let mission = session
        .map(|session| truncate(&session.mission, 120))
        .unwrap_or_else(|| "no active manual session".into());
    let selected = selected_chat_file_context(state, files);
    let (expected, suspicious, blocked) = files
        .map(|fs| {
            (
                fs.iter().filter(|f| f.verdict.is_accepted()).count(),
                fs.iter()
                    .filter(|f| f.verdict == FileVerdict::Unasked)
                    .count(),
                fs.iter().filter(|f| f.verdict.is_blocked()).count(),
            )
        })
        .unwrap_or((0, 0, 0));
    format!(
        "Current scope: {} expected, {} suspicious, {} blocked. Mission: {}. Selected: {}",
        expected, suspicious, blocked, mission, selected
    )
}

fn selected_chat_file_context(state: &WatchState, files: Option<&[AnnotatedFile]>) -> String {
    sorted_visible_files(files, false)
        .get(state.selected_file)
        .map(|file| {
            let (badge, _) = verdict_badge(&file.verdict, &state.theme);
            format!(
                "{} {} +{} -{}",
                file.diff.path.display(),
                badge,
                file.diff.additions,
                file.diff.deletions
            )
        })
        .unwrap_or_else(|| "none".into())
}

fn chat_capability_context(config: &config::Config) -> String {
    let skills = integration_asset_names(".agentscope/skill");
    let plugins = integration_asset_names(".agentscope/plugin");
    format!(
        "AgentScope is a Rust CLI that acts as a scope firewall and audit layer for AI coding agents.\n\
         It records your mission, watches Git changes, applies deterministic policy, and optionally \
         asks a local LLM judge whether the diff still matches the mission.\n\n\
         INSTALLATION:\n\
         - cargo install agentscope  (or: cargo build --release in repo)\n\
         - Requires Git in PATH. Optional: Ollama for local LLM judging.\n\
         - Run `agentscope init` in any Git repo to create agentscope.yaml\n\n\
         MODES (keyboard):\n\
         - 1 = Review   — see every changed file, its verdict, and the decision panel\n\
         - 2 = Chat     — ask questions, run slash commands, get scope answers\n\
         - 3 = Dashboard — scope stats, per-agent breakdown, judge health\n\
         - 4 = Sessions — active and stale agent missions\n\n\
         REVIEW KEYS:  ↑↓=select  enter=diff  a=allow  b=block  j=judge  [/]=resize panel\n\
         GLOBAL KEYS:  t=theme  ?=help  q=quit  m=toggle mouse\n\n\
         VERDICTS:\n\
         - EXPECTED   — file path matches the active mission scope\n\
         - SUSPICIOUS — file not covered by any mission rule\n\
         - BLOCKED    — matched a blocked policy pattern (e.g. .env, *.key, src/auth/**)\n\
         - IGNORED    — clean / no tracked changes\n\n\
         CLI COMMANDS:\n\
         agentscope init              create agentscope.yaml\n\
         agentscope start \"mission\"   start a manual session\n\
         agentscope watch             open TUI (default)\n\
         agentscope check [--json]    check current diff against policy\n\
         agentscope diff [--problems] show diff filtered to problems\n\
         agentscope judge [-m model]  run LLM judge on current diff\n\
         agentscope model list/set/test/pull  manage Ollama judge models\n\
         agentscope config show/set/edit/reset  manage config\n\
         agentscope hook install/uninstall/status  manage pre-commit hook\n\
         agentscope agents detect/doctor/context  inspect agent context\n\
         agentscope monitor --agent auto  watch + auto-attach\n\
         agentscope chat new/list/show/delete/restore/purge  manage chat logs\n\
         agentscope sessions list/latest/show  inspect agent sessions\n\n\
         CHAT SLASH COMMANDS:\n\
         /explain selected   explain the currently selected file's verdict\n\
         /report             show full scope summary\n\
         /filter suspicious  show only suspicious+blocked files in Review\n\
         /sessions [agent]   list local agent sessions\n\
         /latest [agent]     show latest session for an agent\n\
         /theme <name>       switch theme (agentscope/codex/claude/openclaw/high-contrast)\n\
         /judge-provider     list or switch judge provider\n\
         /judge-model        list or set judge model\n\
         /new-chat <title>   start a new chat log\n\
         /clear-chat         clear visible messages\n\
         /chats              list saved chat logs\n\
         /delete-chat        archive current chat\n\
         /help               show all slash commands\n\n\
         POLICY (agentscope.yaml):\n\
         - blocked: glob patterns always blocked (e.g. .env, *.key, src/auth/**)\n\
         - warn: patterns that warn but don't block\n\
         - max_files_changed: hard limit on file count (0=disabled)\n\
         - max_lines_changed: hard limit on lines changed (0=disabled)\n\n\
         JUDGE / LLM:\n\
         - provider: ollama (local/private), claude (ANTHROPIC_API_KEY), openai (OPENAI_API_KEY), gemini (GEMINI_API_KEY), openrouter (OPENROUTER_API_KEY)\n\
         - model: auto-selected per provider, or set with /judge-model (e.g. claude-haiku-4-5-20251001, gpt-4o-mini, gemini-2.0-flash-lite, openai/gpt-4o-mini)\n\
         - model: e.g. qwen3.5:2b, llama3, gemma4:e2b\n\
         - Ollama must be running locally (http://localhost:11434)\n\n\
         SUPPORTED AGENTS: {agents}\n\
         OLLAMA LAUNCH: {launches}\n\
         THEMES: agentscope, codex, claude, openclaw, high-contrast\n\
         PROJECT SKILLS: {skills}\n\
         PROJECT PLUGINS: {plugins}\n\
         CONFIGURED JUDGE: {provider} / {model}",
        agents = agents::supported_agents().join(", "),
        launches = supported_launch_commands().join("; "),
        skills = list_or_none(skills),
        plugins = list_or_none(plugins),
        provider = judge_provider_label(&config.judge.provider),
        model = config.judge.model
    )
}

fn supported_launch_commands() -> Vec<String> {
    [
        "claude-code",
        "codex-app",
        "gemini-cli",
        "antigravity",
        "openclaw",
        "hermes",
        "codex",
        "opencode",
    ]
    .into_iter()
    .filter_map(|agent| agents::launch_command(agent, "qwen3.5"))
    .collect()
}

fn assistant_session_context(config: &config::Config) -> String {
    match crate::assistant_sessions::index_sessions(config) {
        Ok(sessions) if sessions.is_empty() => "- No local assistant sessions indexed.".into(),
        Ok(sessions) => sessions
            .iter()
            .take(8)
            .map(|s| {
                format!(
                    "- {} {} {}",
                    s.agent,
                    s.modified_at,
                    s.mission.as_deref().unwrap_or(&s.preview)
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Err(err) => format!("- Could not index assistant sessions: {}", err),
    }
}

fn integration_asset_names(root: &str) -> Vec<String> {
    fs::read_dir(root)
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(Result::ok))
        .filter_map(|entry| entry.file_name().to_str().map(ToString::to_string))
        .collect()
}

fn list_or_none(items: Vec<String>) -> String {
    if items.is_empty() {
        "none".into()
    } else {
        items.join(", ")
    }
}

fn send_chat_message(
    text: &str,
    config: &config::Config,
    state: &mut WatchState,
    session: Option<&crate::session::Session>,
    files: Option<&[AnnotatedFile]>,
) {
    // Handle chat slash commands locally without sending to AI
    if text.starts_with('/') {
        handle_chat_slash_command(text, state, session, files);
        return;
    }

    // Persist user message to disk if we have an active chat session
    if let Some(ref chat_id) = state.chat_session_id.clone() {
        let _ = crate::chat::append_message(chat_id, "user", text);
    }

    state.chat_messages.push(TuiChatMessage {
        sender: "YOU".into(),
        content: text.to_string(),
        pending: false,
    });
    state.chat_messages.push(TuiChatMessage {
        sender: "assistant".into(),
        content: String::new(),
        pending: true,
    });

    // Auto-scroll to show the pending response
    let total = state.chat_messages.len();
    if total > 4 {
        state.chat_scroll = total.saturating_sub(4);
    }

    let mission = session
        .map(|s| s.mission.clone())
        .unwrap_or_else(|| "(no active mission)".into());
    let scope_stats = files
        .map(|fs| {
            let in_scope = fs.iter().filter(|f| f.verdict.is_accepted()).count();
            let unasked = fs
                .iter()
                .filter(|f| f.verdict == FileVerdict::Unasked)
                .count();
            let blocked = fs.iter().filter(|f| f.verdict.is_blocked()).count();
            format!(
                "{} expected, {} suspicious, {} blocked",
                in_scope, unasked, blocked
            )
        })
        .unwrap_or_else(|| "no files tracked".into());
    let scope_answer = local_scope_answer(state, session, files);
    let selected_context = selected_chat_file_context(state, files);

    let history: String = state
        .chat_messages
        .iter()
        .filter(|m| !m.pending)
        .map(|m| format!("{}: {}", m.sender, m.content))
        .collect::<Vec<_>>()
        .join("\n");

    let pending = state.chat_pending.clone();
    let config = config.clone();
    let text = text.to_string();
    tokio::spawn(async move {
        let response = answer_chat_message_async(
            text,
            config,
            mission,
            scope_stats,
            selected_context,
            scope_answer,
            history,
        )
        .await;
        *pending.lock().unwrap() = Some(response);
    });
}

fn post_chat_local_command(
    text: &str,
    state: &mut WatchState,
    session: Option<&crate::session::Session>,
    files: Option<&[AnnotatedFile]>,
) {
    handle_chat_slash_command(text, state, session, files);
}

fn post_chat_context(
    state: &mut WatchState,
    session: Option<&crate::session::Session>,
    files: Option<&[AnnotatedFile]>,
) {
    let file_count = files.map(|fs| fs.len()).unwrap_or(0);
    let selected = sorted_visible_files(files, false)
        .get(state.selected_file)
        .map(|file| {
            let (badge, _) = verdict_badge(&file.verdict, &state.theme);
            format!("{} ({})", file.diff.path.display(), badge)
        })
        .unwrap_or_else(|| "no selected file".into());
    let mission = session
        .map(|session| truncate(&session.mission, 120))
        .unwrap_or_else(|| "no active manual session".into());
    let content = format!(
        "Visible context: {} changed file(s), selected {}, {} active detected mission(s), judge {} / {}.\nMission: {}",
        file_count,
        selected,
        state.active_missions.len(),
        state.judge_provider_label,
        state.judge_model,
        mission
    );
    state.chat_messages.push(TuiChatMessage {
        sender: "SYSTEM".into(),
        content,
        pending: false,
    });
    let total = state.chat_messages.len();
    if total > 4 {
        state.chat_scroll = total.saturating_sub(4);
    }
}

fn handle_chat_slash_command(
    text: &str,
    state: &mut WatchState,
    session: Option<&crate::session::Session>,
    files: Option<&[AnnotatedFile]>,
) {
    let body = text.trim_start_matches('/');
    let mut parts = body.splitn(2, char::is_whitespace);
    let cmd = parts.next().unwrap_or("").trim();
    let arg = parts.next().map(str::trim).filter(|s| !s.is_empty());

    // Push the user message first
    state.chat_messages.push(TuiChatMessage {
        sender: "YOU".into(),
        content: text.to_string(),
        pending: false,
    });

    let response = match cmd {
        "explain" => {
            let target = arg.unwrap_or("selected");
            let sorted = sorted_visible_files(files, false);
            let file = if target == "selected" {
                sorted.get(state.selected_file).copied()
            } else {
                sorted
                    .iter()
                    .find(|f| f.diff.path.to_string_lossy().contains(target))
                    .copied()
            };
            match file {
                None => format!(
                    "No file found for \"{}\". Use ↑↓ to select a file first.",
                    target
                ),
                Some(f) => {
                    let (badge, _) = verdict_badge(&f.verdict, &state.theme);
                    let agent = match_tag(&f.matched_agents);
                    let reason = match &f.verdict {
                        FileVerdict::Blocked { policy } => {
                            format!("blocked by policy rule: {}", policy)
                        }
                        FileVerdict::Allowed => "explicitly allowed by policy".into(),
                        FileVerdict::InScope => "matched the active agent mission scope".into(),
                        FileVerdict::Unasked => {
                            "not covered by any active mission — requires manual review".into()
                        }
                        FileVerdict::Clean => "no tracked changes".into(),
                    };
                    format!(
                        "{} is {} (+{} -{}) modified by {}. {}",
                        f.diff.path.display(),
                        badge,
                        f.diff.additions,
                        f.diff.deletions,
                        agent,
                        reason
                    )
                }
            }
        }
        "report" => {
            let mission_txt = session
                .map(|s| s.mission.as_str())
                .unwrap_or("(no active mission)");
            let (n_e, n_s, n_b, n_i) = files
                .map(|fs| {
                    let e = fs.iter().filter(|f| f.verdict.is_accepted()).count();
                    let s = fs
                        .iter()
                        .filter(|f| f.verdict == FileVerdict::Unasked)
                        .count();
                    let b = fs.iter().filter(|f| f.verdict.is_blocked()).count();
                    let i = fs
                        .iter()
                        .filter(|f| f.verdict == FileVerdict::Clean)
                        .count();
                    (e, s, b, i)
                })
                .unwrap_or((0, 0, 0, 0));
            format!(
                "Mission: \"{}\"\n{} expected  {} suspicious  {} blocked  {} ignored",
                mission_txt, n_e, n_s, n_b, n_i
            )
        }
        "filter" => {
            let kind = arg.unwrap_or("");
            match kind {
                "suspicious" | "unasked" => {
                    state.problems_only = true;
                    "Filter: showing suspicious and blocked files only (press esc in Review to clear)".into()
                }
                "all" | "clear" => {
                    state.problems_only = false;
                    "Filter cleared — showing all files".into()
                }
                _ => format!(
                    "Unknown filter \"{}\". Try: /filter suspicious, /filter all",
                    kind
                ),
            }
        }
        "clear-chat" | "clear" => {
            clear_visible_chat(state);
            return;
        }
        "help" => "/explain selected  — explain selected file\n\
             /report            — show scope summary\n\
             /sessions claude   — list local Claude sessions\n\
             /latest codex      — show latest Codex session\n\
             /new-chat <title>  — start a new chat\n\
             /clear-chat        — clear visible messages\n\
             /chats             — list saved chats\n\
             /delete-chat       — archive current chat\n\
             esc=done  ctrl+u=clear input  ↑↓=scroll"
            .into(),
        _ => format!("Unknown command /{cmd}. Try /explain selected, /report, /filter, or /help"),
    };

    state.chat_messages.push(TuiChatMessage {
        sender: "assistant".into(),
        content: response,
        pending: false,
    });

    let total = state.chat_messages.len();
    if total > 4 {
        state.chat_scroll = total.saturating_sub(4);
    }
}

fn sender_color(sender: &str, theme: &Theme) -> Color {
    match sender {
        "YOU" => theme.accent,
        "assistant" => theme.text,
        "AGENTSCOPE" => theme.success,
        "JUDGE" => theme.accent,
        "CLAUDE" => theme.agent_claude,
        "CODEX" | "CODEX APP" => theme.agent_codex,
        "OPENCLAW" | "HERMES" => theme.accent,
        "SYSTEM" => theme.agent_system,
        _ => theme.text_muted,
    }
}

fn render_chat_pane(
    f: &mut Frame,
    area: Rect,
    _session: Option<&crate::session::Session>,
    _files: Option<&[AnnotatedFile]>,
    state: &WatchState,
) {
    let theme = &state.theme;

    // ── Outer block ──────────────────────────────────────────────────────────
    let block = Block::default()
        .title(Span::styled(
            " Chat ",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().bg(theme.bg));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height < 5 {
        return;
    }

    // ── Layout: transcript (min) + composer (3) ──────────────────────────────
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(4), Constraint::Length(3)])
        .split(inner);

    let transcript_area = chunks[0];
    let composer_area = chunks[1];

    // ── Message transcript ────────────────────────────────────────────────────
    let max_w = (transcript_area.width.saturating_sub(4) as usize).max(20);
    state.chat_transcript_area.set(transcript_area);
    let all_lines = chat_render_lines(state, transcript_area.width, max_w);

    let max_scroll = all_lines
        .len()
        .saturating_sub(transcript_area.height as usize);
    let scroll = state.chat_scroll.min(max_scroll);
    let visible: Vec<Line> = all_lines
        .into_iter()
        .skip(scroll)
        .take(transcript_area.height as usize)
        .collect();
    f.render_widget(Paragraph::new(visible), transcript_area);

    // ── Composer ──────────────────────────────────────────────────────────────
    let divider = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(theme.border));
    let composer_inner = divider.inner(composer_area);
    f.render_widget(divider, composer_area);

    let composer_line = if state.input_mode == InputMode::Chat {
        Line::from(vec![
            Span::styled(
                "  ",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(state.input_buf.as_str(), Style::default().fg(theme.text)),
            Span::styled("▌", Style::default().fg(theme.accent)),
        ])
    } else {
        Line::from(vec![
            Span::styled("  enter=compose  ", Style::default().fg(theme.text_subtle)),
            Span::styled("/explain selected", Style::default().fg(theme.accent)),
            Span::styled(
                "  /new-chat  /clear-chat  /chats  /delete-chat",
                Style::default().fg(theme.text_subtle),
            ),
        ])
    };
    f.render_widget(Paragraph::new(composer_line), composer_inner);
}

fn chat_render_lines(state: &WatchState, width: u16, max_w: usize) -> Vec<Line<'static>> {
    let theme = &state.theme;
    let mut all_lines: Vec<Line> = Vec::new();

    if state.chat_messages.is_empty() {
        all_lines.push(Line::from(Span::raw("")));
        all_lines.push(Line::from(Span::styled(
            "  Start a conversation about your agent's changes.",
            Style::default().fg(theme.text_muted),
        )));
        all_lines.push(Line::from(Span::raw("")));
        all_lines.push(Line::from(vec![
            Span::styled("  Try: ", Style::default().fg(theme.text_subtle)),
            Span::styled(
                "/explain selected",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  or  ", Style::default().fg(theme.text_subtle)),
            Span::styled(
                "/report",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        all_lines.push(Line::from(Span::styled(
            "  Press i to compose  ·  /help for commands",
            Style::default().fg(theme.text_subtle),
        )));
    } else {
        for msg in &state.chat_messages {
            let sender_col = sender_color(&msg.sender, theme);
            let bubble_bg = chat_bubble_bg(&msg.sender, theme);
            let content = if msg.pending {
                let dots = ".".repeat(((state.refresh_count / 3) % 4) as usize);
                format!("thinking{}", dots)
            } else {
                msg.content.clone()
            };
            let content_color = if msg.pending {
                theme.text_subtle
            } else {
                theme.text
            };

            // Show a sender label only for non-user, non-assistant named senders
            // (SYSTEM, AGENTSCOPE, JUDGE, CLAUDE, CODEX). User ("YOU") and
            // plain assistant messages are distinguished by background alone.
            let is_user = msg.sender == "YOU";
            let is_assistant = msg.sender == "assistant";
            if !is_user && !is_assistant {
                all_lines.push(padded_chat_line(
                    format!("  {} ", msg.sender),
                    Style::default()
                        .fg(sender_col)
                        .bg(bubble_bg)
                        .add_modifier(Modifier::BOLD),
                    width,
                ));
            }

            // Content lines (Markdown-ish, wrapped for a terminal)
            for chunk in format_chat_text(&content, max_w) {
                all_lines.push(padded_chat_line(
                    chunk,
                    Style::default().fg(content_color).bg(bubble_bg),
                    width,
                ));
            }

            // Blank line between messages
            all_lines.push(Line::from(Span::raw("")));
        }
    }

    all_lines
}

fn chat_plain_lines(state: &WatchState, width: u16) -> Vec<String> {
    let max_w = (width.saturating_sub(4) as usize).max(20);
    let mut lines = Vec::new();
    if state.chat_messages.is_empty() {
        return lines;
    }
    for msg in &state.chat_messages {
        let content = if msg.pending {
            "thinking".to_string()
        } else {
            msg.content.clone()
        };
        if msg.sender != "assistant" {
            lines.push(msg.sender.clone());
        }
        lines.extend(format_chat_text(&content, max_w));
        lines.push(String::new());
    }
    lines
}

fn wrap_text(text: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return vec![text.to_string()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
        } else if current.len() + 1 + word.len() <= max_width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(current.clone());
            current = word.to_string();
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn format_chat_text(text: &str, max_width: usize) -> Vec<String> {
    let normalized = normalize_chat_markdown(text);
    let mut lines = Vec::new();
    for raw in normalized.lines() {
        let line = raw.trim();
        if line.is_empty() {
            lines.push(String::new());
            continue;
        }
        let (prefix, body) = if let Some(body) = line.strip_prefix("- ") {
            ("- ", strip_markdown_inline(body))
        } else if let Some(body) = line.strip_prefix("* ") {
            ("- ", strip_markdown_inline(body))
        } else {
            ("", strip_markdown_inline(line))
        };
        let wrap_width = max_width.saturating_sub(prefix.len()).max(10);
        for (idx, chunk) in wrap_text(&body, wrap_width).into_iter().enumerate() {
            if prefix.is_empty() {
                lines.push(chunk);
            } else if idx == 0 {
                lines.push(format!("{}{}", prefix, chunk));
            } else {
                lines.push(format!("  {}", chunk));
            }
        }
    }
    if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    }
}

fn normalize_chat_markdown(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    let mut previous = '\n';
    while let Some(ch) = chars.next() {
        if (ch == '*' || ch == '-') && chars.peek() == Some(&' ') && previous == ' ' {
            out.push('\n');
            out.push_str("- ");
            let _ = chars.next();
            previous = ' ';
            continue;
        }
        out.push(ch);
        previous = ch;
    }
    out
}

fn strip_markdown_inline(text: &str) -> String {
    text.replace("**", "").replace('`', "").replace("\\n", "\n")
}

fn chat_bubble_bg(sender: &str, theme: &Theme) -> Color {
    match sender {
        "YOU" => theme.user_bubble,
        "SYSTEM" => theme.panel,
        _ => theme.assistant_bubble,
    }
}

fn padded_chat_line(text: String, style: Style, width: u16) -> Line<'static> {
    let inner_width = width.saturating_sub(4) as usize;
    let mut content = format!("  {}", text);
    let used = content.chars().count();
    if used < inner_width {
        content.push_str(&" ".repeat(inner_width - used));
    }
    Line::from(Span::styled(content, style))
}

fn start_judge(
    config: &config::Config,
    state: &mut WatchState,
    session: Option<&crate::session::Session>,
    files: Option<&[AnnotatedFile]>,
) {
    let current = state.judge_status.lock().unwrap().clone();
    if matches!(current, JudgeStatus::Running) {
        state.set_flash("judge is already running…");
    } else if session.is_some() || !state.active_missions.is_empty() {
        if let Some(file_list) = files {
            state.set_flash("🔍 running judge…");
            state.push_log("judge: running", state.theme.warning);
            let judge_status = state.judge_status.clone();
            *judge_status.lock().unwrap() = JudgeStatus::Running;

            let mission = aggregate_mission_text(state, session);
            let annotated = file_list.to_vec();
            let judge_config = config.judge.clone();

            tokio::spawn(async move {
                let result = crate::judge::evaluate(&mission, &annotated, &judge_config).await;

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

#[cfg(test)]
mod command_tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};
    use std::path::PathBuf;

    #[test]
    fn parse_slash_commands_with_optional_args() {
        assert_eq!(parse_tui_command("/status").unwrap(), TuiCommand::Status);
        assert_eq!(parse_tui_command(" /judge ").unwrap(), TuiCommand::Judge);
        assert_eq!(
            parse_tui_command("/diff src/main.rs").unwrap(),
            TuiCommand::Diff(Some(PathBuf::from("src/main.rs")))
        );
        assert_eq!(
            parse_tui_command("/theme").unwrap(),
            TuiCommand::Theme(None)
        );
        assert_eq!(
            parse_tui_command("/theme codex").unwrap(),
            TuiCommand::Theme(Some("codex".into()))
        );
    }

    #[test]
    fn parse_policy_commands_and_quit_aliases() {
        assert_eq!(
            parse_tui_command("/allow src/auth/session.ts").unwrap(),
            TuiCommand::Allow(Some("src/auth/session.ts".into()))
        );
        assert_eq!(
            parse_tui_command("/block generated/**").unwrap(),
            TuiCommand::Block(Some("generated/**".into()))
        );
        assert_eq!(parse_tui_command("/quit").unwrap(), TuiCommand::Quit);
        assert_eq!(parse_tui_command("/exit").unwrap(), TuiCommand::Quit);
    }

    #[test]
    fn parse_rejects_unknown_and_non_slash_input() {
        assert!(parse_tui_command("status").is_err());
        let err = parse_tui_command("/wat").unwrap_err();
        assert!(err.contains("unknown command"));
    }

    #[test]
    fn command_suggestions_show_all_commands_after_slash() {
        let suggestions = command_suggestions("/");

        assert!(suggestions.iter().any(|cmd| cmd.name == "/theme"));
        assert!(suggestions.iter().any(|cmd| cmd.name == "/allow"));
        assert!(suggestions.iter().any(|cmd| cmd.name == "/block"));
    }

    #[test]
    fn command_suggestions_filter_by_prefix() {
        let suggestions = command_suggestions("/th");

        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].name, "/theme");
    }

    #[test]
    fn autocomplete_completes_command_name_and_preserves_theme_arg() {
        assert_eq!(autocomplete_first("/th"), "/theme ");
        assert_eq!(autocomplete_first("/theme o"), "/theme openclaw");
    }

    #[test]
    fn parse_judge_and_aggregate_commands() {
        assert_eq!(
            parse_tui_command("/judge-provider codex").unwrap(),
            TuiCommand::JudgeProvider(Some("codex".into()))
        );
        assert_eq!(
            parse_tui_command("/ollama-model qwen3.5:2b").unwrap(),
            TuiCommand::OllamaModel(Some("qwen3.5:2b".into()))
        );
        assert_eq!(parse_tui_command("/agents").unwrap(), TuiCommand::Agents);
        assert_eq!(
            parse_tui_command("/dashboard").unwrap(),
            TuiCommand::Dashboard
        );
    }

    #[test]
    fn parse_chat_commands_for_global_palette() {
        assert_eq!(
            parse_tui_command("/explain selected").unwrap(),
            TuiCommand::Explain(Some("selected".into()))
        );
        assert_eq!(
            parse_tui_command("/report").unwrap(),
            TuiCommand::ChatReport
        );
        assert_eq!(
            parse_tui_command("/filter suspicious").unwrap(),
            TuiCommand::ChatFilter(Some("suspicious".into()))
        );
        assert_eq!(
            parse_tui_command("/clear-chat").unwrap(),
            TuiCommand::ClearChat
        );
        assert_eq!(
            parse_tui_command("/delete-chat 01ABC").unwrap(),
            TuiCommand::DeleteChat(Some("01ABC".into()))
        );
        assert_eq!(
            parse_tui_command("/chat-context").unwrap(),
            TuiCommand::ChatContext
        );
    }

    #[test]
    fn command_suggestions_include_chat_commands() {
        let suggestions = command_suggestions("/ex");
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].name, "/explain");

        let suggestions = command_suggestions("/chat");
        assert!(suggestions.iter().any(|cmd| cmd.name == "/chat"));
        assert!(suggestions.iter().any(|cmd| cmd.name == "/chat-context"));
    }

    #[test]
    fn sorted_visible_files_matches_rendered_order() {
        use crate::git::{DiffStatus, FileDiff};

        let files = vec![
            AnnotatedFile {
                diff: FileDiff {
                    path: PathBuf::from("z_expected.rs"),
                    additions: 1,
                    deletions: 0,
                    status: DiffStatus::Modified,
                },
                verdict: FileVerdict::InScope,
                matched_agents: vec![],
            },
            AnnotatedFile {
                diff: FileDiff {
                    path: PathBuf::from("a_suspicious.rs"),
                    additions: 10,
                    deletions: 0,
                    status: DiffStatus::Modified,
                },
                verdict: FileVerdict::Unasked,
                matched_agents: vec![],
            },
            AnnotatedFile {
                diff: FileDiff {
                    path: PathBuf::from("b_blocked.rs"),
                    additions: 1,
                    deletions: 0,
                    status: DiffStatus::Modified,
                },
                verdict: FileVerdict::Blocked {
                    policy: "b_*".into(),
                },
                matched_agents: vec![],
            },
        ];

        let sorted = sorted_visible_files(Some(&files), false);
        assert_eq!(sorted[0].diff.path, PathBuf::from("b_blocked.rs"));
        assert_eq!(sorted[1].diff.path, PathBuf::from("a_suspicious.rs"));
        assert_eq!(sorted[2].diff.path, PathBuf::from("z_expected.rs"));
    }

    #[test]
    fn file_list_hides_raw_unmatched_agent_label() {
        use crate::git::{DiffStatus, FileDiff};

        let files = vec![AnnotatedFile {
            diff: FileDiff {
                path: PathBuf::from("src/ui.rs"),
                additions: 12,
                deletions: 2,
                status: DiffStatus::Modified,
            },
            verdict: FileVerdict::Unasked,
            matched_agents: vec![],
        }];
        let state = WatchState::new("agentscope");
        let backend = TestBackend::new(80, 8);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|f| {
                render_file_list(f, Rect::new(0, 0, 80, 8), Some(&files), &state);
            })
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("SUSPICIOUS"));
        assert!(rendered.contains("src/ui.rs"));
        assert!(!rendered.contains("UNMATCHED"));
    }

    #[test]
    fn file_detail_uses_product_copy_for_missing_agent() {
        use crate::git::{DiffStatus, FileDiff};

        let files = vec![AnnotatedFile {
            diff: FileDiff {
                path: PathBuf::from("src/judge.rs"),
                additions: 268,
                deletions: 8,
                status: DiffStatus::Modified,
            },
            verdict: FileVerdict::Blocked {
                policy: "src/judge.rs".into(),
            },
            matched_agents: vec![],
        }];
        let state = WatchState::new("agentscope");
        let backend = TestBackend::new(80, 8);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|f| {
                render_file_detail(f, Rect::new(0, 0, 80, 8), Some(&files), &state);
            })
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("agent: none"));
        assert!(!rendered.contains("UNMATCHED"));
    }

    #[test]
    fn header_keeps_blocked_count_visible_at_terminal_width() {
        use crate::git::{DiffStatus, FileDiff};

        let files = vec![
            AnnotatedFile {
                diff: FileDiff {
                    path: PathBuf::from("src/ok.rs"),
                    additions: 1,
                    deletions: 0,
                    status: DiffStatus::Modified,
                },
                verdict: FileVerdict::InScope,
                matched_agents: vec![],
            },
            AnnotatedFile {
                diff: FileDiff {
                    path: PathBuf::from("src/watch.rs"),
                    additions: 2,
                    deletions: 0,
                    status: DiffStatus::Modified,
                },
                verdict: FileVerdict::Unasked,
                matched_agents: vec![],
            },
            AnnotatedFile {
                diff: FileDiff {
                    path: PathBuf::from(".env"),
                    additions: 1,
                    deletions: 0,
                    status: DiffStatus::Modified,
                },
                verdict: FileVerdict::Blocked {
                    policy: ".env".into(),
                },
                matched_agents: vec![],
            },
        ];
        let mut state = WatchState::new("agentscope");
        state.active_missions = vec![
            ActiveMission {
                agent: "codex".into(),
                mission: "A very long mission that should move to the second header row".into(),
                confidence: 0.9,
                source_path: None,
                timestamp: None,
                age_seconds: Some(1),
            },
            ActiveMission {
                agent: "claude-code".into(),
                mission: "secondary mission".into(),
                confidence: 0.8,
                source_path: None,
                timestamp: None,
                age_seconds: Some(1),
            },
        ];
        let backend = TestBackend::new(80, 3);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|f| {
                render_header(f, Rect::new(0, 0, 80, 3), None, Some(&files), &state);
            })
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("1 exp"));
        assert!(rendered.contains("1 susp"));
        assert!(rendered.contains("1 block"));
        assert!(rendered.contains("mission"));
        assert!(rendered.contains("up 0s"));
    }

    #[test]
    fn header_keeps_uptime_unit_visible_after_ten_seconds() {
        use crate::git::{DiffStatus, FileDiff};

        let files = vec![AnnotatedFile {
            diff: FileDiff {
                path: PathBuf::from("src/ok.rs"),
                additions: 1,
                deletions: 0,
                status: DiffStatus::Modified,
            },
            verdict: FileVerdict::InScope,
            matched_agents: vec![],
        }];
        let mut state = WatchState::new("agentscope");
        state.started_at = Instant::now() - Duration::from_secs(10);
        state.active_missions = vec![ActiveMission {
            agent: "codex".into(),
            mission: "A very long mission title that should never crowd the uptime suffix".into(),
            confidence: 0.9,
            source_path: None,
            timestamp: None,
            age_seconds: Some(1),
        }];
        let backend = TestBackend::new(80, 3);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|f| {
                render_header(f, Rect::new(0, 0, 80, 3), None, Some(&files), &state);
            })
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("up 10s"));
    }

    #[test]
    fn summary_bar_keeps_risk_counts_visible_at_terminal_width() {
        use crate::git::{DiffStatus, FileDiff};

        let files = vec![
            AnnotatedFile {
                diff: FileDiff {
                    path: PathBuf::from("src/ok.rs"),
                    additions: 1,
                    deletions: 0,
                    status: DiffStatus::Modified,
                },
                verdict: FileVerdict::InScope,
                matched_agents: vec![],
            },
            AnnotatedFile {
                diff: FileDiff {
                    path: PathBuf::from("src/watch.rs"),
                    additions: 2,
                    deletions: 0,
                    status: DiffStatus::Modified,
                },
                verdict: FileVerdict::Unasked,
                matched_agents: vec![],
            },
            AnnotatedFile {
                diff: FileDiff {
                    path: PathBuf::from(".env"),
                    additions: 1,
                    deletions: 0,
                    status: DiffStatus::Modified,
                },
                verdict: FileVerdict::Blocked {
                    policy: ".env".into(),
                },
                matched_agents: vec![],
            },
        ];
        let state = WatchState::new("agentscope");
        let backend = TestBackend::new(80, 3);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|f| {
                render_summary_bar(f, Rect::new(0, 0, 80, 3), Some(&files), &state);
            })
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("1 exp"));
        assert!(rendered.contains("1 susp"));
        assert!(rendered.contains("1 blk"));
    }

    #[test]
    fn summary_bar_omits_dangling_hint_separator_at_terminal_width() {
        use crate::git::{DiffStatus, FileDiff};

        let files = vec![
            AnnotatedFile {
                diff: FileDiff {
                    path: PathBuf::from("src/ok.rs"),
                    additions: 1,
                    deletions: 0,
                    status: DiffStatus::Modified,
                },
                verdict: FileVerdict::InScope,
                matched_agents: vec![],
            },
            AnnotatedFile {
                diff: FileDiff {
                    path: PathBuf::from("src/watch.rs"),
                    additions: 2,
                    deletions: 0,
                    status: DiffStatus::Modified,
                },
                verdict: FileVerdict::Unasked,
                matched_agents: vec![],
            },
            AnnotatedFile {
                diff: FileDiff {
                    path: PathBuf::from(".env"),
                    additions: 1,
                    deletions: 0,
                    status: DiffStatus::Modified,
                },
                verdict: FileVerdict::Blocked {
                    policy: ".env".into(),
                },
                matched_agents: vec![],
            },
        ];
        let state = WatchState::new("agentscope");
        let backend = TestBackend::new(80, 3);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|f| {
                render_summary_bar(f, Rect::new(0, 0, 80, 3), Some(&files), &state);
            })
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        let row = rendered
            .lines()
            .find(|line| line.contains("1=review"))
            .unwrap_or("");

        assert!(row.contains("1 blk"));
        assert!(!row.contains("1 blk  ·"));
    }

    #[test]
    fn compact_review_layout_prioritizes_changes_and_decision() {
        use crate::git::{DiffStatus, FileDiff};

        let files = vec![
            AnnotatedFile {
                diff: FileDiff {
                    path: PathBuf::from("src/judge.rs"),
                    additions: 268,
                    deletions: 8,
                    status: DiffStatus::Modified,
                },
                verdict: FileVerdict::Blocked {
                    policy: "src/judge.rs".into(),
                },
                matched_agents: vec![],
            },
            AnnotatedFile {
                diff: FileDiff {
                    path: PathBuf::from("src/tui.rs"),
                    additions: 42,
                    deletions: 4,
                    status: DiffStatus::Modified,
                },
                verdict: FileVerdict::Unasked,
                matched_agents: vec![],
            },
        ];
        let mut state = WatchState::new("agentscope");
        state.active_missions = vec![ActiveMission {
            agent: "codex".into(),
            mission: "Polish the terminal UI without changing functionality".into(),
            confidence: 0.75,
            source_path: None,
            timestamp: None,
            age_seconds: Some(1),
        }];
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|f| ui(f, None, Some(&files), &state))
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains(" Changes "));
        assert!(rendered.contains(" Decision "));
        assert!(!rendered.contains(" Missions "));
    }

    #[test]
    fn decision_panel_names_why_and_actions() {
        use crate::git::{DiffStatus, FileDiff};

        let files = vec![AnnotatedFile {
            diff: FileDiff {
                path: PathBuf::from("src/judge.rs"),
                additions: 268,
                deletions: 8,
                status: DiffStatus::Modified,
            },
            verdict: FileVerdict::Blocked {
                policy: "src/judge.rs".into(),
            },
            matched_agents: vec![],
        }];
        let state = WatchState::new("agentscope");
        let backend = TestBackend::new(80, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|f| {
                render_file_detail(f, Rect::new(0, 0, 80, 10), Some(&files), &state);
            })
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("Why"));
        assert!(rendered.contains("Actions"));
        assert!(rendered.contains("matched blocked policy"));
    }

    #[test]
    fn decision_panel_keeps_action_commands_visible_when_compact() {
        use crate::git::{DiffStatus, FileDiff};

        let files = vec![AnnotatedFile {
            diff: FileDiff {
                path: PathBuf::from("src/judge.rs"),
                additions: 268,
                deletions: 8,
                status: DiffStatus::Modified,
            },
            verdict: FileVerdict::Blocked {
                policy: "src/judge.rs".into(),
            },
            matched_agents: vec![],
        }];
        let state = WatchState::new("agentscope");
        let backend = TestBackend::new(80, 9);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|f| {
                render_file_detail(f, Rect::new(0, 0, 80, 9), Some(&files), &state);
            })
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("Actions"));
        assert!(rendered.contains("enter=full diff"));
    }

    #[test]
    fn chat_text_formatter_splits_inline_bullets() {
        let lines = format_chat_text(
            "I can: * **Analyze:** Review files. * **Inspect:** Explain selected files.",
            80,
        );
        assert!(lines.iter().any(|line| line == "- Analyze: Review files."));
        assert!(lines
            .iter()
            .any(|line| line == "- Inspect: Explain selected files."));
    }

    #[test]
    fn codex_provider_maps_to_openai() {
        assert_eq!(
            parse_judge_provider("codex").unwrap(),
            config::JudgeProvider::Openai
        );
    }

    #[test]
    fn autocomplete_uses_selected_value_for_provider_and_ollama_model() {
        let mut state = WatchState::new("agentscope");
        state.command_selected = 1;
        assert_eq!(
            autocomplete_command_input("/judge-provider ", &state),
            "/judge-provider openai"
        );

        state.ollama_models = vec!["llama3".into(), "qwen3.5:2b".into()];
        state.command_selected = 1;
        assert_eq!(
            autocomplete_command_input("/ollama-model ", &state),
            "/ollama-model qwen3.5:2b"
        );
    }
}
