# AgentScope skill · cursor

This file instructs `cursor` to use AgentScope as a scope firewall before and after making changes.

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
