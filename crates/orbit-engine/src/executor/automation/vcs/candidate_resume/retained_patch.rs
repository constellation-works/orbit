//! Apply the retained final-recovery edits an operator's source-repair
//! admission binds over the adopted candidate.
//!
//! The host rereads the bytes from the source's own checkout and checks them
//! against the bound digest; this side checks the digest again before Git
//! sees the bytes. The patch is applied with a three-way fallback onto the
//! squash-merged candidate, so the edits reach the successor as uncommitted
//! changes and its own commit step delivers them under the usual gates.

use std::collections::BTreeMap;
use std::path::{Component, Path};

use orbit_common::OrbitError;
use orbit_common::security::release::sha256_hex;
use orbit_types::workflow::SourceRepairRetainedPatch;

use crate::executor::automation::vcs::git::{
    git_output_bytes, git_run_bytes, git_success, timeout_recovery_error,
};

const CONFLICT_SIDES_DIR: &str = ".orbit/tmp/retained-patch-conflicts";

/// Saved index stage bytes for a conflicting path whose working file carries
/// no inline merge markers (for example a binary conflict).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RetainedConflictSide {
    pub(super) path: String,
    /// Workspace-relative path under `.orbit/tmp/` holding stage 3 (retained edit).
    pub(super) retained_path: String,
    /// Workspace-relative path under `.orbit/tmp/` holding stage 2 (current/integration).
    pub(super) current_path: Option<String>,
    /// Workspace-relative path under `.orbit/tmp/` holding stage 1 (common ancestor).
    pub(super) base_path: Option<String>,
}

/// What applying the bound edits left in the checkout.
pub(super) enum RetainedApplied {
    /// The edits applied without conflict.
    Clean,
    /// The edits left conflicts in `paths`, preserving any unmarked/binary
    /// sides in `preserved_sides` before clearing the unmerged index.
    Conflict {
        paths: Vec<String>,
        output: String,
        preserved_sides: Vec<RetainedConflictSide>,
    },
    /// Nothing usable was applied, and why.
    Refused(String),
}

/// Stage 1/2/3 object IDs recorded by `git ls-files --unmerged -z` per path.
type UnmergedStages = [Option<String>; 3];

/// Apply `bytes`, which must be exactly the `bound` patch, in `workspace`.
pub(super) fn apply_retained_patch(
    process_env: &orbit_common::Env,
    workspace: &Path,
    bound: &SourceRepairRetainedPatch,
    bytes: &[u8],
) -> Result<RetainedApplied, OrbitError> {
    if sha256_hex(bytes) != bound.sha256 || bytes.len() as u64 != bound.bytes {
        return Ok(RetainedApplied::Refused(
            "retained final-recovery edits do not match the bound digest".into(),
        ));
    }
    // The candidate is applied as unstaged edits, but `--3way` needs the index
    // to agree with the working copy on every path it touches. Stage the
    // candidate for the apply and unstage everything after, so the successor
    // still sees one plain dirty checkout without unmerged index entries.
    git_success(process_env, workspace, &["add", "--all"])?;
    let applied = git_run_bytes(
        process_env,
        workspace,
        &["apply", "--3way", "--whitespace=nowarn", "-"],
        Some(bytes),
    )?;
    if applied.timed_out {
        return Err(timeout_recovery_error(
            applied.timeout_ms,
            format!(
                "retained edits: git apply timed out after {}ms",
                applied.timeout_ms
            ),
        ));
    }
    if applied.success {
        git_success(process_env, workspace, &["reset", "--quiet"])?;
        return Ok(RetainedApplied::Clean);
    }

    let unmerged = read_unmerged_stages(process_env, workspace)?;
    if unmerged.is_empty() {
        git_success(process_env, workspace, &["reset", "--quiet"])?;
        return Ok(RetainedApplied::Refused(format!(
            "retained final-recovery edits do not apply to the candidate: {}",
            applied.stderr.trim()
        )));
    }

    // Materialize stage blobs for binary/unmarked conflicts before resetting the
    // index; otherwise `reset` destroys stage 3 and leaves only the integration
    // working file. Reset runs only after the files are durably written.
    let preserved_sides =
        preserve_unmarked_conflict_sides(process_env, workspace, &bound.sha256, &unmerged)?;
    git_success(process_env, workspace, &["reset", "--quiet"])?;

    let paths = unmerged.into_keys().collect();
    Ok(RetainedApplied::Conflict {
        paths,
        output: applied.stderr,
        preserved_sides,
    })
}

