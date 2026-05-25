# AgentScope

AgentScope is a Rust CLI that keeps AI coding agents accountable to the work you actually asked for.

It records or detects a mission, watches Git changes, applies deterministic policy, and optionally asks a judge model whether the diff still matches the mission.

AgentScope does not replace Codex, Claude Code, Cursor, Gemini CLI, OpenCode, or Copilot. It sits beside them as a repo safety layer.

## The 30-second version

```bash
agentscope init
agentscope start "Fix checkout button loading state" --agent codex

# Run your coding agent normally.

agentscope watch
agentscope check
```

If you are already inside a supported agent session, let AgentScope infer the mission from local agent logs:

```bash
agentscope agents detect
agentscope attach --agent auto
agentscope attach --agent auto --apply
agentscope monitor --agent auto
```

Safe default: `attach` is a dry run. It prints the inferred mission, source path, and confidence. It writes `.agentscope/session.json` only with `--apply`.

## What it checks

```text
IN SCOPE   src/components/CheckoutButton.tsx   +28 -4
UNASKED    package.json                         +2 -2
BLOCKED    .env.local                           +1 -0

BLOCK .env.local matched blocked path policy
JUDGE ollama / qwen3.5:2b
DRIFT DETECTED - review unasked files before commit
```

AgentScope has two layers:

| Layer | Purpose |
|---|---|
| Git and policy | Deterministic checks for changed files, blocked paths, warn paths, and size limits. |
| Mission context | Manual mission from `agentscope start`, or inferred mission from supported local agent logs. |
| Judge | Optional model-based drift review through Ollama, Claude, or OpenAI. |

Deterministic policy wins. A model can help explain drift, but it cannot make `.env` or protected auth paths safe.

## Install

Build from source:

```bash
git clone git@github.com:abdouloued/agentscopev2.git
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

## Core workflow

### 1. Initialize once per repo

```bash
agentscope init
```

This creates:

```text
agentscope.yaml
.agentscope/
```

### 2. Start a mission manually

```bash
agentscope start "Fix the rate-limit bug in api/middleware.ts" --agent codex
```

This records:

| Field | Meaning |
|---|---|
| mission | The work the agent is supposed to do. |
| agent | A label for the tool doing the work. |
| git baseline | The commit AgentScope diffs against. |
| started_at | Session timestamp. |

### 3. Or attach to the current agent context

```bash
agentscope agents detect
agentscope agents doctor
agentscope agents context --agent codex
agentscope attach --agent auto
agentscope attach --agent auto --apply
```

AgentScope reads local logs best-effort. Missing sources are normal: they usually mean that agent is not installed, has not created logs yet, or stores logs somewhere custom.

Use `agentscope agents doctor` when detection looks wrong. It shows the paths checked and the fallback command:

```bash
agentscope start "your mission"
```

### 4. Watch while the agent works

```bash
agentscope watch
```

`watch` shows the active session from `.agentscope/session.json`.

```bash
agentscope monitor --agent auto
```

`monitor` first tries to infer the current agent mission, optionally auto-attaches high-confidence missions, then opens the live TUI.

### 5. Check before commit

```bash
agentscope diff --problems
agentscope check
agentscope check --json
```

Exit code `0` means no blocked files were found. Exit code `1` means AgentScope found a policy violation.

## Agent-aware monitoring

Supported local context readers:

| Agent | Default local source |
|---|---|
| Claude Code | `~/.claude/projects/**/{*.jsonl,*.json,*.txt,*.md}` |
| Codex CLI / Codex app | `~/.codex/sessions/**/rollout-*.jsonl` and related session files |
| OpenCode | `~/.local/share/opencode/project/**/storage/` |
| Cursor | `~/.cursor/projects/**/agent-transcripts/` |
| Gemini CLI | `~/.gemini/tmp/**/chats/` plus nearby JSON logs |
| Copilot CLI | `~/.copilot/session-state/` |

Detection is local-only. AgentScope does not upload transcripts. It extracts the latest usable user task, filters out tool calls, patch hunks, metadata, login commands, and app wrapper text, then returns a confidence score.

Override paths in `agentscope.yaml` when an agent stores logs somewhere else:

```yaml
agents:
  auto_detect: true
  auto_attach: false
  preferred:
    - codex
    - claude-code
    - cursor
  sources:
    codex:
      enabled: true
      paths:
        - "~/.codex/sessions"
        - "~/Library/Application Support/Codex/sessions"
    gemini-cli:
      enabled: false
