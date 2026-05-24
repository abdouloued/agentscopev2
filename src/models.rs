//! Model and provider management — Claude Code / Codex style.
//! Lists Ollama models, sets defaults, tests models, manages config.

use anyhow::Result;
use console::style;

use crate::config::{self, JudgeProvider};
use crate::output::Printer;

// ── Model commands ────────────────────────────────────────────────────────────

/// List all available Ollama models + show which is currently selected
pub async fn list_models() -> Result<()> {
    let p = Printer::new();
    let config = config::load_or_default();

    let current_model = &config.judge.model;
    let current_provider = &config.judge.provider;

    println!();
    println!("  {} {}",
        style("agentscope models").cyan().bold(),
        style("— available LLM judges").dim(),
    );
    println!();

    // Show current selection
    let provider_name = match current_provider {
        JudgeProvider::Ollama => "ollama",
        JudgeProvider::Claude => "claude",
        JudgeProvider::Openai => "openai",
        JudgeProvider::None => "none",
    };
    println!("  {}  {} {}",
        style("▸").color256(135).bold(),
        style("Current:").dim(),
        style(format!("{} / {}", provider_name, current_model)).white().bold(),
    );
    println!("  {}  {} {}",
        style(" ").dim(),
        style("Endpoint:").dim(),
        style(&config.judge.endpoint).dim(),
    );
    println!();

    // List Ollama models
    println!("  {}", style("── Ollama Models ──").dim());
    println!();

    match fetch_ollama_models(&config.judge.endpoint).await {
        Ok(models) => {
            if models.is_empty() {
                p.hint("No models found. Run: ollama pull qwen3.5:2b");
            } else {
                for m in &models {
                    let is_current = m.name == *current_model && *current_provider == JudgeProvider::Ollama;
                    let marker = if is_current { "●" } else { "○" };
                    let marker_color = if is_current { style(marker).green().bold() } else { style(marker).dim() };
                    let name_style = if is_current {
                        style(&m.name).green().bold()
                    } else {
                        style(&m.name).white()
                    };

                    println!("  {}  {}  {}  {}",
                        marker_color,
                        name_style,
                        style(&m.size).dim(),
                        if is_current { style("← active").green() } else { style("").dim() },
                    );
                }
            }
        }
        Err(e) => {
            println!("  {}  {}",
                style("✕").red(),
                style(format!("Could not reach Ollama: {}", e)).red(),
            );
            p.hint("Make sure Ollama is running: ollama serve");
        }
    }

    println!();

    // Cloud providers
    println!("  {}", style("── Cloud Providers ──").dim());
    println!();

    let claude_active = *current_provider == JudgeProvider::Claude;
    let openai_active = *current_provider == JudgeProvider::Openai;

    let claude_key = std::env::var("ANTHROPIC_API_KEY").is_ok();
    let openai_key = std::env::var("OPENAI_API_KEY").is_ok();

    println!("  {}  {}  {}",
        if claude_active { style("●").green().bold() } else { style("○").dim() },
        if claude_active { style("claude").green().bold() } else { style("claude").white() },
        if claude_key { style("(API key set)").green() } else { style("(set ANTHROPIC_API_KEY)").dim() },
    );
    println!("  {}  {}  {}",
        if openai_active { style("●").green().bold() } else { style("○").dim() },
        if openai_active { style("openai").green().bold() } else { style("openai").white() },
        if openai_key { style("(API key set)").green() } else { style("(set OPENAI_API_KEY)").dim() },
    );

    println!();

    // Usage hints
    println!("  {}", style("── Quick Commands ──").dim());
    println!("  {}  {}", style("  agentscope model set qwen3.5:2b").cyan(), style("— set default Ollama model").dim());
    println!("  {}  {}", style("  agentscope model set -p claude claude-sonnet-4-20250514").cyan(), style("— use Claude").dim());
    println!("  {}  {}", style("  agentscope model test").cyan(), style("— test current model").dim());
    println!("  {}  {}", style("  agentscope model pull llama3").cyan(), style("— download a model").dim());
    println!("  {}  {}", style("  agentscope judge -m gemma4:e2b").cyan(), style("— one-off judge with any model").dim());
    println!();

    Ok(())
}

