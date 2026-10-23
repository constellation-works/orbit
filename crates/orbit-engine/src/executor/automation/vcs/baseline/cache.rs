use super::super::git::git_run;
use super::super::required::command::RequiredCommandRun;
use super::super::worktree::scratch_checkout_path;
use super::command::run_validation_command_until;
use super::compare::{BaseCommandResult, BaselineCheck, not_comparable};
use crate::context::RuntimeHost;
use orbit_common::OrbitError;
use orbit_common::fs::git::git_common_dir;
use orbit_common::security::release::sha256_hex;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Directory under the Git common directory holding base results.
const CACHE_DIR: &str = "orbit-baseline";

/// Version 2 moved the base checkout out of the common directory, so a
/// version 1 result's output names paths under the old checkout root.
/// Version 3 records the command's summary [ORB-15131]; an older passing
/// result cannot show that it executed any test.
/// Version 4 ignores deadline-shortened timeouts [ORB-15350]; a shortened
/// timeout is treated as inconclusive and never cached.
/// Version 5 excludes capture-limit and signal stops: they are infrastructure
/// failures, never conclusive base results.
/// Version 6 records the failures the complete output names [ORB-15375].
/// Version 7 retains complete-output host coverage refusals for flaky admission.
/// Version 8 runs in host-owned source-path target namespaces: earlier results
/// could have executed sandbox-built binaries [F2026-10-342].
/// Version 9 keys workspace output bindings as part of the execution contract.
/// Version 10 withholds partial command-evidence payloads before caching;
/// earlier results can contain unrecognized credential boundary fragments.
pub(super) const CACHE_SCHEMA_VERSION: u32 = 10;

/// Where version 1 checked bases out, under [`CACHE_DIR`]. Any checkout left
/// there trips the Linux Git protection scan until it is removed.
const LEGACY_WORKTREES_DIR: &str = "worktrees";

pub(in super::super) struct CachePaths {
    pub(super) result: PathBuf,
    pub(super) lock: PathBuf,
    pub(super) inconclusive_hold: PathBuf,
    pub(super) claim_inconclusive: PathBuf,
    pub(in super::super) worktree: PathBuf,
    pub(super) legacy_worktrees: PathBuf,
}

pub(in super::super) fn cache_paths(
    repo: &Path,
    base_sha: &str,
    command: &str,
    selection: Option<&Value>,
    build_output_env: &[String],
) -> Result<CachePaths, OrbitError> {
    // Preserve earlier evidence at its old key rather than overwriting it
    // when the execution contract changes.
    let mut identity = format!(
        "{CACHE_SCHEMA_VERSION}\0{}\0{}",
        base_sha.trim(),
        command.trim()
    );
    // The selection is handed to the command outside its text, so a run of
    // another selection is another result [ORB-15131].
    if let Some(selection) = selection {
        identity.push('\0');
        identity.push_str(&selection.to_string());
    }
    identity.push('\0');
    identity.push_str(&serde_json::json!(build_output_env).to_string());
    let key = sha256_hex(identity.as_bytes());
    let dir = git_common_dir(repo)?.join(CACHE_DIR);
    Ok(CachePaths {
        result: dir.join(format!("{key}.json")),
        lock: dir.join(format!("{key}.lock")),
        inconclusive_hold: dir.join(format!("{key}.hold-inconclusive.json")),
        claim_inconclusive: dir.join(format!("{key}.claim-inconclusive.json")),
        worktree: scratch_checkout_path(repo, &format!("{CACHE_DIR}-{key}"))?,
        legacy_worktrees: dir.join(LEGACY_WORKTREES_DIR),
    })
}

/// A base run this repository's cache records: the scratch checkout it ran
/// in and the lock it held throughout.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(in super::super) struct RecordedBaseRun {
    pub(in super::super) worktree: PathBuf,
    pub(in super::super) lock: PathBuf,
}

/// Every base run whose cache lock exists. The lock file is created before
/// a run starts and kept afterwards, so it is the durable record that the
/// run's checkout (and so its host validation target) existed; holding the
/// lock proves no base run is using either.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(in super::super) fn recorded_base_runs(repo: &Path) -> Vec<RecordedBaseRun> {
    let Ok(dir) = git_common_dir(repo).map(|common| common.join(CACHE_DIR)) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let key = name.to_str()?.strip_suffix(".lock")?;
            if key.len() != 64
                || !key
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
            {
                return None;
            }
            Some(RecordedBaseRun {
                worktree: scratch_checkout_path(repo, &format!("{CACHE_DIR}-{key}")).ok()?,
                lock: entry.path(),
            })
        })
        .collect()
}

