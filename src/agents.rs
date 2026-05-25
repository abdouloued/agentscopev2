use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use ulid::Ulid;

use crate::config::{self, Config};
use crate::git;
use crate::output::Printer;
use crate::session::{self, Session};

const MIN_ATTACH_CONFIDENCE: f32 = 0.45;
const HIGH_CONFIDENCE: f32 = 0.70;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentContext {
    pub agent: String,
    pub mission: Option<String>,
    pub source_path: Option<PathBuf>,
    pub timestamp: Option<String>,
    pub confidence: f32,
    pub notes: Vec<String>,
}

impl AgentContext {
    fn missing(agent: &str, notes: Vec<String>) -> Self {
        Self {
            agent: agent.to_string(),
            mission: None,
            source_path: None,
            timestamp: None,
            confidence: 0.0,
            notes,
        }
    }

    pub fn found(&self) -> bool {
        self.mission.is_some()
    }
}

pub fn supported_agents() -> Vec<&'static str> {
    vec![
        "claude-code",
        "codex",
        "opencode",
        "cursor",
        "gemini-cli",
        "copilot-cli",
    ]
}

pub async fn detect_command() -> Result<()> {
    let config = config::load_or_default();
    let contexts = detect_all(&config)?;
    print_context_table(&contexts);
    Ok(())
}

pub async fn doctor_command() -> Result<()> {
    let config = config::load_or_default();
    let contexts = detect_all(&config)?;
    println!("  Agent source health");
    println!("  Missing sources are normal when an agent is not installed or has no sessions yet.");
    println!();

    for context in &contexts {
        if context.found() {
            let source = context
                .source_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "unknown source".into());
            println!(
                "  {:<13} found      confidence {:.2}  {}",
                context.agent, context.confidence, source
            );
        } else {
            println!("  {:<13} missing", context.agent);
            for path in source_paths(&config, &context.agent) {
                println!("  {:<13}   checked {}", "", expand_path(&path).display());
            }
        }
    }

    println!();
    println!("  Repair options:");
    println!("  - Start the agent once so it creates local session history.");
    println!("  - Override paths in agentscope.yaml under agents.sources.<agent>.paths.");
    println!("  - Fall back to manual scope with: agentscope start \"your mission\"");
    Ok(())
}

pub async fn context_command(agent: String) -> Result<()> {
    let config = config::load_or_default();
    let context = select_context(&config, &agent)?;
    print_context_detail(&context);
    Ok(())
}

pub async fn attach_command(agent: String, apply: bool) -> Result<()> {
    let config = config::load_or_default();
    let context = select_context(&config, &agent)?;

    let Some(mission) = context.mission.clone() else {
        anyhow::bail!("No mission found for {}", context.agent);
    };

    if context.confidence < MIN_ATTACH_CONFIDENCE {
        anyhow::bail!(
            "Refusing to attach low-confidence mission for {} ({:.2})",
            context.agent,
            context.confidence
        );
    }

    if !apply {
        println!("  agent       {}", context.agent);
        println!("  confidence  {:.2}", context.confidence);
        println!("  mission     \"{}\"", mission);
        if let Some(path) = context.source_path.as_ref() {
            println!("  source      {}", path.display());
        }
        println!();
        println!("  dry run only - rerun with --apply to write .agentscope/session.json");
        return Ok(());
    }

    let session = session_from_context(&context, mission)?;
    session::save_session(&session)?;
    session::append_session_activity("agent_attach", &session)?;

    let p = Printer::new();
    p.success(&format!(
        "Attached {} mission from local context",
        context.agent
    ));
    p.session_one_liner(&session);
    Ok(())
}

