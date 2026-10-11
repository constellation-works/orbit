//! Which file repairs a host continuation of a stopped rebase adopts.
//!
//! A conflict recovery invocation may find its conflict files already
//! repaired: final recovery repairs conflict files without Git writes and
//! resumes, and the next `pr_conflict_recovery` then has nothing left to
//! change. Those earlier repairs are adopted when the files no longer hold
//! what Git left at the stop, so a no-op recovery continues the rebase
//! instead of demanding the same edits again [F2026-10-237]. Unstaged
//! tracked repairs made before the invocation join the continued commit as
//! companions; untracked payloads that predate it and host-owned `.orbit/`
//! state never do.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use super::DispatchError;
use super::boundary_guard::is_host_owned_path;
use super::fingerprint::{GitWorktreeFingerprint, git_output_raw, git_stdout, git_stdout_bytes};

/// Git's buffer-is-binary heuristic: a NUL within the first 8000 bytes.
const BINARY_SNIFF_BYTES: usize = 8000;

/// The authorized conflict paths that hold no repair: neither changed by
/// this invocation (`changed`) nor repaired before it.
pub(super) fn unrepaired_conflict_paths(
    root: &Path,
    conflicting_paths: &[String],
    changed: &[String],
) -> Result<Vec<String>, DispatchError> {
    let mut unrepaired = Vec::new();
    for path in conflicting_paths {
        if !changed.contains(path) && !repaired_before_invocation(root, path)? {
            unrepaired.push(path.clone());
        }
    }
    Ok(unrepaired)
}

/// Tracked paths outside the conflict set that already carried an unstaged
/// repair (an edit or a deletion) when the invocation started. Staged
/// changes are already in the index the continuation commits; untracked
/// files that predate the invocation and host-owned state stay out.
pub(super) fn earlier_companion_paths(
    before: &GitWorktreeFingerprint,
    conflicting_paths: &[String],
) -> BTreeSet<String> {
    before
        .path_states
        .iter()
        .filter(|(path, _)| !conflicting_paths.contains(path) && !is_host_owned_path(path))
        .filter(|(_, state)| {
            state.index_entry_sha256.is_some()
                && state.untracked_content_sha256.is_none()
                && (state.worktree_patch_sha256.is_some() || !state.worktree_present)
        })
        .map(|(path, _)| path.clone())
        .collect()
}

/// Whether an unmerged `path` no longer holds what Git left at the stop.
///
/// Git leaves conflict markers where it merged text. Where it could not (a
/// side is missing, as in modify/delete, or a side is binary or merges as
/// binary) it leaves one side's content instead, so content equal to a side
/// is still the untouched stop there. A path Git would have written but that
/// is now absent was deleted as its resolution. Anything else (a symlink or
/// directory in the way) is not adopted; the invocation must repair it.
fn repaired_before_invocation(root: &Path, path: &str) -> Result<bool, DispatchError> {
    let stages = conflict_stages(root, path)?;
    let sides = [stages.get(&2), stages.get(&3)];
    let file = root.join(path);
    let Ok(metadata) = fs::symlink_metadata(&file) else {
        return Ok(sides.iter().any(Option::is_some));
    };
    if !metadata.is_file() {
        return Ok(false);
    }
    let content = fs::read(&file).map_err(|error| {
        DispatchError::CliInvocationPermanent(format!(
            "inspect the conflict file '{path}': {error}"
        ))
    })?;
    if has_conflict_markers(&content) {
        return Ok(false);
    }
    if textual_merge(root, path, &sides)? {
        return Ok(true);
    }
    let blob = git_stdout(root, &["hash-object", "--", path])?;
    Ok(!sides.iter().flatten().any(|side| side.oid == blob))
}

struct StageEntry {
    mode: String,
    oid: String,
}

fn conflict_stages(root: &Path, path: &str) -> Result<BTreeMap<u8, StageEntry>, DispatchError> {
    let listing = git_stdout_bytes(root, &["ls-files", "--unmerged", "-z", "--", path])?;
    let mut stages = BTreeMap::new();
    for record in listing.split(|byte| *byte == 0) {
        let record = String::from_utf8_lossy(record);
        let Some((meta, entry_path)) = record.split_once('\t') else {
            continue;
        };
        if entry_path != path {
            continue;
        }
        let mut fields = meta.split_whitespace();
        let (Some(mode), Some(oid), Some(stage)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if let Ok(stage) = stage.parse::<u8>() {
            stages.insert(
                stage,
                StageEntry {
                    mode: mode.to_string(),
                    oid: oid.to_string(),
                },
            );
        }
    }
    Ok(stages)
}

/// Whether Git merged this path's sides as text, writing conflict markers:
/// both sides are regular files with text content and the path's `merge`
/// attribute does not make it binary.
fn textual_merge(
    root: &Path,
    path: &str,
    sides: &[Option<&StageEntry>; 2],
) -> Result<bool, DispatchError> {
    let [Some(ours), Some(theirs)] = sides else {
        return Ok(false);
    };
    for side in [ours, theirs] {
        if !matches!(side.mode.as_str(), "100644" | "100755") || blob_is_binary(root, &side.oid)? {
            return Ok(false);
        }
    }
    let merge = git_stdout(root, &["check-attr", "merge", "--", path])?;
    Ok(!matches!(
        merge.rsplit(": ").next(),
        Some("unset" | "binary")
    ))
}

fn blob_is_binary(root: &Path, oid: &str) -> Result<bool, DispatchError> {
    let output = git_output_raw(root, &["cat-file", "blob", oid])?;
    if !output.success {
        return Err(DispatchError::CliInvocationPermanent(format!(
            "read the conflict side blob {oid}"
        )));
    }
    Ok(output
        .stdout
        .iter()
        .take(BINARY_SNIFF_BYTES)
        .any(|byte| *byte == 0))
}

/// Whether `content` still holds a conflict hunk: an opening `<<<<<<<` and a
/// closing `>>>>>>>` marker line. A lone `=======` is a Markdown underline as
/// often as a marker; `git diff --check` judges what is left.
fn has_conflict_markers(content: &[u8]) -> bool {
    let has_line = |marker: &[u8]| {
        content
            .split(|byte| *byte == b'\n')
            .any(|line| line.starts_with(marker))
    };
    has_line(b"<<<<<<<") && has_line(b">>>>>>>")
}
