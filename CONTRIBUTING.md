# Contributing to AgentScope

> **Status:** Early development — currently tested with [Ollama](https://ollama.com) and local models.
> Multi-provider support (Claude, OpenAI, Gemini, OpenRouter) is implemented but undertested.
> All contributions, bug reports, and feedback are welcome.

---

## Quick start for contributors

```bash
# 1. Fork on GitHub, then clone your fork
git clone https://github.com/YOUR_USERNAME/agentscopev2.git
cd agentscopev2

# 2. Build
cargo build

# 3. Run tests
cargo test

# 4. Install locally for manual testing
cargo install --path . --force

# 5. Try it in any git repo
cd ~/my-project
agentscope init
agentscope start "test mission" --agent claude
agentscope check
```

**Requirements:** Rust 1.75+ (`rustup update stable`), Git.

---

## Project layout

```
src/
├── main.rs             Entry point, command dispatch
├── cli.rs              Clap CLI definitions and subcommands
├── config.rs           YAML config, presets, agent integration templates
├── agents.rs           Agent detection, attach, MCP server, skills/plugins
├── git.rs              git2 integration — diffs, baselines
├── policy.rs           Glob-based policy engine, scope hints
├── session.rs          Session lifecycle (start, check, status, attach)
├── assistant_sessions.rs  Index and surface local agent session logs
├── judge.rs            LLM judge (Ollama, Claude, OpenAI, Gemini, OpenRouter)
├── models.rs           Judge model management (list, set, pull, test)
├── output.rs           Terminal formatting, CheckReport, Printer
├── tui.rs              Ratatui live TUI cockpit
├── hooks.rs            Pre-commit / pre-push git hook installer
├── launchers.rs        Native and Ollama-based agent launcher commands
├── audit.rs            Activity log and session history
├── chat.rs             TUI chat panel (provider-agnostic)
└── theme.rs            TUI theme definitions

plugins/agentscope/     Claude Code plugin and MCP tools
docs/                   Additional documentation
tests/                  Integration tests
```

---

## How to contribute

### Reporting bugs

Open a [GitHub issue](https://github.com/abdouloued/agentscopev2/issues) with:
- What you expected vs. what happened
- Steps to reproduce
- `agentscope --version` output
- OS and Rust version (`rustc --version`)

### Suggesting features

Open an issue tagged `enhancement` with:
- The problem you're solving
- Your proposed solution
- Why this belongs in core vs. a plugin/skill

### Pull requests

1. **Fork** the repo and create a branch from `main`
2. **Write tests** for new functionality — see `tests/cli_test.rs`
3. **Run `cargo test`** — all tests must pass
4. **Run `cargo clippy -- -D warnings`** — no new warnings
5. **Format with `cargo fmt`**
6. **Open a PR** with a clear description of what and why

---

## Good first contributions

| Area | Examples |
|------|---------|
| **Agent readers** | Add support for Aider, Continue, Windsurf, or update changed log formats |
| **Policy engine** | Regex path matching, file-type rules, custom validators |
| **Judge providers** | Groq, Mistral API, llama.cpp, LM Studio |
| **Output formats** | SARIF, GitHub annotations, JSON summary |
| **TUI polish** | File preview pane, diff line highlighting, keyboard shortcut improvements |
| **Launchers** | Verify/test `ollama launch` commands on different platforms |
| **Documentation** | Tutorials, usage examples, integration guides |
| **Tests** | More coverage for policy engine, agent readers, session lifecycle |

---

## Testing agent-aware monitoring

Agent readers must fail gracefully. A missing or unreadable local source should never cause `agentscope check` to fail.

For mission extraction changes, add regression tests using the fake log paths:

| Agent | Default log location |
|-------|---------------------|
| Claude Code | `~/.claude/projects/<hash>/<session>.jsonl` |
| Codex CLI | `~/.codex/sessions/<date>/rollout-<id>.jsonl` |
| OpenCode | `~/.local/share/opencode/project/<hash>/storage/chat.json` |
| Cursor | `~/.cursor/projects/<hash>/agent-transcripts/transcript.jsonl` |
| Gemini CLI | `~/.gemini/tmp/<hash>/chats/chat.json` |
| Copilot CLI | `~/.copilot/session-state/<id>/events.jsonl` |

---

## LLM judge testing

The default judge uses Ollama. To test locally:

```bash
# Install Ollama: https://ollama.com
ollama pull qwen3.5:2b

# Test the judge
agentscope judge

# Or with a different model
agentscope judge -m llama3.2:3b
```

For cloud providers (Claude, OpenAI, etc.), set the relevant env var before testing:

```bash
ANTHROPIC_API_KEY=... agentscope judge -p claude
OPENAI_API_KEY=...    agentscope judge -p openai -m gpt-4o-mini
```

---

## Commit style

Use conventional commits:

```
feat: add Aider agent context reader
fix: correct session ID parsing for Codex v2 log format
docs: add Ollama quickstart to README
chore: remove stray root config.rs
```

---

## Code style

- Standard Rust conventions (`cargo fmt`)
- `anyhow::Result` for application-level error handling
- `thiserror` for library-style errors with structured variants
- Functions stay small and focused — prefer clarity over cleverness
- Every public function or non-obvious block gets a short doc comment

---

## License

By contributing, you agree your contributions will be licensed under the [MIT License](LICENSE).

## Code of conduct

Be kind. Be constructive. We're all here to make AI agents safer.
