use anyhow::Result;
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

#[derive(Debug, Clone, PartialEq)]
pub struct ActiveMission {
    pub agent: String,
    pub mission: String,
    pub confidence: f32,
    pub source_path: Option<PathBuf>,
    pub timestamp: Option<String>,
    pub age_seconds: Option<i64>,
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

pub fn active_missions(config: &Config) -> Result<(Vec<ActiveMission>, Vec<AgentContext>)> {
    let contexts = detect_all(config)?;
    Ok(active_missions_from_contexts(
        contexts,
        config.agents.active_window_hours,
        Utc::now(),
    ))
}

pub fn active_missions_from_contexts(
    contexts: Vec<AgentContext>,
    active_window_hours: u64,
    now: chrono::DateTime<Utc>,
) -> (Vec<ActiveMission>, Vec<AgentContext>) {
    let window_secs = (active_window_hours as i64).saturating_mul(60 * 60);
    let mut active = Vec::new();
    let mut ignored = Vec::new();

    for context in contexts {
        let Some(mission) = context.mission.clone() else {
            ignored.push(context);
            continue;
        };
        let age_seconds = context.timestamp.as_deref().and_then(|timestamp| {
            chrono::DateTime::parse_from_rfc3339(timestamp)
                .ok()
                .map(|dt| {
                    now.signed_duration_since(dt.with_timezone(&Utc))
                        .num_seconds()
                })
        });
        let fresh = age_seconds
            .map(|age| age >= 0 && age <= window_secs)
            .unwrap_or(true);
        if context.confidence >= HIGH_CONFIDENCE && fresh {
            active.push(ActiveMission {
                agent: context.agent.clone(),
                mission,
                confidence: context.confidence,
                source_path: context.source_path.clone(),
                timestamp: context.timestamp.clone(),
                age_seconds,
            });
        } else {
            ignored.push(context);
        }
    }

    active.sort_by(|a, b| a.agent.cmp(&b.agent));
    (active, ignored)
}

pub fn supported_agents() -> Vec<&'static str> {
    vec![
        "claude-code",
        "codex",
        "codex-app",
        "opencode",
        "openclaw",
        "hermes",
        "cursor",
        "gemini-cli",
        "antigravity",
        "copilot-cli",
    ]
}

