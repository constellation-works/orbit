use std::path::Path;

use orbit_common::OrbitError;
use orbit_exec::{EnvironmentMode, NoSandbox, run_process};

use super::super::git::{git_output, git_output_paths, git_output_raw, git_request, git_success};
use super::author::GitAuthor;

pub(super) fn git_commit_with_identity(
    workspace_path: &Path,
    message: &str,
    resolved_model: Option<&str>,
) -> Result<(), OrbitError> {
    // ADR-0299: the persisted model identifies the author while the
    // process-scoped Orbit identity remains the sole workflow committer. The
    // generic fallback remains commit-capable when a run has no resolved model.
    let author = resolved_model
        .map(GitAuthor::resolved_model)
        .unwrap_or_else(GitAuthor::orbit);
    let committer = GitAuthor::orbit();
    let mut args = vec!["commit".to_string()];
    args.push("--author".to_string());
    args.push(author.spec());
    args.extend(["-m".to_string(), message.to_string()]);
    git_success_dynamic_with_identity(workspace_path, &args, &author, &committer)
}

/// Commit only the explicitly supplied paths while preserving other staged
/// candidates for their owning task.
pub(super) fn git_commit_paths_with_identity(
    workspace_path: &Path,
    message: &str,
    resolved_model: Option<&str>,
    paths: &[String],
) -> Result<(), OrbitError> {
    let author = resolved_model
        .map(GitAuthor::resolved_model)
        .unwrap_or_else(GitAuthor::orbit);
    let committer = GitAuthor::orbit();
    let mut args = vec![
        "commit".to_string(),
        "--only".to_string(),
        "--author".to_string(),
        author.spec(),
        "-m".to_string(),
        message.to_string(),
        "--".to_string(),
    ];
    args.extend(paths.iter().cloned());
    git_success_dynamic_with_identity(workspace_path, &args, &author, &committer)
}

/// Commit with an explicit author; the workflow committer stays Orbit.
pub(super) fn git_commit_as(
    workspace_path: &Path,
    message: &str,
    author: &GitAuthor,
) -> Result<(), OrbitError> {
    let committer = GitAuthor::orbit();
    let args = vec![
        "commit".to_string(),
        "--author".to_string(),
        author.spec(),
        "-m".to_string(),
        message.to_string(),
    ];
    git_success_dynamic_with_identity(workspace_path, &args, author, &committer)
}

pub(super) fn stage_paths(workspace_path: &Path, files: &[String]) -> Result<(), OrbitError> {
    if files.is_empty() {
        return Ok(());
    }

    let mut args = vec!["add".to_string(), "-A".to_string(), "--".to_string()];
    args.extend(files.iter().cloned());
    git_success_dynamic(workspace_path, &args)
}

pub(super) fn staged_changed_files(workspace_path: &Path) -> Result<Vec<String>, OrbitError> {
    git_output_paths(
        workspace_path,
        &["diff", "--cached", "--name-only", "-z", "--relative"],
    )
}

fn git_success_dynamic(current_dir: &Path, args: &[String]) -> Result<(), OrbitError> {
    let args = args.iter().map(String::as_str).collect::<Vec<_>>();
    git_success(current_dir, &args)
}

fn git_success_dynamic_with_identity(
    current_dir: &Path,
    args: &[String],
    author: &GitAuthor,
    committer: &GitAuthor,
) -> Result<(), OrbitError> {
    let env_overrides = [
        ("GIT_AUTHOR_NAME", author.name()),
        ("GIT_AUTHOR_EMAIL", author.email()),
        ("GIT_COMMITTER_NAME", committer.name()),
        ("GIT_COMMITTER_EMAIL", committer.email()),
    ];
    let args_ref = args.iter().map(String::as_str).collect::<Vec<_>>();
    let timeout_ms = super::super::git::GitTimeoutBudget::current().timeout_for(&args_ref);
    let mut request = git_request(current_dir, &args_ref, timeout_ms);
    if let EnvironmentMode::ClearAndSet(environment) = &mut request.environment_mode {
        environment.extend(
            env_overrides
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string())),
        );
    }
    let result = run_process(&request, &NoSandbox)?;

    if !result.success {
        return Err(OrbitError::Execution(format!(
            "git {} failed in '{}': {}",
            args.join(" "),
            current_dir.display(),
            result.stderr.trim()
        )));
    }

    Ok(())
}

pub(super) fn ensure_named_branch(workspace_path: &Path) -> Result<(), OrbitError> {
    let actual_branch = git_output(workspace_path, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let actual_branch = actual_branch.trim();
    if actual_branch == "HEAD" {
        return Err(OrbitError::Execution(format!(
            "workspace '{}' has detached HEAD; expected a named branch",
            workspace_path.display(),
        )));
    }
    Ok(())
}

/// Uses [`git_output_raw`] rather than [`git_output`]: the latter trims the
/// whole output, which would misalign the index/worktree columns of a
/// single-line result by one byte (see `git_output`'s doc comment).
pub(super) fn ensure_no_unmerged_changes(workspace_path: &Path) -> Result<(), OrbitError> {
    let status = git_output_raw(workspace_path, &["status", "--porcelain"])?;
    for line in status.lines() {
        if line.len() < 2 {
            continue;
        }
        let bytes = line.as_bytes();
        let x = bytes[0] as char;
        let y = bytes[1] as char;
        if x == 'U' || y == 'U' || (x == 'A' && y == 'A') || (x == 'D' && y == 'D') {
            return Err(OrbitError::Execution(format!(
                "task worktree '{}' has unresolved merge conflicts",
                workspace_path.display()
            )));
        }
    }
    Ok(())
}
