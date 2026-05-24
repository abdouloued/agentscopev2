use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "agentscope",
    version,
    about = "Did your AI agent do only what you asked?",
    long_about = "AgentScope is a scope firewall and audit layer for AI coding agents.\nIt records your mission, watches what the agent actually changes,\nand blocks policy violations before they reach git.",
    after_help = "EXAMPLES:\n  agentscope start \"Fix the rate-limit bug in api/middleware.ts\"\n  agentscope check\n  agentscope judge -m qwen3.5:2b\n  agentscope model list\n  agentscope model set llama3\n  agentscope config show\n  agentscope config set model qwen3.5:2b\n  agentscope report\n  agentscope diff --problems\n  agentscope hook install\n  agentscope audit last-5\n  agentscope use claude\n",
    styles = clap_styles(),
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Begin a new agent session with a stated mission
    Start {
        /// What you asked the agent to do (in plain English)
        #[arg(value_name = "MISSION")]
        mission: String,

        /// Which agent you're running
        #[arg(short, long, value_enum, default_value = "claude")]
        agent: AgentKind,

        /// Watch mode: re-check automatically on every file change
        #[arg(short, long)]
        watch: bool,
    },

    /// Check the current session against your mission and policy
    Check {
        /// Session ID to check (defaults to active session)
        #[arg(long)]
        session_id: Option<String>,

        /// Output raw JSON (for CI pipelines)
        #[arg(long)]
        json: bool,

        /// Copy a Markdown session summary to clipboard
        #[arg(long)]
        share: bool,
    },

    /// Run the LLM judge on current changes — pick any model
    Judge {
        /// LLM provider: ollama, claude, openai (default: from config)
        #[arg(short, long, value_enum)]
        provider: Option<JudgeProviderArg>,

        /// Model name, e.g. qwen3.5:2b, claude-sonnet-4-20250514, gpt-4o (default: from config)
        #[arg(short, long)]
        model: Option<String>,

        /// Ollama endpoint override (default: http://localhost:11434)
        #[arg(long)]
        endpoint: Option<String>,

        /// Output raw JSON instead of pretty-print
        #[arg(long)]
        json: bool,
    },

    /// Manage LLM models — list, set, test, pull
    #[command(alias = "models")]
    Model {
        #[command(subcommand)]
        action: ModelAction,
    },

    /// View and edit agentscope configuration
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },

    /// Generate a detailed session report (terminal or markdown)
    Report {
        /// Output as Markdown (for sharing in PRs)
        #[arg(long)]
        markdown: bool,
    },

    /// Show git diff with scope annotations (shortcut for quick review)
    Diff {
        /// Show only blocked and unasked files
        #[arg(long)]
        problems: bool,
    },

    /// Install or manage git hooks for automatic scope checking
    Hook {
        #[command(subcommand)]
        action: HookAction,
    },

    /// Audit past sessions — what changed, why, when
    Audit {
        /// Range: "last-5", "today", "this-week", or a git range like "HEAD~3..HEAD"
        #[arg(value_name = "RANGE", default_value = "last-5")]
        range: String,

        /// Filter to a specific session ID
        #[arg(long)]
        session_id: Option<String>,
    },

    /// Configure AgentScope to work natively with an agent
    Use {
        /// Agent to integrate
        #[arg(value_enum)]
        agent: AgentKind,
    },

    /// Initialize a new agentscope.yaml config in the current repo
    Init {
        /// Config preset for your workflow
        #[arg(long, value_enum, default_value = "solo")]
        preset: Preset,
    },

    /// Live TUI dashboard — watch sessions in real time
    Watch,

    /// Show the current active session status (one-liner)
    Status,
}

// ── Model subcommands ────────────────────────────────────────────────────────

#[derive(Subcommand, Clone, Debug)]
pub enum ModelAction {
    /// List available Ollama models and cloud providers
    #[command(alias = "ls")]
    List,

    /// Set the default judge model
    Set {
        /// Model name (e.g. qwen3.5:2b, llama3, gemma4:e2b)
        model: String,

        /// Provider to use
        #[arg(short, long, value_enum)]
        provider: Option<JudgeProviderArg>,

        /// Custom endpoint
        #[arg(long)]
        endpoint: Option<String>,
    },

