//! Git inspection and fetch helpers for the pilot source snapshot.

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use orbit_common::OrbitError;
use orbit_common::fs::git::{
    GIT_FETCH_CAS_ATTEMPTS, git_fetch_cas_retry_delay, run_git, run_git_within,
    should_retry_git_ref_cas, try_with_git_fetch_lock,
};
use orbit_engine::DispatchError;

/// The tree id of the commit `revision` in `workspace`: `Ok(Err(stderr))`
/// when Git has no readable tree for it.
pub(crate) fn tree_of(
    workspace: &Path,
    revision: &str,
) -> Result<Result<String, String>, OrbitError> {
    let spec = format!("{revision}^{{tree}}");
    let output = run_git(workspace, &["rev-parse", "--verify", &spec])?;
    let tree = output.stdout.trim();
    if !output.success || !is_commit_id(tree) {
        return Ok(Err(output.stderr.trim().to_string()));
    }
    Ok(Ok(tree.to_string()))
}

/// Local-error wrapper so [`try_with_git_fetch_lock`] (which requires
/// `E: From<io::Error>`) can carry lock-acquisition failures or a Git failure
/// out of the locked closure.
#[derive(Debug)]
pub(super) enum FetchLockError {
    Io(io::Error),
    Git(String),
}

impl From<io::Error> for FetchLockError {
    fn from(error: io::Error) -> Self {
        FetchLockError::Io(error)
    }
}

impl std::fmt::Display for FetchLockError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchLockError::Io(error) => write!(formatter, "git fetch lock: {error}"),
            FetchLockError::Git(message) => formatter.write_str(message),
        }
    }
}

pub(super) fn is_git_work_tree(action: &str, workspace: &Path) -> Result<bool, DispatchError> {
    let output = git(action, workspace, &["rev-parse", "--is-inside-work-tree"])?;
    Ok(output.success && output.stdout.trim() == "true")
}

pub(super) fn has_origin_remote(action: &str, workspace: &Path) -> Result<bool, DispatchError> {
    let remotes = git(action, workspace, &["remote"])?;
    if !remotes.success {
        return Ok(false);
    }
    Ok(remotes
        .stdout
        .lines()
        .map(str::trim)
        .any(|name| name == "origin"))
}

/// Fetch `origin/<branch>` and return the commit origin had, with each
/// attempt bounded by `deadline`.
///
/// The fetch writes only objects and a ref private to this call
/// (`refs/orbit/pilot-source/…`, deleted again before returning): no
/// remote-tracking ref, tag or `FETCH_HEAD`. Nothing it writes is shared with
/// another Orbit fetch, so it takes no lock and a slow remote never holds up
/// another workspace fetch; a stalled remote is still killed at the deadline.
pub(in crate::application::task::pilot) fn fetch_origin_branch(
    action: &str,
    workspace: &Path,
    branch: &str,
    deadline: Duration,
) -> Result<String, DispatchError> {
    let private_ref = private_source_ref();
    let spec = format!("+refs/heads/{branch}:{private_ref}");
    let mut last_stderr = String::new();
    for attempt in 0..GIT_FETCH_CAS_ATTEMPTS {
        let output = run_git_within(
            workspace,
            &[
                "fetch",
                "--no-tags",
                "--no-recurse-submodules",
                "--no-write-fetch-head",
                // Without it Git also updates `refs/remotes/origin/<branch>`
                // from the configured refspec, outside the shared lock.
                "--refmap=",
                "origin",
                &spec,
            ],
            deadline,
        )
        .map_err(|error| {
            let failure = match error {
                OrbitError::ProcessTimeout { .. } => "remote failure: timed out fetching",
                _ => "failed to fetch",
            };
            DispatchError::action_failed(
                action,
                format!(
                    "{failure} origin/{branch} in '{}': {error}",
                    workspace.display()
                ),
            )
        })?;
        if output.success {
            let revision = rev_parse_commit(action, workspace, &private_ref);
            delete_private_source_ref(workspace, &private_ref, revision.as_deref().ok());
            return revision;
        }
        last_stderr = output.stderr.trim().to_string();
        if should_retry_git_ref_cas(attempt, &last_stderr) {
            tracing::warn!(
                attempt,
                branch,
                "retrying origin fetch after git ref update contention"
            );
            std::thread::sleep(git_fetch_cas_retry_delay());
            continue;
        }
        break;
    }
    Err(DispatchError::action_failed(
        action,
        format!(
            "remote failure: could not fetch origin/{branch} in '{}': {last_stderr}",
            workspace.display(),
        ),
    ))
}

/// Namespace of the per-call refs [`fetch_origin_branch`] fetches into.
const PRIVATE_SOURCE_REF_PREFIX: &str = "refs/orbit/pilot-source/";

