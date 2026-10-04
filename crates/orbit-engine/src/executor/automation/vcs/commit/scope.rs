use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::selector::anchor_path;
use orbit_types::task::Task;

use super::super::git::git_output_paths;

pub(super) fn filter_changed_files_for_task(
    changed_files: &BTreeSet<String>,
    workspace_path: &Path,
    task: &Task,
) -> Vec<String> {
    let scopes = task_scopes(task, workspace_path);
    if scopes.is_empty() {
        return Vec::new();
    }

    changed_files
        .iter()
        .filter(|file| scopes.iter().any(|scope| path_matches_scope(file, scope)))
        .cloned()
        .collect()
}

/// The run-scoped scratch root (`$ORBIT_SCRATCH_DIR`). Repositories normally
/// ignore it; it is excluded here as well so no selector can deliver scratch.
const SCRATCH_DIR: &str = ".orbit/tmp";

/// Which task selectors declare a new (untracked) path as intended delivery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NewPathIntent {
    /// Only an exact `file:` selector. A worker that can write task state
    /// appends one after creating the file; directory selectors are ownership
    /// boundaries, not new-file intent.
    ExactFile,
    /// The frozen selectors, including file siblings and their `tests/` child. A
    /// claimed worker cannot write owner task state or stage on a writable
    /// index, so the footprint the owner admitted is the only intent it can
    /// carry (distributed-drain design §3).
    AdmittedFootprint,
}

/// Resolve the concrete paths a task worker authorized for delivery.
///
/// Tracked changes already have repository identity. A new file has no such
/// identity, so a task selector `intent` accepts (or a pre-staged index entry
/// from a writable caller) supplies intent. Untracked paths under the scratch
/// root are never candidates. Refusing unknown untracked paths before staging
/// preserves both their bytes and the exact index the worker left behind.
pub(super) fn task_candidate_paths(
    workspace_path: &Path,
    tasks: &[Task],
    intent: NewPathIntent,
) -> Result<BTreeSet<String>, OrbitError> {
    let staged = git_output_paths(
        workspace_path,
        &["diff", "--cached", "--name-only", "-z", "--relative"],
    )?
    .into_iter()
    .collect::<BTreeSet<_>>();
    let untracked = git_output_paths(
        workspace_path,
        &["ls-files", "--others", "--exclude-standard", "-z", "--"],
    )?
    .into_iter()
    .filter(|path| !path_matches_scope(path, SCRATCH_DIR))
    .collect::<Vec<_>>();
    let exact_scopes = tasks
        .iter()
        .flat_map(|task| exact_file_scopes(task, workspace_path))
        .collect::<BTreeSet<_>>();
    let declared = |path: &str| match intent {
        NewPathIntent::ExactFile => exact_scopes.contains(path),
        NewPathIntent::AdmittedFootprint => tasks
            .iter()
            .any(|task| claimed_new_path_matches(path, &task.context_files, workspace_path)),
    };
    let unknown = untracked
        .iter()
        .filter(|path| !staged.contains(*path) && !declared(path))
        .cloned()
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        let remedy = match intent {
            NewPathIntent::ExactFile => {
                "Declare every intended new source path with an exact `file:` task selector (or \
                 stage it explicitly on a writable index)"
            }
            NewPathIntent::AdmittedFootprint => {
                "A claimed run delivers new paths only inside the claim's frozen footprint; keep \
                 new source under an admitted `file:` or `dir:` selector, or ask the owner to \
                 widen the task before a fresh claim"
            }
        };
        return Err(OrbitError::Execution(format!(
            "task delivery refused unknown untracked paths: {unknown:?}. {remedy}, and write \
             scratch and evidence under `{SCRATCH_DIR}/`. Orbit did not change the index or any \
             listed file"
        )));
    }

    let mut candidates = git_output_paths(
        workspace_path,
        &["diff", "--name-only", "-z", "--relative", "HEAD", "--"],
    )?
    .into_iter()
    .collect::<BTreeSet<_>>();
    candidates.extend(untracked.into_iter().filter(|path| declared(path)));
    Ok(candidates)
}

/// Verify new paths from the owner's observed Git candidate against the frozen
/// selectors. Neither reported selector additions nor the worker's path list
/// can widen this decision. Rename detection is disabled to check destinations.
pub fn validate_claim_new_paths(
    workspace_path: &Path,
    selectors: &[String],
    base: &str,
    candidate: &str,
) -> Result<(), OrbitError> {
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
    let unknown = new_paths
        .into_iter()
        .filter(|path| {
            path_matches_scope(path, SCRATCH_DIR)
                || !claimed_new_path_matches(path, selectors, workspace_path)
        })
        .collect::<Vec<_>>();
    if unknown.is_empty() {
        return Ok(());
    }
    Err(OrbitError::Execution(format!(
        "task delivery refused unknown untracked paths: {unknown:?}. Owner handoff validation \
         requires new paths under an admitted directory or beside an admitted file, including \
         its module's tests/ child"
    )))
}

fn claimed_new_path_matches(path: &str, selectors: &[String], workspace: &Path) -> bool {
    selectors.iter().any(|selector| {
        let Some(scope) = normalize_task_scope(selector, workspace) else {
            return false;
        };
        if path_matches_scope(path, &scope) {
            return true;
        }
        if !selector.starts_with("file:") {
            return false;
        }
        let parent = Path::new(&scope).parent().unwrap_or_else(|| Path::new(""));
        let candidate = Path::new(path);
        candidate.parent() == Some(parent) || candidate.starts_with(parent.join("tests"))
    })
}

pub(super) fn ensure_candidate_ownership(
    candidate_paths: &BTreeSet<String>,
    workspace_path: &Path,
    tasks: &[Task],
    require_unique_owner: bool,
) -> Result<(), OrbitError> {
    let scopes = tasks
        .iter()
        .map(|task| {
            (
                task.id.as_str(),
                task_scopes(task, workspace_path)
                    .into_iter()
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();
    let mut unowned = Vec::new();
    let mut ambiguous = Vec::new();
    for path in candidate_paths {
        let owners = scopes
            .iter()
            .filter(|(_, scopes)| scopes.iter().any(|scope| path_matches_scope(path, scope)))
            .map(|(task_id, _)| *task_id)
            .collect::<Vec<_>>();
        if owners.is_empty() {
            unowned.push(path.clone());
        } else if require_unique_owner && owners.len() > 1 {
            ambiguous.push((path.clone(), owners));
        }
    }
    if unowned.is_empty() && ambiguous.is_empty() {
        return Ok(());
    }

    Err(OrbitError::Execution(format!(
        "task delivery refused candidate paths without deterministic task ownership: \
         unowned={unowned:?}, ambiguous={ambiguous:?}. Update the participating tasks' explicit \
         file or directory selectors so every path has {} owner before publication. Orbit did not \
         change the index or any listed file",
        if require_unique_owner {
            "exactly one"
        } else {
            "at least one"
        }
    )))
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
