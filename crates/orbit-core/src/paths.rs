use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use orbit_common::OrbitError;

pub(crate) const ORBIT_ROOT_TOKEN: &str = "{{ORBIT_ROOT}}";

pub(crate) fn home_dir() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("HOME")
        && !home.trim().is_empty()
    {
        return Some(PathBuf::from(home));
    }
    if let Ok(profile) = std::env::var("USERPROFILE")
        && !profile.trim().is_empty()
    {
        return Some(PathBuf::from(profile));
    }
    None
}

pub(crate) fn cwd_orbit_root(cwd: &Path) -> PathBuf {
    normalize_path_components(&cwd.join(".orbit"))
}

pub(crate) fn resolve_path_value(
    raw: &str,
    base_dir: &Path,
    field_name: &str,
) -> Result<PathBuf, OrbitError> {
    let value = raw.trim();
    if value.is_empty() {
        return Err(OrbitError::InvalidInput(format!(
            "{field_name} must not be empty"
        )));
    }
    if value == "~" || value.starts_with("~/") {
        let home = home_dir().ok_or_else(|| {
            OrbitError::InvalidInput(
                "cannot expand '~' because HOME/USERPROFILE is not set".to_string(),
            )
        })?;
        let suffix = value.trim_start_matches("~/");
        return Ok(normalize_path_components(&home.join(suffix)));
    }
    let path = PathBuf::from(value);
    if path.is_relative() {
        return Ok(normalize_path_components(&base_dir.join(path)));
    }
    Ok(normalize_path_components(&path))
}

pub(crate) fn find_git_repo_root(start: &Path) -> Option<PathBuf> {
    for ancestor in start.ancestors() {
        let git_path = ancestor.join(".git");
        if git_path.is_dir() {
            return Some(ancestor.to_path_buf());
        }
        if git_path.is_file() {
            // Git worktree: .git is a file pointing to the main repo's gitdir.
            // Follow the pointer so orbit tool calls from within a worktree resolve
            // to the main repo's .orbit directory rather than creating a new one.
            if let Some(main_root) = resolve_main_repo_from_worktree_gitfile(&git_path) {
                return Some(main_root);
            }
        }
    }
    None
}

pub(crate) fn find_git_worktree_root(start: &Path) -> Option<PathBuf> {
    git_rev_parse_path(start, "--show-toplevel").or_else(|| {
        start
            .ancestors()
            .find(|ancestor| {
                let git_path = ancestor.join(".git");
                git_path.is_dir() || git_path.is_file()
            })
            .map(normalize_path_components)
    })
}

/// Resolve the linked worktree a call is running in, when that worktree is a
/// *different* checkout of the repository rooted at `canonical_repo_root`.
///
/// Returns `None` when the caller stands in the registered checkout itself,
/// outside any Git checkout, or in a checkout of another repository. A path
/// prefix test cannot answer this: managed job-run worktrees live under
/// `<repo>/.orbit/state/worktrees/**`, so they sit inside the registered
/// checkout's directory tree while being separate checkouts. Git's shared
/// directory is the identity that actually distinguishes the two.
pub(crate) fn find_linked_worktree_root(
    start: &Path,
    canonical_repo_root: &Path,
) -> Option<PathBuf> {
    let checkout = find_git_worktree_root(start)?.canonicalize().ok()?;
    if checkout == canonical_repo_root {
        return None;
    }

    let caller_git_dir = shared_git_dir(&checkout)?;
    let registered_git_dir = shared_git_dir(canonical_repo_root)?;
    (caller_git_dir == registered_git_dir).then_some(checkout)
}

fn shared_git_dir(checkout: &Path) -> Option<PathBuf> {
    let common_dir = orbit_common::fs::git::git_common_dir(checkout).ok()?;
    Some(common_dir.canonicalize().unwrap_or(common_dir))
}

pub(crate) fn find_git_main_worktree_root(start: &Path) -> Option<PathBuf> {
    find_git_main_worktree_root_with_git(start)
        .or_else(|| find_git_main_worktree_root_from_gitfile(start))
}

fn find_git_main_worktree_root_with_git(start: &Path) -> Option<PathBuf> {
    let git_dir = git_rev_parse_path(start, "--git-dir")?;
    let common_dir = git_rev_parse_path(start, "--git-common-dir")?;
    if git_dir == common_dir {
        return None;
    }

    main_root_from_common_git_dir(&common_dir).or_else(|| git_worktree_list_main_root(start))
}

fn git_rev_parse_path(start: &Path, flag: &str) -> Option<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(start)
        .args(["rev-parse", "--path-format=absolute", flag])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8(output.stdout).ok()?;
    let raw_path = stdout.lines().next()?.trim();
    if raw_path.is_empty() {
        return None;
    }

    Some(normalize_path_components(Path::new(raw_path)))
}

fn main_root_from_common_git_dir(common_dir: &Path) -> Option<PathBuf> {
    if common_dir.file_name() == Some(OsStr::new(".git")) {
        return common_dir.parent().map(normalize_path_components);
    }
    None
}

fn git_worktree_list_main_root(start: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(start)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8(output.stdout).ok()?;
    let first_worktree = stdout
        .lines()
        .find_map(|line| line.strip_prefix("worktree "))?;
    if first_worktree.trim().is_empty() {
        return None;
    }

    Some(normalize_path_components(Path::new(first_worktree)))
}

fn find_git_main_worktree_root_from_gitfile(start: &Path) -> Option<PathBuf> {
    for ancestor in start.ancestors() {
        let git_path = ancestor.join(".git");
        if git_path.is_file() {
            return resolve_main_repo_from_worktree_gitfile(&git_path);
        }
        if git_path.is_dir() {
            return None;
        }
    }
    None
}

/// Parses a worktree `.git` file to find the main repository root.
///
/// A worktree `.git` file contains a single line:
///   `gitdir: /path/to/main/repo/.git/worktrees/<name>`
///
/// The main repo root is three levels up from that path.
fn resolve_main_repo_from_worktree_gitfile(git_file: &Path) -> Option<PathBuf> {
    let content = std::fs::read_to_string(git_file).ok()?;
    let raw_path = content.strip_prefix("gitdir:")?.trim();
    let gitdir = if Path::new(raw_path).is_absolute() {
        PathBuf::from(raw_path)
    } else {
        git_file.parent()?.join(raw_path)
    };
    // gitdir = /main/repo/.git/worktrees/<name>
    // repo root = gitdir/../../../  (up past worktrees/, .git/, repo/)
    let repo_root = gitdir.parent()?.parent()?.parent()?;
    if repo_root.join(".git").is_dir() {
        Some(repo_root.to_path_buf())
    } else {
        None
    }
}

pub(crate) fn normalize_path_components(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        if matches!(component, Component::CurDir) {
            continue;
        }
        normalized.push(component.as_os_str());
    }
    if normalized.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        normalized
    }
}
