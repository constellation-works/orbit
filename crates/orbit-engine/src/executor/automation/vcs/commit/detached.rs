//! A commit of the worktree's uncommitted changes that moves no ref and
//! leaves the index and worktree as they are [ORB-15314].

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_exec::{EnvironmentMode, NoSandbox, run_process};

use super::super::git::{
    GitTimeoutBudget, git_failure_error, git_output, git_output_paths, git_request,
};
use super::author::{GitAuthor, reviewer_author};
use super::scope::{
    NewPathPolicy, irregular_candidate_paths, task_candidate_paths, validate_claim_new_paths,
};

/// Name of the scratch index, under the worktree's private Git directory.
const SCRATCH_INDEX: &str = "orbit-detached-repair.index";

/// Commit every uncommitted change in `workspace_path`, new files included,
/// as one commit whose sole parent is `HEAD`, authored by the reviewer of
/// `reviewer_model` (Orbit when none is known) and committed by Orbit.
///
/// The changes are staged into a scratch index, so the worktree's own index,
/// files and branch are untouched; the commit is reachable only through the
/// ref the caller points at it. `None` when nothing differs from `HEAD`.
pub(in crate::executor::automation::vcs) fn commit_worktree_detached(
    workspace_path: &Path,
    reviewer_model: Option<&str>,
    message: &str,
) -> Result<Option<String>, OrbitError> {
    let author = reviewer_model
        .map(reviewer_author)
        .unwrap_or_else(GitAuthor::orbit);
    commit_detached(workspace_path, &author, message, None)
}

/// Preserve an unfinished claimed implementation without moving its branch,
/// index or files. Refuse the entire snapshot before writing objects when a
/// protected or irregular candidate path is present; scratch is never staged.
pub(in crate::executor::automation::vcs) fn commit_claimed_partial(
    workspace_path: &Path,
    message: &str,
) -> Result<Option<String>, OrbitError> {
    super::git_ops::ensure_no_unmerged_changes(workspace_path)?;
    let mut paths = task_candidate_paths(workspace_path, NewPathPolicy::Claimed)?;
    // Ordinary worktree diffs ignore submodules to avoid executing a nested
    // repository's config. Inspect staged changes without worktree traversal
    // as well, so a newly staged gitlink cannot disappear from the refusal.
    paths.extend(git_output_paths(
        workspace_path,
        &[
            "diff",
            "--cached",
            "--name-only",
            "--no-renames",
            "--ignore-submodules=none",
            "-z",
            "HEAD",
            "--",
        ],
    )?);
    let mut refused = irregular_candidate_paths(workspace_path, &paths);
    refused.extend(
        paths
            .iter()
            .filter(|path| !orbit_common::fs::selector::claim_new_path_is_safe(path))
            .cloned(),
    );
    refused.sort();
    refused.dedup();
    if !refused.is_empty() {
        return Err(OrbitError::Execution(format!(
            "partial candidate refused protected or non-regular paths: {refused:?}; \
             no candidate was carried and the branch, index and files are unchanged"
        )));
    }
    let paths = paths.into_iter().collect::<Vec<_>>();
    if paths.is_empty() {
        return Ok(None);
    }
    commit_detached(workspace_path, &GitAuthor::orbit(), message, Some(&paths))
}

fn commit_detached(
    workspace_path: &Path,
    author: &GitAuthor,
    message: &str,
    paths: Option<&[String]>,
) -> Result<Option<String>, OrbitError> {
    let head = git_output(workspace_path, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    let head_tree = git_output(workspace_path, &["rev-parse", "--verify", "HEAD^{tree}"])?;
    let index = scratch_index(workspace_path)?;
    let index_env = index.to_string_lossy().into_owned();
    let staged = (|| {
        let scratch = [("GIT_INDEX_FILE", index_env.as_str())];
        git_with_env(workspace_path, &["read-tree", "HEAD"], &scratch)?;
        if let Some(paths) = paths {
            let mut args = vec!["--literal-pathspecs", "add", "--all", "--"];
            args.extend(paths.iter().map(String::as_str));
            git_with_env(workspace_path, &args, &scratch)?;
        } else {
            git_with_env(workspace_path, &["add", "--all", "--", "."], &scratch)?;
        }
        git_with_env(workspace_path, &["write-tree"], &scratch)
    })();
    if let Err(error) = std::fs::remove_file(&index)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(index = %index.display(), %error, "scratch index left behind");
    }
    let tree = staged?;
    if tree == head_tree {
        return Ok(None);
    }
    if paths.is_some() {
        // Re-check the immutable snapshot: even a path that changed type
        // during staging must never publish a symlink or gitlink.
        validate_claim_new_paths(workspace_path, &[], &head, &tree)?;
    }
    let committer = GitAuthor::orbit();
    let commit = git_with_env(
        workspace_path,
        &["commit-tree", &tree, "-p", &head, "-m", message],
        &[
            ("GIT_AUTHOR_NAME", author.name()),
            ("GIT_AUTHOR_EMAIL", author.email()),
            ("GIT_COMMITTER_NAME", committer.name()),
            ("GIT_COMMITTER_EMAIL", committer.email()),
        ],
    )?;
    Ok(Some(commit))
}

/// The scratch index path, absolute: `--git-path` answers relative to the
/// worktree when its Git directory is inside it.
fn scratch_index(workspace_path: &Path) -> Result<PathBuf, OrbitError> {
    let path = PathBuf::from(git_output(
        workspace_path,
        &["rev-parse", "--git-path", SCRATCH_INDEX],
    )?);
    Ok(if path.is_absolute() {
        path
    } else {
        workspace_path.join(path)
    })
}

/// Run Git with `env` added to its secured environment; its trimmed stdout.
fn git_with_env(
    workspace_path: &Path,
    args: &[&str],
    env: &[(&str, &str)],
) -> Result<String, OrbitError> {
    let timeout_ms = GitTimeoutBudget::current().timeout_for(args);
    let mut request = git_request(workspace_path, args, timeout_ms);
    if let EnvironmentMode::ClearAndSet(environment) = &mut request.environment_mode {
        environment.extend(
            env.iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string())),
        );
    }
    let result = run_process(&request, &NoSandbox)?;
    if !result.success {
        return Err(git_failure_error(
            workspace_path,
            args,
            result.exit_code,
            &result.stderr,
        ));
    }
    Ok(result.stdout.trim().to_string())
}
