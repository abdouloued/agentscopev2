# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

AgentScope is a Rust CLI that acts as a scope firewall and audit layer for AI coding agents. It records or detects your mission, watches Git changes, applies deterministic policy, and optionally asks a judge model whether the diff still matches the mission.

## Architecture

```mermaid
graph TB
    subgraph AgentScope Core
        A1[src/main.rs]
        A2[src/cli.rs]
        A3[src/config.rs]
        A4[src/policy.rs]
        A5[src/models.rs]
        A6[src/session.rs]
        A7[src/git.rs]
        A8[src/tui.rs]
    end

    subgraph External
        B1[cargo]
        B2[ollama/qwen3.5:2b]
        B3[AgentSession State]
        B4[Activity Log]
    end

    A1 --> A2
    A2 --> A3
    A3 --> A4
    A4 --> A5
    A5 --> A6
    A6 --> A7
    A7 --> A8
    A8 --> B2
```

### Core Components

| Component | Purpose |
|-----------|---------|
| **CLI** (`src/cli.rs`) | Entry point with subcommands: start, check, judge, models, config, audit, attach, monitor, mcp, skills, plugins |
| **Policy Engine** (`src/policy.rs`) | Checks file paths against blocked/warn patterns, extracts scope hints from mission, classifies changes |
| **Session Manager** (`src/session.rs`) | Manages agent sessions, activity logs, and session states |
| **Git Diff Tool** (`src/git.rs`) | Computes working tree diffs against HEAD commit |
| **Model Runner** (`src/models.rs`) | List, set, test, pull Ollama models; set judge defaults |
| **TUI** (`src/tui.rs`) | Live dashboard showing verdicts, health, file lists, sparklines |
| **Activity Log** | JSONL activity log (`src/session.rs`) |

## Quick Commands

### Init / Start a Session

```bash
# Create a new agentscope.yaml config
agentscope init

# Start a manual mission
agentscope start "Fix checkout button loading state" --agent codex

# Start with watching for changes
agentscope start "Fix checkout button loading state" --agent codex --watch
```

### Agent Context & Monitoring

```bash
# Detect local agent context
agentscope agents detect

# Doctor configuration issues
agentscope agents doctor

# Print inferred context
agentscope agents context --agent auto

# Attach to inferred session
agentscope attach --agent auto
agentscope attach --agent auto --apply

# Watch with auto-detect
agentscope watch

# Monitor and auto-attach high-confidence sessions
agentscope monitor --agent auto
```

### Check Before Commit

```bash
# Full check with JSON output
agentscope check --json

# Diff with problem-focused output
agentscope diff --problems
```

### Judge Models

```bash
# List available judge models
agentscope model list

# Set default model
agentscope model set qwen3.5:2b

# Test a model
agentscope model test

# Pull a model from Ollama
agentscope model pull llama3

# One-off judge with any model
agentscope judge -m gemma4:e2b
```

### Config Management

```bash
# Show current configuration
agentscope config show

# Set a config value
agentscope config set model qwen3.5:2b
agentscope config set judge.enabled true
agentscope config set max_files 50
agentscope config set team.enabled true

# Open config file in editor
agentscope config edit

# Reset to preset
agentscope config reset solo
```

### Hooks (Pre-commit)

```bash
# Install hook
agentscope hook install

# Uninstall hook
agentscope hook uninstall

# Check hook status
agentscope hook status
```

## Configuration (`agentscope.yaml`)

```yaml
policy:
  # Glob patterns always blocked
  blocked:
    - ".env"
    - ".env.*"
    - "**/.env"
    - "**/.env.*"
    - "**/secrets/**"
    - "**/*.pem"
    - "**/*.key"
    - "src/auth/**"
    - "**/migrations/**"

  # Glob patterns that trigger warning but not block
  warn:
    - "package-lock.json"
    - "yarn.lock"
    - "Cargo.lock"
    - "**/config/**"

  # Max files changed (0 = disabled)
  max_files_changed: 20

  # Max lines changed (0 = disabled)
  max_lines_changed: 800

judge:
  enabled: true
  provider: ollama
  model: "qwen3.5:2b"
  endpoint: "http://localhost:11434"

team:
  enabled: true
  share_logs: true
  log_path: "artifacts"

agents:
  auto_detect: true
  auto_attach: false
  preferred:
    - codex
    - claude-code
    - cursor
```

