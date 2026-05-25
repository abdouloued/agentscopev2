use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::cli::{AgentKind, Preset};

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

    #[serde(default)]
    pub agents: AgentsConfig,
}

fn default_version() -> u8 {
    1
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            policy: PolicyConfig::default(),
            judge: JudgeConfig::default(),
            team: TeamConfig::default(),
            agents: AgentsConfig::default(),
        }
    }
}

// ── Policy ────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone)]
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

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            blocked: default_blocked(),
            warn: default_warn(),
            max_lines_changed: 0,
            max_files_changed: 0,
        }
    }
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

// ── Agent context detection ─────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AgentsConfig {
    #[serde(default = "default_true")]
    pub auto_detect: bool,
    #[serde(default)]
    pub auto_attach: bool,
    #[serde(default = "default_preferred_agents")]
    pub preferred: Vec<String>,
    #[serde(default)]
    pub sources: std::collections::BTreeMap<String, AgentSourceConfig>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AgentSourceConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub paths: Vec<String>,
}

impl Default for AgentsConfig {
    fn default() -> Self {
        Self {
            auto_detect: true,
            auto_attach: false,
            preferred: default_preferred_agents(),
            sources: std::collections::BTreeMap::new(),
        }
    }
}

impl Default for AgentSourceConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            paths: Vec::new(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_preferred_agents() -> Vec<String> {
    vec![
        "claude-code".into(),
        "codex".into(),
        "opencode".into(),
        "cursor".into(),
        "gemini-cli".into(),
        "copilot-cli".into(),
    ]
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
            std::fs::write("CLAUDE.md", agent_rules_content("Claude Code"))?;
            p.success("Wrote CLAUDE.md — Claude Code will now respect AgentScope sessions");
        }
        AgentKind::Cursor => {
            std::fs::create_dir_all(".cursor/rules")?;
            std::fs::write(".cursor/rules/agentscope.md", agent_rules_content("Cursor"))?;
            p.success("Wrote .cursor/rules/agentscope.md");
        }
        AgentKind::Gemini => {
            std::fs::write("GEMINI.md", agent_rules_content("Gemini CLI"))?;
            p.success("Wrote GEMINI.md");
        }
        AgentKind::Codex | AgentKind::CodexApp => {
            std::fs::write("AGENTS.md", agent_rules_content("Codex"))?;
            p.success("Wrote AGENTS.md — Codex will now respect AgentScope sessions");
        }
        AgentKind::Opencode => {
            std::fs::write("AGENTS.md", agent_rules_content("OpenCode"))?;
            p.success("Wrote AGENTS.md — OpenCode will now respect AgentScope sessions");
        }
        AgentKind::Openclaw => {
            std::fs::write("AGENTS.md", agent_rules_content("OpenClaw"))?;
            p.success("Wrote AGENTS.md — OpenClaw will now respect AgentScope sessions");
        }
        AgentKind::Hermes => {
            std::fs::write("AGENTS.md", agent_rules_content("Hermes Agent"))?;
            p.success("Wrote AGENTS.md — Hermes will now respect AgentScope sessions");
        }
        AgentKind::Copilot => {
            std::fs::write("AGENTS.md", agent_rules_content("Copilot CLI"))?;
            p.success("Wrote AGENTS.md — Copilot will now respect AgentScope sessions");
        }
        AgentKind::Droid => {
            std::fs::write("AGENTS.md", agent_rules_content("Droid"))?;
            p.success("Wrote AGENTS.md — Droid will now respect AgentScope sessions");
        }
        AgentKind::Pi => {
            std::fs::write("AGENTS.md", agent_rules_content("Pi"))?;
            p.success("Wrote AGENTS.md — Pi will now respect AgentScope sessions");
        }
        AgentKind::Custom => {
            p.hint("Custom agent — add agentscope check to your agent's post-run hook manually.");
            p.hint("See: agentscope use claude for an example integration file.");
        }
    }
    Ok(())
}

