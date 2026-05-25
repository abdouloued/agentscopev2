use anyhow::{Context, Result};
use git2::{DiffOptions, Repository};
use std::collections::HashMap;
use std::path::PathBuf;

/// Snapshot of a single file's diff stats
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct FileDiff {
    pub path: PathBuf,
    pub additions: usize,
    pub deletions: usize,
    pub status: DiffStatus,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DiffStatus {
    Modified,
    Added,
    Deleted,
    Renamed { from: PathBuf },
}

/// The current working-tree diff against HEAD (or a baseline commit)
#[derive(Debug, Default)]
pub struct WorkingTreeDiff {
    pub files: Vec<FileDiff>,
}

impl WorkingTreeDiff {
    pub fn total_additions(&self) -> usize {
        self.files.iter().map(|f| f.additions).sum()
    }

    pub fn total_deletions(&self) -> usize {
        self.files.iter().map(|f| f.deletions).sum()
    }

    pub fn total_lines_changed(&self) -> usize {
        self.total_additions() + self.total_deletions()
    }
}

/// Open the git repo at or above cwd
pub fn open_repo() -> Result<Repository> {
    let cwd = std::env::current_dir()?;
    Repository::discover(&cwd).context("Not inside a git repository. AgentScope requires git.")
}

/// Capture the current HEAD commit SHA as a baseline
pub fn capture_baseline(repo: &Repository) -> Result<String> {
    let head = repo
        .head()
        .context("No commits yet — make at least one commit first")?;
    let commit = head.peel_to_commit()?;
    Ok(commit.id().to_string())
}

/// Diff working tree (including staged changes) against HEAD
pub fn working_tree_diff(repo: &Repository) -> Result<WorkingTreeDiff> {
    let head_tree = {
        let head = repo.head()?;
        let commit = head.peel_to_commit()?;
        commit.tree()?
    };

    let mut diff_opts = DiffOptions::new();
    diff_opts.include_untracked(true);
    diff_opts.recurse_untracked_dirs(true);

    let diff = repo
        .diff_tree_to_workdir_with_index(Some(&head_tree), Some(&mut diff_opts))
        .context("Failed to compute diff")?;

    // Collect file paths and statuses
    let mut file_map: HashMap<PathBuf, FileDiff> = HashMap::new();

    // First pass: gather all changed files
    let stats = diff.stats()?;
    let _ = stats; // just to force computation

    diff.foreach(
        &mut |delta, _progress| {
            let path = delta
                .new_file()
                .path()
                .or_else(|| delta.old_file().path())
                .map(PathBuf::from)
                .unwrap_or_default();

            let status = match delta.status() {
                git2::Delta::Added => DiffStatus::Added,
                git2::Delta::Deleted => DiffStatus::Deleted,
                git2::Delta::Renamed => DiffStatus::Renamed {
                    from: delta
                        .old_file()
                        .path()
                        .map(PathBuf::from)
                        .unwrap_or_default(),
                },
                _ => DiffStatus::Modified,
            };

            file_map.entry(path.clone()).or_insert(FileDiff {
                path,
                additions: 0,
                deletions: 0,
                status,
            });
            true
        },
        None,
        None,
        None,
    )?;

    // Second pass: count line additions/deletions
    let diff2 = repo
        .diff_tree_to_workdir_with_index(Some(&head_tree), Some(&mut diff_opts))
        .context("Failed to compute diff for line stats")?;

    diff2.foreach(
        &mut |_delta, _progress| true,
        None,
        None,
        Some(&mut |delta, _hunk, line| {
            let path = delta
                .new_file()
                .path()
                .map(PathBuf::from)
                .unwrap_or_default();
            if let Some(entry) = file_map.get_mut(&path) {
                match line.origin() {
                    '+' => entry.additions += 1,
                    '-' => entry.deletions += 1,
                    _ => {}
                }
            }
            true
        }),
    )?;

    let mut files: Vec<FileDiff> = file_map.into_values().collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(WorkingTreeDiff { files })
}

/// Diff between two commit SHAs — used by audit
#[allow(dead_code)]
pub fn diff_between(repo: &Repository, from: &str, to: &str) -> Result<WorkingTreeDiff> {
    let from_commit = repo.find_commit(git2::Oid::from_str(from)?)?;
    let to_commit = repo.find_commit(git2::Oid::from_str(to)?)?;

    let from_tree = from_commit.tree()?;
    let to_tree = to_commit.tree()?;

    let diff = repo.diff_tree_to_tree(Some(&from_tree), Some(&to_tree), None)?;

    let mut files: Vec<FileDiff> = Vec::new();

    diff.foreach(
        &mut |delta, _| {
            let path = delta
                .new_file()
                .path()
                .or_else(|| delta.old_file().path())
                .map(PathBuf::from)
                .unwrap_or_default();

            let status = match delta.status() {
                git2::Delta::Added => DiffStatus::Added,
                git2::Delta::Deleted => DiffStatus::Deleted,
                _ => DiffStatus::Modified,
            };

            files.push(FileDiff {
                path,
                additions: 0,
                deletions: 0,
                status,
            });
            true
        },
        None,
        None,
        None,
    )?;

    Ok(WorkingTreeDiff { files })
}

/// Get recent commits (for audit range)
#[allow(dead_code)]
pub fn recent_commits(repo: &Repository, n: usize) -> Result<Vec<CommitInfo>> {
    let mut walk = repo.revwalk()?;
    walk.push_head()?;
    walk.set_sorting(git2::Sort::TIME)?;

    let commits = walk
        .take(n)
        .filter_map(|oid| {
            let oid = oid.ok()?;
            let commit = repo.find_commit(oid).ok()?;
            Some(CommitInfo {
                sha: oid.to_string(),
                sha_short: oid.to_string()[..7].to_string(),
                message: commit.summary().unwrap_or("").to_string(),
                timestamp: commit.time().seconds(),
            })
        })
        .collect();

    Ok(commits)
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct CommitInfo {
    pub sha: String,
    pub sha_short: String,
    pub message: String,
    pub timestamp: i64,
}
