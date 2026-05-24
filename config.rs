use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::cli::{AgentKind, Preset};
use crate::output::theme;

pub const CONFIG_FILE: &str = "agentscope.yaml";
pub const SESSION_DIR: &str = ".agentscope";
pub const ACTIVITY_LOG: &str = ".agentscope/activity.jsonl";

// ── Top-level config struct ───────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Config {
    #[serde(default = "default_version")]
    pub version: u8,

    #[serde(default)]
    pub policy: PolicyConfig,

    #[serde(default)]
    pub judge: JudgeConfig,

    #[serde(default)]
    pub team: TeamConfig,
}

fn default_version() -> u8 { 1 }

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            policy: PolicyConfig::default(),
            judge: JudgeConfig::default(),
            team: TeamConfig::default(),
        }
    }
}

// ── Policy ────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct PolicyConfig {
    /// Glob patterns always blocked regardless of mission
    #[serde(default = "default_blocked")]
    pub blocked: Vec<String>,

    /// Glob patterns that trigger a warning but don't block
    #[serde(default = "default_warn")]
    pub warn: Vec<String>,

    /// Max lines changed before a warning fires (0 = disabled)
    #[serde(default)]
    pub max_lines_changed: usize,

    /// Max files changed before a warning fires (0 = disabled)
    #[serde(default)]
    pub max_files_changed: usize,
}

fn default_blocked() -> Vec<String> {
    vec![
        ".env".into(),
        ".env.*".into(),
        "**/.env".into(),
        "**/.env.*".into(),
        "**/secrets/**".into(),
        "**/*.pem".into(),
        "**/*.key".into(),
        "src/auth/**".into(),
        "**/migrations/**".into(),
    ]
}

fn default_warn() -> Vec<String> {
    vec![
        "package-lock.json".into(),
        "yarn.lock".into(),
        "Cargo.lock".into(),
        "**/config/**".into(),
    ]
}

// ── Judge ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct JudgeConfig {
    pub enabled: bool,
    pub provider: JudgeProvider,
    pub model: String,
    pub endpoint: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum JudgeProvider {
    #[default]
    Ollama,
    Claude,
    Openai,
    None,
}

impl Default for JudgeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            provider: JudgeProvider::Ollama,
            model: "qwen3.5:2b".into(),
            endpoint: "http://localhost:11434".into(),
        }
    }
}

// ── Team ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct TeamConfig {
    pub enabled: bool,
    pub share_logs: bool,
    pub log_path: Option<PathBuf>,
}

// ── Load / write ──────────────────────────────────────────────────────────────

pub fn load() -> Result<Config> {
    let path = find_config_file()?;
    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("Could not read {}", path.display()))?;
    let config: Config = serde_yaml::from_str(&contents)
        .with_context(|| format!("Invalid YAML in {}", path.display()))?;
    Ok(config)
}

pub fn load_or_default() -> Config {
    load().unwrap_or_default()
}

fn find_config_file() -> Result<PathBuf> {
    let mut dir = std::env::current_dir()?;
    loop {
        let candidate = dir.join(CONFIG_FILE);
        if candidate.exists() {
            return Ok(candidate);
        }
        if !dir.pop() {
            anyhow::bail!(
                "No {} found. Run `agentscope init` to create one.",
                CONFIG_FILE
            );
        }
    }
}

// ── Commands ──────────────────────────────────────────────────────────────────

pub async fn init(preset: Preset) -> Result<()> {
    use crate::output::Printer;
    let p = Printer::new();

    let path = Path::new(CONFIG_FILE);
    if path.exists() {
        p.warn(&format!("{} already exists — skipping", CONFIG_FILE));
        return Ok(());
    }

    let config = preset_config(&preset);
    let yaml = serde_yaml::to_string(&config)?;
    let header = format!(
        "# AgentScope configuration — preset: {:?}\n# Docs: https://agentscope.dev/config\n\n",
        preset
    );
    std::fs::write(path, header + &yaml)?;

    std::fs::create_dir_all(SESSION_DIR)?;

    // Append SESSION_DIR to .gitignore if present
    let gi = Path::new(".gitignore");
    if gi.exists() {
        let contents = std::fs::read_to_string(gi)?;
        if !contents.contains(SESSION_DIR) {
            std::fs::write(gi, format!("{}\n{}/\n", contents.trim_end(), SESSION_DIR))?;
        }
    }

    p.success(&format!("Created {}", CONFIG_FILE));
    p.hint("Next: agentscope start \"your mission here\"");
    Ok(())
}

pub async fn integrate_agent(agent: AgentKind) -> Result<()> {
    use crate::output::Printer;
    let p = Printer::new();

    match agent {
        AgentKind::Claude => {
            let content = claude_md_content();
            std::fs::write("CLAUDE.md", content)?;
            p.success("Wrote CLAUDE.md — Claude Code will now respect AgentScope sessions");
        }
        AgentKind::Cursor => {
            std::fs::create_dir_all(".cursor/rules")?;
            std::fs::write(".cursor/rules/agentscope.md", cursor_rules_content())?;
            p.success("Wrote .cursor/rules/agentscope.md");
        }
        AgentKind::Gemini => {
            std::fs::write("GEMINI.md", gemini_md_content())?;
            p.success("Wrote GEMINI.md");
        }
        _ => {
            p.hint(&format!(
                "No native integration for {}. Add agentscope check to your workflow manually.",
                agent
            ));
        }
    }
    Ok(())
}

fn preset_config(preset: &Preset) -> Config {
    let mut config = Config::default();
    match preset {
        Preset::Solo => {
            config.policy.max_files_changed = 20;
            config.judge.enabled = true;
        }
        Preset::Team => {
            config.policy.max_files_changed = 10;
            config.team.enabled = true;
            config.team.share_logs = true;
            config.judge.enabled = true;
        }
        Preset::Ci => {
            config.judge.enabled = false;
            config.policy.max_files_changed = 5;
        }
    }
    config
}

fn claude_md_content() -> &'static str {
    r#"# AgentScope Integration

This repo uses AgentScope to track and audit AI agent sessions.

## Rules for Claude Code

1. Before starting work, read the active session with `cat .agentscope/session.json`
2. Only modify files that are relevant to the stated mission
3. Never modify files matching: `.env*`, `src/auth/**`, `**/migrations/**`, `*.pem`, `*.key`
4. After completing work, confirm with: `agentscope check`

## Why these rules exist

AgentScope records every file you touch and compares it to the stated mission.
Edits outside scope are flagged as UNASKED. Blocked-path edits halt the session.
This is a safety and audit layer — not a limitation on your capability.
"#
}

fn cursor_rules_content() -> &'static str {
    r#"# AgentScope rules for Cursor

- Read `.agentscope/session.json` before beginning any task
- Scope your changes to files relevant to the active session mission
- Never edit `.env*`, `src/auth/**`, `**/migrations/**`, `*.pem`, `*.key`
- Run `agentscope check` after completing each task
"#
}

fn gemini_md_content() -> &'static str {
    r#"# AgentScope Integration

This repo uses AgentScope for agent session tracking.
Read `.agentscope/session.json` for the active mission before making changes.
Run `agentscope check` when done.
"#
}