pub(crate) fn preset_config(preset: &Preset) -> Config {
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

fn agent_rules_content(agent_name: &str) -> String {
    format!(
        r#"# AgentScope Integration

This repo uses [AgentScope](https://github.com/abdouloued/agentscopev2) to track and audit AI agent sessions.

## Rules for {agent_name}

1. Before starting work, read the active session: `cat .agentscope/session.json`
2. Only modify files that are relevant to the stated mission
3. Never modify files matching these blocked patterns:
   - `.env*` — environment secrets
   - `src/auth/**` — authentication logic
   - `**/migrations/**` — database migrations
   - `*.pem`, `*.key` — cryptographic keys
4. After completing work, verify with: `agentscope check`

## Why these rules exist

AgentScope records every file you touch and compares it to the stated mission.
Edits outside scope are flagged as **UNASKED**. Blocked-path edits **halt the session**.
This is a safety and audit layer — not a limitation on your capability.

## Quick reference

```bash
# Check what the session expects
cat .agentscope/session.json | jq '.mission'

# Verify your changes when done
agentscope check

# See detailed status
agentscope status
```
"#,
        agent_name = agent_name,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Preset;

    #[test]
    fn default_config_version_is_one() {
        let config = Config::default();
        assert_eq!(config.version, 1);
    }

    #[test]
    fn default_config_judge_model() {
        let config = Config::default();
        assert_eq!(config.judge.model, "qwen3.5:2b");
    }

    #[test]
    fn default_config_judge_enabled() {
        let config = Config::default();
        assert!(config.judge.enabled);
    }

    #[test]
    fn default_config_judge_provider_ollama() {
        let config = Config::default();
        assert_eq!(config.judge.provider, JudgeProvider::Ollama);
    }

    #[test]
    fn default_config_judge_endpoint() {
        let config = Config::default();
        assert_eq!(config.judge.endpoint, "http://localhost:11434");
    }

    #[test]
    fn default_config_has_blocked_patterns() {
        let config = Config::default();
        assert!(!config.policy.blocked.is_empty());
        assert!(config.policy.blocked.contains(&".env".to_string()));
    }

    #[test]
    fn default_config_has_warn_patterns() {
        let config = Config::default();
        assert!(!config.policy.warn.is_empty());
        assert!(config.policy.warn.contains(&"Cargo.lock".to_string()));
    }

    #[test]
    fn default_config_team_disabled() {
        let config = Config::default();
        assert!(!config.team.enabled);
    }

    #[test]
    fn default_agents_config_is_safe() {
        let config = Config::default();
        assert!(config.agents.auto_detect);
        assert!(!config.agents.auto_attach);
        assert!(config.agents.preferred.contains(&"codex".to_string()));
    }

    #[test]
    fn default_config_limits_are_zero() {
        let config = Config::default();
        assert_eq!(config.policy.max_files_changed, 0);
        assert_eq!(config.policy.max_lines_changed, 0);
    }

    #[test]
    fn preset_solo_max_files_20() {
        let config = preset_config(&Preset::Solo);
        assert_eq!(config.policy.max_files_changed, 20);
    }

    #[test]
    fn preset_solo_judge_enabled() {
        let config = preset_config(&Preset::Solo);
        assert!(config.judge.enabled);
    }

    #[test]
    fn preset_team_max_files_10() {
        let config = preset_config(&Preset::Team);
        assert_eq!(config.policy.max_files_changed, 10);
    }

    #[test]
    fn preset_team_enables_sharing() {
        let config = preset_config(&Preset::Team);
        assert!(config.team.enabled);
        assert!(config.team.share_logs);
    }

    #[test]
    fn preset_ci_judge_disabled() {
        let config = preset_config(&Preset::Ci);
        assert!(!config.judge.enabled);
    }

    #[test]
    fn preset_ci_max_files_5() {
        let config = preset_config(&Preset::Ci);
        assert_eq!(config.policy.max_files_changed, 5);
    }

    #[test]
    fn parse_yaml_minimal() {
        let yaml = "version: 1";
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.version, 1);
        assert!(config.judge.enabled);
        assert_eq!(config.judge.model, "qwen3.5:2b");
    }

    #[test]
    fn parse_yaml_empty_uses_defaults() {
        let yaml = "{}";
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.version, 1);
        assert_eq!(config.judge.model, "qwen3.5:2b");
    }

    #[test]
    fn parse_yaml_custom_model() {
        let yaml = "version: 1
judge:
  enabled: true
  provider: openai
  model: gpt-4o
  endpoint: https://api.openai.com";
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.judge.model, "gpt-4o");
        assert_eq!(config.judge.provider, JudgeProvider::Openai);
    }

    #[test]
    fn parse_yaml_with_limits() {
        let yaml = "policy:
  max_files_changed: 15
  max_lines_changed: 500";
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.policy.max_files_changed, 15);
        assert_eq!(config.policy.max_lines_changed, 500);
    }

    #[test]
    fn config_yaml_roundtrip() {
        let config = preset_config(&Preset::Team);
        let yaml = serde_yaml::to_string(&config).unwrap();
        let parsed: Config = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed.version, config.version);
        assert_eq!(parsed.judge.model, config.judge.model);
        assert_eq!(
            parsed.policy.max_files_changed,
            config.policy.max_files_changed
        );
        assert_eq!(parsed.team.enabled, config.team.enabled);
        assert_eq!(parsed.agents.auto_attach, config.agents.auto_attach);
    }
}