pub async fn monitor_command(agent: String, auto_attach: bool) -> Result<()> {
    let config = config::load_or_default();
    let context = select_context(&config, &agent).ok();

    if let Some(context) = context.as_ref() {
        if context.found() {
            println!(
                "  agent context  {}  confidence {:.2}",
                context.agent, context.confidence
            );
            if let Some(mission) = context.mission.as_ref() {
                println!("  inferred       \"{}\"", mission);
            }
            if (auto_attach || config.agents.auto_attach) && context.confidence >= HIGH_CONFIDENCE {
                let mission = context.mission.clone().unwrap_or_default();
                let session = session_from_context(context, mission)?;
                session::save_session(&session)?;
                session::append_session_activity("agent_auto_attach", &session)?;
                println!("  attached       .agentscope/session.json");
            }
        } else {
            println!("  agent context  {} not found", context.agent);
        }
    }

    crate::tui::run_watch().await
}

pub async fn skills_command(action: crate::cli::IntegrationAction) -> Result<()> {
    integration_command("skill", action)
}

pub async fn plugins_command(action: crate::cli::IntegrationAction) -> Result<()> {
    integration_command("plugin", action)
}

pub async fn mcp_command() -> Result<()> {
    use std::io::{self, Read};
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    if input.trim().is_empty() {
        println!(
            "{}",
            serde_json::json!({
                "jsonrpc": "2.0",
                "result": {
                    "tools": [
                        "scope_status",
                        "scope_check",
                        "scope_start",
                        "agent_detect",
                        "agent_context",
                        "agent_attach"
                    ]
                }
            })
        );
        return Ok(());
    }

    let request: serde_json::Value = serde_json::from_str(&input)?;
    let id = request
        .get("id")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let method = request.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let result = match method {
        "agent_detect" => serde_json::to_value(detect_all(&config::load_or_default())?)?,
        "agent_context" => {
            let agent = request
                .pointer("/params/agent")
                .and_then(|v| v.as_str())
                .unwrap_or("auto");
            serde_json::to_value(select_context(&config::load_or_default(), agent)?)?
        }
        "scope_status" => match session::load_active_session() {
            Ok(session) => serde_json::to_value(session)?,
            Err(e) => serde_json::json!({ "error": e.to_string() }),
        },
        "scope_check" => {
            serde_json::json!({ "hint": "run agentscope check for terminal policy output" })
        }
        "scope_start" => serde_json::json!({ "hint": "run agentscope start \"mission\"" }),
        "agent_attach" => {
            serde_json::json!({ "hint": "run agentscope attach --agent auto --apply" })
        }
        _ => serde_json::json!({ "error": format!("unknown method: {}", method) }),
    };
    println!(
        "{}",
        serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result })
    );
    Ok(())
}

pub fn detect_all(config: &Config) -> Result<Vec<AgentContext>> {
    supported_agents()
        .into_iter()
        .filter(|agent| source_enabled(config, agent))
        .map(|agent| detect_agent(config, agent))
        .collect()
}

fn select_context(config: &Config, requested: &str) -> Result<AgentContext> {
    if requested == "auto" {
        let contexts = detect_all(config)?;
        if let Some(agent) = config
            .agents
            .preferred
            .iter()
            .filter_map(|preferred| contexts.iter().find(|ctx| ctx.agent == *preferred))
            .find(|ctx| ctx.found())
        {
            return Ok(agent.clone());
        }
        if let Some(found) = contexts.iter().find(|ctx| ctx.found()) {
            return Ok(found.clone());
        }
        return contexts
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No agent sources are enabled"));
    }

    let normalized = normalize_agent(requested)?;
    detect_agent(config, normalized)
}

