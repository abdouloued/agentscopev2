use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use ulid::Ulid;

use crate::cli::{AgentKind, JudgeProviderArg};
use crate::config::{self, ACTIVITY_LOG, SESSION_DIR, JudgeConfig, JudgeProvider};
use crate::git;
use crate::judge;
use crate::output::{CheckReport, Printer};
use crate::policy::PolicyEngine;

pub const ACTIVE_SESSION_FILE: &str = ".agentscope/session.json";

// ── Session data ──────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Session {
    pub id: String,
    pub mission: String,
    pub agent: String,
    pub git_baseline: String,
    pub started_at: String,
    pub repo_root: PathBuf,
}

// ── start ─────────────────────────────────────────────────────────────────────

pub async fn start(mission: String, agent: AgentKind, watch: bool) -> Result<()> {
    let p = Printer::new();
    let _config = config::load_or_default();

    let repo = git::open_repo()?;
    let baseline = git::capture_baseline(&repo)?;
    let repo_root = repo.workdir().unwrap_or_else(|| std::path::Path::new(".")).to_path_buf();

    std::fs::create_dir_all(SESSION_DIR)?;

    let session = Session {
        id: Ulid::new().to_string(),
        mission: mission.clone(),
        agent: agent.to_string(),
        git_baseline: baseline.clone(),
        started_at: Utc::now().to_rfc3339(),
        repo_root: repo_root.clone(),
    };

    save_session(&session)?;
    append_activity("session_start", &session)?;

    p.session_started(&session);

    if watch {
        crate::tui::run_watch().await?;
    }

    Ok(())
}

// ── check ─────────────────────────────────────────────────────────────────────

pub async fn check(session_id: Option<String>, json: bool, share: bool) -> Result<()> {
    let p = Printer::new();
    let config = config::load_or_default();

    let session = match session_id {
        Some(id) => load_session_by_id(&id)?,
        None => load_active_session()?,
    };

    let repo = git::open_repo()?;
    let diff = git::working_tree_diff(&repo)?;

    let engine = PolicyEngine::from_config(&config.policy)?;
    let annotated = engine.annotate(&diff.files, &session.mission);
    let limit_warnings = engine.check_limits(diff.files.len(), diff.total_lines_changed());

    // LLM judge (async, optional)
    let judge_result = if config.judge.enabled && !json {
        judge::evaluate(&session.mission, &annotated, &config.judge).await.ok()
    } else {
        None
    };

    let report = CheckReport {
        session: session.clone(),
        annotated: annotated.clone(),
        limit_warnings,
        judge_result: judge_result.clone(),
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&report.to_json())?);
        return Ok(());
    }

    p.print_check_report(&report);

    if share {
        let markdown = report.to_markdown();
        // In a real build: use arboard crate for clipboard
        println!("\n--- Markdown summary (copy to clipboard) ---\n{}", markdown);
    }

    // Append to activity log
    append_activity("session_check", &session)?;

    // Exit code: 1 if any BLOCKED files
    let has_blocks = annotated.iter().any(|f| f.verdict.is_blocked());
    if has_blocks {
        std::process::exit(1);
    }

    Ok(())
}

// ── judge (standalone) ────────────────────────────────────────────────────────

pub async fn judge(
    provider: Option<JudgeProviderArg>,
    model: Option<String>,
    endpoint: Option<String>,
    json: bool,
) -> Result<()> {
    let p = Printer::new();
    let config = config::load_or_default();
    let session = load_active_session()?;

    let repo = git::open_repo()?;
    let diff = git::working_tree_diff(&repo)?;

    let engine = PolicyEngine::from_config(&config.policy)?;
    let annotated = engine.annotate(&diff.files, &session.mission);

    // Build judge config with CLI overrides
    let judge_config = build_judge_config(&config.judge, provider, model, endpoint);

    p.hint(&format!(
        "Using {} / {}",
        match judge_config.provider {
            JudgeProvider::Ollama => "ollama",
            JudgeProvider::Claude => "claude",
            JudgeProvider::Openai => "openai",
            JudgeProvider::None => "none",
        },
        judge_config.model,
    ));

    let result = judge::evaluate(&session.mission, &annotated, &judge_config).await?;

    if json {
        let j = serde_json::json!({
            "confidence": result.confidence,
            "verdict": result.verdict.label(),
            "reasoning": result.reasoning,
            "provider": result.provider,
            "model": result.model,
        });
        println!("{}", serde_json::to_string_pretty(&j)?);
    } else {
        p.print_judge_result(&result);
    }

    Ok(())
}

/// Build a JudgeConfig with CLI overrides applied on top of the config file defaults
fn build_judge_config(
    base: &JudgeConfig,
    provider: Option<JudgeProviderArg>,
    model: Option<String>,
    endpoint: Option<String>,
) -> JudgeConfig {
    let mut cfg = base.clone();

    if let Some(p) = provider {
        cfg.provider = match p {
            JudgeProviderArg::Ollama => JudgeProvider::Ollama,
            JudgeProviderArg::Claude => JudgeProvider::Claude,
            JudgeProviderArg::Openai => JudgeProvider::Openai,
        };
    }

    if let Some(m) = model {
        cfg.model = m;
    }

    if let Some(e) = endpoint {
        cfg.endpoint = e;
    }

    cfg.enabled = true; // always enabled when user explicitly runs judge
    cfg
}

