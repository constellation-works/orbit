use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::selector::{anchor_path, claim_new_path_is_safe};
use orbit_types::task::{
    CONTEXT_FILES_WIDENED_EVENT, ContextFilesWidening, ContextWideningStep, Task,
};

use crate::context::RuntimeHost;

use super::super::git::git_output_paths;

/// The run-scoped scratch root (`$ORBIT_SCRATCH_DIR`). Repositories normally
/// ignore it; it is excluded here as well so no selector can deliver scratch.
const SCRATCH_DIR: &str = ".orbit/tmp";

/// Which new (untracked) paths delivery accepts.
///
/// Implementers, recovery agents and reviewers may create any path the work
/// requires; selectors and footprint locks are not a delivery gate. Delivery
/// widens the task's selectors to cover what it accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NewPathPolicy {
    /// An owner run: every untracked path outside scratch, refusing any
    /// candidate Git would stage as something other than a regular file.
    Owner,
    /// A claimed leaf: every untracked path outside scratch that the owner
    /// can accept as footprint widening at handoff — no traversal, Git or
    /// `.orbit` metadata, or environment-secret path. Protected names and
    /// environment patterns ignore ASCII case on every host.
    Claimed,
    /// A step recovery's repair: every untracked path outside scratch. The
    /// caller classifies protected and irregular paths itself before staging.
    Recovery,
}

/// Resolve the concrete paths a task run delivers: every tracked change and
/// every untracked path outside the scratch root. Gitignored output is never
/// listed. Refusing a protected claimed path, or an owner candidate that is
/// not a regular file, before staging preserves both its bytes and the exact
/// index the worker left behind.
/// Rename detection stays off so both source deletions and destination
/// additions are available for task attribution and path-scoped commits.
pub(super) fn task_candidate_paths(
    workspace_path: &Path,
    policy: NewPathPolicy,
) -> Result<BTreeSet<String>, OrbitError> {
    let untracked = git_output_paths(
        workspace_path,
        &["ls-files", "--others", "--exclude-standard", "-z", "--"],
    )?
    .into_iter()
    .filter(|path| !path_matches_scope(path, SCRATCH_DIR))
    .collect::<Vec<_>>();
    if policy == NewPathPolicy::Claimed {
        let protected = untracked
            .iter()
            .filter(|path| !claim_new_path_is_safe(path))
            .cloned()
            .collect::<Vec<_>>();
        if !protected.is_empty() {
            return Err(OrbitError::Execution(format!(
                "task delivery refused protected untracked paths: {protected:?}. A claimed run \
                 cannot deliver Git or `.orbit` metadata or environment files (including `.envrc`); \
                 protected names and environment patterns ignore ASCII case on every host. Write scratch \
                 and evidence under `{SCRATCH_DIR}/`. Orbit did not change the index or any \
                 listed file"
            )));
        }
    }

    let mut candidates = git_output_paths(
        workspace_path,
        &[
            "diff",
            "--name-only",
            "--no-renames",
            "-z",
            "--relative",
            "HEAD",
            "--",
        ],
    )?
    .into_iter()
    .collect::<BTreeSet<_>>();
    candidates.extend(untracked);
    if policy == NewPathPolicy::Owner {
        // A nested repository would be committed as a gitlink, and every later
        // host status or diff could then run its own config (filters, fsmonitor)
        // outside the sandbox.
        let irregular = irregular_candidate_paths(workspace_path, &candidates);
        if !irregular.is_empty() {
            return Err(OrbitError::Execution(format!(
                "task delivery refused non-regular candidate paths: {irregular:?}. Delivery \
                 commits only regular files and removals, never a symlink, a nested Git \
                 repository (gitlink) or a `.git` path. Orbit did not change the index or any \
                 listed file"
            )));
        }
    }
    Ok(candidates)
}

/// Candidates Git would not stage as a regular file or a removal: a path with
/// a `.git` component (ASCII case ignored), a symlink, or a directory. Git
/// lists an untracked nested repository as one directory path, which `add`
/// turns into a gitlink.
pub(super) fn irregular_candidate_paths<'a>(
    workspace_path: &Path,
    paths: impl IntoIterator<Item = &'a String>,
) -> Vec<String> {
    paths
        .into_iter()
        .filter(|path| {
            path.ends_with('/')
                || path
                    .split('/')
                    .any(|part| part.eq_ignore_ascii_case(".git"))
                || !regular_or_removed(workspace_path, path)
        })
        .cloned()
        .collect()
}

