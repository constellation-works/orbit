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

/// Resolve the concrete paths a task worker authorized for delivery.
///
/// Tracked changes already have repository identity. A new file has no such
/// identity, so its exact task file selector (or a pre-staged index entry from
/// a writable caller) supplies intent. Refusing unknown untracked paths before
/// staging preserves both their bytes and the exact index the worker left
/// behind.
pub(super) fn task_candidate_paths(
    workspace_path: &Path,
    tasks: &[Task],
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
    )?;
    let declared_new_paths = tasks
        .iter()
        .flat_map(|task| exact_file_scopes(task, workspace_path))
        .collect::<BTreeSet<_>>();
    let unknown = untracked
        .iter()
        .filter(|path| !staged.contains(*path) && !declared_new_paths.contains(*path))
        .cloned()
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        return Err(OrbitError::Execution(format!(
            "task delivery refused unknown untracked paths: {unknown:?}. Declare every intended new \
             source path with an exact `file:` task selector (or stage it explicitly on a writable \
             index), and leave scratch and evidence outside the worktree. Orbit did not change the \
             index or any listed file"
        )));
    }

    let mut candidates = git_output_paths(
        workspace_path,
        &["diff", "--name-only", "-z", "--relative", "HEAD", "--"],
    )?
    .into_iter()
    .collect::<BTreeSet<_>>();
    candidates.extend(
        untracked
            .into_iter()
            .filter(|path| declared_new_paths.contains(path)),
    );
    Ok(candidates)
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

pub(super) fn normalize_task_scope(raw: &str, workspace_path: &Path) -> Option<String> {
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
                normalized.pop();
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }

    let value = normalized.to_string_lossy().replace('\\', "/");
    (!value.is_empty()).then_some(value)
}

pub(super) fn path_matches_scope(path: &str, scope: &str) -> bool {
    path == scope
        || scope == "."
        || path
            .strip_prefix(scope)
            .is_some_and(|suffix| suffix.starts_with('/'))
}
