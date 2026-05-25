# Quickstart

Use this when you want AgentScope running in a real coding session.

## Manual mission

```bash
agentscope init
agentscope start "Fix checkout button loading state" --agent codex
```

Run your coding agent normally.

```bash
agentscope watch
agentscope check
```

## Agent-aware mission

```bash
agentscope init
agentscope agents doctor
agentscope agents detect
agentscope attach --agent auto
```

If the dry run looks right:

```bash
agentscope attach --agent auto --apply
agentscope monitor --agent auto
```

## Before commit

```bash
agentscope diff --problems
agentscope check
```

Optional:

```bash
agentscope judge -m qwen3.5:2b
agentscope report --markdown
```

## If something is missing

```bash
agentscope agents doctor
```

Then either update `agentscope.yaml` with a custom source path, or skip detection for this session:

```bash
agentscope start "the exact mission" --agent codex
```