fn detect_agent(config: &Config, agent: &str) -> Result<AgentContext> {
    let paths = source_paths(config, agent);
    let mut notes = Vec::new();
    let mut newest: Option<(PathBuf, std::time::SystemTime)> = None;

    for base in &paths {
        let expanded = expand_path(base);
        if !expanded.exists() {
            notes.push(format!("{} not found", expanded.display()));
            continue;
        }
        if expanded.is_file() {
            let modified = expanded
                .metadata()?
                .modified()
                .unwrap_or(std::time::UNIX_EPOCH);
            newest = newer(newest, expanded, modified);
        } else {
            for file in collect_files(&expanded)? {
                let modified = file.metadata()?.modified().unwrap_or(std::time::UNIX_EPOCH);
                newest = newer(newest, file, modified);
            }
        }
    }

    let Some((path, modified)) = newest else {
        return Ok(AgentContext::missing(agent, notes));
    };

    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("Could not read {}", path.display()))?;
    let mission = extract_mission(&contents);
    let confidence = mission
        .as_ref()
        .map(|m| confidence_for(agent, m, &path))
        .unwrap_or(0.0);
    let timestamp = modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| chrono::DateTime::<Utc>::from_timestamp(duration.as_secs() as i64, 0))
        .map(|dt| dt.to_rfc3339());

    let mut notes = notes;
    notes.push("newest source selected".into());

    Ok(AgentContext {
        agent: agent.to_string(),
        mission,
        source_path: Some(path),
        timestamp,
        confidence,
        notes,
    })
}

fn newer(
    current: Option<(PathBuf, std::time::SystemTime)>,
    path: PathBuf,
    modified: std::time::SystemTime,
) -> Option<(PathBuf, std::time::SystemTime)> {
    match current {
        Some((old_path, old_modified)) if old_modified >= modified => {
            Some((old_path, old_modified))
        }
        _ => Some((path, modified)),
    }
}

fn collect_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        for entry in std::fs::read_dir(&path)? {
            let entry = entry?;
            let entry_path = entry.path();
            if entry_path.is_dir() {
                stack.push(entry_path);
            } else if is_context_file(&entry_path) {
                files.push(entry_path);
            }
        }
    }
    Ok(files)
}

fn is_context_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("jsonl" | "json" | "txt" | "md")
    ) || path
        .file_name()
        .and_then(|f| f.to_str())
        .is_some_and(|n| n.contains("transcript") || n.contains("chat") || n.contains("rollout"))
}

fn extract_mission(contents: &str) -> Option<String> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(contents) {
        if let Some(text) = extract_json_text(&value) {
            let cleaned = clean_mission(text);
            if !cleaned.is_empty() {
                return Some(cleaned);
            }
        }
    }

    let mut candidate = None;
    for line in contents.lines().filter(|line| !line.trim().is_empty()) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(text) = extract_json_text(&value) {
                let cleaned = clean_mission(text);
                if !cleaned.is_empty() {
                    candidate = Some(cleaned);
                }
            }
        } else {
            let cleaned = clean_mission(line.trim().to_string());
            if !cleaned.is_empty() {
                candidate = Some(cleaned);
            }
        }
    }
    candidate
}

fn extract_json_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Array(items) => items.iter().find_map(extract_json_text),
        serde_json::Value::Object(map) => {
            if map
                .get("error")
                .and_then(|v| v.as_str())
                .is_some_and(|error| error.contains("authentication"))
            {
                return None;
            }

            if map
                .get("role")
                .and_then(|v| v.as_str())
                .is_some_and(|role| role != "user")
            {
                return None;
            }

            if map
                .get("type")
                .and_then(|v| v.as_str())
                .is_some_and(|kind| {
                    matches!(
                        kind,
                        "function_call"
                            | "function_call_output"
                            | "custom_tool_call"
                            | "custom_tool_call_output"
                            | "patch_apply_begin"
                            | "patch_apply_end"
                            | "reasoning"
                            | "token_count"
                            | "tool_result"
                            | "agent_message"
                    )
                })
            {
                return None;
            }

            for key in [
                "mission",
                "objective",
                "prompt",
                "lastPrompt",
                "message",
                "content",
                "text",
                "input",
            ] {
                if let Some(text) = map.get(key).and_then(extract_json_text) {
                    return Some(text);
                }
            }
            map.iter()
                .filter(|(key, _)| {
                    matches!(key.as_str(), "payload" | "goal" | "params" | "request")
                })
                .map(|(_, value)| value)
                .find_map(extract_json_text)
        }
        _ => None,
    }
}