/// Set the default model (updates agentscope.yaml)
pub async fn set_model(
    model: String,
    provider: Option<crate::cli::JudgeProviderArg>,
    endpoint: Option<String>,
) -> Result<()> {
    let p = Printer::new();
    let mut config = config::load_or_default();

    // Apply overrides
    config.judge.model = model.clone();

    if let Some(prov) = provider {
        config.judge.provider = match prov {
            crate::cli::JudgeProviderArg::Ollama => JudgeProvider::Ollama,
            crate::cli::JudgeProviderArg::Claude => JudgeProvider::Claude,
            crate::cli::JudgeProviderArg::Openai => JudgeProvider::Openai,
        };
    }

    if let Some(ep) = endpoint {
        config.judge.endpoint = ep;
    }

    // Write back to agentscope.yaml
    write_config(&config)?;

    let provider_name = match config.judge.provider {
        JudgeProvider::Ollama => "ollama",
        JudgeProvider::Claude => "claude",
        JudgeProvider::Openai => "openai",
        JudgeProvider::None => "none",
    };

    p.success(&format!("Default model set: {} / {}", provider_name, model));
    p.hint("This will be used for all future judge runs.");

    Ok(())
}

/// Test a model by sending a simple prompt
pub async fn test_model(model: Option<String>) -> Result<()> {
    let p = Printer::new();
    let config = config::load_or_default();

    let test_model = model.unwrap_or_else(|| config.judge.model.clone());

    println!();
    println!("  {} {} {}",
        style("Testing").dim(),
        style(&test_model).cyan().bold(),
        style("…").dim(),
    );

    let client = reqwest::Client::new();
    let mut body = serde_json::json!({
        "model": test_model,
        "prompt": "Reply with exactly this JSON and nothing else: {\"status\": \"ok\", \"model\": \"your-name\"}",
        "stream": false,
        "options": {
            "temperature": 0.0,
            "num_predict": 50,
        }
    });

    if test_model.starts_with("qwen3") {
        body.as_object_mut().unwrap().insert("think".into(), serde_json::json!(false));
    }

    let start = std::time::Instant::now();
    let url = format!("{}/api/generate", config.judge.endpoint);

    match client.post(&url).json(&body).timeout(std::time::Duration::from_secs(30)).send().await {
        Ok(response) => {
            let elapsed = start.elapsed();
            let raw: serde_json::Value = response.json().await?;
            let text = raw["response"].as_str().unwrap_or("(no response)");

            println!();
            p.success(&format!("Response in {:.1}s", elapsed.as_secs_f64()));
            println!("  {}  {}",
                style("→").dim(),
                style(text.trim()).white(),
            );

            // Check if it's valid JSON
            let clean = text.trim()
                .trim_start_matches("```json").trim_start_matches("```")
                .trim_end_matches("```").trim();
            if serde_json::from_str::<serde_json::Value>(clean).is_ok() {
                p.success("Model returns valid JSON ✓");
            } else {
                p.warn("Model did not return valid JSON — judge may need retries");
            }
        }
        Err(e) => {
            println!();
            println!("  {}  {}",
                style("✕").red().bold(),
                style(format!("Failed: {}", e)).red(),
            );
            p.hint("Make sure Ollama is running: ollama serve");
            p.hint(&format!("Make sure model is downloaded: ollama pull {}", test_model));
        }
    }

    println!();
    Ok(())
}

/// Pull a model from Ollama
pub async fn pull_model(model: String) -> Result<()> {
    let p = Printer::new();

    println!();
    println!("  {} {}",
        style("Pulling").dim(),
        style(&model).cyan().bold(),
    );
    println!("  {}", style("This may take a while for large models…").dim());
    println!();

    // Use ollama CLI for pull (it shows progress)
    let status = tokio::process::Command::new("ollama")
        .args(["pull", &model])
        .status()
        .await?;

    if status.success() {
        println!();
        p.success(&format!("Model {} is ready", model));
        p.hint(&format!("Set as default: agentscope model set {}", model));
    } else {
        println!();
        p.warn(&format!("Failed to pull {} — check ollama logs", model));
    }

    Ok(())
}

