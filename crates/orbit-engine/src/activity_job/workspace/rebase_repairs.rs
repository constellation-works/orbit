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
//! Built-in text merges are recognizable by their markers; binary ancestors
//! can instead leave a side untouched. Custom drivers can leave arbitrary
//! output, so their earlier repairs require Git's original `AUTO_MERGE` tree.
//! A modify/delete conflict resolved by keeping one side unchanged looks
//! exactly like the untouched stop, so it is adopted only through the
//! leaf's authenticated resolution intent, checked here against the index
//! stages and the worktree [F2026-11-277].

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use super::DispatchError;
use super::boundary_guard::is_host_owned_path;
use super::fingerprint::{GitWorktreeFingerprint, git_output_raw, git_stdout, git_stdout_bytes};
use super::rebase_recovery::ResolutionIntent;

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

/// Verify each authenticated resolution intent against the stopped index
/// and the worktree, returning the paths it adopts as repaired. Every intent
/// must name an authorized, non-host-owned conflict path in `authorized`
/// that is a modify/delete conflict (exactly one side present). A kept side
/// must exist in the index stages as a regular file, and the worktree must
/// hold that exact blob and mode as a regular, marker-free file; a deletion
/// must leave nothing at the path. Any mismatch refuses the whole intent.
pub(super) fn adopt_resolution_intents(
    root: &Path,
    intents: &[ResolutionIntent],
    authorized: &[String],
) -> Result<Vec<String>, String> {
    let mut adopted = Vec::new();
    for intent in intents {
        let path = intent.path.as_str();
        let refused = |reason: &str| format!("the resolution intent for '{path}' {reason}");
        if !authorized.iter().any(|authorized| authorized == path) || is_host_owned_path(path) {
            return Err(refused("names a path outside the authorized conflict set"));
        }
        let stages = conflict_stages(root, path).map_err(|error| error.to_string())?;
        if stages.contains_key(&2) == stages.contains_key(&3) {
            return Err(refused("does not name a modify/delete conflict"));
        }
        let file = root.join(path);
        let metadata = match fs::symlink_metadata(&file) {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(refused(&format!("cannot inspect the path: {error}"))),
        };
        let Some(stage) = intent.resolution.kept_stage() else {
            if metadata.is_some() {
                return Err(refused("records a deletion, but the path still exists"));
            }
            adopted.push(intent.path.clone());
            continue;
        };
        let side = stages
            .get(&stage)
            .ok_or_else(|| refused("names a side the index stages do not hold"))?;
        if !matches!(side.mode.as_str(), "100644" | "100755") {
            return Err(refused("names a side that is not a regular file"));
        }
        let Some(metadata) = metadata.filter(fs::Metadata::is_file) else {
            return Err(refused(
                "keeps a side, but the path is missing, a symlink or a directory",
            ));
        };
        let content = fs::read(&file).map_err(|error| refused(&format!("cannot read: {error}")))?;
        if has_conflict_markers(&content) {
            return Err(refused("keeps a side, but the file holds conflict markers"));
        }
        let blob = git_stdout(root, &["hash-object", "--", path]).map_err(|e| e.to_string())?;
        if blob != side.oid {
            return Err(refused("keeps a side the worktree content differs from"));
        }
        if !mode_matches(root, &metadata, &side.mode).map_err(|error| error.to_string())? {
            return Err(refused(
                "keeps a side whose file mode the worktree differs from",
            ));
        }
        adopted.push(intent.path.clone());
    }
    Ok(adopted)
}

/// Whether the worktree file's executable bit matches the stage mode, where
/// Git tracks file modes at all.
fn mode_matches(root: &Path, metadata: &fs::Metadata, mode: &str) -> Result<bool, DispatchError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let tracked = git_stdout(
            root,
            &[
                "config",
                "--type=bool",
                "--default",
                "true",
                "core.fileMode",
            ],
        )?;
        if tracked == "true" {
            let executable = metadata.permissions().mode() & 0o111 != 0;
            return Ok(executable == (mode == "100755"));
        }
        Ok(true)
    }
    #[cfg(not(unix))]
    {
        let _ = (root, metadata, mode);
        Ok(true)
    }
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
/// side is missing, as in modify/delete, or any stage is binary or merges as
/// binary) it leaves one side's content instead, so content equal to a side
/// is still the untouched stop there. Custom drivers may leave arbitrary
/// content: only a difference from Git's recorded stopped output proves a
/// repair. A path Git would have written but that is now absent was deleted
/// as its resolution. Anything else (a symlink or
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
    let blob = git_stdout(root, &["hash-object", "--", path])?;
    if let Some(tree) = auto_merge_tree(root)? {
        let stopped = git_output_raw(root, &["rev-parse", "--verify", &format!("{tree}:{path}")])?;
        return Ok(stopped.success && String::from_utf8_lossy(&stopped.stdout).trim() != blob);
    }
    let driver = merge_driver(root, path)?;
    if driver == "text" && textual_merge(root, &stages)? {
        return Ok(true);
    }
    // Without the original output, custom drivers' arbitrary writes cannot
    // be distinguished from an earlier repair, even by comparing both sides.
    if !matches!(driver.as_str(), "text" | "binary") {
        return Ok(false);
    }
    Ok(!sides.iter().flatten().any(|side| side.oid == blob))
}

/// The stopped merge's original worktree tree, when the Git strategy records it.
pub(super) fn auto_merge_tree(root: &Path) -> Result<Option<String>, DispatchError> {
    let output = git_output_raw(root, &["rev-parse", "--verify", "AUTO_MERGE^{tree}"])?;
    Ok(output
        .success
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string()))
}

/// Respect both the path attribute and the driver selected for unspecified paths.
fn merge_driver(root: &Path, path: &str) -> Result<String, DispatchError> {
    let merge = git_stdout(root, &["check-attr", "merge", "--", path])?;
    match merge.rsplit(": ").next() {
        Some("set") => Ok("text".to_string()),
        Some("unset") => Ok("binary".to_string()),
        Some("unspecified") => git_stdout(
            root,
            &["config", "--default", "text", "--get", "merge.default"],
        ),
        Some(driver) => Ok(driver.to_string()),
        None => Ok(String::new()),
    }
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

/// Whether a built-in text merge writes markers: both sides must exist and
/// every present stage, including the ancestor, must be a regular text file.
fn textual_merge(root: &Path, stages: &BTreeMap<u8, StageEntry>) -> Result<bool, DispatchError> {
    if !stages.contains_key(&2) || !stages.contains_key(&3) {
        return Ok(false);
    }
    for stage in stages.values() {
        if !matches!(stage.mode.as_str(), "100644" | "100755") || blob_is_binary(root, &stage.oid)?
        {
            return Ok(false);
        }
    }
    Ok(true)
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
pub(super) fn has_conflict_markers(content: &[u8]) -> bool {
    let has_line = |marker: &[u8]| {
        content
            .split(|byte| *byte == b'\n')
            .any(|line| line.starts_with(marker))
    };
    has_line(b"<<<<<<<") && has_line(b">>>>>>>")
}
