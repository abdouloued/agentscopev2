# AgentScope Agent-Aware Monitoring Status

## Summary

Agent-aware monitoring is implemented for the first production pass.

AgentScope now reads local agent context best-effort, infers the active mission, exposes that context through CLI and MCP-style JSON, and keeps Git/filesystem policy checks as the enforcement layer.

Safe default: AgentScope does not silently overwrite `.agentscope/session.json`. It suggests inferred missions unless the user runs `agentscope attach --agent auto --apply` or explicitly enables auto-attach.

## Implemented

- Local context readers for:
  - Claude Code
  - Codex CLI / Codex app
  - OpenCode
  - Cursor
  - Gemini CLI
  - Copilot CLI
- CLI commands:
  - `agentscope agents detect`
  - `agentscope agents doctor`
  - `agentscope agents context --agent <agent|auto>`
  - `agentscope attach --agent <agent|auto> [--apply]`
  - `agentscope monitor --agent <agent|auto> [--auto-attach]`
  - `agentscope mcp`
  - `agentscope skills list/install --agent <agent|all>`
  - `agentscope plugins list/install --agent <agent|all>`
- Config:
  - `agents.auto_detect`
  - `agents.auto_attach`
  - `agents.preferred`
  - `agents.sources.<agent>.enabled`
  - `agents.sources.<agent>.paths`
- Session metadata:
  - `mission_source`
  - `mission_confidence`
  - `detected_agent`
  - `source_path`
- Mission extraction filters for:
  - tool calls
  - tool outputs
  - patch markers
  - diff hunks
  - timestamps
  - metadata fields
  - file paths
  - login and slash commands
  - Codex app browser context wrappers

## Behavior Rules

- Deterministic Git and policy checks remain authoritative.
- Agent-log mission inference improves setup and monitoring context only.
- Local log paths are defaults, not guarantees.
- Missing sources are normal and handled by `agentscope agents doctor`.
- Logs are read locally.
- Auto-attach is opt-in.
- Low-confidence missions do not auto-attach.

## Documentation

User-facing docs live in:

- `README.md`
- `docs/quickstart.md`
- `docs/agent-aware-monitoring.md`

The public site should present shipped features first, then clearly label MCP/skills/plugins as generated local integration assets rather than native marketplace installs.

## Verification

Current passing checks:

- `cargo test`
- `cargo build`
- `agentscope agents doctor`
- `agentscope agents detect`
- `agentscope mcp`
- `agentscope attach --agent codex`

## Next Product Improvements

- Show mission source and confidence inside the TUI header.
- Add richer MCP request/response docs.
- Generate more useful agent-specific `SKILL.md` files instead of placeholder README assets.
- Add examples for custom agent source paths.
- Publish install/release instructions once the public release path is final.