// ── report ────────────────────────────────────────────────────────────────────

pub async fn report(markdown: bool) -> Result<()> {
    let p = Printer::new();
    let config = config::load_or_default();
    let session = load_active_session()?;

    let repo = git::open_repo()?;
    let diff = git::working_tree_diff(&repo)?;

    let engine = PolicyEngine::from_config(&config.policy)?;
    let annotated = engine.annotate(&diff.files, &session.mission);
    let limit_warnings = engine.check_limits(diff.files.len(), diff.total_lines_changed());

    // Run judge
    let judge_result = if config.judge.enabled {
        judge::evaluate(&session.mission, &annotated, &config.judge).await.ok()
    } else {
        None
    };

    let report = CheckReport {
        session: session.clone(),
        annotated,
        limit_warnings,
        judge_result,
    };

    if markdown {
        println!("{}", report.to_markdown());
    } else {
        p.print_full_report(&report);
    }

    Ok(())
}

// ── diff (quick annotated view) ──────────────────────────────────────────────

pub async fn diff(problems: bool) -> Result<()> {
    let p = Printer::new();
    let config = config::load_or_default();
    let session = load_active_session()?;

    let repo = git::open_repo()?;
    let diff_result = git::working_tree_diff(&repo)?;

    let engine = PolicyEngine::from_config(&config.policy)?;
    let annotated = engine.annotate(&diff_result.files, &session.mission);

    let filtered: Vec<_> = if problems {
        annotated.iter().filter(|f| {
            f.verdict.is_blocked() || f.verdict == crate::policy::FileVerdict::Unasked
        }).collect()
    } else {
        annotated.iter().collect()
    };

    if filtered.is_empty() {
        if problems {
            p.success("No problems found — all changes are in scope");
        } else {
            p.hint("No changes detected in working tree");
        }
        return Ok(());
    }

    println!();
    println!("  {} {} · {}",
        console::style(&session.id[..8]).cyan(),
        console::style("·").dim(),
        console::style(&session.mission).white(),
    );
    println!();

    for file in &filtered {
        p.print_file_row_public(file);
    }

    println!();

    let total_add: usize = filtered.iter().map(|f| f.diff.additions).sum();
    let total_del: usize = filtered.iter().map(|f| f.diff.deletions).sum();
    let blocked = filtered.iter().filter(|f| f.verdict.is_blocked()).count();
    let unasked = filtered.iter().filter(|f| f.verdict == crate::policy::FileVerdict::Unasked).count();
    let in_scope = filtered.iter().filter(|f| f.verdict == crate::policy::FileVerdict::InScope).count();

    println!("  {} files  {}  {}  {}  {}",
        filtered.len(),
        console::style(format!("+{}", total_add)).green(),
        console::style(format!("-{}", total_del)).red(),
        console::style("·").dim(),
        if blocked > 0 {
            console::style(format!("{} blocked", blocked)).red().bold().to_string()
        } else if unasked > 0 {
            console::style(format!("{} unasked", unasked)).yellow().to_string()
        } else {
            console::style(format!("{} in scope", in_scope)).green().to_string()
        },
    );
    println!();

    Ok(())
}

// ── status ────────────────────────────────────────────────────────────────────

pub async fn status() -> Result<()> {
    let p = Printer::new();

    match load_active_session() {
        Ok(session) => p.session_one_liner(&session),
        Err(_) => p.hint("No active session. Run: agentscope start \"your mission\""),
    }

    Ok(())
}

// ── Persistence ───────────────────────────────────────────────────────────────

fn save_session(session: &Session) -> Result<()> {
    let json = serde_json::to_string_pretty(session)?;
    std::fs::write(ACTIVE_SESSION_FILE, json)?;
    Ok(())
}

pub fn load_active_session() -> Result<Session> {
    let path = std::path::Path::new(ACTIVE_SESSION_FILE);
    if !path.exists() {
        anyhow::bail!("No active session. Run: agentscope start \"your mission\"");
    }
    let json = std::fs::read_to_string(path)?;
    let session: Session = serde_json::from_str(&json)?;
    Ok(session)
}

fn load_session_by_id(id: &str) -> Result<Session> {
    // Look through activity log for session with matching id
    let log_path = std::path::Path::new(ACTIVITY_LOG);
    if !log_path.exists() {
        anyhow::bail!("No activity log found");
    }

    let content = std::fs::read_to_string(log_path)?;
    for line in content.lines().rev() {
        if let Ok(entry) = serde_json::from_str::<ActivityEntry>(line) {
            if entry.session.id == id {
                return Ok(entry.session);
            }
        }
    }

    anyhow::bail!("Session {} not found", id);
}

fn append_activity(event: &str, session: &Session) -> Result<()> {
    let entry = ActivityEntry {
        event: event.to_string(),
        timestamp: Utc::now().to_rfc3339(),
        session: session.clone(),
    };
    let line = serde_json::to_string(&entry)?;
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(ACTIVITY_LOG)?;
    writeln!(file, "{}", line)?;
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
struct ActivityEntry {
    event: String,
    timestamp: String,
    session: Session,
}
