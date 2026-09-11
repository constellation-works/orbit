use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::fs::selector::{
    anchor_path, canonical_selector_in_workspace, exists_in_workspace,
};
use orbit_types::task::{TaskHistoryEntry, TaskType};
use std::path::{Path, PathBuf};

use crate::OrbitRuntime;
use crate::paths::find_linked_worktree_root;

pub(super) fn normalize_workspace_path(
    repo_root: &Path,
    workspace: Option<&str>,
) -> Result<Option<String>, OrbitError> {
    let Some(workspace) = workspace.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };

    let canonical_repo_root = repo_root.canonicalize().map_err(|error| {
        OrbitError::InvalidInput(format!(
            "failed to resolve repository root '{}': {error}",
            repo_root.display()
        ))
    })?;
    let candidate = if Path::new(workspace).is_absolute() {
        PathBuf::from(workspace)
    } else {
        canonical_repo_root.join(workspace)
    };
    let canonical_workspace = candidate.canonicalize().map_err(|error| {
        OrbitError::InvalidInput(format!(
            "workspace (`workspace_path` on the dashboard API) '{}' must be a filesystem path to \
             an existing directory inside the repository — e.g. the repository root '.' — never a \
             logical workspace id such as a bridge `ws_*` id: {error}",
            candidate.display()
        ))
    })?;
    if !canonical_workspace.is_dir() {
        return Err(OrbitError::InvalidInput(format!(
            "workspace (`workspace_path` on the dashboard API) '{}' must be a directory inside \
             the repository, not a file",
            canonical_workspace.display()
        )));
    }

    if !canonical_workspace.starts_with(&canonical_repo_root) {
        return Err(OrbitError::InvalidInput(format!(
            "workspace (`workspace_path` on the dashboard API) '{}' must be a filesystem path \
             inside repository '{}', not outside it",
            canonical_workspace.display(),
            canonical_repo_root.display()
        )));
    }

    Ok(Some(canonical_workspace.to_string_lossy().into_owned()))
}

pub(crate) fn context_workspace_root(repo_root: &Path, workspace_path: Option<&str>) -> PathBuf {
    workspace_path
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root.to_path_buf())
}

pub(super) fn context_files_pruned_history_entry(
    actor: &str,
    dropped: &[String],
) -> TaskHistoryEntry {
    TaskHistoryEntry {
        at: Utc::now(),
        by: actor.to_string(),
        event: "context_files_pruned".to_string(),
        note: Some(format!(
            "dropped: {} (selector anchor not found in workspace)",
            dropped.join(", ")
        )),
        from_status: None,
        to_status: None,
    }
}

/// Canonicalize context selectors for a task write, leaving selectors that do
/// not exist yet in place.
///
/// The core write path stays permissive on purpose: task-pilot apply,
/// automation seeding, and runtime host updates legitimately record targets the
/// task is about to create, and read paths prune what is missing. Operator
/// surfaces guard typos separately through
/// [`OrbitRuntime::ensure_context_selectors_exist`].
pub(crate) fn normalize_context_files_for_write(
    candidates: Vec<String>,
    workspace_root: &Path,
) -> Result<Vec<String>, OrbitError> {
    candidates
        .into_iter()
        .map(|entry| {
            canonical_selector_in_workspace(entry.as_str(), workspace_root)
                .map_err(|error| OrbitError::InvalidInput(error.to_string()))
        })
        .collect()
}

impl OrbitRuntime {
    /// Reject context selectors that do not name an existing target in the
    /// workspace the task write will use.
    ///
    /// This is an operator-surface guard: `orbit task add` / `orbit task
    /// update` and the `orbit.task.add` / `orbit.task.update` tools call it so
    /// a mistyped selector cannot ship a task whose context is dead on
    /// arrival. Both surfaces expose an explicit escape for the deliberate
    /// not-yet-created target. `add_task` and `update_task` themselves stay
    /// permissive so internal callers are unaffected.
    pub fn ensure_context_selectors_exist(
        &self,
        selectors: &[String],
        workspace_path: Option<&str>,
    ) -> Result<(), OrbitError> {
        if selectors.is_empty() {
            return Ok(());
        }

        let roots = self.context_selector_roots(workspace_path)?;

        selectors
            .iter()
            .try_for_each(|selector| ensure_selector_resolves(selector, &roots))
    }