fn read_unmerged_stages(
    process_env: &orbit_common::Env,
    workspace: &Path,
) -> Result<BTreeMap<String, UnmergedStages>, OrbitError> {
    let raw = git_output_bytes(process_env, workspace, &["ls-files", "--unmerged", "-z"])?;
    let mut entries: BTreeMap<String, UnmergedStages> = BTreeMap::new();
    for record in raw.split(|byte| *byte == 0).filter(|rec| !rec.is_empty()) {
        let Some(tab) = record.iter().position(|byte| *byte == b'\t') else {
            return Err(OrbitError::Execution(
                "retained edits: malformed unmerged index entry".into(),
            ));
        };
        let meta = std::str::from_utf8(&record[..tab]).map_err(|_| {
            OrbitError::Execution("retained edits: non-UTF-8 unmerged index metadata".into())
        })?;
        let path = std::str::from_utf8(&record[tab + 1..]).map_err(|_| {
            OrbitError::Execution("retained edits: conflicting path is not valid UTF-8".into())
        })?;
        let mut fields = meta.split_whitespace();
        let (Some(_mode), Some(oid), Some(stage_str), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(OrbitError::Execution(format!(
                "retained edits: malformed unmerged metadata for '{path}'"
            )));
        };
        let stage = stage_str
            .parse::<usize>()
            .ok()
            .filter(|stage_number| (1..=3).contains(stage_number))
            .ok_or_else(|| {
                OrbitError::Execution(format!(
                    "retained edits: unexpected index stage '{stage_str}' for '{path}'"
                ))
            })?;
        entries.entry(path.to_owned()).or_default()[stage - 1] = Some(oid.to_owned());
    }
    Ok(entries)
}

fn preserve_unmarked_conflict_sides(
    process_env: &orbit_common::Env,
    workspace: &Path,
    patch_sha256: &str,
    unmerged: &BTreeMap<String, UnmergedStages>,
) -> Result<Vec<RetainedConflictSide>, OrbitError> {
    let short_digest = &patch_sha256[..patch_sha256.len().min(12)];
    let conflict_root = format!("{CONFLICT_SIDES_DIR}/{short_digest}");
    let mut preserved = Vec::new();

    for (path, [stage1, stage2, stage3]) in unmerged {
        let Some(retained_oid) = stage3.as_deref() else {
            continue;
        };
        let current_bytes = stage2
            .as_deref()
            .map(|oid| git_output_bytes(process_env, workspace, &["cat-file", "blob", oid]))
            .transpose()?;
        let working_bytes = read_optional_file(&workspace.join(path))?;
        let working_matches_current = current_bytes
            .as_ref()
            .is_some_and(|ours| working_bytes.as_ref() == Some(ours));
        let lacks_markers = working_bytes
            .as_deref()
            .is_none_or(|bytes| !has_conflict_markers(bytes));
        if !working_matches_current && !lacks_markers {
            continue;
        }
        validate_safe_relative_path(path)?;

        let retained_bytes =
            git_output_bytes(process_env, workspace, &["cat-file", "blob", retained_oid])?;
        let retained_path =
            write_conflict_side(workspace, &conflict_root, "retained", path, &retained_bytes)?;
        let current_path = current_bytes
            .as_deref()
            .map(|bytes| write_conflict_side(workspace, &conflict_root, "current", path, bytes))
            .transpose()?;
        let base_path = match stage1.as_deref() {
            Some(base_oid) => {
                let bytes =
                    git_output_bytes(process_env, workspace, &["cat-file", "blob", base_oid])?;
                Some(write_conflict_side(
                    workspace,
                    &conflict_root,
                    "base",
                    path,
                    &bytes,
                )?)
            }
            None => None,
        };
        preserved.push(RetainedConflictSide {
            path: path.clone(),
            retained_path,
            current_path,
            base_path,
        });
    }
    Ok(preserved)
}

fn read_optional_file(path: &Path) -> Result<Option<Vec<u8>>, OrbitError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(OrbitError::Io(format!(
            "read conflicting file '{}': {error}",
            path.display()
        ))),
    }
}

fn has_conflict_markers(bytes: &[u8]) -> bool {
    let mut saw_ours = false;
    let mut saw_sep = false;
    for raw_line in bytes.split(|byte| *byte == b'\n') {
        let line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
        if !saw_ours {
            if line.starts_with(b"<<<<<<<") {
                saw_ours = true;
            }
        } else if !saw_sep {
            if line == b"=======" {
                saw_sep = true;
            }
        } else if line.starts_with(b">>>>>>>") {
            return true;
        }
    }
    false
}

fn validate_safe_relative_path(path: &str) -> Result<(), OrbitError> {
    let mut components = Path::new(path).components();
    let Some(first) = components.next() else {
        return Err(OrbitError::Execution(
            "retained edits: empty conflicting path".into(),
        ));
    };
    for component in std::iter::once(first).chain(components) {
        match component {
            Component::Normal(part) if !part.eq_ignore_ascii_case(".git") => {}
            _ => {
                return Err(OrbitError::Execution(format!(
                    "retained edits: unsafe conflicting path '{path}'"
                )));
            }
        }
    }
    Ok(())
}

fn write_conflict_side(
    workspace: &Path,
    conflict_root: &str,
    side: &str,
    path: &str,
    bytes: &[u8],
) -> Result<String, OrbitError> {
    let relative = format!("{conflict_root}/{side}/{path}");
    let relative_path = Path::new(&relative);
    let parent = relative_path.parent().ok_or_else(|| {
        OrbitError::Execution(format!(
            "retained edits: missing conflict-side directory for '{relative}'"
        ))
    })?;
    let file_name = relative_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            OrbitError::Execution(format!(
                "retained edits: invalid conflict-side file name for '{relative}'"
            ))
        })?;
    orbit_common::fs::io::write_private_bytes_beneath(workspace, parent, file_name, bytes)
        .map_err(|error| {
            OrbitError::Io(format!("write conflict-side file '{relative}': {error}"))
        })?;
    Ok(relative)
}
