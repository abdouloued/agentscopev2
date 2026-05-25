//! Git hook management for AgentScope.
//! Installs/uninstalls a pre-commit hook that enforces scope policy.

use anyhow::Result;
use std::fs;
use std::path::Path;

use crate::output::Printer;

const HOOK_PATH: &str = ".git/hooks/pre-commit";
const HOOK_BACKUP: &str = ".git/hooks/pre-commit.bak";

const HOOK_MARKER: &str = "# AgentScope pre-commit hook";

const HOOK_SCRIPT: &str = r#"#!/bin/sh
# AgentScope pre-commit hook
# Automatically checks scope compliance before every commit.
# To skip: git commit --no-verify

if [ -f ".agentscope/session.json" ]; then
    echo ""
    echo "🔍 AgentScope: checking scope compliance..."
    echo ""

    agentscope check 2>/dev/null
    EXIT_CODE=$?

    if [ $EXIT_CODE -ne 0 ]; then
        echo ""
        echo "❌ AgentScope: commit BLOCKED — policy violations found"
        echo ""
        echo "   Fix the violations, then commit again."
        echo "   Or skip this check with: git commit --no-verify"
        echo ""
        exit 1
    fi

    echo ""
    echo "✅ AgentScope: all changes in scope — proceeding with commit"
    echo ""
fi
"#;

pub async fn install() -> Result<()> {
    let p = Printer::new();

    // Make sure we're in a git repo
    let hooks_dir = Path::new(".git/hooks");
    if !hooks_dir.exists() {
        anyhow::bail!("Not a git repository. Run `git init` first.");
    }

    // Check if hook already exists
    let hook_path = Path::new(HOOK_PATH);
    if hook_path.exists() {
        let existing = fs::read_to_string(hook_path)?;
        if existing.contains(HOOK_MARKER) {
            p.warn("AgentScope pre-commit hook is already installed");
            return Ok(());
        }

        // Back up existing hook
        fs::copy(hook_path, HOOK_BACKUP)?;
        p.hint(&format!("Backed up existing hook → {}", HOOK_BACKUP));
    }

    // Write our hook
    fs::write(hook_path, HOOK_SCRIPT)?;

    // Make executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o755);
        fs::set_permissions(hook_path, perms)?;
    }

    p.success("Installed pre-commit hook");
    p.hint("Every commit will now run `agentscope check` automatically.");
    p.hint("Skip with: git commit --no-verify");

    Ok(())
}

pub async fn uninstall() -> Result<()> {
    let p = Printer::new();

    let hook_path = Path::new(HOOK_PATH);
    if !hook_path.exists() {
        p.hint("No pre-commit hook found — nothing to remove");
        return Ok(());
    }

    let contents = fs::read_to_string(hook_path)?;
    if !contents.contains(HOOK_MARKER) {
        p.warn("Pre-commit hook exists but was NOT installed by AgentScope — skipping");
        p.hint("Remove it manually if needed.");
        return Ok(());
    }

    fs::remove_file(hook_path)?;

    // Restore backup if it exists
    let backup_path = Path::new(HOOK_BACKUP);
    if backup_path.exists() {
        fs::rename(backup_path, hook_path)?;
        p.success("Removed AgentScope hook, restored previous hook from backup");
    } else {
        p.success("Removed AgentScope pre-commit hook");
    }

    Ok(())
}

pub async fn status() -> Result<()> {
    let p = Printer::new();

    let hook_path = Path::new(HOOK_PATH);
    if !hook_path.exists() {
        println!(
            "  {} pre-commit hook: {}",
            console::style("○").dim(),
            console::style("not installed").dim(),
        );
        p.hint("Install with: agentscope hook install");
        return Ok(());
    }

    let contents = fs::read_to_string(hook_path)?;
    if contents.contains(HOOK_MARKER) {
        println!(
            "  {} pre-commit hook: {}",
            console::style("●").green(),
            console::style("installed (AgentScope)").green().bold(),
        );
        p.hint("Every commit runs `agentscope check` automatically.");
        p.hint("Remove with: agentscope hook uninstall");
    } else {
        println!(
            "  {} pre-commit hook: {}",
            console::style("●").yellow(),
            console::style("installed (third-party)").yellow(),
        );
        p.hint("A non-AgentScope hook is present. Install will back it up first.");
    }

    Ok(())
}