    /// Resolve the checkouts this call may validate selectors against.
    fn context_selector_roots(
        &self,
        workspace_path: Option<&str>,
    ) -> Result<ContextSelectorRoots, OrbitError> {
        let repo_root = &self.paths().repo_root;
        let canonical_repo_root = repo_root.canonicalize().map_err(|error| {
            OrbitError::InvalidInput(format!(
                "failed to resolve repository root '{}': {error}",
                repo_root.display()
            ))
        })?;

        let normalized_workspace = normalize_workspace_path(repo_root, workspace_path)?;
        let workspace_root =
            context_workspace_root(&canonical_repo_root, normalized_workspace.as_deref());
        let workspace = workspace_root.canonicalize().map_err(|error| {
            OrbitError::InvalidInput(format!(
                "failed to resolve workspace root '{}': {error}",
                workspace_root.display()
            ))
        })?;

        let caller_worktree = std::env::current_dir()
            .ok()
            .and_then(|cwd| find_linked_worktree_root(&cwd, &canonical_repo_root))
            .and_then(|worktree| {
                mirrored_workspace_root(&workspace, &canonical_repo_root, &worktree)
            });

        Ok(ContextSelectorRoots {
            workspace,
            caller_worktree,
        })
    }
}

/// The checkouts an operator-supplied context selector may resolve against.
struct ContextSelectorRoots {
    /// Workspace root inside the registered checkout.
    workspace: PathBuf,
    /// The same workspace root inside the linked worktree the call runs in,
    /// when the caller is in one and that directory exists there. A managed
    /// job run executes in such a worktree, so a file it just created exists
    /// only here until the work merges.
    caller_worktree: Option<PathBuf>,
}

/// Place the registered checkout's workspace root at the same relative
/// position inside `worktree`, so a sub-directory workspace keeps its meaning
/// there. Returns `None` when that directory does not exist in the worktree.
fn mirrored_workspace_root(
    workspace: &Path,
    canonical_repo_root: &Path,
    worktree: &Path,
) -> Option<PathBuf> {
    let relative = workspace.strip_prefix(canonical_repo_root).ok()?;
    let mirrored = worktree.join(relative).canonicalize().ok()?;
    mirrored.is_dir().then_some(mirrored)
}

/// Check one operator-supplied selector against the checkouts this call may
/// use: it must be a supported kind, name an existing anchor, and match that
/// target's file/directory kind.
///
/// The registered checkout answers first, so its message is the one an
/// operator sees for a genuinely dead selector. The caller's worktree answers
/// second: a file created there does not exist in the registered checkout yet,
/// and declaring it must not require turning the guard off for the whole call.
fn ensure_selector_resolves(entry: &str, roots: &ContextSelectorRoots) -> Result<(), OrbitError> {
    let trimmed = entry.trim();
    if trimmed.is_empty() {
        return Err(OrbitError::InvalidInput(
            "selector input must not be empty".to_string(),
        ));
    }

    if trimmed.starts_with("module:") || trimmed.starts_with("command:") {
        return Err(unsupported_selector_kind(entry));
    }

    match resolve_selector_in(entry, trimmed, &roots.workspace) {
        Ok(()) => Ok(()),
        Err(registered_failure) => match roots.caller_worktree.as_deref() {
            Some(worktree) => {
                resolve_selector_in(entry, trimmed, worktree).map_err(|_| registered_failure)
            }
            None => Err(registered_failure),
        },
    }
}

/// Resolve one already-screened selector against a single canonicalized
/// checkout root.
fn resolve_selector_in(
    entry: &str,
    trimmed: &str,
    canonical_workspace: &Path,
) -> Result<(), OrbitError> {
    let canonical =
        canonical_selector_in_workspace(trimmed, canonical_workspace).map_err(|error| {
            OrbitError::InvalidInput(format!("selector `{entry}` is invalid: {error}"))
        })?;

    if canonical.starts_with("module:") || canonical.starts_with("command:") {
        return Err(unsupported_selector_kind(entry));
    }

    if !exists_in_workspace(&canonical, canonical_workspace) {
        return Err(OrbitError::InvalidInput(format!(
            "selector `{entry}` does not resolve to an existing in-workspace target"
        )));
    }

    let anchor = anchor_path(&canonical).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "selector `{entry}` has no filesystem anchor: {error}"
        ))
    })?;
    let resolved = canonical_workspace.join(anchor);
    let matches_target_kind = if canonical.starts_with("dir:") {
        resolved.is_dir()
    } else {
        resolved.is_file()
    };

    if !matches_target_kind {
        return Err(OrbitError::InvalidInput(format!(
            "selector `{entry}` does not match the target's file/directory kind"
        )));
    }

    Ok(())
}

fn unsupported_selector_kind(entry: &str) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "selector `{entry}` must use file:, dir:, or symbol:"
    ))
}

