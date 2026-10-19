//! Prove an evidence commit is reachable from the base, refreshing its remote-tracking ref.

use std::path::Path;
use std::process::Command;

use orbit_common::fs::git::{GIT_REMOTE_TIMEOUT, run_git, with_git_fetch_lock};
use orbit_common::process::run_bounded_capped;

const REMOTE_REF_REFRESH_OUTPUT_LIMIT: usize = 16 * 1024;

/// Resolve `commit` and prove it is reachable from `base_ref`, returning its
/// full SHA. The decision parser already admitted `commit` as hexadecimal, so
/// neither value can be read as a Git option.
pub(super) fn verify_on_base(
    repo_root: &Path,
    base_ref: &str,
    commit: &str,
) -> Result<String, String> {
    let base_ref = base_ref.trim();
    if base_ref.is_empty() || base_ref.starts_with('-') {
        return Err(format!(
            "base ref '{base_ref}' is not a usable ref; no refresh fetch was performed"
        ));
    }
    let resolve = |rev: &str, what: &str| -> Result<String, String> {
        let spec = format!("{rev}^{{commit}}");
        let output = run_git(repo_root, &["rev-parse", "--verify", "--quiet", &spec])
            .map_err(|error| error.to_string())?;
        let sha = output.stdout.trim();
        if output.success && !sha.is_empty() {
            Ok(sha.to_string())
        } else {
            Err(format!("{what} '{rev}' does not resolve to a commit"))
        }
    };
    let commit = resolve(commit.trim(), "evidence commit")
        .map_err(|error| format!("{error}; no refresh fetch was performed"))?;
    let base = resolve(base_ref, "base ref")
        .map_err(|error| format!("{error}; no refresh fetch was performed"))?;
    let ancestor = run_git(repo_root, &["merge-base", "--is-ancestor", &commit, &base])
        .map_err(|error| format!("{error}; no refresh fetch was performed"))?;
    if ancestor.success {
        Ok(commit)
    } else {
        let unreachable =
            || format!("evidence commit {commit} is not reachable from '{base_ref}' ({base})");
        let target = match remote_tracking_target(repo_root, base_ref) {
            Ok(Some(target)) => target,
            Ok(None) => {
                return Err(format!(
                    "{}; no refresh fetch was performed because the base ref is not a remote-tracking ref",
                    unreachable()
                ));
            }
            Err(error) => {
                return Err(format!(
                    "{}; no refresh fetch was performed because the remote-tracking ref could not be identified: {error}",
                    unreachable()
                ));
            }
        };
        let (remote, branch) = target;
        let fetch = refresh_remote_tracking_ref(repo_root, &remote, &branch);
        let refreshed_base = resolve(base_ref, "base ref").map_err(|error| {
            format!(
                "{}; one refresh fetch was attempted for '{base_ref}' but the base ref did not resolve afterward: {error}",
                unreachable()
            )
        })?;
        let retried = run_git(
            repo_root,
            &["merge-base", "--is-ancestor", &commit, &refreshed_base],
        )
        .map_err(|error| {
            format!(
                "{}; one refresh fetch was attempted for '{base_ref}' but the ancestry retry failed: {error}",
                unreachable()
            )
        })?;
        if retried.success {
            return Ok(commit);
        }
        let refresh_result = match fetch {
            Ok(()) => format!("remote-tracking ref '{base_ref}' was refreshed by one fetch"),
            Err(error) => format!(
                "one refresh fetch was attempted for remote-tracking ref '{base_ref}' but failed: {error}"
            ),
        };
        Err(format!(
            "evidence commit {commit} is not reachable from '{base_ref}' ({refreshed_base}) after {refresh_result}"
        ))
    }
}

/// Identify a configured remote and branch only when `base_ref` names a
/// remote-tracking ref directly. Revision expressions are deliberately not
/// treated as fetch targets.
fn remote_tracking_target(
    repo_root: &Path,
    base_ref: &str,
) -> Result<Option<(String, String)>, String> {
    let ref_name = base_ref.strip_prefix("refs/remotes/").unwrap_or(base_ref);
    let remotes = run_git(repo_root, &["remote"]).map_err(|error| error.to_string())?;
    if !remotes.success {
        return Ok(None);
    }
    let mut names = remotes
        .stdout
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    names.sort_by_key(|name| std::cmp::Reverse(name.len()));
    for remote in names {
        let prefix = format!("{remote}/");
        let Some(branch) = ref_name.strip_prefix(&prefix) else {
            continue;
        };
        if branch == "HEAD" {
            let full_ref = format!("refs/remotes/{remote}/HEAD");
            let symbolic = run_git(
                repo_root,
                &["symbolic-ref", "--quiet", "--short", &full_ref],
            )
            .map_err(|error| error.to_string())?;
            if symbolic.success {
                return remote_tracking_target(repo_root, symbolic.stdout.trim());
            }
            return Ok(None);
        }
        if is_fetchable_remote_branch(branch) {
            return Ok(Some((remote, branch.to_owned())));
        }
        return Ok(None);
    }
    Ok(None)
}

fn is_fetchable_remote_branch(branch: &str) -> bool {
    !branch.is_empty()
        && !branch.starts_with('-')
        && !branch.starts_with('/')
        && !branch.ends_with('/')
        && !branch.ends_with('.')
        && !branch.contains("//")
        && !branch.contains("..")
        && !branch.contains("@{")
        && !branch.chars().any(|character| {
            character.is_control()
                || character.is_whitespace()
                || matches!(character, '~' | '^' | ':' | '?' | '*' | '[' | '\\')
        })
}

/// Refresh just one remote-tracking branch while holding Orbit's shared fetch
/// lock. The process runner bounds both the wait and captured output.
fn refresh_remote_tracking_ref(repo_root: &Path, remote: &str, branch: &str) -> Result<(), String> {
    let refspec = format!("+refs/heads/{branch}:refs/remotes/{remote}/{branch}");
    with_git_fetch_lock(repo_root, || {
        let mut command = Command::new("git");
        command
            .current_dir(repo_root)
            .args([
                "fetch",
                "--no-tags",
                "--no-recurse-submodules",
                remote,
                &refspec,
            ])
            .env("GIT_TERMINAL_PROMPT", "0");
        let output = run_bounded_capped(
            &mut command,
            GIT_REMOTE_TIMEOUT,
            REMOTE_REF_REFRESH_OUTPUT_LIMIT,
        )
        .map_err(|error| std::io::Error::other(error.to_string()))?;
        if output.status.success() {
            return Ok(());
        }
        Err(std::io::Error::other(format!(
            "git fetch exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    })
    .map_err(|error| error.to_string())
}