// ── Config commands ──────────────────────────────────────────────────────────

/// Show current config in a pretty format
pub async fn config_show() -> Result<()> {
    let config = config::load_or_default();

    let provider_name = match config.judge.provider {
        JudgeProvider::Ollama => "ollama",
        JudgeProvider::Claude => "claude",
        JudgeProvider::Openai => "openai",
        JudgeProvider::None => "none (disabled)",
    };

    println!();
    println!("  {} {}", 
        style("agentscope config").cyan().bold(),
        style("— current settings").dim(),
    );
    println!();

    // Judge section
    println!("  {}", style("── Judge ──").dim());
    println!("  {}  {}",
        style("  enabled   ").dim(),
        if config.judge.enabled { style("true").green() } else { style("false").red() },
    );
    println!("  {}  {}",
        style("  provider  ").dim(),
        style(provider_name).white().bold(),
    );
    println!("  {}  {}",
        style("  model     ").dim(),
        style(&config.judge.model).cyan().bold(),
    );
    println!("  {}  {}",
        style("  endpoint  ").dim(),
        style(&config.judge.endpoint).dim(),
    );
    println!();

    // Policy section
    println!("  {}", style("── Policy ──").dim());
    println!("  {}  {}",
        style("  blocked   ").dim(),
        style(format!("{} patterns", config.policy.blocked.len())).white(),
    );
    for pattern in &config.policy.blocked {
        println!("  {}  {}", style("            ").dim(), style(pattern).red());
    }
    println!("  {}  {}",
        style("  warn      ").dim(),
        style(format!("{} patterns", config.policy.warn.len())).white(),
    );
    for pattern in &config.policy.warn {
        println!("  {}  {}", style("            ").dim(), style(pattern).yellow());
    }
    if config.policy.max_files_changed > 0 {
        println!("  {}  {}",
            style("  max_files ").dim(),
            style(config.policy.max_files_changed.to_string()).white(),
        );
    }
    if config.policy.max_lines_changed > 0 {
        println!("  {}  {}",
            style("  max_lines ").dim(),
            style(config.policy.max_lines_changed.to_string()).white(),
        );
    }
    println!();

    // Team section
    println!("  {}", style("── Team ──").dim());
    println!("  {}  {}",
        style("  enabled   ").dim(),
        if config.team.enabled { style("true").green() } else { style("false").dim() },
    );
    println!("  {}  {}",
        style("  share_logs").dim(),
        if config.team.share_logs { style("true").green() } else { style("false").dim() },
    );
    println!();

    // Config file location
    println!("  {}  {}",
        style("config file").dim(),
        style("agentscope.yaml").cyan(),
    );
    println!("  {}  {}",
        style("edit with  ").dim(),
        style("agentscope config edit").cyan(),
    );
    println!();

    Ok(())
}