```

Product-ready rule: use automatic detection to reduce typing, not to hide uncertainty. Low-confidence missions do not auto-attach.

## Commands

| Command | What it does |
|---|---|
| `agentscope init` | Create `agentscope.yaml` and local session storage. |
| `agentscope start "mission" --agent codex` | Start a manual mission. |
| `agentscope agents detect` | Show supported agents and detected missions. |
| `agentscope agents doctor` | Explain missing sources and checked paths. |
| `agentscope agents context --agent auto` | Print one inferred context in detail. |
| `agentscope attach --agent auto` | Dry-run mission inference. |
| `agentscope attach --agent auto --apply` | Write inferred mission to `.agentscope/session.json`. |
| `agentscope watch` | Live TUI for the active manual or attached session. |
| `agentscope monitor --agent auto` | Detect context, optionally attach, then watch. |
| `agentscope diff --problems` | Show only unasked and blocked changed files. |
| `agentscope check` | Enforce policy and scope checks. |
| `agentscope judge -m qwen3.5:2b` | Run optional LLM drift review. |
| `agentscope model list` | List judge models/providers. |
| `agentscope config show` | Print effective config. |
| `agentscope hook install` | Install a pre-commit safety hook. |
| `agentscope report --markdown` | Generate a shareable report. |
| `agentscope mcp` | Expose JSON-RPC style tools for compatible agents. |
| `agentscope skills install --agent all` | Generate project-local instruction files. |
| `agentscope plugins install --agent all` | Generate project-local plugin assets. |

## MCP, skills, and plugins

`agentscope mcp` exposes these JSON methods:

| Method | Purpose |
|---|---|
| `scope_status` | Return the active session if one exists. |
| `scope_check` | Point compatible tools to the terminal check path. |
| `scope_start` | Point compatible tools to session creation. |
| `agent_detect` | Return all supported agent detections. |
| `agent_context` | Return one agent context. |
| `agent_attach` | Point compatible tools to safe attach behavior. |

Skills and plugins are generated local assets. They are not a marketplace integration and do not install native Stop hooks. They give agents and projects clear instructions for when to run AgentScope.

```bash
agentscope skills list --agent all
agentscope skills install --agent codex
agentscope plugins install --agent all
```

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

## Judge

The default judge provider is Ollama with `qwen3.5:2b`:

```bash
ollama pull qwen3.5:2b
agentscope judge -m qwen3.5:2b
```

Config:

```yaml
judge:
  enabled: true
  provider: ollama
  model: "qwen3.5:2b"
  endpoint: "http://localhost:11434"
```

Supported providers:

| Provider | Notes |
|---|---|
| Ollama | Local by default. |
| Claude | Cloud API provider. |
| OpenAI | Cloud API provider. |
| None | Disable judge and use deterministic policy only. |

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

## Troubleshooting

### `watch` keeps showing the old mission

`agentscope watch` uses the active `.agentscope/session.json`. Update it with one of:

```bash
agentscope start "new mission" --agent codex
agentscope attach --agent auto --apply
```

### Agent detection says `not found`

Run:

```bash
agentscope agents doctor
```

Then either:

| Situation | Fix |
|---|---|
| Agent has no logs yet | Run the agent once, then detect again. |
| Agent stores logs elsewhere | Add `agents.sources.<agent>.paths` in `agentscope.yaml`. |
| Detection confidence is low | Use manual `agentscope start "mission"`. |
| Multiple agents are present | Reorder `agents.preferred`. |

### The inferred mission is wrong

Use manual start for that session:

```bash
agentscope start "exact mission here" --agent codex
```

Then open an issue with a sanitized sample of the local log format so the reader can improve.

## Development

```bash
cargo fmt
cargo test
cargo build
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for contributor workflow and project structure.

## License

MIT. See [LICENSE](LICENSE).