pub(crate) fn launch_command(agent: &str, model: &str) -> Option<String> {
    let target = match normalize_agent(agent).ok()? {
        "claude-code" => "claude",
        "codex" => "codex",
        "opencode" => "opencode",
        "openclaw" => "openclaw",
        "hermes" => "hermes",
        _ => return None,
    };
    Some(format!("ollama launch {target} --model {model}"))
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
            if let Some(command) = launch_command(&context.agent, "qwen3.5") {
                println!("  {:<13}   launch  {}", "", command);
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

    // If a real TTY is available, open the full TUI dashboard
    if crate::tui::is_tty() {
        return crate::tui::run_watch().await;
    }

    // Non-TTY fallback: plain-text polling monitor
    println!();
    println!("  monitoring  (plain-text mode — no TTY detected)");
    println!("  press ctrl-c to stop");
    println!();

    let mut last_hash: Option<String> = None;
    loop {
        let repo = crate::git::open_repo();
        if let Ok(repo) = repo {
            let wt = crate::git::working_tree_diff(&repo);
            if let Ok(wt) = wt {
                let hash = format!("{:?}", wt.files.len());
                if Some(&hash) != last_hash.as_ref() {
                    last_hash = Some(hash);
                    let session = crate::session::load_active_session().ok();
                    let mission = session
                        .as_ref()
                        .map(|s| s.mission.as_str())
                        .unwrap_or("<no active mission>");
                    let policy = crate::policy::PolicyEngine::from_config(&config.policy)
                        .unwrap_or_else(|_| {
                            crate::policy::PolicyEngine::from_config(
                                &crate::config::PolicyConfig::default(),
                            )
                            .unwrap()
                        });
                    let annotated = policy.annotate(&wt.files, mission);
                    let expected = annotated
                        .iter()
                        .filter(|f| matches!(f.verdict, crate::policy::FileVerdict::InScope))
                        .count();
                    let suspicious = annotated
                        .iter()
                        .filter(|f| matches!(f.verdict, crate::policy::FileVerdict::Unasked))
                        .count();
                    let blocked = annotated
                        .iter()
                        .filter(|f| matches!(f.verdict, crate::policy::FileVerdict::Blocked { .. }))
                        .count();
                    let ts = chrono::Utc::now().format("%H:%M:%S");
                    println!(
                        "  [{}]  {} expected  {} suspicious  {} blocked  |  mission: {}",
                        ts, expected, suspicious, blocked, mission
                    );
                }
            }
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    }
}

pub async fn skills_command(action: crate::cli::IntegrationAction) -> Result<()> {
    integration_command("skill", action)
}

pub async fn plugins_command(action: crate::cli::IntegrationAction) -> Result<()> {
    integration_command("plugin", action)
}

pub async fn mcp_command() -> Result<()> {
    use std::io::{self, BufRead, Write};

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        let request: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let method = request.get("method").and_then(|v| v.as_str()).unwrap_or("");
        // Notifications have no "id" — skip silently
        let id = match request.get("id").cloned() {
            Some(id) => id,
            None => continue,
        };

        let response = match method {
            "initialize" => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "agentscope", "version": "0.1.0" }
                }
            }),

            "tools/list" => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "tools": [
                        {
                            "name": "scope_status",
                            "description": "Get the active AgentScope session — mission, agent name, session ID, and start time.",
                            "inputSchema": { "type": "object", "properties": {} }
                        },
                        {
                            "name": "scope_check",
                            "description": "Run a scope compliance check on the current git working tree. Returns a summary of EXPECTED / SUSPICIOUS / BLOCKED file verdicts.",
                            "inputSchema": { "type": "object", "properties": {} }
                        },
                        {
                            "name": "scope_start",
                            "description": "Start a new AgentScope monitoring session with a mission description.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "mission": { "type": "string", "description": "The task or mission description." },
                                    "agent": { "type": "string", "description": "Agent hint: codex, claude, auto, etc." }
                                },
                                "required": ["mission"]
                            }
                        },
                        {
                            "name": "agent_detect",
                            "description": "Detect which AI coding agents are currently active in this repository.",
                            "inputSchema": { "type": "object", "properties": {} }
                        },
                        {
                            "name": "agent_context",
                            "description": "Get the latest mission context for a specific agent.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "agent": { "type": "string", "description": "Agent name or 'auto'." }
                                }
                            }
                        }
                    ]
                }
            }),

            "tools/call" => {
                let tool = request
                    .pointer("/params/name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or(serde_json::json!({}));

                let text = match tool {
                    "scope_status" => match session::load_active_session() {
                        Ok(s) => format!(
                            "Session: {}\nAgent:   {}\nMission: {}\nStarted: {}",
                            &s.id[..12.min(s.id.len())],
                            s.agent,
                            s.mission,
                            s.started_at
                        ),
                        Err(_) => "No active session.\nRun: agentscope start \"<your mission>\""
                            .to_string(),
                    },

                    "scope_check" => match session::load_active_session() {
                        Ok(s) => {
                            let cfg = config::load_or_default();
                            match git::open_repo().and_then(|repo| git::working_tree_diff(&repo)) {
                                Ok(wt) => {
                                    let policy =
                                        match crate::policy::PolicyEngine::from_config(&cfg.policy)
                                        {
                                            Ok(p) => p,
                                            Err(e) => return Err(e),
                                        };
                                    let annotated = policy.annotate(&wt.files, &s.mission);
                                    let expected = annotated
                                        .iter()
                                        .filter(|f| f.verdict.is_accepted())
                                        .count();
                                    let suspicious = annotated
                                        .iter()
                                        .filter(|f| {
                                            f.verdict == crate::policy::FileVerdict::Unasked
                                        })
                                        .count();
                                    let blocked =
                                        annotated.iter().filter(|f| f.verdict.is_blocked()).count();
                                    format!(
                                            "Mission: {}\nFiles: {} total  |  {} expected  |  {} suspicious  |  {} blocked",
                                            s.mission,
                                            annotated.len(),
                                            expected,
                                            suspicious,
                                            blocked
                                        )
                                }
                                Err(e) => format!("Git diff error: {}", e),
                            }
                        }
                        Err(_) => {
                            "No active session. Run: agentscope start \"<mission>\"".to_string()
                        }
                    },

                    "scope_start" => {
                        let mission = args
                            .get("mission")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .trim()
                            .to_string();
                        let agent = args.get("agent").and_then(|v| v.as_str()).unwrap_or("auto");
                        if mission.is_empty() {
                            "Error: 'mission' is required.".to_string()
                        } else {
                            format!(
                                "Run in your terminal:\n  agentscope start \"{}\" --agent {}",
                                mission, agent
                            )
                        }
                    }

                    "agent_detect" => match detect_all(&config::load_or_default()) {
                        Ok(contexts) => {
                            let found: Vec<_> = contexts.iter().filter(|c| c.found()).collect();
                            if found.is_empty() {
                                "No active agent sessions detected.".to_string()
                            } else {
                                found
                                    .iter()
                                    .map(|c| {
                                        format!(
                                            "{}  confidence={:.0}%  mission={}",
                                            c.agent,
                                            c.confidence * 100.0,
                                            c.mission.as_deref().unwrap_or("(none)")
                                        )
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            }
                        }
                        Err(e) => format!("Detection error: {}", e),
                    },

                    "agent_context" => {
                        let agent = args.get("agent").and_then(|v| v.as_str()).unwrap_or("auto");
                        match select_context(&config::load_or_default(), agent) {
                            Ok(c) => format!(
                                "Agent:      {}\nMission:    {}\nConfidence: {:.0}%\nTimestamp:  {}",
                                c.agent,
                                c.mission.as_deref().unwrap_or("(none)"),
                                c.confidence * 100.0,
                                c.timestamp.as_deref().unwrap_or("(unknown)")
                            ),
                            Err(e) => format!("Error: {}", e),
                        }
                    }

                    _ => format!("Unknown tool: {}", tool),
                };

                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "content": [{ "type": "text", "text": text }] }
                })
            }

            _ => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("Method not found: {}", method) }
            }),
        };

        let resp = serde_json::to_string(&response)?;
        writeln!(out, "{}", resp)?;
        out.flush()?;
    }

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
    let mut candidates: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();

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
            candidates.push((expanded, modified));
        } else {
            for file in collect_files(agent, &expanded)? {
                if let Ok(metadata) = file.metadata() {
                    let modified = metadata.modified().unwrap_or(std::time::UNIX_EPOCH);
                    candidates.push((file, modified));
                }
            }
        }
    }

    candidates.sort_by(|(_, a), (_, b)| b.cmp(a));
    let had_candidates = !candidates.is_empty();

    for (path, modified) in candidates {
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let mission = extract_mission(&contents);
        let Some(mission_text) = mission else {
            continue;
        };
        let confidence = confidence_for(agent, &mission_text, &path);
        let timestamp = modified
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|duration| {
                chrono::DateTime::<Utc>::from_timestamp(duration.as_secs() as i64, 0)
            })
            .map(|dt| dt.to_rfc3339());

        notes.push("newest mission-bearing source selected".into());

        return Ok(AgentContext {
            agent: agent.to_string(),
            mission: Some(mission_text),
            source_path: Some(path),
            timestamp,
            confidence,
            notes,
        });
    }

    if !had_candidates {
        return Ok(AgentContext::missing(agent, notes));
    }

    notes.push("sources found but no mission text extracted".into());
    Ok(AgentContext::missing(agent, notes))
}

