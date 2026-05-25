use globset::{Glob, GlobSet, GlobSetBuilder};
use std::path::Path;

use crate::config::PolicyConfig;
use crate::git::FileDiff;

/// How AgentScope classifies a changed file relative to the mission
#[derive(Debug, Clone, PartialEq)]
pub enum FileVerdict {
    /// Matches stated mission + not blocked
    InScope,
    /// Not in mission scope but not blocked — warn only
    Unasked,
    /// Matched a blocked glob — halt session
    Blocked { policy: String },
    /// No changes (included for completeness)
    #[allow(dead_code)]
    Clean,
}

impl FileVerdict {
    pub fn is_blocked(&self) -> bool {
        matches!(self, FileVerdict::Blocked { .. })
    }

    pub fn label(&self) -> &'static str {
        match self {
            FileVerdict::InScope => "IN SCOPE",
            FileVerdict::Unasked => "UNASKED",
            FileVerdict::Blocked { .. } => "BLOCKED",
            FileVerdict::Clean => "CLEAN",
        }
    }
}

/// A file diff annotated with its policy verdict
#[derive(Debug, Clone)]
pub struct AnnotatedFile {
    pub diff: FileDiff,
    pub verdict: FileVerdict,
}

/// The compiled, ready-to-evaluate policy engine
pub struct PolicyEngine {
    blocked_set: GlobSet,
    warn_set: GlobSet,
    blocked_patterns: Vec<String>,
    #[allow(dead_code)]
    warn_patterns: Vec<String>,
    pub max_files: usize,
    pub max_lines: usize,
}

impl PolicyEngine {
    pub fn from_config(config: &PolicyConfig) -> anyhow::Result<Self> {
        let mut blocked_builder = GlobSetBuilder::new();
        for pattern in &config.blocked {
            blocked_builder.add(Glob::new(pattern)?);
        }

        let mut warn_builder = GlobSetBuilder::new();
        for pattern in &config.warn {
            warn_builder.add(Glob::new(pattern)?);
        }

        Ok(Self {
            blocked_set: blocked_builder.build()?,
            warn_set: warn_builder.build()?,
            blocked_patterns: config.blocked.clone(),
            warn_patterns: config.warn.clone(),
            max_files: config.max_files_changed,
            max_lines: config.max_lines_changed,
        })
    }

    /// Check a single file path against policy
    pub fn check_path(&self, path: &Path) -> FileVerdict {
        if self.blocked_set.is_match(path) {
            // Find which pattern matched for the error message
            let policy = self
                .blocked_patterns
                .iter()
                .enumerate()
                .find(|(i, _)| {
                    let matches = self.blocked_set.matches(path);
                    matches.contains(i)
                })
                .map(|(_, p)| p.clone())
                .unwrap_or_else(|| "blocked-path policy".into());

            return FileVerdict::Blocked { policy };
        }

        if self.warn_set.is_match(path) {
            // Treat warn-listed files as Unasked (yellow) for now
            return FileVerdict::Unasked;
        }

        // Default: Unasked — the mission classifier upgrades this to InScope
        FileVerdict::Unasked
    }

    /// Annotate a list of diffs using the mission text to infer scope
    pub fn annotate(&self, diffs: &[FileDiff], mission: &str) -> Vec<AnnotatedFile> {
        let scope_hints = extract_scope_hints(mission);

        diffs
            .iter()
            .map(|diff| {
                let verdict = match self.check_path(&diff.path) {
                    blocked @ FileVerdict::Blocked { .. } => blocked,
                    _ => {
                        if is_in_scope(&diff.path, &scope_hints) {
                            FileVerdict::InScope
                        } else {
                            FileVerdict::Unasked
                        }
                    }
                };
                AnnotatedFile {
                    diff: diff.clone(),
                    verdict,
                }
            })
            .collect()
    }

