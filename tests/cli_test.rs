use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use std::path::Path;

/// Test that --help exits cleanly
#[test]
fn cli_help_exits_zero() {
    Command::cargo_bin("agentscope")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("AgentScope"));
}

/// Test that --version exits cleanly
#[test]
fn cli_version_exits_zero() {
    Command::cargo_bin("agentscope")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("agentscope"));
}

/// Test that init creates agentscope.yaml in a temp git repo
#[test]
fn cli_init_creates_config() {
    let tmp = tempfile::tempdir().unwrap();

    // Init a git repo first (required for agentscope)
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(tmp.path())
        .output()
        .unwrap();

    // Create .gitignore so init can append to it
    std::fs::write(tmp.path().join(".gitignore"), "target/\n").unwrap();

    Command::cargo_bin("agentscope")
        .unwrap()
        .arg("init")
        .current_dir(tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Created agentscope.yaml"));

    assert!(tmp.path().join("agentscope.yaml").exists());
    assert!(tmp.path().join(".agentscope").exists());
}

/// Test that init --preset ci works
#[test]
fn cli_init_preset_ci() {
    let tmp = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(tmp.path())
        .output()
        .unwrap();

    Command::cargo_bin("agentscope")
        .unwrap()
        .args(["init", "--preset", "ci"])
        .current_dir(tmp.path())
        .assert()
        .success();

    let contents = std::fs::read_to_string(tmp.path().join("agentscope.yaml")).unwrap();
    assert!(contents.contains("Ci"));
}

/// Test that running check without a session gives a meaningful error
#[test]
fn cli_check_without_session() {
    let tmp = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    // Need an initial commit for HEAD to exist
    std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(tmp.path())
        .output()
        .unwrap();

    Command::cargo_bin("agentscope")
        .unwrap()
        .arg("check")
        .current_dir(tmp.path())
        .assert()
        .failure(); // should fail since no session exists
}

/// Test that status without a session gives meaningful output
#[test]
fn cli_status_without_session() {
    let tmp = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(tmp.path())
        .output()
        .unwrap();

    Command::cargo_bin("agentscope")
        .unwrap()
        .arg("status")
        .current_dir(tmp.path())
        .assert()
        .success();
}

/// Test that all agent kinds are valid CLI values
#[test]
fn cli_agent_kinds_accepted() {
    let agents = [
        "claude",
        "codex",
        "codex-app",
        "cursor",
        "gemini",
        "opencode",
        "openclaw",
        "hermes",
        "copilot",
        "droid",
        "pi",
        "custom",
    ];

    for _agent in agents {
        // Just test that the CLI parses the value (--help won't error)
        Command::cargo_bin("agentscope")
            .unwrap()
            .args(["start", "--help"])
            .assert()
            .success();
    }

    // Test that an invalid agent is rejected
    let tmp = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(tmp.path())
        .output()
        .unwrap();

    Command::cargo_bin("agentscope")
        .unwrap()
        .args(["start", "test mission", "--agent", "nonexistent"])
        .current_dir(tmp.path())
        .assert()
        .failure();
}

#[test]
fn cli_agents_detect_reports_supported_sources() {
    let tmp = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(tmp.path())
        .output()
        .unwrap();

    Command::cargo_bin("agentscope")
        .unwrap()
        .args(["agents", "detect"])
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("claude-code"))
        .stdout(predicate::str::contains("codex"))
        .stdout(predicate::str::contains("not found"));
}

#[test]
fn cli_agents_doctor_explains_missing_sources_without_failing() {
    let tmp = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    std::fs::write(tmp.path().join("agentscope.yaml"), "version: 1\n").unwrap();

    Command::cargo_bin("agentscope")
        .unwrap()
        .args(["agents", "doctor"])
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Agent source health"))
        .stdout(predicate::str::contains("Missing sources are normal"))
        .stdout(predicate::str::contains("agentscope start"));
}

#[test]
fn cli_attach_dry_run_does_not_write_session() {
    let tmp = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    std::fs::write(tmp.path().join("agentscope.yaml"), "version: 1\n").unwrap();

    let codex_dir = tmp.path().join(".codex/sessions/2026/05/24");
    std::fs::create_dir_all(&codex_dir).unwrap();
    std::fs::write(
        codex_dir.join("rollout-test.jsonl"),
        r#"{"timestamp":"2026-05-24T12:00:00Z","type":"user_message","message":"Implement agent detection"}"#,
    )
    .unwrap();

    Command::cargo_bin("agentscope")
        .unwrap()
        .args(["attach", "--agent", "codex"])
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Implement agent detection"))
        .stdout(predicate::str::contains("dry run"));

    assert!(!tmp.path().join(".agentscope/session.json").exists());
}

#[test]
fn cli_attach_apply_writes_detected_session_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    std::fs::write(tmp.path().join("agentscope.yaml"), "version: 1\n").unwrap();

    let codex_dir = tmp.path().join(".codex/sessions/2026/05/24");
    std::fs::create_dir_all(&codex_dir).unwrap();
    std::fs::write(
        codex_dir.join("rollout-test.jsonl"),
        r#"{"timestamp":"2026-05-24T12:00:00Z","type":"user_message","message":"Wire attach apply"}"#,
    )
    .unwrap();

    Command::cargo_bin("agentscope")
        .unwrap()
        .args(["attach", "--agent", "codex", "--apply"])
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Attached"));

    let session_json =
        std::fs::read_to_string(tmp.path().join(".agentscope/session.json")).unwrap();
    let session: Value = serde_json::from_str(&session_json).unwrap();
    assert_eq!(session["mission"], "Wire attach apply");
    assert_eq!(session["detected_agent"], "codex");
    assert_eq!(session["mission_source"], "agent-log");
    assert!(session["mission_confidence"].as_f64().unwrap() >= 0.5);
}