/// Set a config value by key path (e.g., "judge.model", "judge.provider")
pub async fn config_set(key: String, value: String) -> Result<()> {
    let p = Printer::new();
    let mut config = config::load_or_default();

    match key.as_str() {
        "judge.model" | "model" => {
            config.judge.model = value.clone();
            p.success(&format!("judge.model = {}", value));
        }
        "judge.provider" | "provider" => {
            config.judge.provider = match value.as_str() {
                "ollama" => JudgeProvider::Ollama,
                "claude" => JudgeProvider::Claude,
                "openai" => JudgeProvider::Openai,
                "none" => JudgeProvider::None,
                _ => anyhow::bail!("Unknown provider: {}. Use: ollama, claude, openai, none", value),
            };
            p.success(&format!("judge.provider = {}", value));
        }
        "judge.endpoint" | "endpoint" => {
            config.judge.endpoint = value.clone();
            p.success(&format!("judge.endpoint = {}", value));
        }
        "judge.enabled" => {
            config.judge.enabled = value.parse::<bool>()
                .map_err(|_| anyhow::anyhow!("Expected true or false"))?;
            p.success(&format!("judge.enabled = {}", value));
        }
        "policy.max_files" | "max_files" => {
            config.policy.max_files_changed = value.parse()
                .map_err(|_| anyhow::anyhow!("Expected a number"))?;
            p.success(&format!("policy.max_files_changed = {}", value));
        }
        "policy.max_lines" | "max_lines" => {
            config.policy.max_lines_changed = value.parse()
                .map_err(|_| anyhow::anyhow!("Expected a number"))?;
            p.success(&format!("policy.max_lines_changed = {}", value));
        }
        "team.enabled" => {
            config.team.enabled = value.parse::<bool>()
                .map_err(|_| anyhow::anyhow!("Expected true or false"))?;
            p.success(&format!("team.enabled = {}", value));
        }
        "team.share_logs" => {
            config.team.enabled = value.parse::<bool>()
                .map_err(|_| anyhow::anyhow!("Expected true or false"))?;
            p.success(&format!("team.share_logs = {}", value));
        }
        _ => {
            anyhow::bail!(
                "Unknown config key: {}\n\nAvailable keys:\n  model, provider, endpoint, judge.enabled\n  max_files, max_lines\n  team.enabled, team.share_logs",
                key
            );
        }
    }

    write_config(&config)?;
    Ok(())
}

/// Open config in $EDITOR
pub async fn config_edit() -> Result<()> {
    let p = Printer::new();
    let config_path = "agentscope.yaml";

    if !std::path::Path::new(config_path).exists() {
        anyhow::bail!("No agentscope.yaml found. Run: agentscope init");
    }

    let editor = std::env::var("EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .unwrap_or_else(|_| "vi".to_string());

    p.hint(&format!("Opening {} in {}…", config_path, editor));

    let status = tokio::process::Command::new(&editor)
        .arg(config_path)
        .status()
        .await?;

    if status.success() {
        p.success("Config saved");
    } else {
        p.warn("Editor exited with an error");
    }

    Ok(())
}

/// Reset config to defaults
pub async fn config_reset(preset: crate::cli::Preset) -> Result<()> {
    let p = Printer::new();
    let config = config::preset_config(&preset);
    write_config(&config)?;
    p.success(&format!("Config reset to {:?} preset", preset));
    Ok(())
}

// ── Helpers ──────────────────────────────────────────────────────────────────

struct OllamaModel {
    name: String,
    size: String,
}

async fn fetch_ollama_models(endpoint: &str) -> Result<Vec<OllamaModel>> {
    let client = reqwest::Client::new();
    let url = format!("{}/api/tags", endpoint);

    let response = client.get(&url)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await?;

    let data: serde_json::Value = response.json().await?;
    let models = data["models"].as_array()
        .map(|arr| {
            arr.iter().map(|m| {
                let name = m["name"].as_str().unwrap_or("unknown").to_string();
                let size_bytes = m["size"].as_u64().unwrap_or(0);
                let size = format_size(size_bytes);
                OllamaModel { name, size }
            }).collect()
        })
        .unwrap_or_default();

    Ok(models)
}

fn format_size(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1_000_000_000.0)
    } else if bytes >= 1_000_000 {
        format!("{:.0} MB", bytes as f64 / 1_000_000.0)
    } else {
        format!("{} B", bytes)
    }
}

fn write_config(config: &config::Config) -> Result<()> {
    let yaml = serde_yaml::to_string(config)?;
    let path = "agentscope.yaml";

    // Preserve header comment if file exists
    let header = if std::path::Path::new(path).exists() {
        let existing = std::fs::read_to_string(path)?;
        existing.lines()
            .take_while(|l| l.starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        "# AgentScope configuration\n# Docs: https://agentscope.dev/config".to_string()
    };

    let content = if header.is_empty() {
        yaml
    } else {
        format!("{}\n\n{}", header, yaml)
    };

    std::fs::write(path, content)?;
    Ok(())
}