/// The result another run already recorded for `command` on `base_sha` with
/// `selection`, judged against it, without waiting for or starting a run.
pub(in super::super) fn cached_base_check<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &Path,
    base_sha: &str,
    command: &str,
    selection: Option<&Value>,
) -> Option<BaselineCheck> {
    let cache = cache_paths(
        workspace_path,
        base_sha,
        command,
        selection,
        &host.validation_build_output_env(),
    )
    .ok()?;
    let result = read_cached(&cache.result, selection)?;
    Some(BaselineCheck {
        base_sha: base_sha.to_string(),
        not_comparable: not_comparable(selection, &result),
        result: Ok(result),
        cached: true,
        exact_replay: selection
            .is_some_and(|selection| selection.get(super::compare::FAILING_TESTS_KEY).is_some()),
    })
}

pub(super) fn read_cached(path: &Path, selection: Option<&Value>) -> Option<BaseCommandResult> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice::<BaseCommandResult>(&bytes)
        .ok()
        .filter(|result| result.schema_version == CACHE_SCHEMA_VERSION)
        // Preserve compatible full-cache reuse. Narrow results recorded before
        // identity proof (or with incomplete proof) must be rerun under the
        // existing exclusive lock, never accepted from count/selection alone.
        .filter(|result| {
            !selection
                .is_some_and(|selection| selection.get(super::compare::FAILING_TESTS_KEY).is_some())
                || not_comparable(selection, result).is_none()
        })
}

/// Run `command` in a fresh detached worktree of `base_sha`, then remove it.
pub(super) fn run_on_base<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &Path,
    worktree: &Path,
    base_sha: &str,
    command: &str,
    selection: Option<&Value>,
    deadline: Option<Instant>,
) -> Result<RequiredCommandRun, OrbitError> {
    in_detached_worktree(workspace_path, worktree, base_sha, |checkout| {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(OrbitError::Execution(
                "the base comparison deadline expired during checkout".into(),
            ));
        }
        run_validation_command_until(host, checkout, command, selection, deadline)
    })?
}

/// Check `commit` out into a fresh detached worktree at `worktree`, run `f`
/// there, then remove the worktree whatever `f` returned.
pub(in super::super) fn in_detached_worktree<T>(
    workspace_path: &Path,
    worktree: &Path,
    commit: &str,
    f: impl FnOnce(&Path) -> T,
) -> Result<T, OrbitError> {
    remove_worktree(workspace_path, worktree);
    if let Some(parent) = worktree.parent() {
        orbit_common::fs::io::create_private_dir_all(parent).map_err(|error| {
            OrbitError::Execution(format!("create '{}': {error}", parent.display()))
        })?;
    }
    let path = worktree.to_string_lossy();
    let added = git_run(
        workspace_path,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            "--end-of-options",
            &path,
            commit,
        ],
    )?;
    if !added.success {
        remove_worktree(workspace_path, worktree);
        return Err(OrbitError::Execution(format!(
            "git worktree add {commit}: {}",
            added.stderr.trim()
        )));
    }
    let result = f(worktree);
    remove_worktree(workspace_path, worktree);
    Ok(result)
}

/// Best-effort removal of every checkout an older Orbit left under the Git
/// common directory, then of the directory itself.
pub(super) fn remove_legacy_checkouts(workspace_path: &Path, legacy: &Path) {
    let Ok(entries) = std::fs::read_dir(legacy) else {
        return;
    };
    for entry in entries.flatten() {
        remove_worktree(workspace_path, &entry.path());
    }
    if let Err(error) = std::fs::remove_dir(legacy) {
        tracing::warn!(
            path = %legacy.display(),
            "could not remove the legacy base checkout directory: {error}"
        );
    }
}

/// Best-effort removal of a base worktree and its registration.
fn remove_worktree(workspace_path: &Path, worktree: &Path) {
    if worktree.exists() {
        let path = worktree.to_string_lossy();
        let _ = git_run(
            workspace_path,
            &["worktree", "remove", "--force", "--end-of-options", &path],
        );
        if worktree.exists()
            && let Err(error) = std::fs::remove_dir_all(worktree)
        {
            tracing::warn!(
                path = %worktree.display(),
                "could not remove the required validation base worktree: {error}"
            );
        }
    }
    let _ = git_run(workspace_path, &["worktree", "prune"]);
}
