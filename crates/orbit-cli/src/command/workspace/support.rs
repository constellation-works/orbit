use std::path::{Path, PathBuf};

use orbit_common::fs::io::atomic_write_text;
use orbit_core::OrbitError;

pub(super) fn dir_name_or_fallback(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("default")
        .to_string()
}

/// The `origin` URL, or `None` when there is none. A Git that timed out is
/// an error, never read as no remote; one that could not start (no `git` on
/// `PATH`) is logged and reads as no remote.
pub(super) fn detect_git_remote(cwd: &Path) -> Result<Option<String>, OrbitError> {
    match orbit_common::fs::git::run_git(cwd, &["remote", "get-url", "origin"]) {
        Ok(output) => Ok(output.success.then(|| output.stdout.trim().to_string())),
        Err(error @ OrbitError::ProcessTimeout { .. }) => Err(error),
        Err(error) => {
            tracing::warn!("cannot read the origin remote URL: {error}");
            Ok(None)
        }
    }
}

pub(super) fn ensure_orbit_gitignore_entry(
    workspace_root: &Path,
    orbit_dir: &Path,
) -> Result<(), OrbitError> {
    let Some(gitignore_path) = orbit_gitignore_path(workspace_root, orbit_dir) else {
        return Ok(());
    };
    write_orbit_gitignore_entry(&gitignore_path)
}

/// The `.gitignore` that carries the managed `.orbit/` block, if any.
///
/// A checkout-local Orbit data directory is ignored at its repository root.
/// A checkout at a Git repository root is ignored even when `--root` keeps
/// Orbit's data directory elsewhere: delivery worktrees are still created
/// under `<checkout>/.orbit/state/worktrees/`, and left unignored each one is
/// an untracked nested checkout that breaks the primary-checkout Git snapshot.
pub(super) fn orbit_gitignore_path(workspace_root: &Path, orbit_dir: &Path) -> Option<PathBuf> {
    checkout_local_orbit_root(workspace_root, orbit_dir)
        .or_else(|| is_git_repo_root(workspace_root).then_some(workspace_root))
        .map(|root| root.join(".gitignore"))
}

/// Whether workspace initialization writes Orbit definitions into the checkout.
///
/// False when `--root` relocates the data directory, so onboarding guidance
/// can point at the files that were actually created.
pub(super) fn manages_checkout_local_orbit_files(workspace_root: &Path, orbit_dir: &Path) -> bool {
    checkout_local_orbit_root(workspace_root, orbit_dir).is_some()
}

/// The repository root whose `.orbit` is the Orbit data directory itself.
fn checkout_local_orbit_root<'a>(
    workspace_root: &'a Path,
    orbit_dir: &'a Path,
) -> Option<&'a Path> {
    // Legacy: walking up from a subdir, orbit_dir is `<repo>/.orbit` whose
    // parent is a git repo root.
    if orbit_dir.file_name().and_then(|name| name.to_str()) == Some(".orbit")
        && let Some(repo_root) = orbit_dir.parent()
        && is_git_repo_root(repo_root)
    {
        return Some(repo_root);
    }

    // Default: orbit_dir lives directly inside workspace_root as `.orbit`.
    if is_git_repo_root(workspace_root) && orbit_dir == workspace_root.join(".orbit") {
        return Some(workspace_root);
    }

    None
}

fn is_git_repo_root(path: &Path) -> bool {
    path.join(".git").exists()
}

/// The Orbit-managed `.gitignore` block written by `orbit workspace init`.
///
/// `.orbit/` is per-user checkout state (config, routines, auto-tasks,
/// resources). It is not a repository artifact, so the managed block ignores
/// the whole directory with no `!` re-includes.
const ORBIT_GITIGNORE_BLOCK: &[&str] = &[
    "# Orbit per-user state — not a repository artifact.",
    ".orbit/",
];

/// Lines earlier managed blocks wrote that the current policy retires.
///
/// Includes the previous re-include form (`.orbit/*` plus `!.orbit/...`) so
/// `init`/`sync` rewrite an existing checkout instead of leaving stale
/// negations above the new block.
const RETIRED_ORBIT_BLOCK_LINES: &[&str] = &[
    ".orbit/*",
    "!.orbit/auto_tasks/",
    "!.orbit/resources/",
    "!.orbit/routines/",
    "!.orbit/config.toml",
    "!.orbit/learnings/",
    "!.orbit/adrs/",
    ".orbit/adrs/index.sqlite*",
    ".orbit/adrs/proposed/",
    ".orbit/adrs/superseded/",
    ".orbit/**/*.lock",
];

/// Legacy ignore lines written by earlier `orbit workspace init` versions that
/// are *not* the canonical `.orbit/` form. A bare `.orbit/` is the desired
/// managed line and must not be treated as something to replace.
const LEGACY_ORBIT_LINES: &[&str] = &[".orbit", "/.orbit", "/.orbit/"];

/// Renders [`ORBIT_GITIGNORE_BLOCK`] as newline-terminated text.
fn orbit_gitignore_block() -> String {
    let mut block = String::new();
    for line in ORBIT_GITIGNORE_BLOCK {
        block.push_str(line);
        block.push('\n');
    }
    block
}

fn write_orbit_gitignore_entry(gitignore_path: &Path) -> Result<(), OrbitError> {
    let content = match std::fs::read_to_string(gitignore_path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(OrbitError::Io(error.to_string())),
    };

    // Idempotent no-op: the full managed block is already present and neither a
    // non-canonical legacy line nor a retired line from an older block lingers.
    if gitignore_has_managed_block(&content)
        && !gitignore_has_legacy_orbit_line(&content)
        && !gitignore_has_retired_block_line(&content)
    {
        return Ok(());
    }

    // Rebuild: drop any legacy bare lines, any line retired from an older
    // managed block, and any pre-existing managed-block lines (partial or
    // full), preserving the operator's other content and order, then append the
    // canonical block once at the end.
    let mut next = String::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if LEGACY_ORBIT_LINES.contains(&trimmed)
            || RETIRED_ORBIT_BLOCK_LINES.contains(&trimmed)
            || ORBIT_GITIGNORE_BLOCK.contains(&trimmed)
        {
            continue;
        }
        next.push_str(line);
        next.push('\n');
    }
    next.push_str(&orbit_gitignore_block());
    atomic_write_text(gitignore_path, &next).map_err(|error| OrbitError::Io(error.to_string()))
}

fn gitignore_has_managed_block(content: &str) -> bool {
    let lines: Vec<&str> = content.lines().map(str::trim).collect();
    ORBIT_GITIGNORE_BLOCK
        .iter()
        .all(|entry| lines.contains(entry))
}

fn gitignore_has_legacy_orbit_line(content: &str) -> bool {
    content
        .lines()
        .any(|line| LEGACY_ORBIT_LINES.contains(&line.trim()))
}

fn gitignore_has_retired_block_line(content: &str) -> bool {
    content
        .lines()
        .any(|line| RETIRED_ORBIT_BLOCK_LINES.contains(&line.trim()))
}
