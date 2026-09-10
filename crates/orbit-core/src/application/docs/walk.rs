use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use orbit_common::OrbitError;

use super::config::DocsRoot;
use super::frontmatter::read_doc_tolerant;
use super::path_util::{path_to_slash_string, repo_relative_path};
use super::types::{DocRecord, WalkedDoc};

#[cfg(test)]
thread_local! {
    static GIT_CHECK_IGNORE_INVOCATIONS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn record_git_check_ignore_invocation() {
    GIT_CHECK_IGNORE_INVOCATIONS.with(|calls| calls.set(calls.get() + 1));
}

/// Reset the git-check-ignore invocation counter (test helper).
/// Visibility widened for ORB-00250 sibling tests (tests/walk.rs + shared
/// helpers in tests/mod.rs) per docs/design-patterns/test_layout.md.
#[cfg(test)]
pub(super) fn reset_git_check_ignore_invocations() {
    GIT_CHECK_IGNORE_INVOCATIONS.with(|calls| calls.set(0));
}

/// Return the current git-check-ignore invocation count (test helper).
/// Visibility widened for ORB-00250 sibling tests.
#[cfg(test)]
pub(super) fn git_check_ignore_invocations() -> usize {
    GIT_CHECK_IGNORE_INVOCATIONS.with(std::cell::Cell::get)
}

/// Walk the configured roots and return one record per doc, dropping the
/// bodies. Callers that need the text should walk with
/// [`walk_docs_with_bodies`] rather than re-opening each file.
pub fn walk_docs_roots(repo_root: &Path, roots: &[DocsRoot]) -> Result<Vec<DocRecord>, OrbitError> {
    Ok(walk_docs_with_bodies(repo_root, roots)?
        .into_iter()
        .map(|doc| doc.record)
        .collect())
}

/// Walk the configured roots, keeping each doc's body from the same read that
/// produced its frontmatter.
pub(super) fn walk_docs_with_bodies(
    repo_root: &Path,
    roots: &[DocsRoot],
) -> Result<Vec<WalkedDoc>, OrbitError> {
    // A path may be reachable from more than one configured root; if any
    // contributing root names it explicitly (respect_gitignore = false),
    // that override wins over a root that would still filter it.
    let mut candidates: HashMap<PathBuf, bool> = HashMap::new();
    for root in roots {
        let mut found = Vec::new();
        for path in expand_root(repo_root, &root.path)? {
            if path_is_or_contains_dot_orbit(repo_root, &path) {
                continue;
            }
            if path.is_file() {
                maybe_push_doc_candidate(repo_root, &path, &mut found)?;
            } else if path.is_dir() {
                walk_dir(repo_root, &path, &mut found)?;
            }
        }
        for relative in found {
            candidates
                .entry(relative)
                .and_modify(|respect_gitignore| {
                    *respect_gitignore = *respect_gitignore && root.respect_gitignore;
                })
                .or_insert(root.respect_gitignore);
        }
    }

    let gitignore_checked = candidates
        .iter()
        .filter(|(_, respect_gitignore)| **respect_gitignore)
        .map(|(relative, _)| relative.clone())
        .collect::<Vec<_>>();
    let ignored = git_ignored_paths(repo_root, &gitignore_checked);

    let mut docs = Vec::new();
    for (relative, respect_gitignore) in candidates {
        if respect_gitignore && ignored.contains(&relative) {
            continue;
        }
        let path = repo_root.join(&relative);
        let parsed = read_doc_tolerant(&relative, &path)?;
        docs.push(WalkedDoc {
            record: DocRecord {
                path: path_to_slash_string(&relative),
                frontmatter: parsed.frontmatter,
            },
            body: parsed.body,
        });
    }
    docs.sort_by(|left, right| left.record.path.cmp(&right.record.path));
    docs.dedup_by(|left, right| left.record.path == right.record.path);
    Ok(docs)
}

pub(super) fn expand_root(repo_root: &Path, root: &str) -> Result<Vec<PathBuf>, OrbitError> {
    let trimmed = root.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let root_path = Path::new(trimmed);
    let absolute = if root_path.is_absolute() {
        root_path.to_path_buf()
    } else {
        repo_root.join(root_path)
    };
    if !trimmed.contains('*') {
        if absolute.exists() {
            return Ok(vec![validated_docs_root_path(repo_root, &absolute)?]);
        }
        return Ok(Vec::new());
    }

    // Wildcard expansion is intentionally workspace-relative. Apart from
    // making the supported pattern language unambiguous, rejecting rooted and
    // parent-directory components keeps configured text from selecting the
    // starting point for a filesystem walk outside the repository.
    if root_path.is_absolute()
        || root_path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::Prefix(_)
            )
        })
    {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    expand_wildcard_segments(repo_root, Path::new(trimmed), &mut out)?;
    Ok(out)
}