pub(crate) use crate::runtime::task::canonicalize_context_files_for_read;

/// Compute advisory warnings for an `orbit.task.add` call based on the raw
/// `context_files` (or legacy `context`) values supplied by the caller and the
/// effective task type. Warnings are purely response-time signals; they do not
/// block creation or mutate the stored task.
pub(crate) fn compute_task_add_warnings(
    context_files: &[String],
    task_type: TaskType,
) -> Vec<String> {
    let mut warnings = Vec::new();

    let is_chore = task_type == TaskType::Chore;
    if context_files.is_empty() && !is_chore {
        warnings.push(
            "task created without context_files — consider adding selectors for files/dirs/symbols this task will modify (use orbit.task.update with context_files)".to_string(),
        );
    }

    let over: Vec<String> = context_files
        .iter()
        .filter(|e| is_over_inclusion_selector(e))
        .cloned()
        .collect();
    if !over.is_empty() {
        let listed = over.join(", ");
        warnings.push(format!(
            "context_files contains entries that look like reference material rather than modification targets: {}. Cite reference docs in the description; reserve context_files for files the task will modify or delete.",
            listed
        ));
    }

    warnings
}

/// Returns true for entries that match the over-inclusion patterns (repo-root
/// convention docs, docs/design-patterns/**, .claude/**/CLAUDE.md). Feature
/// design docs under docs/design/<feature>/ are deliberately excluded per
/// CLAUDE.md "Same-PR updates".
fn is_over_inclusion_selector(entry: &str) -> bool {
    let raw = entry.trim();
    if raw.is_empty() {
        return false;
    }
    // Strip selector prefix if present; for symbol: take only the path portion.
    let path_part = if let Some(p) = raw.strip_prefix("file:") {
        p
    } else if let Some(p) = raw.strip_prefix("dir:") {
        p
    } else if let Some(p) = raw.strip_prefix("symbol:") {
        p.split('#').next().unwrap_or(p)
    } else {
        raw
    };
    let p = path_part.trim();

    // docs/design-patterns/** (including bare dir)
    if p == "docs/design-patterns" || p.starts_with("docs/design-patterns/") {
        return true;
    }

    // repo-root convention files (exact match)
    if matches!(
        p,
        "ARCHITECTURE.md" | "CLAUDE.md" | "RELEASING.md" | "CHANGELOG.md" | "README.md"
    ) {
        return true;
    }

    // .claude/CLAUDE.md or .claude/**/CLAUDE.md
    if p == ".claude/CLAUDE.md" || (p.starts_with(".claude/") && p.ends_with("/CLAUDE.md")) {
        return true;
    }

    false
}

pub(super) fn extract_task_path_mentions(text: &str) -> Vec<String> {
    let mut paths = std::collections::BTreeSet::new();
    for raw in text.split_whitespace() {
        let trimmed = raw.trim_matches(|ch: char| {
            matches!(
                ch,
                '`' | '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ',' | ':' | ';'
            )
        });
        let trimmed = trimmed.trim_end_matches(&['.', '!', '?'][..]);
        if let Some(path) = normalize_path_token(trimmed) {
            paths.insert(path);
        }
    }
    paths.into_iter().collect()
}

pub(super) fn normalize_path_token(token: &str) -> Option<String> {
    if token.is_empty() || token.contains("://") {
        return None;
    }

    let token = token.trim_matches('`').trim_end_matches('/');
    if token.is_empty() {
        return None;
    }
    let anchored = anchor_path(token)
        .ok()
        .map(|path| path.to_string_lossy().replace('\\', "/"));
    let path_token = anchored.as_deref().unwrap_or(token).to_string();

    let standalone_files = [
        "Cargo.toml",
        "Cargo.lock",
        "Makefile",
        "README.md",
        "AGENTS.md",
        "CLAUDE.md",
    ];
    let has_known_prefix = [
        "./",
        "../",
        "crates/",
        "src/",
        "tests/",
        "scripts/",
        "docs/",
        "examples/",
        ".orbit/",
    ]
    .iter()
    .any(|prefix| path_token.starts_with(prefix));
    let last_segment_looks_like_file = path_token
        .rsplit('/')
        .next()
        .is_some_and(|segment| segment.contains('.'));

    if has_known_prefix
        || standalone_files.contains(&path_token.as_str())
        || (path_token.contains('/') && last_segment_looks_like_file)
    {
        return Some(anchored.unwrap_or(path_token));
    }

    None
}

pub(super) fn task_path_exists(workspace_root: &Path, raw_path: &str) -> bool {
    exists_in_workspace(raw_path, workspace_root)
}