    /// Check limit policies
    pub fn check_limits(&self, file_count: usize, line_count: usize) -> Vec<LimitWarning> {
        let mut warnings = Vec::new();

        if self.max_files > 0 && file_count > self.max_files {
            warnings.push(LimitWarning::TooManyFiles {
                actual: file_count,
                limit: self.max_files,
            });
        }

        if self.max_lines > 0 && line_count > self.max_lines {
            warnings.push(LimitWarning::TooManyLines {
                actual: line_count,
                limit: self.max_lines,
            });
        }

        warnings
    }
}

#[derive(Debug)]
pub enum LimitWarning {
    TooManyFiles { actual: usize, limit: usize },
    TooManyLines { actual: usize, limit: usize },
}

/// Extract file/path hints from a natural language mission string.
/// E.g. "Fix rate-limit bug in api/middleware.ts" → ["api", "middleware", "middleware.ts"]
pub(crate) fn extract_scope_hints(mission: &str) -> Vec<String> {
    // Grab anything that looks like a path segment or filename
    let re_path = regex_lite::Regex::new(
        r"[\w.-]+\.(?:ts|js|rs|py|go|rb|java|c|cpp|h|css|html|json|yaml|toml|md)",
    )
    .unwrap();
    let re_dir = regex_lite::Regex::new(r"[\w-]+/[\w./:-]+").unwrap();

    let mut hints: Vec<String> = Vec::new();

    for m in re_path.find_iter(mission) {
        hints.push(m.as_str().to_lowercase());
    }
    for m in re_dir.find_iter(mission) {
        hints.push(m.as_str().to_lowercase());
    }

    // Also add individual words longer than 3 chars as fuzzy hints
    for word in mission.split_whitespace() {
        let w = word
            .trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase();
        if w.len() > 3 {
            hints.push(w);
        }
    }

    hints
}