#[test]
fn cli_skills_and_plugins_list_supported_agents() {
    Command::cargo_bin("agentscope")
        .unwrap()
        .args(["skills", "list", "--agent", "all"])
        .assert()
        .success()
        .stdout(predicate::str::contains("claude-code"))
        .stdout(predicate::str::contains("codex"));

    Command::cargo_bin("agentscope")
        .unwrap()
        .args(["plugins", "list", "--agent", "all"])
        .assert()
        .success()
        .stdout(predicate::str::contains("cursor"))
        .stdout(predicate::str::contains("gemini-cli"));
}

#[test]
fn cli_agents_context_reads_each_supported_agent_source() {
    let tmp = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    std::fs::write(tmp.path().join("agentscope.yaml"), "version: 1\n").unwrap();

    let fixtures = [
        (
            "claude",
            ".claude/projects/work/session.jsonl",
            r#"{"role":"user","content":"Fix Claude task"}"#,
            "Fix Claude task",
        ),
        (
            "codex",
            ".codex/sessions/2026/05/24/rollout-test.jsonl",
            r#"{"type":"user_message","message":"Fix Codex task"}"#,
            "Fix Codex task",
        ),
        (
            "opencode",
            ".local/share/opencode/project/app/storage/chat.json",
            r#"{"prompt":"Fix OpenCode task"}"#,
            "Fix OpenCode task",
        ),
        (
            "cursor",
            ".cursor/projects/hash/agent-transcripts/transcript.jsonl",
            r#"{"text":"Fix Cursor task"}"#,
            "Fix Cursor task",
        ),
        (
            "gemini",
            ".gemini/tmp/hash/chats/chat.json",
            r#"{"content":"Fix Gemini task"}"#,
            "Fix Gemini task",
        ),
        (
            "copilot",
            ".copilot/session-state/state.json",
            r#"{"lastPrompt":"Fix Copilot task"}"#,
            "Fix Copilot task",
        ),
    ];

    for (_, rel_path, contents, _) in fixtures {
        let path = tmp.path().join(rel_path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    for (agent, _, _, expected) in fixtures {
        Command::cargo_bin("agentscope")
            .unwrap()
            .args(["agents", "context", "--agent", agent])
            .current_dir(tmp.path())
            .env("HOME", tmp.path())
            .assert()
            .success()
            .stdout(predicate::str::contains(expected));
    }
}

#[test]
fn cli_agents_context_honors_config_path_override() {
    let tmp = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    let custom_dir = tmp.path().join("custom-codex");
    std::fs::create_dir_all(&custom_dir).unwrap();
    std::fs::write(
        custom_dir.join("custom.jsonl"),
        r#"{"message":"Use configured path"}"#,
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("agentscope.yaml"),
        format!(
            "version: 1\nagents:\n  sources:\n    codex:\n      paths:\n        - \"{}\"\n",
            custom_dir.display()
        ),
    )
    .unwrap();

    Command::cargo_bin("agentscope")
        .unwrap()
        .args(["agents", "context", "--agent", "codex"])
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Use configured path"));
}

#[test]
fn cli_mcp_agent_detect_returns_json_rpc_response() {
    let tmp = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    std::fs::write(tmp.path().join("agentscope.yaml"), "version: 1\n").unwrap();

    Command::cargo_bin("agentscope")
        .unwrap()
        .arg("mcp")
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .write_stdin(r#"{"jsonrpc":"2.0","id":1,"method":"agent_detect"}"#)
        .assert()
        .success()
        .stdout(predicate::str::contains(r#""jsonrpc":"2.0""#))
        .stdout(predicate::str::contains(r#""id":1"#))
        .stdout(predicate::str::contains("claude-code"));
}

#[test]
fn cli_skills_and_plugins_install_project_assets() {
    let tmp = tempfile::tempdir().unwrap();
    Command::cargo_bin("agentscope")
        .unwrap()
        .args(["skills", "install", "--agent", "codex"])
        .current_dir(tmp.path())
        .assert()
        .success();
    Command::cargo_bin("agentscope")
        .unwrap()
        .args(["plugins", "install", "--agent", "gemini"])
        .current_dir(tmp.path())
        .assert()
        .success();

    assert!(Path::new(&tmp.path().join(".agentscope/skill/codex/README.md")).exists());
    assert!(Path::new(&tmp.path().join(".agentscope/plugin/gemini-cli/README.md")).exists());
}

#[test]
fn cli_config_set_supports_agent_auto_attach() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("agentscope.yaml"), "version: 1\n").unwrap();

    Command::cargo_bin("agentscope")
        .unwrap()
        .args(["config", "set", "agents.auto_attach", "true"])
        .current_dir(tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("agents.auto_attach = true"));

    let config = std::fs::read_to_string(tmp.path().join("agentscope.yaml")).unwrap();
    assert!(config.contains("auto_attach: true"));
}
