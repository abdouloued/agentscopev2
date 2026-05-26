# Agent-Aware Monitoring

AgentScope can work in two modes:

1. Manual mission mode: you run `agentscope start "mission"`.
2. Agent-aware mode: AgentScope reads local agent context and suggests or attaches the mission.

Manual mode is always supported. Agent-aware mode is a convenience layer for product-ready workflows where users should not have to retype the same prompt into config every time.

## Recommended user flow

```bash
agentscope init
agentscope agents doctor
agentscope agents detect
agentscope attach --agent auto
agentscope attach --agent auto --apply
agentscope monitor --agent auto
```

Use `attach` first as a dry run. It prints:

- detected agent
- inferred mission
- confidence
- source file

Only `--apply` writes `.agentscope/session.json`.

## Supported sources

| Agent | Default source |
|---|---|
| Claude Code | `$CLAUDE_CONFIG_DIR/projects` or `~/.claude/projects` |
| Codex CLI | `$CODEX_HOME/sessions` or `~/.codex/sessions` |
| Codex App | `$CODEX_APP_HOME/sessions`, `~/Library/Application Support/Codex/sessions`, or related Codex app session files |
| OpenCode | `$OPENCODE_DATA_DIR/project` or `~/.local/share/opencode/project` |
| OpenClaw | `$OPENCLAW_HOME/{agents,sessions}` or `~/.openclaw/{agents,sessions}` |
| Hermes Agent | `$HERMES_HOME/{agents,sessions}` or `~/.hermes/{agents,sessions}` |
| Cursor | `~/.cursor/projects` |
| Gemini CLI | `~/.gemini/tmp` |
| Antigravity | `~/.gemini/antigravity-cli`, `~/.gemini/antigravity*`, `~/.antigravity`, or `~/Library/Application Support/Antigravity*` |
| GitHub Copilot CLI / VS Code Copilot Chat | `$COPILOT_HOME/session-state`, `~/.copilot/session-state`, and VS Code `workspaceStorage/**/GitHub.copilot-chat/transcripts` |

AgentScope recursively scans likely JSON, JSONL, text, markdown, chat, transcript, and rollout files under those roots.

Native and Ollama launch aliases that AgentScope recognizes:

```bash
ollama launch claude --model qwen3.5
open -a Codex
ollama launch openclaw --model qwen3.5
ollama launch hermes --model qwen3.5
ollama launch codex --model qwen3.5
ollama launch opencode --model qwen3.5
```

## Missing sources

Missing is not an error. It usually means:

- the agent is not installed
- the agent has not created local history yet
- the user is on a different product version
- the logs live in a custom location
- the latest log contains only metadata, tool calls, or login commands

Use:

```bash
agentscope agents doctor
```

Then choose:

| Need | Command |
|---|---|
| Continue immediately | `agentscope start "mission"` |
| Inspect one agent | `agentscope agents context --agent codex` |
| Override a path | Edit `agentscope.yaml` |
| Disable noisy source | `agents.sources.<agent>.enabled: false` |

## Config

```yaml
agents:
  auto_detect: true
  auto_attach: false
  preferred:
    - codex
    - codex-app
    - claude-code
    - hermes
    - cursor
    - gemini-cli
    - antigravity
    - opencode
    - openclaw
    - copilot-cli
  sources:
    codex:
      enabled: true
      paths:
        - "~/.codex/sessions"
    codex-app:
      enabled: true
      paths:
        - "~/Library/Application Support/Codex/sessions"
    hermes:
      enabled: true
      paths:
        - "~/.hermes/sessions"
    copilot-cli:
      enabled: true
      paths:
        - "~/.copilot/session-state"
        - "~/Library/Application Support/Code/User/workspaceStorage"
    gemini-cli:
      enabled: false
    antigravity:
      enabled: true
      paths:
        - "~/.gemini/antigravity-cli"
        - "~/Library/Application Support/Antigravity IDE/User/globalStorage"
```

## Confidence rules

AgentScope gives higher confidence when:

- the source belongs to a supported agent
- the file exists and is recent
- the extracted mission has enough words to look like a real task

AgentScope refuses to attach very low-confidence missions. Auto-attach is off by default and should stay opt-in.

## What gets filtered

AgentScope ignores common non-mission text:

- assistant, system, developer, and tool messages
- tool calls and tool outputs
- patch markers such as `*** Begin Patch`
- diff hunks
- timestamps and file paths
- JSON metadata fields
- login and slash commands such as `/model`
- Codex app browser wrapper text before `My request for Codex:`

## MCP, skills, and plugins

`agentscope mcp` exposes JSON-style methods for compatible tools:

- `scope_status`
- `scope_check`
- `scope_start`
- `agent_detect`
- `agent_context`
- `agent_attach`

`agentscope skills install` and `agentscope plugins install` create project-local assets. They do not claim native marketplace installs or automatic Stop hooks.

## Product principle

AgentScope should make the best path easy without hiding uncertainty:

- detect automatically
- show source and confidence
- dry-run before write
- fall back to manual mission
- keep enforcement in deterministic Git and policy checks