/// Fuzzy match: does this path seem related to what was asked?
pub(crate) fn is_in_scope(path: &Path, hints: &[String]) -> bool {
    let path_str = path.to_string_lossy().to_lowercase();
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    hints
        .iter()
        .any(|hint| path_str.contains(hint.as_str()) || hint.contains(stem.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PolicyConfig;
    use crate::git::{DiffStatus, FileDiff};
    use std::path::PathBuf;

    /// Helper: build a PolicyEngine from default PolicyConfig
    fn default_engine() -> PolicyEngine {
        PolicyEngine::from_config(&PolicyConfig::default()).unwrap()
    }

    /// Helper: build a PolicyEngine with custom config
    fn engine_with(
        blocked: Vec<&str>,
        warn: Vec<&str>,
        max_files: usize,
        max_lines: usize,
    ) -> PolicyEngine {
        let config = PolicyConfig {
            blocked: blocked.into_iter().map(String::from).collect(),
            warn: warn.into_iter().map(String::from).collect(),
            max_files_changed: max_files,
            max_lines_changed: max_lines,
        };
        PolicyEngine::from_config(&config).unwrap()
    }

    /// Helper: create a FileDiff for testing
    fn make_diff(path: &str, additions: usize, deletions: usize) -> FileDiff {
        FileDiff {
            path: PathBuf::from(path),
            additions,
            deletions,
            status: DiffStatus::Modified,
        }
    }

    // ── Blocked path tests ────────────────────────────────────────────────

    #[test]
    fn blocked_dot_env() {
        let engine = default_engine();
        assert!(engine.check_path(Path::new(".env")).is_blocked());
    }

    #[test]
    fn blocked_dot_env_local() {
        let engine = default_engine();
        assert!(engine.check_path(Path::new(".env.local")).is_blocked());
    }

    #[test]
    fn blocked_nested_dot_env() {
        let engine = default_engine();
        assert!(engine.check_path(Path::new("backend/.env")).is_blocked());
        assert!(engine
            .check_path(Path::new("backend/.env.production"))
            .is_blocked());
    }

    #[test]
    fn blocked_secrets_dir() {
        let engine = default_engine();
        assert!(engine
            .check_path(Path::new("config/secrets/api_key.txt"))
            .is_blocked());
        assert!(engine
            .check_path(Path::new("deploy/secrets/cert.pem"))
            .is_blocked());
    }

    #[test]
    fn blocked_pem_and_key_files() {
        let engine = default_engine();
        assert!(engine.check_path(Path::new("ssl/server.pem")).is_blocked());
        assert!(engine
            .check_path(Path::new("certs/private.key"))
            .is_blocked());
        assert!(engine
            .check_path(Path::new("deep/nested/dir/cert.pem"))
            .is_blocked());
    }

    #[test]
    fn blocked_auth_dir() {
        let engine = default_engine();
        assert!(engine
            .check_path(Path::new("src/auth/session.ts"))
            .is_blocked());
        assert!(engine
            .check_path(Path::new("src/auth/login.rs"))
            .is_blocked());
    }

    #[test]
    fn blocked_migrations_dir() {
        let engine = default_engine();
        assert!(engine
            .check_path(Path::new("db/migrations/001_init.sql"))
            .is_blocked());
        assert!(engine
            .check_path(Path::new("src/migrations/v2.sql"))
            .is_blocked());
    }

    #[test]
    fn blocked_verdict_contains_matching_policy() {
        let engine = default_engine();
        let verdict = engine.check_path(Path::new(".env"));
        match verdict {
            FileVerdict::Blocked { policy } => {
                assert!(
                    policy == ".env" || policy == "**/.env",
                    "Expected policy to match .env glob, got: {}",
                    policy
                );
            }
            other => panic!("Expected Blocked, got {:?}", other),
        }
    }

    // ── Warn path tests ───────────────────────────────────────────────────

    #[test]
    fn warn_package_lock_json() {
        let engine = default_engine();
        // warn paths return Unasked
        let verdict = engine.check_path(Path::new("package-lock.json"));
        assert_eq!(verdict, FileVerdict::Unasked);
    }

    #[test]
    fn warn_cargo_lock() {
        let engine = default_engine();
        let verdict = engine.check_path(Path::new("Cargo.lock"));
        assert_eq!(verdict, FileVerdict::Unasked);
    }

    #[test]
    fn warn_yarn_lock() {
        let engine = default_engine();
        let verdict = engine.check_path(Path::new("yarn.lock"));
        assert_eq!(verdict, FileVerdict::Unasked);
    }

    #[test]
    fn warn_config_dir() {
        let engine = default_engine();
        let verdict = engine.check_path(Path::new("src/config/settings.yaml"));
        assert_eq!(verdict, FileVerdict::Unasked);
    }

    // ── Non-blocked paths pass through ────────────────────────────────────

    #[test]
    fn normal_source_file_not_blocked() {
        let engine = default_engine();
        assert!(!engine.check_path(Path::new("src/main.rs")).is_blocked());
    }

    #[test]
    fn normal_readme_not_blocked() {
        let engine = default_engine();
        assert!(!engine.check_path(Path::new("README.md")).is_blocked());
    }

    #[test]
    fn normal_test_file_not_blocked() {
        let engine = default_engine();
        assert!(!engine
            .check_path(Path::new("tests/integration.rs"))
            .is_blocked());
    }

    #[test]
    fn normal_nested_source_not_blocked() {
        let engine = default_engine();
        assert!(!engine
            .check_path(Path::new("src/api/handler.ts"))
            .is_blocked());
    }

    // Default verdict for non-blocked, non-warn paths is Unasked
    #[test]
    fn non_blocked_non_warn_returns_unasked() {
        let engine = default_engine();
        let verdict = engine.check_path(Path::new("src/lib.rs"));
        assert_eq!(verdict, FileVerdict::Unasked);
    }

    // ── Scope hint extraction ─────────────────────────────────────────────

    #[test]
    fn extract_scope_hints_finds_filenames() {
        let hints = extract_scope_hints("Fix rate-limit bug in api/middleware.ts");
        assert!(hints.contains(&"middleware.ts".to_string()));
    }

    #[test]
    fn extract_scope_hints_finds_dir_paths() {
        let hints = extract_scope_hints("Fix rate-limit bug in api/middleware.ts");
        assert!(hints.iter().any(|h| h.contains("api/")));
    }

    #[test]
    fn extract_scope_hints_finds_long_words() {
        let hints = extract_scope_hints("Fix the authentication handler in server.rs");
        assert!(hints.contains(&"authentication".to_string()));
        assert!(hints.contains(&"handler".to_string()));
        assert!(hints.contains(&"server.rs".to_string()));
    }

    #[test]
    fn extract_scope_hints_ignores_short_words() {
        let hints = extract_scope_hints("Fix the bug in api");
        // Words <= 3 chars should not appear
        assert!(!hints.contains(&"fix".to_string()));
        assert!(!hints.contains(&"the".to_string()));
        assert!(!hints.contains(&"bug".to_string()));
        assert!(!hints.contains(&"in".to_string()));
        assert!(!hints.contains(&"api".to_string()));
    }

    #[test]
    fn extract_scope_hints_lowercases_everything() {
        let hints = extract_scope_hints("Update README.md and Cargo.toml");
        assert!(hints.contains(&"readme.md".to_string()));
        assert!(hints.contains(&"cargo.toml".to_string()));
    }

    #[test]
    fn extract_scope_hints_multiple_extensions() {
        let hints = extract_scope_hints("Refactor utils.py and helpers.go");
        assert!(hints.contains(&"utils.py".to_string()));
        assert!(hints.contains(&"helpers.go".to_string()));
    }

    // ── is_in_scope ───────────────────────────────────────────────────────

    #[test]
    fn is_in_scope_matches_exact_filename_hint() {
        let hints = vec!["middleware.ts".to_string()];
        assert!(is_in_scope(Path::new("src/api/middleware.ts"), &hints));
    }

    #[test]
    fn is_in_scope_matches_directory_hint() {
        let hints = vec!["api/middleware.ts".to_string()];
        assert!(is_in_scope(Path::new("src/api/middleware.ts"), &hints));
    }

    #[test]
    fn is_in_scope_matches_stem_contained_in_hint() {
        // hint "authentication" contains stem "auth" → match
        let hints = vec!["authentication".to_string()];
        assert!(is_in_scope(Path::new("src/auth.rs"), &hints));
    }

    #[test]
    fn is_in_scope_no_match_for_unrelated_file() {
        let hints = vec!["middleware.ts".to_string()];
        assert!(!is_in_scope(Path::new("src/database/pool.rs"), &hints));
    }

    #[test]
    fn is_in_scope_case_insensitive() {
        let hints = vec!["readme.md".to_string()];
        assert!(is_in_scope(Path::new("README.md"), &hints));
    }

    // ── annotate() ────────────────────────────────────────────────────────

    #[test]
    fn annotate_classifies_blocked_files() {
        let engine = default_engine();
        let diffs = vec![make_diff(".env", 1, 0)];
        let annotated = engine.annotate(&diffs, "Fix the login bug");
        assert_eq!(annotated.len(), 1);
        assert!(annotated[0].verdict.is_blocked());
    }

    #[test]
    fn annotate_classifies_in_scope_files() {
        let engine = default_engine();
        let diffs = vec![make_diff("src/api/middleware.ts", 10, 3)];
        let annotated = engine.annotate(&diffs, "Fix rate-limit bug in api/middleware.ts");
        assert_eq!(annotated.len(), 1);
        assert_eq!(annotated[0].verdict, FileVerdict::InScope);
    }

    #[test]
    fn annotate_classifies_unasked_files() {
        let engine = default_engine();
        let diffs = vec![make_diff("src/database/pool.rs", 5, 2)];
        let annotated = engine.annotate(&diffs, "Fix rate-limit bug in api/middleware.ts");
        assert_eq!(annotated.len(), 1);
        assert_eq!(annotated[0].verdict, FileVerdict::Unasked);
    }

    #[test]
    fn annotate_mixed_verdicts() {
        let engine = default_engine();
        let diffs = vec![
            make_diff("src/api/middleware.ts", 10, 3), // in scope
            make_diff(".env", 1, 0),                   // blocked
            make_diff("src/database/pool.rs", 5, 2),   // unasked
        ];
        let annotated = engine.annotate(&diffs, "Fix rate-limit bug in api/middleware.ts");
        assert_eq!(annotated.len(), 3);
        assert_eq!(annotated[0].verdict, FileVerdict::InScope);
        assert!(annotated[1].verdict.is_blocked());
        assert_eq!(annotated[2].verdict, FileVerdict::Unasked);
    }

    #[test]
    fn annotate_blocked_overrides_scope() {
        // Even if a blocked file matches scope hints, it stays blocked
        let engine = default_engine();
        let diffs = vec![make_diff("src/auth/session.ts", 5, 0)];
        let annotated = engine.annotate(&diffs, "Fix session handling in src/auth/session.ts");
        assert_eq!(annotated.len(), 1);
        assert!(annotated[0].verdict.is_blocked());
    }

    // ── check_limits ──────────────────────────────────────────────────────

    #[test]
    fn check_limits_no_warnings_when_under_limits() {
        let engine = engine_with(vec![], vec![], 10, 100);
        let warnings = engine.check_limits(5, 50);
        assert!(warnings.is_empty());
    }

    #[test]
    fn check_limits_warns_too_many_files() {
        let engine = engine_with(vec![], vec![], 10, 100);
        let warnings = engine.check_limits(15, 50);
        assert_eq!(warnings.len(), 1);
        assert!(matches!(
            warnings[0],
            LimitWarning::TooManyFiles {
                actual: 15,
                limit: 10
            }
        ));
    }

    #[test]
    fn check_limits_warns_too_many_lines() {
        let engine = engine_with(vec![], vec![], 10, 100);
        let warnings = engine.check_limits(5, 150);
        assert_eq!(warnings.len(), 1);
        assert!(matches!(
            warnings[0],
            LimitWarning::TooManyLines {
                actual: 150,
                limit: 100
            }
        ));
    }

    #[test]
    fn check_limits_warns_both() {
        let engine = engine_with(vec![], vec![], 10, 100);
        let warnings = engine.check_limits(15, 150);
        assert_eq!(warnings.len(), 2);
    }

    #[test]
    fn check_limits_disabled_when_zero() {
        let engine = engine_with(vec![], vec![], 0, 0);
        let warnings = engine.check_limits(999, 99999);
        assert!(warnings.is_empty());
    }

    #[test]
    fn check_limits_exact_boundary_no_warning() {
        let engine = engine_with(vec![], vec![], 10, 100);
        let warnings = engine.check_limits(10, 100);
        assert!(warnings.is_empty());
    }

    // ── FileVerdict helpers ───────────────────────────────────────────────

    #[test]
    fn file_verdict_labels() {
        assert_eq!(FileVerdict::InScope.label(), "IN SCOPE");
        assert_eq!(FileVerdict::Unasked.label(), "UNASKED");
        assert_eq!(
            FileVerdict::Blocked {
                policy: "test".into()
            }
            .label(),
            "BLOCKED"
        );
        assert_eq!(FileVerdict::Clean.label(), "CLEAN");
    }

    #[test]
    fn file_verdict_is_blocked() {
        assert!(!FileVerdict::InScope.is_blocked());
        assert!(!FileVerdict::Unasked.is_blocked());
        assert!(FileVerdict::Blocked { policy: "x".into() }.is_blocked());
        assert!(!FileVerdict::Clean.is_blocked());
    }
}