    /// Test a model with a simple prompt
    Test {
        /// Model to test (defaults to current default)
        model: Option<String>,
    },

    /// Pull/download a model from Ollama
    Pull {
        /// Model to pull (e.g. llama3, qwen3.5:2b)
        model: String,
    },
}

// ── Config subcommands ──────────────────────────────────────────────────────

#[derive(Subcommand, Clone, Debug)]
pub enum ConfigAction {
    /// Show current configuration
    Show,

    /// Set a config value (e.g. agentscope config set model qwen3.5:2b)
    Set {
        /// Config key (model, provider, endpoint, max_files, max_lines, judge.enabled, team.enabled)
        key: String,

        /// Value to set
        value: String,
    },

    /// Open agentscope.yaml in your $EDITOR
    Edit,

    /// Reset config to a preset
    Reset {
        /// Preset to reset to
        #[arg(value_enum, default_value = "solo")]
        preset: Preset,
    },

    /// Show the config file path
    Path,
}

// ── Hook subcommands ────────────────────────────────────────────────────────

#[derive(Subcommand, Clone, Debug)]
pub enum HookAction {
    /// Install a pre-commit hook that runs `agentscope check`
    Install,
    /// Remove the AgentScope pre-commit hook
    Uninstall,
    /// Show current hook status
    Status,
}

/// Provider argument for judge/model commands
#[derive(ValueEnum, Clone, Debug)]
pub enum JudgeProviderArg {
    /// Local Ollama (default)
    Ollama,
    /// Anthropic Claude API
    Claude,
    /// OpenAI API
    Openai,
}

#[derive(ValueEnum, Clone, Debug, PartialEq)]
pub enum AgentKind {
    /// Anthropic Claude Code
    Claude,
    /// OpenAI Codex CLI
    Codex,
    /// OpenAI Codex App (GUI)
    #[value(name = "codex-app")]
    CodexApp,
    /// Cursor AI editor
    Cursor,
    /// Google Gemini CLI
    Gemini,
    /// Anomaly's OpenCode
    Opencode,
    /// OpenClaw personal AI
    Openclaw,
    /// Nous Research Hermes Agent
    Hermes,
    /// GitHub Copilot CLI
    Copilot,
    /// Factory Droid
    Droid,
    /// Minimal AI agent toolkit
    Pi,
    /// Any other agent
    Custom,
}

impl std::fmt::Display for AgentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            AgentKind::Claude => "claude-code",
            AgentKind::Codex => "codex",
            AgentKind::CodexApp => "codex-app",
            AgentKind::Cursor => "cursor",
            AgentKind::Gemini => "gemini-cli",
            AgentKind::Opencode => "opencode",
            AgentKind::Openclaw => "openclaw",
            AgentKind::Hermes => "hermes",
            AgentKind::Copilot => "copilot-cli",
            AgentKind::Droid => "droid",
            AgentKind::Pi => "pi",
            AgentKind::Custom => "custom",
        };
        write!(f, "{}", s)
    }
}

#[derive(ValueEnum, Clone, Debug)]
pub enum Preset {
    /// Solo developer on a personal project
    Solo,
    /// Engineering team with shared audit logs
    Team,
    /// CI pipeline integration
    Ci,
}

fn clap_styles() -> clap::builder::Styles {
    use clap::builder::styling::{AnsiColor, Color, Style};
    clap::builder::Styles::styled()
        .header(Style::new().bold().fg_color(Some(Color::Ansi(AnsiColor::Cyan))))
        .usage(Style::new().bold().fg_color(Some(Color::Ansi(AnsiColor::Cyan))))
        .literal(Style::new().fg_color(Some(Color::Ansi(AnsiColor::BrightWhite))))
        .placeholder(Style::new().fg_color(Some(Color::Ansi(AnsiColor::White))))
        .error(Style::new().bold().fg_color(Some(Color::Ansi(AnsiColor::Red))))
        .valid(Style::new().fg_color(Some(Color::Ansi(AnsiColor::Green))))
        .invalid(Style::new().fg_color(Some(Color::Ansi(AnsiColor::Red))))
}