fn regular_or_removed(workspace_path: &Path, path: &str) -> bool {
    match std::fs::symlink_metadata(workspace_path.join(path)) {
        Ok(metadata) => metadata.file_type().is_file(),
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    }
}

/// Independently read additions and tracked type changes (rename detection
/// off), refusing anything but a regular file in the candidate tree. Added
/// paths also exclude traversal, Git or `.orbit` metadata and environment
/// secrets, including `.envrc`, matching protected names and environment
/// patterns without regard to ASCII case on every host. Return the additions
/// and the exact widening request from the
/// original admission selectors: every added path they do not cover.
pub fn validate_claim_new_paths(
    workspace_path: &Path,
    selectors: &[String],
    base: &str,
    candidate: &str,
) -> Result<(Vec<String>, Vec<String>), OrbitError> {
    let new_paths = git_output_paths(
        workspace_path,
        &[
            "diff",
            "--name-only",
            "--diff-filter=A",
            "--no-renames",
            "-z",
            base,
            candidate,
            "--",
        ],
    )?;
    let type_changed_paths = git_output_paths(
        workspace_path,
        &[
            "diff",
            "--name-only",
            "--diff-filter=T",
            "--no-renames",
            "-z",
            base,
            candidate,
            "--",
        ],
    )?;
    let mut refused = new_paths
        .iter()
        .filter(|path| !claim_new_path_is_safe(path))
        .collect::<BTreeSet<_>>();
    for path in new_paths.iter().chain(&type_changed_paths) {
        // Refuse candidate symlinks and gitlinks even when the owner's
        // worktree has not checked out this commit. Inspect the immutable
        // Git tree mode, including replacements of existing tracked files.
        let entry = super::super::git::git_output(
            workspace_path,
            &["--literal-pathspecs", "ls-tree", candidate, "--", path],
        )?;
        if !entry.starts_with("100644 ") && !entry.starts_with("100755 ") {
            refused.insert(path);
        }
    }
    if !refused.is_empty() {
        return Err(OrbitError::Execution(format!(
            "task delivery refused protected candidate paths: {refused:?}. Owner footprint widening \
             accepts any regular file outside Git and `.orbit` metadata and environment files \
             (including `.envrc`); protected names and environment patterns ignore ASCII case on every host; \
             candidate symlinks and gitlinks are refused"
        )));
    }
    let scopes = selectors
        .iter()
        .filter_map(|selector| normalize_task_scope(selector, workspace_path))
        .collect::<Vec<_>>();
    let mut widening = new_paths
        .iter()
        .filter(|path| !scopes.iter().any(|scope| path_matches_scope(path, scope)))
        .cloned()
        .collect::<Vec<_>>();
    widening.sort();
    Ok((new_paths, widening))
}