fn clean_mission(text: String) -> String {
    let lines = text.lines().map(str::trim).collect::<Vec<_>>();
    if let Some(request) = request_block_mission(&lines) {
        return request;
    }

    lines
        .into_iter()
        .find(|line| is_mission_line(line))
        .unwrap_or("")
        .trim_matches('"')
        .to_string()
}

fn request_block_mission(lines: &[&str]) -> Option<String> {
    let marker_index = lines
        .iter()
        .position(|line| line.to_ascii_lowercase().contains("my request for"))?;

    lines
        .iter()
        .skip(marker_index + 1)
        .copied()
        .find(|line| is_mission_line(line))
        .map(|line| line.trim_matches('"').to_string())
}

fn is_mission_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    !line.is_empty()
        && !lower.contains("tool_result")
        && !lower.contains("assistant")
        && !lower.contains("authentication_failed")
        && !looks_like_context_header(&lower)
        && !looks_like_json_field_noise(&lower)
        && !looks_like_agent_command(line)
        && !looks_like_patch_marker(&lower)
        && !looks_like_diff_line(line)
        && !looks_like_timestamp(line)
        && !looks_like_file_path(line)
        && line.len() > 3
}

fn looks_like_context_header(lower: &str) -> bool {
    lower.starts_with("# in app browser")
        || lower.starts_with("## my request for")
        || lower.starts_with("- current url:")
        || lower.starts_with("- the user has")
}

fn looks_like_json_field_noise(lower: &str) -> bool {
    let trimmed = lower.trim_start_matches([' ', '\t', '"']);
    (lower.trim_start().starts_with('"') && trimmed.contains("\":"))
        || trimmed.starts_with("timestamp")
        || trimmed.starts_with("role")
        || trimmed.starts_with("type")
        || trimmed.starts_with("id")
        || trimmed.starts_with("model")
        || trimmed.starts_with("metadata")
        || trimmed.starts_with("created_at")
        || trimmed.starts_with("updated_at")
}

fn looks_like_agent_command(line: &str) -> bool {
    let first = line
        .trim()
        .trim_matches('"')
        .trim_end_matches(',')
        .split_whitespace()
        .next()
        .unwrap_or("");

    matches!(
        first,
        "login"
            | "logout"
            | "/model"
            | "/login"
            | "/logout"
            | "/help"
            | "/clear"
            | "/quit"
            | "/exit"
            | "/theme"
    )
}

fn looks_like_patch_marker(lower: &str) -> bool {
    lower.starts_with("*** begin patch")
        || lower.starts_with("*** end patch")
        || lower.starts_with("*** update file:")
        || lower.starts_with("*** add file:")
        || lower.starts_with("*** delete file:")
        || lower.starts_with("@@")
}

fn looks_like_diff_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("---")
        || trimmed.starts_with("+++")
        || trimmed
            .as_bytes()
            .first()
            .is_some_and(|first| matches!(first, b'+' | b'-'))
            && trimmed
                .as_bytes()
                .get(1)
                .is_some_and(|second| second.is_ascii_whitespace())
}

fn looks_like_timestamp(line: &str) -> bool {
    line.len() >= 20
        && line.as_bytes().get(4) == Some(&b'-')
        && line.as_bytes().get(7) == Some(&b'-')
        && line.contains('T')
}

fn looks_like_file_path(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    (line.starts_with('/') || line.starts_with("~/") || line.starts_with("./"))
        && [
            ".rs", ".ts", ".tsx", ".js", ".jsx", ".swift", ".py", ".json", ".yaml", ".yml",
        ]
        .iter()
        .any(|suffix| lower.ends_with(suffix))
}