fn collect_files(agent: &str, root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&path) else {
            continue;
        };
        for entry in entries {
            let entry = entry?;
            let entry_path = entry.path();
            if entry_path.is_dir() {
                stack.push(entry_path);
            } else if is_context_file_for_agent(agent, &entry_path) {
                files.push(entry_path);
            }
        }
    }
    Ok(files)
}

fn is_context_file_for_agent(agent: &str, path: &Path) -> bool {
    let path_text = path.to_string_lossy();
    if agent == "copilot-cli" {
        return path
            .file_name()
            .and_then(|f| f.to_str())
            .is_some_and(|name| name == "events.jsonl")
            || path_text.contains("GitHub.copilot-chat/transcripts") && is_context_file(path)
            || path_text.contains(".copilot/session-state")
                && path
                    .file_name()
                    .and_then(|f| f.to_str())
                    .is_some_and(|name| name == "events.jsonl");
    }

    if agent == "openclaw" {
        return path_text.contains(".openclaw/agents")
            || path_text.contains(".openclaw/sessions")
            || is_context_file(path);
    }

    if agent == "hermes" {
        return path_text.contains(".hermes/agents")
            || path_text.contains(".hermes/sessions")
            || is_context_file(path);
    }

    if agent == "antigravity" {
        return (path_text.contains(".gemini/antigravity")
            || path_text.contains("Antigravity")
            || path_text.contains(".antigravity"))
            && is_context_file(path);
    }

    is_context_file(path)
}

fn is_context_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("jsonl" | "json" | "txt" | "md" | "yaml" | "yml")
    ) || path
        .file_name()
        .and_then(|f| f.to_str())
        .is_some_and(|n| n.contains("transcript") || n.contains("chat") || n.contains("rollout"))
}