/// A ref name no other fetch, in this process or another, uses at once.
fn private_source_ref() -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    format!(
        "{PRIVATE_SOURCE_REF_PREFIX}{}-{nanos}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

/// Best-effort removal of a [`private_source_ref`]. A leftover only keeps its
/// commit reachable; it never feeds a later pin, which always fetches anew.
fn delete_private_source_ref(workspace: &Path, private_ref: &str, revision: Option<&str>) {
    let mut args = vec!["update-ref", "-d", private_ref];
    args.extend(revision);
    match run_git(workspace, &args) {
        Ok(output) if output.success => {}
        Ok(output) => tracing::warn!(
            private_ref,
            stderr = output.stderr.trim(),
            "could not delete the pilot's private source ref"
        ),
        Err(error) => tracing::warn!(
            private_ref,
            %error,
            "could not delete the pilot's private source ref"
        ),
    }
}

/// Advance `refs/remotes/origin/<branch>` to the freshly fetched `revision`
/// when the shared fetch lock is free now, as fetching under the lock used
/// to. A held lock means another Orbit fetch is updating remote-tracking refs
/// at this moment, so the update is skipped rather than waited for: the pin
/// never reads the shared ref. Under the lock the ref only moves forward, by
/// compare-and-swap, so a newer value another writer published since this
/// fetch is never rolled back. A failure is logged, never the pin's failure.
pub(super) fn publish_remote_tracking_ref(workspace: &Path, branch: &str, revision: &str) {
    let tracking_ref = format!("refs/remotes/origin/{branch}");
    let published = try_with_git_fetch_lock(workspace, || {
        let current = run_git(
            workspace,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{tracking_ref}^{{commit}}"),
            ],
        )
        .map_err(|error| FetchLockError::Git(error.to_string()))?;
        let current = current.success.then(|| current.stdout.trim().to_string());
        let expected = match current.as_deref() {
            Some(current) if current == revision => return Ok(()),
            Some(current) => {
                let ancestor = run_git(
                    workspace,
                    &["merge-base", "--is-ancestor", current, revision],
                )
                .map_err(|error| FetchLockError::Git(error.to_string()))?;
                if !ancestor.success {
                    return Ok(());
                }
                current.to_string()
            }
            // An empty old value makes the update create-only.
            None => String::new(),
        };
        let output = run_git(
            workspace,
            &[
                "update-ref",
                "-m",
                "orbit: task-pilot source fetch",
                &tracking_ref,
                revision,
                &expected,
            ],
        )
        .map_err(|error| FetchLockError::Git(error.to_string()))?;
        if output.success {
            Ok(())
        } else {
            Err(FetchLockError::Git(output.stderr.trim().to_string()))
        }
    });
    match published {
        Ok(Some(())) => {}
        Ok(None) => tracing::debug!(
            branch,
            "git fetch lock held by another fetch; left origin/{branch} to it"
        ),
        Err(error) => tracing::warn!(
            branch,
            %error,
            "could not advance origin/{branch} to the pilot's fetched source"
        ),
    }
}

pub(super) fn rev_parse_commit(
    action: &str,
    workspace: &Path,
    rev: &str,
) -> Result<String, DispatchError> {
    let spec = format!("{rev}^{{commit}}");
    let output = git(action, workspace, &["rev-parse", "--verify", &spec])?;
    if !output.success {
        return Err(DispatchError::action_failed(
            action,
            format!(
                "unable to resolve source revision `{rev}`: {}",
                output.stderr.trim()
            ),
        ));
    }
    let sha = output.stdout.trim();
    if !is_commit_id(sha) {
        return Err(DispatchError::action_failed(
            action,
            format!("resolved `{rev}` to a non-commit id {sha:?}"),
        ));
    }
    Ok(sha.to_string())
}

pub(super) fn git_tree_path(anchor: &Path) -> Option<String> {
    if anchor.is_absolute() {
        return None;
    }
    let mut parts = Vec::new();
    for component in anchor.components() {
        match component {
            std::path::Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            std::path::Component::CurDir => {}
            _ => return None,
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

/// Whether `value` is a full SHA-1 or SHA-256 commit id, in either hex case.
pub(crate) fn is_commit_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.chars().all(|ch| ch.is_ascii_hexdigit())
}

pub(super) fn git(
    action: &str,
    workspace: &Path,
    args: &[&str],
) -> Result<orbit_common::fs::git::GitCommandOutput, DispatchError> {
    run_git(workspace, args).map_err(|error| {
        DispatchError::action_failed(
            action,
            format!(
                "git {} failed in '{}': {error}",
                args.join(" "),
                workspace.display()
            ),
        )
    })
}
