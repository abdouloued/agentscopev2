# Contributing to AgentScope

Thank you for your interest in contributing to AgentScope! This project aims to be the universal safety layer for AI coding agents, and we welcome contributions from the community.

## Getting Started

```bash
# Fork and clone the repo
git clone git@github.com:YOUR_USERNAME/agentscopev2.git
cd agentscopev2

# Build
cargo build

# Run tests
cargo test

# Run with debug output
RUST_LOG=debug cargo run -- check
```

## Development Setup

**Requirements:**
- Rust 1.75+ (`rustup update`)
- Git
- [Ollama](https://ollama.ai) (optional, for LLM judge testing)

```bash
# Pull the default judge model (optional)
ollama pull qwen3.5:2b
```

## How to Contribute

### Reporting Bugs

Open an issue with:
- What you expected to happen
- What actually happened
- Steps to reproduce
- `agentscope --version` output
- Your OS and Rust version

### Suggesting Features

Open an issue tagged `enhancement` with:
- The problem you're solving
- Your proposed solution
- Why this belongs in core vs. a plugin

### Pull Requests

1. **Fork** the repo and create a branch from `main`
2. **Write tests** for any new functionality
3. **Run `cargo test`** — all tests must pass
4. **Run `cargo clippy`** — no new warnings
5. **Format with `cargo fmt`**
6. **Open a PR** with a clear description of what and why

### What We're Looking For

| Area | Examples |
|---|---|
| **Agent context readers** | Aider, Continue, Windsurf, changed Claude/Codex/Cursor/Gemini log formats |
| **Policy engine features** | Regex path matching, file-type rules, custom validators |
| **LLM judge providers** | Groq, local llama.cpp, Mistral API |
| **Output formats** | SARIF, GitHub annotations, Slack webhooks |
| **TUI improvements** | File preview, diff view, keyboard shortcuts |
| **Documentation** | Tutorials, blog posts, video walkthroughs |

## Project Structure

```
src/
├── main.rs      # Entry point, command dispatch
├── cli.rs       # Clap CLI definitions
├── config.rs    # YAML config, agent integration templates
├── agents.rs    # Local agent context detection, attach, MCP, skills/plugins
├── git.rs       # git2 integration (diffs, baselines)
├── policy.rs    # Glob-based policy engine, scope hints
├── session.rs   # Session lifecycle (start, check, status)
├── judge.rs     # LLM judge (Ollama, Claude, OpenAI)
├── output.rs    # Terminal formatting, CheckReport
├── tui.rs       # Ratatui live dashboard
└── audit.rs     # Activity log and session history
```

## Code Style

- Follow standard Rust conventions (`cargo fmt`)
- Use `anyhow::Result` for error handling in application code
- Use `thiserror` for library-style error types
- Keep functions small and well-documented
- Prefer clarity over cleverness

## Testing

```bash
# Run all tests
cargo test

# Run a specific test
cargo test test_blocked_paths

# Run with output
cargo test -- --nocapture
```

### Testing agent-aware monitoring

Agent readers must fail soft. A missing local source should never make `agentscope check` fail.

Use a temporary `HOME` in tests and create fake local logs under the supported default paths:

| Agent | Test path |
|---|---|
| Claude Code | `.claude/projects/work/session.jsonl` |
| Codex | `.codex/sessions/2026/05/24/rollout-test.jsonl` |
| OpenCode | `.local/share/opencode/project/app/storage/chat.json` |
| Cursor | `.cursor/projects/hash/agent-transcripts/transcript.jsonl` |
| Gemini CLI | `.gemini/tmp/hash/chats/chat.json` |
| Copilot CLI | `.copilot/session-state/state.json` |

For mission extraction changes, add regression tests for noisy real-world text such as tool calls, patch hunks, metadata-only files, login commands, and wrapped Codex app requests.

## Release Process

Releases are tagged from `main`:

```bash
cargo build --release
# Binary at target/release/agentscope
```

## Code of Conduct

Be kind. Be constructive. We're all here to make AI agents safer.

## License

By contributing, you agree that your contributions will be licensed under the [MIT License](LICENSE).