pub(crate) fn extract_mission(contents: &str) -> Option<String> {
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
                "transformedContent",
                "text",
                "input",
            ] {
                if let Some(text) = map.get(key).and_then(extract_json_text) {
                    return Some(text);
                }
            }
            map.iter()
                .filter(|(key, _)| {
                    matches!(
                        key.as_str(),
                        "payload" | "data" | "goal" | "params" | "request"
                    )
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
        && !looks_like_json_blob(line)
        && !looks_like_agent_command(line)
        && !looks_like_patch_marker(&lower)
        && !looks_like_diff_line(line)
        && !looks_like_timestamp(line)
        && !looks_like_file_path(line)
        && line.len() > 3
}

fn looks_like_json_blob(line: &str) -> bool {
    let trimmed = line.trim_start();
    (trimmed.starts_with('{') && trimmed.contains("\":"))
        || (trimmed.starts_with('[') && trimmed.contains('{'))
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
        || trimmed.starts_with("summary_count")
        || trimmed.starts_with("session_id")
        || trimmed.starts_with("workspace")
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
            | "/ask"
            | "/status"
            | "/diff"
            | "/judge"
            | "/judge-provider"
            | "/judge-model"
            | "/ollama-model"
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
    (lower.starts_with('<') && lower.ends_with('>'))
        || lower.starts_with('|')
        || lower.starts_with("*** begin patch")
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
    // Also write agent-native config files in the locations each agent reads
    if kind == "skill" {
        install_skill_native(agent)?;
    } else {
        install_plugin_native(agent)?;
    }

    // Always write the .agentscope/{kind}/{agent}/README.md record
    let dir = format!(".agentscope/{}/{}", kind, agent);
    std::fs::create_dir_all(&dir)?;
    let file = Path::new(&dir).join("README.md");
    std::fs::write(file, integration_content(kind, agent))?;
    Ok(())
}

fn integration_content(kind: &str, agent: &str) -> String {
    let header = match kind {
        "skill" => format!(
            "# AgentScope skill · {agent}\n\n\
             This file instructs `{agent}` to use AgentScope as a scope firewall \
             before and after making changes.\n"
        ),
        _ => format!(
            "# AgentScope plugin · {agent}\n\n\
             AgentScope is a scope firewall and audit cockpit for AI coding agents.\n\
             It checks whether your Git changes match the active mission.\n"
        ),
    };
    format!(
        "{header}\n\
         ## When to run AgentScope\n\n\
         | Trigger | Command |\n\
         |---------|--------|\n\
         | Before starting work | `agentscope status` |\n\
         | While working | `agentscope watch` (live TUI cockpit) |\n\
         | Before finishing | `agentscope check` |\n\
         | Before committing | `agentscope diff --problems` |\n\n\
         ## Quick reference\n\n\
         ```\n\
         agentscope init                          # one-time repo setup\n\
         agentscope start \"your mission\"          # record what you're doing\n\
         agentscope watch                         # live cockpit (1=review 2=chat 3=dash 4=sessions 5=live)\n\
         agentscope check                         # policy check + scope audit\n\
         agentscope check --json                  # machine-readable output\n\
         agentscope judge                         # ask the LLM judge\n\
         agentscope diff --problems               # show suspicious/blocked files only\n\
         agentscope attach --agent auto --apply   # infer mission from this agent's logs\n\
         ```\n\n\
         ## Status labels\n\n\
         | Badge | Meaning |\n\
         |-------|---------|\n\
         | `EXPECTED` | File matches the active mission scope |\n\
         | `SUSPICIOUS` | Changed but no mission rule matches |\n\
         | `BLOCKED` | Matched a blocked policy path — hard stop |\n\
         | `IGNORED` | Clean, stale, or explicitly excluded |\n\n\
         ## TUI keyboard shortcuts\n\n\
         | Key | Action |\n\
         |-----|--------|\n\
         | `1`–`5` | Switch mode (Review/Chat/Dashboard/Sessions/Live) |\n\
         | `Enter` | Open diff overlay for selected file |\n\
         | `j` | Run judge on selected file |\n\
         | `a` / `b` | Allow / block selected file |\n\
         | `t` | Cycle themes (agentscope/codex/claude/openclaw/high-contrast) |\n\
         | `?` | Help overlay |\n\
         | `q` | Quit |\n\n\
         ## Judge providers\n\n\
         AgentScope supports Ollama (local/private), Claude, OpenAI, Gemini, and OpenRouter.\n\n\
         ```\n\
         agentscope config set judge.provider ollama      # local, private\n\
         agentscope config set judge.provider claude      # requires ANTHROPIC_API_KEY\n\
         agentscope config set judge.provider openai      # requires OPENAI_API_KEY\n\
         agentscope config set judge.provider gemini      # requires GEMINI_API_KEY\n\
         agentscope config set judge.provider openrouter  # requires OPENROUTER_API_KEY\n\
         ```\n\n\
         ## Policy config (`agentscope.yaml`)\n\n\
         ```yaml\n\
         policy:\n\
           blocked:\n\
             - \".env\"\n\
             - \"**/.env.*\"\n\
             - \"**/secrets/**\"\n\
             - \"**/*.pem\"\n\
             - \"**/*.key\"\n\
           warn:\n\
             - \"package-lock.json\"\n\
             - \"yarn.lock\"\n\
             - \"Cargo.lock\"\n\
           max_files_changed: 20\n\
           max_lines_changed: 800\n\
         ```\n\n\
         Blocked patterns are enforced deterministically — no model can override them.\n\n\
         ## More info\n\n\
         Run `agentscope --help` or visit https://github.com/abdouloued/agentscopev2\n"
    )
}

/// Write the instruction file into the location the agent natively reads.
fn install_skill_native(agent: &str) -> Result<()> {
    let content = integration_content("skill", agent);
    match agent {
        "claude-code" => {
            // Claude Code reads CLAUDE.md at repo root
            let path = Path::new("CLAUDE.md");
            merge_or_write(path, "## AgentScope", &content)?;
        }
        "codex" => {
            // Codex CLI reads AGENTS.md at repo root
            std::fs::create_dir_all(".codex")?;
            let path = Path::new("AGENTS.md");
            merge_or_write(path, "## AgentScope", &content)?;
        }
        "cursor" => {
            std::fs::create_dir_all(".cursor/rules")?;
            std::fs::write(".cursor/rules/agentscope.md", &content)?;
        }
        "openclaw" | "hermes" | "codex-app" | "opencode" | "gemini-cli" | "antigravity"
        | "copilot-cli" => {
            // Write into .agentscope/skill/{agent}/instructions.md — agents can pick it up
        }
        _ => {}
    }
    Ok(())
}

/// Write plugin file into agent-native locations (non-instruction assets).
fn install_plugin_native(agent: &str) -> Result<()> {
    let content = integration_content("plugin", agent);
    match agent {
        "claude-code" => {
            // Claude Code — CLAUDE.md (project context)
            let path = Path::new("CLAUDE.md");
            merge_or_write(path, "## AgentScope", &content)?;
            // Also register with Claude Code's plugin system
            if let Err(e) = register_claude_code_plugin() {
                eprintln!("  note: could not register Claude Code plugin: {e}");
                eprintln!("  tip:  manually add agentscopev2 marketplace in Claude Code /plugins settings");
            }
        }
        "codex" => {
            // Codex — AGENTS.md
            let path = Path::new("AGENTS.md");
            merge_or_write(path, "## AgentScope", &content)?;
        }
        "cursor" => {
            std::fs::create_dir_all(".cursor/rules")?;
            std::fs::write(".cursor/rules/agentscope.md", &content)?;
        }
        "copilot-cli" => {
            std::fs::create_dir_all(".github")?;
            std::fs::write(".github/copilot-instructions.md", &content)?;
        }
        _ => {
            // Generic: write into the .agentscope record dir only
        }
    }
    Ok(())
}

/// Register AgentScope with Claude Code's plugin system.
/// Writes plugin files to the Claude cache, registers the marketplace,
/// updates installed_plugins.json, and enables the plugin in settings.json.
fn register_claude_code_plugin() -> Result<()> {
    use serde_json::Value;

    let claude_base = std::env::var("CLAUDE_CONFIG_DIR").unwrap_or_else(|_| "~/.claude".into());
    let claude_dir = expand_path(&claude_base);
    let plugins_dir = claude_dir.join("plugins");

    let mkt_dir = plugins_dir.join("marketplaces").join("agentscopev2");
    let cache_dir = plugins_dir
        .join("cache")
        .join("agentscopev2")
        .join("agentscope")
        .join("1.0.0");

    const PLUGIN_JSON: &str = r#"{"name":"agentscope","version":"1.0.0","description":"AgentScope scope firewall for AI coding agents.","repository":"https://github.com/abdouloued/agentscopev2","license":"MIT+Commons-Clause","skills":"./skills/","mcpServers":"./.mcp.json","keywords":["scope","policy","git","ai-agent","firewall","audit","mission","agentscope"]}"#;
    const MCP_JSON: &str =
        r#"{"mcpServers":{"agentscope":{"command":"agentscope","args":["mcp"],"env":{}}}}"#;
    const CLAUDE_MD: &str = include_str!("../plugins/agentscope/CLAUDE.md");
    const SCOPE_GUARD: &str = include_str!("../plugins/agentscope/skills/scope-guard/SKILL.md");
    const SCOPE_CHECK: &str = include_str!("../plugins/agentscope/skills/scope-check/SKILL.md");

    // ── 1. Write plugin files to cache ──────────────────────────────────────
    for dir in [
        cache_dir.join(".claude-plugin"),
        cache_dir.join("skills").join("scope-guard"),
        cache_dir.join("skills").join("scope-check"),
    ] {
        std::fs::create_dir_all(&dir)?;
    }
    std::fs::write(
        cache_dir.join(".claude-plugin").join("plugin.json"),
        PLUGIN_JSON,
    )?;
    std::fs::write(cache_dir.join(".mcp.json"), MCP_JSON)?;
    std::fs::write(cache_dir.join("CLAUDE.md"), CLAUDE_MD)?;
    std::fs::write(
        cache_dir
            .join("skills")
            .join("scope-guard")
            .join("SKILL.md"),
        SCOPE_GUARD,
    )?;
    std::fs::write(
        cache_dir
            .join("skills")
            .join("scope-check")
            .join("SKILL.md"),
        SCOPE_CHECK,
    )?;

    // ── 2. Write marketplace directory (mirrors openai-codex structure) ─────
    {
        let mkt_plugin_dir = mkt_dir.join("plugins").join("agentscope");
        for dir in [
            mkt_dir.join(".claude-plugin"),
            mkt_plugin_dir.join(".claude-plugin"),
            mkt_plugin_dir.join("skills").join("scope-guard"),
            mkt_plugin_dir.join("skills").join("scope-check"),
        ] {
            std::fs::create_dir_all(&dir)?;
        }
        // marketplace index — must match Claude Code's expected format
        std::fs::write(
            mkt_dir.join(".claude-plugin").join("marketplace.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "name": "agentscopev2",
                "owner": { "name": "AgentScope" },
                "metadata": {
                    "description": "AgentScope scope firewall plugin — monitors Git changes against your mission.",
                    "version": "1.0.0"
                },
                "plugins": [{
                    "name": "agentscope",
                    "description": "Scope firewall for AI coding agents: EXPECTED/SUSPICIOUS/BLOCKED verdicts, LLM judge, live TUI.",
                    "version": "1.0.0",
                    "author": { "name": "AgentScope" },
                    "source": "./plugins/agentscope"
                }]
            }))?,
        )?;
        std::fs::write(
            mkt_plugin_dir.join(".claude-plugin").join("plugin.json"),
            PLUGIN_JSON,
        )?;
        std::fs::write(mkt_plugin_dir.join(".mcp.json"), MCP_JSON)?;
        std::fs::write(mkt_plugin_dir.join("CLAUDE.md"), CLAUDE_MD)?;
        std::fs::write(
            mkt_plugin_dir
                .join("skills")
                .join("scope-guard")
                .join("SKILL.md"),
            SCOPE_GUARD,
        )?;
        std::fs::write(
            mkt_plugin_dir
                .join("skills")
                .join("scope-check")
                .join("SKILL.md"),
            SCOPE_CHECK,
        )?;
    }

    // ── 3. Register marketplace in known_marketplaces.json ─────────────────
    let km_path = plugins_dir.join("known_marketplaces.json");
    let mut km: Value = if km_path.exists() {
        serde_json::from_str(&std::fs::read_to_string(&km_path)?).unwrap_or(serde_json::json!({}))
    } else {
        serde_json::json!({})
    };
    // Always overwrite — use local source so Claude Code doesn't try to fetch from GitHub
    km["agentscopev2"] = serde_json::json!({
        "source": { "source": "local", "path": mkt_dir.display().to_string() },
        "installLocation": mkt_dir.display().to_string(),
        "lastUpdated": Utc::now().to_rfc3339()
    });
    std::fs::write(&km_path, serde_json::to_string_pretty(&km)?)?;

    // ── 4. Add entry to installed_plugins.json ──────────────────────────────
    let ip_path = plugins_dir.join("installed_plugins.json");
    let mut ip: Value = if ip_path.exists() {
        serde_json::from_str(&std::fs::read_to_string(&ip_path)?)
            .unwrap_or(serde_json::json!({"version":2,"plugins":{}}))
    } else {
        serde_json::json!({"version":2,"plugins":{}})
    };
    let now = Utc::now().to_rfc3339();
    ip["plugins"]["agentscope@agentscopev2"] = serde_json::json!([{
        "scope": "user",
        "installPath": cache_dir.display().to_string(),
        "version": "1.0.0",
        "installedAt": now,
        "lastUpdated": now
    }]);
    std::fs::write(&ip_path, serde_json::to_string_pretty(&ip)?)?;

    // ── 5. Enable plugin in settings.json ───────────────────────────────────
    let settings_path = claude_dir.join("settings.json");
    if settings_path.exists() {
        let raw = std::fs::read_to_string(&settings_path)?;
        if let Ok(mut settings) = serde_json::from_str::<Value>(&raw) {
            if let Some(obj) = settings.as_object_mut() {
                let enabled = obj.entry("enabledPlugins").or_insert(serde_json::json!({}));
                enabled["agentscope@agentscopev2"] = serde_json::json!(true);
            }
            std::fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)?;
        }
    }

    Ok(())
}