fn confidence_for(agent: &str, mission: &str, source: &Path) -> f32 {
    let mut confidence: f32 = 0.50;
    if supported_agents().contains(&agent) {
        confidence += 0.10;
    }
    if source.exists() {
        confidence += 0.10;
    }
    if mission.split_whitespace().count() >= 3 {
        confidence += 0.15;
    }
    if mission.len() > 160 {
        confidence -= 0.10;
    }
    confidence.clamp(0.0, 0.95)
}

fn session_from_context(context: &AgentContext, mission: String) -> Result<Session> {
    let repo = git::open_repo()?;
    let baseline = git::capture_baseline(&repo)?;
    let repo_root = repo
        .workdir()
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_path_buf();
    Ok(Session {
        id: Ulid::new().to_string(),
        mission,
        agent: context.agent.clone(),
        git_baseline: baseline,
        started_at: Utc::now().to_rfc3339(),
        repo_root,
        mission_source: Some("agent-log".into()),
        mission_confidence: Some(context.confidence),
        detected_agent: Some(context.agent.clone()),
        source_path: context.source_path.clone(),
    })
}

fn print_context_table(contexts: &[AgentContext]) {
    for context in contexts {
        if let Some(mission) = context.mission.as_ref() {
            println!(
                "  {:<13} found      confidence {:.2}  \"{}\"",
                context.agent, context.confidence, mission
            );
        } else {
            println!("  {:<13} not found", context.agent);
        }
    }
}

fn print_context_detail(context: &AgentContext) {
    println!("  agent       {}", context.agent);
    if let Some(mission) = context.mission.as_ref() {
        println!("  mission     \"{}\"", mission);
    } else {
        println!("  mission     not found");
    }
    println!("  confidence  {:.2}", context.confidence);
    if let Some(path) = context.source_path.as_ref() {
        println!("  source      {}", path.display());
    }
    for note in &context.notes {
        println!("  note        {}", note);
    }
}

fn integration_command(kind: &str, action: crate::cli::IntegrationAction) -> Result<()> {
    match action {
        crate::cli::IntegrationAction::List { agent } => {
            for agent in matching_agents(&agent)? {
                println!("  {:<13} {} available", agent, kind);
            }
        }
        crate::cli::IntegrationAction::Install { agent } => {
            for agent in matching_agents(&agent)? {
                install_integration(kind, agent)?;
                println!("  installed {} for {}", kind, agent);
            }
        }
    }
    Ok(())
}

fn install_integration(kind: &str, agent: &str) -> Result<()> {
    let dir = format!(".agentscope/{}/{}", kind, agent);
    std::fs::create_dir_all(&dir)?;
    let file = Path::new(&dir).join("README.md");
    std::fs::write(
        file,
        format!(
            "# AgentScope {kind} for {agent}\n\nRun `agentscope status` before work and `agentscope check` before finishing.\n"
        ),
    )?;
    Ok(())
}

fn matching_agents(agent: &str) -> Result<Vec<&'static str>> {
    if agent == "all" {
        return Ok(supported_agents());
    }
    Ok(vec![normalize_agent(agent)?])
}

fn normalize_agent(agent: &str) -> Result<&'static str> {
    match agent {
        "claude" | "claude-code" => Ok("claude-code"),
        "codex" | "codex-cli" => Ok("codex"),
        "opencode" => Ok("opencode"),
        "cursor" => Ok("cursor"),
        "gemini" | "gemini-cli" => Ok("gemini-cli"),
        "copilot" | "copilot-cli" => Ok("copilot-cli"),
        other => anyhow::bail!("Unsupported agent: {}", other),
    }
}

fn source_enabled(config: &Config, agent: &str) -> bool {
    config
        .agents
        .sources
        .get(agent)
        .map(|source| source.enabled)
        .unwrap_or(true)
}

