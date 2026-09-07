use std::path::Path;

use orbit_core::OrbitError;

/// Remove all symlinks in a directory (non-recursive).
pub(super) fn remove_symlinks_in(dir: &Path) -> Result<(), OrbitError> {
    let entries = std::fs::read_dir(dir).map_err(|e| OrbitError::Io(e.to_string()))?;
    for entry in entries {
        let entry = entry.map_err(|e| OrbitError::Io(e.to_string()))?;
        let meta =
            std::fs::symlink_metadata(entry.path()).map_err(|e| OrbitError::Io(e.to_string()))?;
        if meta.file_type().is_symlink() {
            std::fs::remove_file(entry.path()).map_err(|e| OrbitError::Io(e.to_string()))?;
        }
    }
    Ok(())
}

/// Check if a directory is empty.
pub(super) fn is_dir_empty(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(false)
}

pub(super) fn dir_name_or_fallback(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("default")
        .to_string()
}

pub(super) fn detect_git_remote(cwd: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

pub(super) fn ensure_orbit_gitignore_entry(
    workspace_root: &Path,
    orbit_dir: &Path,
) -> Result<(), OrbitError> {
    let Some(gitignore_root) = orbit_gitignore_root(workspace_root, orbit_dir) else {
        return Ok(());
    };
    let gitignore_path = gitignore_root.join(".gitignore");
    write_orbit_gitignore_entry(&gitignore_path)
}

/// Whether workspace initialization manages checkout-local Orbit definitions.
///
/// This is the same condition that determines whether initialization writes
/// the managed `.gitignore` entry, so onboarding guidance can describe only
/// files that were actually created in the checkout.
pub(super) fn manages_checkout_local_orbit_files(workspace_root: &Path, orbit_dir: &Path) -> bool {
    orbit_gitignore_root(workspace_root, orbit_dir).is_some()
}

fn orbit_gitignore_root<'a>(workspace_root: &'a Path, orbit_dir: &'a Path) -> Option<&'a Path> {
    // Legacy: walking up from a subdir, orbit_dir is `<repo>/.orbit` whose
    // parent is a git repo root.
    if orbit_dir.file_name().and_then(|name| name.to_str()) == Some(".orbit")
        && let Some(repo_root) = orbit_dir.parent()
        && is_git_repo_root(repo_root)
    {
        return Some(repo_root);
    }

    // Default: orbit_dir lives directly inside workspace_root as `.orbit`.
    // If the user passed `--root` to relocate Orbit data outside the workspace
    // (or to a non-`.orbit` basename), skip the gitignore write — there is no
    // `<workspace>/.orbit` directory to ignore.
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
/// Blanket-ignores `.orbit/*`, then re-includes the artifact partitions that
/// travel with the repo. Lock files remain excluded.
const ORBIT_GITIGNORE_BLOCK: &[&str] = &[
    ".orbit/*",
    "!.orbit/auto_tasks/",
    "!.orbit/resources/",
    "!.orbit/routines/",
    "!.orbit/config.toml",
    ".orbit/**/*.lock",
];

/// Lines earlier managed blocks wrote that the current policy retires.
const RETIRED_ORBIT_BLOCK_LINES: &[&str] = &[
    "!.orbit/adrs/",
    ".orbit/adrs/index.sqlite*",
    ".orbit/adrs/proposed/",
    ".orbit/adrs/superseded/",
];

/// Legacy bare `.orbit` ignore lines written by earlier `orbit workspace init`
/// versions. A bare `.orbit` ignores the whole directory, so no `!`-negation
/// inside it can ever re-include a partition — these must be *replaced* by the
/// managed block, never merely supplemented.
const LEGACY_ORBIT_LINES: &[&str] = &[".orbit", ".orbit/", "/.orbit", "/.orbit/"];

/// Renders [`ORBIT_GITIGNORE_BLOCK`] as newline-terminated text.
pub(super) fn orbit_gitignore_block() -> String {
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
    // legacy bare `.orbit` line (which would defeat the re-includes) nor a
    // retired line from an older block lingers.
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
    std::fs::write(gitignore_path, next).map_err(|error| OrbitError::Io(error.to_string()))
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