/// Assign every candidate path to the participating tasks that deliver it,
/// returning each task's paths. Nothing is refused.
///
/// A path some task's selectors cover goes to those owners — with `unique`,
/// to exactly one: the task whose agent changed it (a widening its history
/// records), else the owner naming it with an exact `file:` selector, else
/// the first owner in bundle order. A path no selector covers goes to the
/// task whose agent changed it, else the first task, and that task's
/// selectors widen to cover it with `step` provenance.
#[allow(clippy::too_many_arguments)]
pub(in crate::executor::automation::vcs) fn attribute_candidate_paths<H: RuntimeHost + ?Sized>(
    host: &H,
    run_id: &str,
    step: ContextWideningStep,
    activity: &str,
    candidate_paths: &BTreeSet<String>,
    workspace_path: &Path,
    tasks: &[Task],
    unique: bool,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut assigned = tasks
        .iter()
        .map(|task| (task.id.clone(), BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    let Some(first) = tasks.first() else {
        return assigned;
    };
    let scopes = tasks
        .iter()
        .map(|task| {
            (
                task.id.as_str(),
                task_scopes(task, workspace_path),
                exact_file_scopes(task, workspace_path),
            )
        })
        .collect::<Vec<_>>();
    let changed_by = agent_changed_paths(host, tasks);
    let mut widen = BTreeMap::<&str, Vec<String>>::new();
    for path in candidate_paths {
        let owners = scopes
            .iter()
            .filter(|(_, scopes, _)| scopes.iter().any(|scope| path_matches_scope(path, scope)))
            .collect::<Vec<_>>();
        let exact_owner = owners
            .iter()
            .find(|(_, _, exact)| exact.contains(path))
            .map(|(task_id, _, _)| *task_id);
        let owners = owners
            .iter()
            .map(|(task_id, _, _)| *task_id)
            .collect::<Vec<_>>();
        let changer = changed_by.get(path.as_str()).copied();
        let delivering = match owners.as_slice() {
            [] => {
                let task_id = changer.unwrap_or(first.id.as_str());
                widen.entry(task_id).or_default().push(path.clone());
                vec![task_id]
            }
            [owner] => vec![*owner],
            many if unique => vec![
                changer
                    .filter(|task_id| many.contains(task_id))
                    .or(exact_owner)
                    .unwrap_or(many[0]),
            ],
            many => many.to_vec(),
        };
        for task_id in delivering {
            if let Some(paths) = assigned.get_mut(task_id) {
                paths.insert(path.clone());
            }
        }
    }
    for (task_id, paths) in widen {
        // Best-effort: the delivery itself does not depend on the widening,
        // and a later delivery step widens anything still uncovered.
        if let Err(error) = host.widen_task_context_files(task_id, run_id, step, activity, &paths) {
            tracing::warn!(
                target: "orbit.engine.vcs",
                task_id,
                run_id,
                activity,
                error = %error,
                paths = ?paths,
                "could not widen task selectors for delivered paths"
            );
        }
    }
    assigned
}

/// Which participating task's agent changed each path, from the widenings
/// the tasks' histories record (the first recorded task wins).
fn agent_changed_paths<'a, H: RuntimeHost + ?Sized>(
    host: &H,
    tasks: &'a [Task],
) -> BTreeMap<String, &'a str> {
    let mut changed_by = BTreeMap::new();
    for task in tasks {
        let history = host.get_task_history(&task.id).unwrap_or_default();
        for entry in history
            .iter()
            .filter(|entry| entry.event == CONTEXT_FILES_WIDENED_EVENT)
        {
            let Some(widening) = entry
                .note
                .as_deref()
                .and_then(ContextFilesWidening::from_note)
            else {
                continue;
            };
            for selector in widening.selectors {
                if let Some(path) = selector.strip_prefix("file:") {
                    changed_by
                        .entry(path.to_string())
                        .or_insert(task.id.as_str());
                }
            }
        }
    }
    changed_by
}

fn task_scopes(task: &Task, workspace_path: &Path) -> Vec<String> {
    task.context_files
        .iter()
        .filter_map(|raw| normalize_task_scope(raw, workspace_path))
        .collect()
}

fn exact_file_scopes(task: &Task, workspace_path: &Path) -> Vec<String> {
    task.context_files
        .iter()
        .filter(|raw| raw.starts_with("file:"))
        .filter_map(|raw| normalize_task_scope(raw, workspace_path))
        .collect()
}

fn normalize_task_scope(raw: &str, workspace_path: &Path) -> Option<String> {
    let anchor = anchor_path(raw).ok()?;
    let relative = if anchor.is_absolute() {
        anchor.strip_prefix(workspace_path).ok()?.to_path_buf()
    } else {
        anchor
    };
    normalize_relative_path(&relative)
}

fn normalize_relative_path(path: &Path) -> Option<String> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }

    let value = normalized.to_string_lossy().replace('\\', "/");
    Some(if value.is_empty() {
        ".".to_string()
    } else {
        value
    })
}

fn path_matches_scope(path: &str, scope: &str) -> bool {
    path == scope
        || scope == "."
        || path
            .strip_prefix(scope)
            .is_some_and(|suffix| suffix.starts_with('/'))
}