fn source_paths(config: &Config, agent: &str) -> Vec<String> {
    if let Some(source) = config.agents.sources.get(agent) {
        if !source.paths.is_empty() {
            return source.paths.clone();
        }
    }

    match agent {
        "claude-code" => vec!["~/.claude/projects".into()],
        "codex" => vec!["~/.codex/sessions".into()],
        "opencode" => vec!["~/.local/share/opencode/project".into()],
        "cursor" => vec!["~/.cursor/projects".into()],
        "gemini-cli" => vec!["~/.gemini/tmp".into()],
        "copilot-cli" => vec!["~/.copilot/session-state".into()],
        _ => Vec::new(),
    }
}

fn expand_path(path: &str) -> PathBuf {
    if path == "~" {
        return home_dir();
    }
    if let Some(stripped) = path.strip_prefix("~/") {
        return home_dir().join(stripped);
    }
    PathBuf::from(path)
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_latest_jsonl_message() {
        let contents = r#"{"message":"first task"}
{"message":"second task"}"#;
        assert_eq!(extract_mission(contents).unwrap(), "second task");
    }

    #[test]
    fn normalize_accepts_aliases() {
        assert_eq!(normalize_agent("gemini").unwrap(), "gemini-cli");
        assert_eq!(normalize_agent("copilot").unwrap(), "copilot-cli");
    }

    #[test]
    fn extracts_nested_codex_user_message() {
        let contents = r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"/goal follow the instructions in GOAL.md"}]}}"#;
        assert_eq!(
            extract_mission(contents).unwrap(),
            "/goal follow the instructions in GOAL.md"
        );
    }

    #[test]
    fn ignores_authentication_error_messages() {
        let contents = r#"{"type":"assistant","error":"authentication_failed","message":{"content":[{"type":"text","text":"Your organization does not have access to Claude. Please login again or contact your administrator."}]}}"#;
        assert!(extract_mission(contents).is_none());
    }

    #[test]
    fn ignores_timestamps_and_paths_as_missions() {
        assert!(extract_mission(r#"{"timestamp":"2026-05-25T00:30:08.491Z"}"#).is_none());
        assert!(extract_mission(
            r#"{"path":"/tmp/project/src/lib.rs","content":"/tmp/project/src/lib.rs"}"#
        )
        .is_none());
    }

    #[test]
    fn ignores_patch_markers_as_missions() {
        assert!(extract_mission(r#"{"message":"*** Begin Patch"}"#).is_none());
        assert!(extract_mission(r#"{"message":"*** Update File: src/lib.rs"}"#).is_none());
    }

    #[test]
    fn ignores_tool_patch_input_and_keeps_latest_user_prompt() {
        let contents = r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"implement all agent monitoring"}]}}
{"type":"response_item","payload":{"type":"custom_tool_call","input":"*** Begin Patch\n*** Update File: src/agents.rs\n-    old code"}}"#;
        assert_eq!(
            extract_mission(contents).unwrap(),
            "implement all agent monitoring"
        );
    }

    #[test]
    fn extracts_request_from_codex_app_context_block() {
        let contents = r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"\n# In app browser:\n- The user has the in-app browser open.\n- Current URL: file:///tmp/index.html\n\n## My request for Codex:\nimplement all\n"}]}}"#;
        assert_eq!(extract_mission(contents).unwrap(), "implement all");
    }

    #[test]
    fn later_non_mission_text_does_not_erase_user_prompt() {
        let contents = r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"implement all"}]}}
{"message":"*** Begin Patch"}"#;
        assert_eq!(extract_mission(contents).unwrap(), "implement all");
    }

    #[test]
    fn ignores_pretty_json_metadata_lines() {
        assert!(extract_mission(
            r#"{
  "timestamp": "2026-05-11T21:05:47.333Z",
  "type": "metadata"
}"#
        )
        .is_none());
    }

    #[test]
    fn ignores_agent_slash_commands_in_pretty_json_logs() {
        assert!(extract_mission(
            r#"{
  "message": "/model",
  "timestamp": "2026-05-11T21:05:47.333Z"
}"#
        )
        .is_none());
        assert!(extract_mission(r#"{"message":"login"}"#).is_none());
    }
}