/// Resolve a discovered docs path before it reaches a filesystem operation.
///
/// Docs roots are workspace-relative configuration, so canonicalizing both
/// sides prevents `..` traversal and symlinks from redirecting a walk outside
/// the repository. Callers only use this after confirming the candidate
/// exists; a missing literal root remains the walker's documented no-op.
fn validated_docs_root_path(repo_root: &Path, candidate: &Path) -> Result<PathBuf, OrbitError> {
    let canonical_repo = repo_root.canonicalize().map_err(|error| {
        OrbitError::Io(format!("canonicalize {}: {error}", repo_root.display()))
    })?;
    let canonical_candidate = candidate.canonicalize().map_err(|error| {
        OrbitError::Io(format!("canonicalize {}: {error}", candidate.display()))
    })?;
    if !canonical_candidate.starts_with(&canonical_repo) {
        return Err(OrbitError::InvalidInput(format!(
            "docs root path must stay inside the workspace root: {}",
            candidate.display()
        )));
    }
    Ok(canonical_candidate)
}

fn expand_wildcard_segments(
    base: &Path,
    pattern: &Path,
    out: &mut Vec<PathBuf>,
) -> Result<(), OrbitError> {
    fn rec(
        repo_root: &Path,
        base: &Path,
        parts: &[String],
        out: &mut Vec<PathBuf>,
    ) -> Result<(), OrbitError> {
        if parts.is_empty() {
            if base.exists() {
                out.push(validated_docs_root_path(repo_root, base)?);
            }
            return Ok(());
        }
        let head = &parts[0];
        let tail = &parts[1..];
        if head == "*" {
            if !base.is_dir() {
                return Ok(());
            }
            let base = validated_docs_root_path(repo_root, base)?;
            let entries = fs::read_dir(&base)
                .map_err(|error| OrbitError::Io(format!("read {}: {error}", base.display())))?;
            for entry in entries {
                let entry = entry.map_err(|error| OrbitError::Io(error.to_string()))?;
                if entry
                    .file_type()
                    .map_err(|error| OrbitError::Io(error.to_string()))?
                    .is_dir()
                {
                    rec(repo_root, &entry.path(), tail, out)?;
                }
            }
            return Ok(());
        }
        rec(repo_root, &base.join(head), tail, out)
    }

    let parts = pattern
        .components()
        .filter_map(super::path_util::component_str)
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    rec(base, base, &parts, out)
}

fn walk_dir(repo_root: &Path, dir: &Path, candidates: &mut Vec<PathBuf>) -> Result<(), OrbitError> {
    if should_skip_dir(repo_root, dir) {
        return Ok(());
    }
    let mut entries = fs::read_dir(dir)
        .map_err(|error| OrbitError::Io(format!("read {}: {error}", dir.display())))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| OrbitError::Io(error.to_string()))?;
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| OrbitError::Io(error.to_string()))?;
        if file_type.is_dir() {
            walk_dir(repo_root, &path, candidates)?;
        } else if file_type.is_file() {
            maybe_push_doc_candidate(repo_root, &path, candidates)?;
        }
    }
    Ok(())
}

fn maybe_push_doc_candidate(
    repo_root: &Path,
    path: &Path,
    candidates: &mut Vec<PathBuf>,
) -> Result<(), OrbitError> {
    if path.extension().and_then(|value| value.to_str()) != Some("md") {
        return Ok(());
    }
    if path_is_or_contains_dot_orbit(repo_root, path) {
        return Ok(());
    }
    let relative = repo_relative_path(repo_root, path)?;
    candidates.push(relative);
    Ok(())
}

fn should_skip_dir(repo_root: &Path, dir: &Path) -> bool {
    let Some(name) = dir.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    if matches!(name, ".orbit" | ".git" | "node_modules" | "target") {
        return true;
    }
    path_is_or_contains_dot_orbit(repo_root, dir)
}

pub(crate) fn path_is_or_contains_dot_orbit(repo_root: &Path, path: &Path) -> bool {
    let relative = path.strip_prefix(repo_root).unwrap_or(path);
    relative.components().any(
        |component| matches!(component, std::path::Component::Normal(value) if value == ".orbit"),
    )
}

fn git_ignored_paths(repo_root: &Path, relatives: &[PathBuf]) -> HashSet<PathBuf> {
    let mut ignored = HashSet::new();
    if relatives.is_empty() {
        return ignored;
    }
    #[cfg(test)]
    record_git_check_ignore_invocation();
    let mut child = match Command::new("git")
        .arg("check-ignore")
        .arg("-z")
        .arg("--stdin")
        .current_dir(repo_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return ignored,
    };
    let mut wrote_all = true;
    if let Some(mut stdin) = child.stdin.take() {
        for relative in relatives {
            let path = path_to_slash_string(relative);
            if stdin.write_all(path.as_bytes()).is_err() || stdin.write_all(b"\0").is_err() {
                wrote_all = false;
                break;
            }
        }
    }
    if !wrote_all {
        let _ = child.wait();
        return ignored;
    }
    let output = match child.wait_with_output() {
        Ok(output) => output,
        Err(_) => return ignored,
    };
    if !output.status.success() {
        return ignored;
    }
    for raw_path in output.stdout.split(|byte| *byte == 0) {
        if raw_path.is_empty() {
            continue;
        }
        ignored.insert(PathBuf::from(String::from_utf8_lossy(raw_path).to_string()));
    }
    ignored
}