## Workflow Patterns

### Solo Developer

```bash
agentscope init

# Watch session
agentscope watch

# Auto-attach and watch
agentscope monitor --agent auto

# Check before commit
agentscope check --problems
agentscope check --json
```

### Team with Shared Logs

```bash
agentscope init

# Team preset configures max_files 10, shared logs enabled
# Open .cursor/rules/agentscope.md for editor guidelines
```

### CI Pipeline

```bash
agentscope init

# CI preset configures max_files 5, judge disabled
agentscope config reset ci
```

## Key Files

| File | Purpose |
|------|---------|
| `src/main.rs` | CLI entry point and command routing |
| `src/cli.rs` | Subcommand definitions and argument parsing |
| `src/config.rs` | Configuration loading, persistence, presets |
| `src/policy.rs` | Policy engine, path matching, scope hints |
| `src/models.rs` | Judge model management |
| `src/session.rs` | Session state, activity log |
| `src/git.rs` | Git diff computation |
| `src/tui.rs` | Live TUI dashboard |

## Development

```bash
cargo fmt      # Format code
cargo test     # Run tests
cargo build    # Build release
cargo build --release  # Production binary
```

## License

MIT + Commons Clause. See [LICENSE](LICENSE).

## Resources

- [Documentation](README.md)
- [Configuration Guide](README.md#policy)
- [CONTRIBUTING.md](CONTRIBUTING.md)

---

# AgentScope plugin · claude-code

AgentScope is a scope firewall and audit cockpit for AI coding agents.
It checks whether your Git changes match the active mission.

## When to run AgentScope

| Trigger | Command |
|---------|--------|
| Before starting work | `agentscope status` |
| While working | `agentscope watch` (live TUI cockpit) |
| Before finishing | `agentscope check` |
| Before committing | `agentscope diff --problems` |

## Quick reference

```
agentscope init                          # one-time repo setup
agentscope start "your mission"          # record what you're doing
agentscope watch                         # live cockpit (1=review 2=chat 3=dash 4=sessions 5=live)
agentscope check                         # policy check + scope audit
agentscope check --json                  # machine-readable output
agentscope judge                         # ask the LLM judge
agentscope diff --problems               # show suspicious/blocked files only
agentscope attach --agent auto --apply   # infer mission from this agent's logs
```

## Status labels

| Badge | Meaning |
|-------|---------|
| `EXPECTED` | File matches the active mission scope |
| `SUSPICIOUS` | Changed but no mission rule matches |
| `BLOCKED` | Matched a blocked policy path — hard stop |
| `IGNORED` | Clean, stale, or explicitly excluded |

## TUI keyboard shortcuts

| Key | Action |
|-----|--------|
| `1`–`5` | Switch mode (Review/Chat/Dashboard/Sessions/Live) |
| `Enter` | Open diff overlay for selected file |
| `j` | Run judge on selected file |
| `a` / `b` | Allow / block selected file |
| `t` | Cycle themes (agentscope/codex/claude/openclaw/high-contrast) |
| `?` | Help overlay |
| `q` | Quit |

## Judge providers

AgentScope supports Ollama (local/private), Claude, OpenAI, Gemini, and OpenRouter.

```
agentscope config set judge.provider ollama      # local, private
agentscope config set judge.provider claude      # requires ANTHROPIC_API_KEY
agentscope config set judge.provider openai      # requires OPENAI_API_KEY
agentscope config set judge.provider gemini      # requires GEMINI_API_KEY
agentscope config set judge.provider openrouter  # requires OPENROUTER_API_KEY
```

## Policy config (`agentscope.yaml`)

```yaml
policy:
blocked:
- ".env"
- "**/.env.*"
- "**/secrets/**"
- "**/*.pem"
- "**/*.key"
warn:
- "package-lock.json"
- "yarn.lock"
- "Cargo.lock"
max_files_changed: 20
max_lines_changed: 800
```

Blocked patterns are enforced deterministically — no model can override them.

## More info

Run `agentscope --help` or visit https://github.com/abdouloued/agentscopev2

