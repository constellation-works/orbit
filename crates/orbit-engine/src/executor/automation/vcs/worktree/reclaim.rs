//! Declared rebuildable paths; never execute checkout-authored cleanup commands.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::path_glob::RelativePathGlob;
use serde::Serialize;

use super::super::git::git_output_paths;

/// One declared path inspected or deleted by reclamation.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorktreeReclaimReport {
    /// Path relative to the kept worktree.
    pub path: PathBuf,
    /// First configured pattern matching this path (overlaps count only once).
    pub pattern: String,
    /// Reclaimed bytes, or the estimate in dry-run mode.
    pub bytes_reclaimed: u64,
    /// Removal, dry-run eligibility, or the safety gate that retained it.
    pub action: String,
}

pub(super) fn collect(
    worktree: &Path,
    patterns: &[String],
    delete: bool,
) -> Result<Vec<WorktreeReclaimReport>, OrbitError> {
    let root = fs::canonicalize(worktree)?;
    let matches = matching_paths(worktree, patterns, false)?;
    let mut reports: Vec<WorktreeReclaimReport> = Vec::new();
    for (relative, pattern) in matches {
        // Successfully removed parents already account for their children.
        // A refused parent does not hide an independently safe child match.
        if reports.iter().any(|report| {
            relative.starts_with(&report.path)
                && matches!(report.action.as_str(), "removed" | "would_remove")
        }) {
            continue;
        }
        let path = worktree.join(&relative);
        let mut report = WorktreeReclaimReport {
            path: relative.clone(),
            pattern,
            bytes_reclaimed: 0,
            action: String::new(),
        };
        let outcome = (|| -> Result<(), OrbitError> {
            if !confined_path(worktree, &root, &relative)? {
                report.action = "skipped:symlink_or_outside_worktree".into();
            } else {
                let name = relative
                    .to_str()
                    .ok_or_else(|| OrbitError::InvalidInput("reclaim path is not UTF-8".into()))?;
                // Literal pathspecs and NUL records keep agent-chosen names from
                // becoming Git patterns or splitting a filename into records.
                let tracked = git_output_paths(
                    worktree,
                    &[
                        "--literal-pathspecs",
                        "ls-files",
                        "--cached",
                        "-z",
                        "--",
                        name,
                    ],
                )?;
                let others = git_output_paths(
                    worktree,
                    &[
                        "--literal-pathspecs",
                        "ls-files",
                        "--others",
                        "--directory",
                        "-z",
                        "--",
                        name,
                    ],
                )?;
                if !tracked.is_empty() {
                    report.action = "skipped:tracked_content".into();
                } else if others.is_empty() {
                    report.action = "skipped:not_ignored_or_untracked".into();
                } else {
                    report.bytes_reclaimed = path_bytes(&path)?;
                    // Recheck confinement immediately before deletion. Recursive
                    // removal unlinks symlinks within the matched tree; it never
                    // follows them to data outside the worktree.
                    if !confined_path(worktree, &root, &relative)? {
                        report.bytes_reclaimed = 0;
                        report.action = "skipped:symlink_or_outside_worktree".into();
                    } else if delete {
                        if fs::symlink_metadata(&path)?.is_dir() {
                            fs::remove_dir_all(&path)?;
                        } else {
                            fs::remove_file(&path)?;
                        }
                        report.action = "removed".into();
                        tracing::info!(path = %path.display(), pattern = %report.pattern,
                        bytes = report.bytes_reclaimed, "reclaimed declared worktree output");
                    } else {
                        report.action = "would_remove".into();
                    }
                }
            }
            Ok(())
        })();
        if let Err(error) = outcome {
            report.bytes_reclaimed = 0;
            report.action = format!("failed:{error}");
        }
        reports.push(report);
    }
    Ok(reports)
}

fn confined_path(worktree: &Path, root: &Path, relative: &Path) -> Result<bool, OrbitError> {
    let mut path = worktree.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Ok(false);
        };
        path.push(component);
        if fs::symlink_metadata(&path)?.file_type().is_symlink() {
            return Ok(false);
        }
    }
    let canonical = fs::canonicalize(path)?;
    Ok(canonical != root && canonical.starts_with(root))
}

fn path_bytes(path: &Path) -> Result<u64, OrbitError> {
    let mut bytes = 0u64;
    let mut pending = vec![path.to_path_buf()];
    while let Some(path) = pending.pop() {
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            for entry in fs::read_dir(path)? {
                pending.push(entry?.path());
            }
        } else {
            bytes = bytes.saturating_add(metadata.len());
        }
    }
    Ok(bytes)
}

/// Cheap inventory before a history sweep pays for Git registration queries.
pub(super) fn has_matches(worktree: &Path, patterns: &[String]) -> bool {
    fs::symlink_metadata(worktree).is_ok_and(|meta| meta.is_dir() && !meta.file_type().is_symlink())
        && matching_paths(worktree, patterns, true).is_ok_and(|matches| !matches.is_empty())
}

fn matching_paths(
    worktree: &Path,
    patterns: &[String],
    stop_on_first: bool,
) -> Result<BTreeMap<PathBuf, String>, OrbitError> {
    let globs = patterns
        .iter()
        .map(|pattern| RelativePathGlob::new(pattern))
        .collect::<Result<Vec<_>, _>>()?;
    let mut matches = BTreeMap::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        for entry in fs::read_dir(worktree.join(&relative))? {
            let entry = entry?;
            let path = relative.join(entry.file_name());
            // Git's administrative link is never rebuildable output, even
            // when a broad glob would otherwise match it.
            if entry.file_name().eq_ignore_ascii_case(".git") {
                continue;
            }
            let Some(name) = path.to_str() else {
                continue;
            };
            let name = name.replace(std::path::MAIN_SEPARATOR, "/");
            let metadata = fs::symlink_metadata(entry.path())?;
            if let Some(index) = globs.iter().position(|glob| glob.matches(&name)) {
                matches.insert(path.clone(), patterns[index].clone());
                if stop_on_first && !metadata.file_type().is_symlink() {
                    return Ok(matches);
                }
            }
            if metadata.is_dir()
                && !metadata.file_type().is_symlink()
                && globs.iter().any(|glob| glob.may_match_descendant(&name))
            {
                pending.push(path);
            }
        }
    }
    Ok(matches)
}
