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
| Claude Code | `~/.claude/projects` |
| Codex CLI / app | `~/.codex/sessions` |
| OpenCode | `~/.local/share/opencode/project` |
| Cursor | `~/.cursor/projects` |
| Gemini CLI | `~/.gemini/tmp` |
| Copilot CLI | `~/.copilot/session-state` |

AgentScope recursively scans likely JSON, JSONL, text, markdown, chat, transcript, and rollout files under those roots.

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
    - claude-code
    - cursor
    - gemini-cli
    - opencode
    - copilot-cli
  sources:
    codex:
      enabled: true
      paths:
        - "~/.codex/sessions"
    gemini-cli:
      enabled: false
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