/// Append an AgentScope section to an existing file, or create it if it doesn't exist.
/// Idempotent: if the section marker already exists, overwrites only that section.
fn merge_or_write(path: &Path, section_marker: &str, content: &str) -> Result<()> {
    if path.exists() {
        let existing = std::fs::read_to_string(path)?;
        if existing.contains(section_marker) {
            // Already present — overwrite the section
            let before = existing
                .find(section_marker)
                .map(|i| &existing[..i])
                .unwrap_or("");
            std::fs::write(path, format!("{before}{content}"))?;
        } else {
            // Append
            let mut f = std::fs::OpenOptions::new().append(true).open(path)?;
            use std::io::Write;
            writeln!(f, "\n---\n\n{content}")?;
        }
    } else {
        std::fs::write(path, content)?;
    }
    Ok(())
}

fn matching_agents(agent: &str) -> Result<Vec<&'static str>> {
    if agent == "all" {
        return Ok(supported_agents());
    }
    Ok(vec![normalize_agent(agent)?])
}

pub(crate) fn normalize_agent(agent: &str) -> Result<&'static str> {
    match agent {
        "claude" | "claude-code" => Ok("claude-code"),
        "codex" | "codex-cli" => Ok("codex"),
        "codex-app" => Ok("codex-app"),
        "opencode" => Ok("opencode"),
        "openclaw" => Ok("openclaw"),
        "hermes" | "hermes-agent" => Ok("hermes"),
        "cursor" => Ok("cursor"),
        "gemini" | "gemini-cli" => Ok("gemini-cli"),
        "antigravity" | "antigravity-cli" | "antigravity-ide" => Ok("antigravity"),
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

pub(crate) fn source_paths(config: &Config, agent: &str) -> Vec<String> {
    if let Some(source) = config.agents.sources.get(agent) {
        if !source.paths.is_empty() {
            return source.paths.clone();
        }
    }

    match agent {
        "claude-code" => env_or_default("CLAUDE_CONFIG_DIR", "~/.claude")
            .into_iter()
            .map(|base| format!("{base}/projects"))
            .collect(),
        "codex" => env_or_default("CODEX_HOME", "~/.codex")
            .into_iter()
            .map(|base| format!("{base}/sessions"))
            .collect(),
        "codex-app" => env_or_default("CODEX_APP_HOME", "~/Library/Application Support/Codex")
            .into_iter()
            .flat_map(|base| [format!("{base}/sessions"), base])
            .collect(),
        "opencode" => env_or_default("OPENCODE_DATA_DIR", "~/.local/share/opencode")
            .into_iter()
            .map(|base| format!("{base}/project"))
            .collect(),
        "openclaw" => env_or_default("OPENCLAW_HOME", "~/.openclaw")
            .into_iter()
            .flat_map(|base| [format!("{base}/agents"), format!("{base}/sessions"), base])
            .collect(),
        "hermes" => env_or_default("HERMES_HOME", "~/.hermes")
            .into_iter()
            .flat_map(|base| [format!("{base}/agents"), format!("{base}/sessions"), base])
            .collect(),
        "cursor" => vec!["~/.cursor/projects".into()],
        "gemini-cli" => vec!["~/.gemini/tmp".into()],
        "antigravity" => vec![
            "~/.gemini/antigravity-cli".into(),
            "~/.gemini/antigravity".into(),
            "~/.gemini/antigravity-ide".into(),
            "~/.antigravity".into(),
            "~/Library/Application Support/Antigravity".into(),
            "~/Library/Application Support/Antigravity IDE/User/globalStorage".into(),
        ],
        "copilot-cli" => {
            let mut paths: Vec<String> = env_or_default("COPILOT_HOME", "~/.copilot")
                .into_iter()
                .map(|base| format!("{base}/session-state"))
                .collect();
            paths.extend([
                "~/Library/Application Support/Code/User/workspaceStorage".into(),
                "~/Library/Application Support/Code - Insiders/User/workspaceStorage".into(),
                "~/.config/Code/User/workspaceStorage".into(),
                "~/.config/Code - Insiders/User/workspaceStorage".into(),
                "~/.vscode-server/data/User/workspaceStorage".into(),
            ]);
            paths
        }
        _ => Vec::new(),
    }
}

fn env_or_default(env_name: &str, default: &str) -> Vec<String> {
    let mut paths = Vec::new();
    if let Some(value) = std::env::var_os(env_name) {
        let value = PathBuf::from(value).display().to_string();
        if !value.is_empty() {
            paths.push(value);
        }
    }
    paths.push(default.into());
    paths
}

pub(crate) fn expand_path(path: &str) -> PathBuf {
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
        assert_eq!(normalize_agent("codex-app").unwrap(), "codex-app");
        assert_eq!(normalize_agent("openclaw").unwrap(), "openclaw");
        assert_eq!(normalize_agent("hermes-agent").unwrap(), "hermes");
        assert_eq!(normalize_agent("antigravity-ide").unwrap(), "antigravity");
    }

    #[test]
    fn launch_commands_match_ollama_application_names() {
        assert_eq!(
            launch_command("claude", "qwen3.5").as_deref(),
            Some("ollama launch claude --model qwen3.5")
        );
        assert_eq!(launch_command("codex-app", "qwen3.5").as_deref(), None);
        assert_eq!(
            launch_command("openclaw", "qwen3.5").as_deref(),
            Some("ollama launch openclaw --model qwen3.5")
        );
        assert_eq!(
            launch_command("hermes", "qwen3.5").as_deref(),
            Some("ollama launch hermes --model qwen3.5")
        );
        assert_eq!(
            launch_command("codex", "qwen3.5").as_deref(),
            Some("ollama launch codex --model qwen3.5")
        );
        assert_eq!(
            launch_command("opencode", "qwen3.5").as_deref(),
            Some("ollama launch opencode --model qwen3.5")
        );
    }

    #[test]
    fn active_missions_filters_stale_and_low_confidence_contexts() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-05-25T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let recent = "2026-05-25T11:00:00Z".to_string();
        let stale = "2026-05-23T11:00:00Z".to_string();
        let contexts = vec![
            AgentContext {
                agent: "codex".into(),
                mission: Some("Implement TUI".into()),
                source_path: None,
                timestamp: Some(recent.clone()),
                confidence: 0.8,
                notes: Vec::new(),
            },
            AgentContext {
                agent: "claude-code".into(),
                mission: Some("Old task".into()),
                source_path: None,
                timestamp: Some(stale),
                confidence: 0.9,
                notes: Vec::new(),
            },
            AgentContext {
                agent: "gemini-cli".into(),
                mission: Some("Maybe task".into()),
                source_path: None,
                timestamp: Some(recent),
                confidence: 0.4,
                notes: Vec::new(),
            },
        ];

        let (active, ignored) = active_missions_from_contexts(contexts, 24, now);

        assert_eq!(active.len(), 1);
        assert_eq!(active[0].agent, "codex");
        assert_eq!(ignored.len(), 2);
    }

    #[test]
    fn detect_agent_skips_newer_files_without_missions() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("copilot");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(
            source.join("events.jsonl"),
            r#"{"lastPrompt":"Fix real task"}"#,
        )
        .unwrap();
        std::fs::write(source.join("workspace.yaml"), "summary_count: 6\n").unwrap();

        let mut config = Config::default();
        config.agents.sources.insert(
            "copilot-cli".into(),
            crate::config::AgentSourceConfig {
                enabled: true,
                paths: vec![source.display().to_string()],
            },
        );

        let context = detect_agent(&config, "copilot-cli").unwrap();
        assert_eq!(context.mission.as_deref(), Some("Fix real task"));
    }

    #[test]
    fn extracts_github_copilot_user_message_event() {
        let contents = r#"{"id":"1","type":"user.message","data":{"content":"Fix Copilot transcript parsing","transformedContent":"<ide_selection>noise</ide_selection>\n\nFix Copilot transcript parsing"}}"#;
        assert_eq!(
            extract_mission(contents).unwrap(),
            "Fix Copilot transcript parsing"
        );
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
