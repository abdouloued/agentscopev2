# AgentScope

> **⚠️ Early development — currently tested with [Ollama](https://ollama.com) and local models only.**
> Multi-provider judge support (Claude, OpenAI, Gemini, OpenRouter) is implemented but undertested.
> Expect rough edges; contributions and bug reports are welcome.

**AgentScope is a Rust CLI cockpit for AI coding agents.** It records or detects a mission, watches Git changes live, applies deterministic policy, and optionally asks a judge model whether the diff still matches the mission — all inside a polished terminal UI.

AgentScope does not replace Codex, Claude Code, Cursor, Gemini CLI, OpenCode, or Copilot. It sits beside them as a repo safety layer and real-time audit cockpit.

## Install

**Option 1 — pre-built binary (no Rust required):**

Download the latest binary for your platform from [GitHub Releases](https://github.com/abdouloued/agentscopev2/releases):

```bash
# macOS Apple Silicon
curl -L https://github.com/abdouloued/agentscopev2/releases/latest/download/agentscope-aarch64-apple-darwin.tar.gz | tar xz
sudo mv agentscope-aarch64-apple-darwin /usr/local/bin/agentscope

# macOS Intel
curl -L https://github.com/abdouloued/agentscopev2/releases/latest/download/agentscope-x86_64-apple-darwin.tar.gz | tar xz
sudo mv agentscope-x86_64-apple-darwin /usr/local/bin/agentscope

# Linux x86_64
curl -L https://github.com/abdouloued/agentscopev2/releases/latest/download/agentscope-x86_64-linux.tar.gz | tar xz
sudo mv agentscope-x86_64-linux /usr/local/bin/agentscope
```

**Option 2 — from crates.io (requires Rust):**

```bash
cargo install agentscope
```

**Option 3 — build from source:**

```bash
git clone https://github.com/abdouloued/agentscopev2.git
cd agentscope
cargo install --path . --force
```

## The 30-second version

```bash
agentscope init
agentscope start "Fix checkout button loading state" --agent codex

# Run your coding agent normally, then watch the cockpit:
agentscope watch
```

If you are already inside a supported agent session, let AgentScope infer the mission:

```bash
agentscope agents detect
agentscope attach --agent auto --apply
agentscope monitor --agent auto
```

Safe default: `attach` is a dry run. It only writes `.agentscope/session.json` with `--apply`.

---

## The TUI cockpit

`agentscope watch` opens a full-terminal cockpit with five modes:

| Key | Mode | What you see |
|-----|------|--------------|
| `1` | **Review** | Changed files list + live inline diff + verdict panel (default) |
| `2` | **Chat** | Full-screen AI chat with slash-command palette |
| `3` | **Dashboard** | Scope distribution, per-agent stats, judge health, mission timing |
| `4` | **Sessions** | Active and stale agent missions — `Enter` to inspect |
| `5` | **Live** | OS-level file watcher — freshness badges, instant diff on change |

### Status labels

| Badge | Meaning |
|-------|---------|
| `EXPECTED` | File matches the active mission scope |
| `SUSPICIOUS` | File changed but no mission rule matches |
| `BLOCKED` | File matched a blocked path policy (hard stop) |
| `IGNORED` | File is clean, stale, or excluded |

### Keyboard reference

| Key | Action |
|-----|--------|
| `1` / `2` / `3` / `4` / `5` | Switch mode |
| `Tab` / `Shift+Tab` | Move focus between panels |
| `↑` / `↓` / mouse wheel | Scroll file list or chat |
| `Enter` | Open full diff overlay for selected file |
| `Esc` | Close overlay / exit compose mode |
| `t` | Cycle through themes |
| `j` | Run judge on selected file |
| `a` / `b` | Allow / block selected file |
| `[` / `]` | Resize left/right panel split |
| `m` | Toggle mouse mode (on = scroll/click; off = native text select) |
| `?` / `h` | Toggle help overlay |
| `q` | Quit |

### Themes

Five built-in themes, switchable at any time:

| Theme | Description |
|-------|-------------|
| `agentscope` | Default dark theme |
| `codex` | Codex-inspired blue/gray |
| `claude` | Warm amber/cream |
| `openclaw` | OpenClaw-inspired green terminal |
| `high-contrast` | Maximum contrast for accessibility |

Switch with `t` key or from Chat: `/theme claude`

### Live mode

Press `5` or type `/live` to enter the file-watcher mode:

- **Pulsing `●` indicator** in the header shows the watcher is active
- **Freshness badges** on each file: `●` green (changed <5s), `~` yellow (<30s), blank (stable)
- **Inline diff** loads automatically when you select a file — no `Enter` needed
- **Line numbers** appear in both the inline panel and the full diff overlay (`{:>4} │` gutter)
- **Scroll position** shown in the overlay title: `23/156`
- Powered by OS inotify/FSEvents via `notify` — detects saves as they happen

### Chat mode

Press `2` to enter Chat. Chat is full-screen and has its own compose mode:

| Key | Action |
|-----|--------|
| `i` | Enter compose mode (start typing) |
| `Esc` | Exit compose mode (stay in Chat) |
| `Enter` | Send message (when composer is focused) |
| `/` | Open slash-command palette with Tab-completion |

**Slash commands (type `/` to see all):**

| Command | What it does |
|---------|--------------|
| `/judge` | Run the configured LLM judge |
| `/judge-provider [claude\|openai\|ollama\|gemini\|openrouter]` | Switch judge provider |
| `/judge-model [model]` | Set judge model |
| `/explain [selected\|file]` | Ask judge to explain a selected file's verdict |
| `/ask <question>` | Ask a quick question without typing in compose |
| `/status` | Refresh session and file summary |
| `/diff [file]` | Open colored diff for selected or named file |
| `/check` | Summarize policy status in the activity log |
| `/problems` | Toggle blocked/suspicious filter |
| `/allow [file\|glob]` | Persist an allow override in `agentscope.yaml` |
| `/block [file\|glob]` | Persist a blocked pattern in `agentscope.yaml` |
| `/theme [name]` | Switch the TUI theme |
| `/agents` | Show active/stale detected agent missions |
| `/mission` | Show full active mission context |
| `/refresh-agents` | Re-detect agent missions now |
| `/sessions [agent]` | List local agent sessions |
| `/latest [agent]` | Show the latest session per agent |
| `/new-chat [title]` | Create a new persistent chat session |
| `/chats` | List saved chat sessions |
| `/clear-chat` | Clear visible chat messages |
| `/live` | Open live file-change monitor |
| `/dashboard` | Jump to Dashboard mode |
| `/report` | Post a scope report in Chat |
| `/filter [suspicious\|all]` | Filter review files from Chat |
| `/help` | Show command help |
| `/quit` | Exit watch mode |

**Sender labels** are color-coded by background — no prefix clutter.

---

## What it checks

```text
EXPECTED    src/components/CheckoutButton.tsx   +28 -4
SUSPICIOUS  package.json                         +2 -2
BLOCKED     .env.local                           +1 -0

BLOCK  .env.local matched blocked path policy
JUDGE  ollama / qwen3.5:2b
DRIFT DETECTED — review suspicious files before commit
```

AgentScope has three enforcement layers:

| Layer | Purpose |
|-------|---------|
| **Git + policy** | Deterministic checks: blocked paths, warn paths, file/line limits |
| **Mission context** | Manual mission or inferred from local agent logs |
| **Judge** | Optional LLM drift review (local or cloud) |

Deterministic policy wins. A model can help explain drift, but it cannot make `.env` or protected auth paths safe.

---

## Install

Build from source (Rust 1.75+):

```bash
git clone https://github.com/abdouloued/agentscopev2.git
cd agentscopev2
cargo build --release
cp target/release/agentscope ~/.local/bin/
```

For local development:

```bash
cargo build
cargo test
./target/debug/agentscope --help
```

---

## Core workflow

### 1. Initialize once per repo

```bash
agentscope init
```

Creates `agentscope.yaml` and `.agentscope/` local session storage.

### 2. Start a mission manually

```bash
agentscope start "Fix the rate-limit bug in api/middleware.ts" --agent codex
```

Records the mission, agent label, git baseline commit, and timestamp.

### 3. Or attach to the current agent context

```bash
agentscope agents detect           # show all detected missions
agentscope agents doctor           # explain missing sources
agentscope agents context --agent codex
agentscope attach --agent auto     # dry-run: shows inferred mission
agentscope attach --agent auto --apply   # write to session.json
```

Low-confidence missions do not auto-attach. Use `agents doctor` when detection looks wrong.

### 4. Watch while the agent works

```bash
agentscope watch            # open TUI cockpit (Review mode by default)
agentscope monitor --agent auto   # detect + attach + watch in one step
```

### 5. Check before commit

```bash
agentscope diff --problems
agentscope check
agentscope check --json
```

Exit code `0` = no blocked files. Exit code `1` = policy violation found.

---

## Judge (LLM drift review)

### Supported providers

| Provider | Env var required | Notes |
|----------|-----------------|-------|
| **Ollama** | none | Local, private. Default provider. |
| **Claude** | `ANTHROPIC_API_KEY` | Anthropic cloud API |
| **OpenAI** | `OPENAI_API_KEY` | OpenAI cloud API |
| **Gemini** | `GEMINI_API_KEY` or `GOOGLE_API_KEY` | Google cloud API |
| **OpenRouter** | `OPENROUTER_API_KEY` | Routes to 100+ models |
| **None** | — | Disable judge, use deterministic policy only |

### Quick start with Ollama (local, private)

```bash
ollama pull qwen3.5:2b
agentscope judge -m qwen3.5:2b
```

### Switch provider

```bash
# CLI
agentscope config set judge.provider claude
agentscope config set judge.model claude-3-5-haiku-20241022

# Or from Chat mode
/judge-provider claude
/judge-model claude-3-5-haiku-20241022

# Or from the TUI
j  (runs judge on selected file with current provider)
```

### Config

```yaml
judge:
  enabled: true
  provider: ollama          # ollama | claude | openai | gemini | openrouter | none
  model: "qwen3.5:2b"
  endpoint: "http://localhost:11434"   # only used for Ollama
```

---

## Policy

Edit `agentscope.yaml`:

```yaml
policy:
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
  warn:
    - "package-lock.json"
    - "yarn.lock"
    - "Cargo.lock"
    - "**/config/**"
  max_files_changed: 20
  max_lines_changed: 800
```

Blocked patterns are hard stops — the judge cannot override them. Warn patterns appear as `SUSPICIOUS` and go to the judge for review.

---

## Agent-aware monitoring

Supported local context readers:

| Agent | Default local source |
|-------|---------------------|
| Claude Code | `~/.claude/projects/**/{*.jsonl,*.json,*.txt,*.md}` |
| Codex CLI | `~/.codex/sessions/**/rollout-*.jsonl` |
| Codex App | `~/Library/Application Support/Codex/sessions` |
| OpenCode | `~/.local/share/opencode/project/**/storage/` |
| OpenClaw | `~/.openclaw/{agents,sessions}` |
| Hermes Agent | `~/.hermes/{agents,sessions}` |
| Cursor | `~/.cursor/projects/**/agent-transcripts/` |
| Gemini CLI | `~/.gemini/tmp/**/chats/` |
| Antigravity | `~/.gemini/antigravity-cli`, `~/Library/Application Support/Antigravity*` |
| GitHub Copilot CLI | `~/.copilot/session-state/` |
| VS Code Copilot Chat | `workspaceStorage/**/GitHub.copilot-chat/transcripts/` |

Detection is **local-only**. AgentScope does not upload transcripts. It extracts the latest usable user task, filters out tool calls, patch hunks, and metadata, then returns a confidence score.

Override paths in `agentscope.yaml`:

```yaml
agents:
  auto_detect: true
  auto_attach: false
  preferred:
    - codex
    - claude-code
    - hermes
    - openclaw
    - cursor
    - antigravity
  sources:
    codex:
      enabled: true
      paths:
        - "~/.codex/sessions"
        - "~/Library/Application Support/Codex/sessions"
    hermes:
      enabled: true
      paths:
        - "~/.hermes/sessions"
    copilot-cli:
      enabled: true
      paths:
        - "~/.copilot/session-state"
    antigravity:
      enabled: true
      paths:
        - "~/.gemini/antigravity-cli"
        - "~/Library/Application Support/Antigravity IDE/User/globalStorage"
```

---

## Full command reference

| Command | What it does |
|---------|--------------|
| `agentscope init` | Create `agentscope.yaml` and local session storage |
| `agentscope start "mission" --agent codex` | Start a manual mission |
| `agentscope watch` | Open the TUI cockpit |
| `agentscope monitor --agent auto` | Detect context, attach, and watch in one step |
| `agentscope agents detect` | Show supported agents and detected missions |
| `agentscope agents doctor` | Explain missing sources and checked paths |
| `agentscope agents context --agent auto` | Print one inferred context in detail |
| `agentscope attach --agent auto` | Dry-run mission inference |
| `agentscope attach --agent auto --apply` | Write inferred mission to `.agentscope/session.json` |
| `agentscope diff --problems` | Show only suspicious and blocked changed files |
| `agentscope check` | Enforce policy and scope checks |
| `agentscope check --json` | Machine-readable policy check output |
| `agentscope judge -m qwen3.5:2b` | Run optional LLM drift review |
| `agentscope model list` | List judge models and providers |
| `agentscope model set <model>` | Set default judge model |
| `agentscope config show` | Print effective configuration |
| `agentscope config set <key> <value>` | Set a configuration value |
| `agentscope config edit` | Open config file in `$EDITOR` |
| `agentscope config reset [solo\|team\|ci]` | Reset to a preset |
| `agentscope hook install` | Install a pre-commit safety hook |
| `agentscope hook uninstall` | Remove the pre-commit hook |
| `agentscope report --markdown` | Generate a shareable report |
| `agentscope mcp` | Expose JSON-RPC tools for compatible agents |
| `agentscope skills install --agent all` | Generate project-local instruction files |
| `agentscope plugins install --agent all` | Generate project-local plugin assets |

---

## MCP, skills, and plugins

`agentscope mcp` exposes these JSON methods:

| Method | Purpose |
|--------|---------|
| `scope_status` | Return the active session |
| `scope_check` | Point compatible tools to the terminal check path |
| `scope_start` | Point compatible tools to session creation |
| `agent_detect` | Return all supported agent detections |
| `agent_context` | Return one agent context |
| `agent_attach` | Point compatible tools to safe attach behavior |

Skills and plugins are generated local assets (not a marketplace integration). They give agents and editors clear instructions for when to run AgentScope:

```bash
agentscope skills list --agent all
agentscope skills install --agent codex
agentscope plugins install --agent all
```

---

## Configuration presets

```bash
agentscope config reset solo    # individual developer, max_files 20, judge enabled
agentscope config reset team    # shared logs, max_files 10, judge enabled
agentscope config reset ci      # max_files 5, judge disabled, JSON output
```

---

## CI

```bash
agentscope check --json > agentscope-report.json
agentscope check
```

GitHub Actions example:

```yaml
- name: Audit agent changes
  run: |
    agentscope check --json > agentscope-report.json
    agentscope check
```

---

## Troubleshooting

### `watch` shows the old mission

`watch` reads `.agentscope/session.json`. Update it:

```bash
agentscope start "new mission" --agent codex
# or
agentscope attach --agent auto --apply
```

### Agent detection says `not found`

```bash
agentscope agents doctor
```

| Situation | Fix |
|-----------|-----|
| Agent has no logs yet | Run the agent once, then detect again |
| Agent stores logs elsewhere | Add `agents.sources.<agent>.paths` in `agentscope.yaml` |
| Detection confidence is low | Use `agentscope start "mission"` |
| Multiple agents present | Reorder `agents.preferred` |

### The inferred mission is wrong

```bash
agentscope start "exact mission here" --agent codex
```

Then open an issue with a sanitized sample of the local log format.

### Judge returns an error

Check the required env var for your provider:

| Provider | Env var |
|----------|---------|
| Claude | `ANTHROPIC_API_KEY` |
| OpenAI | `OPENAI_API_KEY` |
| Gemini | `GEMINI_API_KEY` or `GOOGLE_API_KEY` |
| OpenRouter | `OPENROUTER_API_KEY` |
| Ollama | none (requires local Ollama running on port 11434) |

### Terminal is too narrow

AgentScope adapts to terminal width:

- **≥ 120 cols**: Side-by-side file list + decision panel
- **< 120 cols**: Stacked vertical layout
- **< 80 cols**: Single-column fallback

Resize your terminal or use `[` / `]` to adjust the panel split.

---

## Development

```bash
cargo fmt
cargo test
cargo build
cargo clippy --all-targets --all-features -- -D warnings
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for contributor workflow and project structure.

---

## License

MIT + Commons Clause. Free to use and modify; commercial resale/hosting of
AgentScope as a service is not permitted. See [LICENSE](LICENSE).
