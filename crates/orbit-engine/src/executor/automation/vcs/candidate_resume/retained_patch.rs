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
    git_output, git_output_bytes, git_output_bytes_with_env, git_output_with_env, git_run_bytes,
    git_success, git_timeout_error, timeout_recovery_error,
};

const CONFLICT_SIDES_DIR: &str = ".orbit/tmp/retained-patch-conflicts";

const RECOVERY: &str = "The source run's checkout and recorded patch are untouched, so inspect the \
     source repair again and submit it once the cause is fixed; this successor checkout is \
     disposable.";

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
    // Snapshot the staged pre-apply tree so conflict-marker checks read the
    // same attributes `git apply --3way` used at merge time, before the patch
    // updates `.gitattributes`.
    git_success(process_env, workspace, &["add", "--all"])?;
    let pre_apply_tree = git_output(process_env, workspace, &["write-tree"])?;
    let applied = match git_run_bytes(
        process_env,
        workspace,
        &["apply", "--3way", "--whitespace=nowarn", "-"],
        Some(bytes),
    ) {
        Ok(applied) => applied,
        Err(error) => {
            let state = release_index(process_env, workspace);
            return Ok(RetainedApplied::Refused(format!(
                "retained final-recovery edits could not be applied: {error}. {state} {RECOVERY}"
            )));
        }
    };
    if applied.timed_out {
        let state = release_index(process_env, workspace);
        return Err(timeout_recovery_error(
            applied.timeout_ms,
            format!(
                "retained edits: git apply timed out after {}ms. {state} {RECOVERY}",
                applied.timeout_ms
            ),
        ));
    }
    if applied.success {
        return Ok(match reset_index(process_env, workspace) {
            Ok(()) => RetainedApplied::Clean,
            Err(error) => RetainedApplied::Refused(format!(
                "retained final-recovery edits applied, but unstaging them failed: {error}. \
                 The successor index still holds the candidate and the edits staged; \
                 `git reset` in it clears them without touching the working files. {RECOVERY}"
            )),
        });
    }

    let unmerged = match read_unmerged_stages(process_env, workspace) {
        Ok(unmerged) => unmerged,
        Err(error) => {
            return Ok(RetainedApplied::Refused(format!(
                "retained final-recovery edits do not apply cleanly ({}) and the conflicting \
                 index entries could not be read: {error}. The successor index was left as Git \
                 wrote it, with the candidate staged and any unmerged entries intact, so no \
                 conflict side is lost. {RECOVERY}",
                applied.stderr.trim()
            )));
        }
    };
    if unmerged.is_empty() {
        return Ok(match reset_index(process_env, workspace) {
            Ok(()) => RetainedApplied::Refused(format!(
                "retained final-recovery edits do not apply to the candidate: {}",
                applied.stderr.trim()
            )),
            Err(error) => RetainedApplied::Refused(format!(
                "retained final-recovery edits do not apply to the candidate: {}. Unstaging the \
                 candidate also failed: {error}. The successor index still holds the candidate \
                 staged; `git reset` in it clears that. {RECOVERY}",
                applied.stderr.trim()
            )),
        });
    }

    // Materialize stage blobs for binary/unmarked conflicts before resetting the
    // index; otherwise `reset` destroys stage 3 and leaves only the integration
    // working file. Reset runs only after the files are durably written, and a
    // failure before that leaves the unmerged entries as the remaining evidence.
    let paths_list = list_paths(unmerged.keys());
    let mut written = Vec::new();
    let preserved_sides = match preserve_unmarked_conflict_sides(
        process_env,
        workspace,
        &bound.sha256,
        &pre_apply_tree,
        &unmerged,
        &mut written,
    ) {
        Ok(sides) => sides,
        Err(error) => {
            return Ok(RetainedApplied::Refused(format!(
                "retained final-recovery edits conflict in {paths_list}, but a conflict side \
                 could not be saved: {error}. The successor index was not reset: it keeps the \
                 unmerged stage entries for {paths_list}, the only copy of any side not \
                 saved, and the working files hold Git's merge result. Saved so far: {}. {RECOVERY}",
                list_paths(written.iter())
            )));
        }
    };
    if let Err(error) = reset_index(process_env, workspace) {
        return Ok(RetainedApplied::Refused(format!(
            "retained final-recovery edits conflict in {paths_list}; the unmarked sides are saved \
             ({}), but unstaging failed: {error}. The successor index still holds the unmerged \
             entries for {paths_list}; every side is now durable, so `git reset` in it is safe. \
             {RECOVERY}",
            list_paths(written.iter())
        )));
    }

    let paths = unmerged.into_keys().collect();
    Ok(RetainedApplied::Conflict {
        paths,
        output: applied.stderr,
        preserved_sides,
    })
}

fn reset_index(process_env: &orbit_common::Env, workspace: &Path) -> Result<(), OrbitError> {
    git_success(process_env, workspace, &["reset", "--quiet"])
}

fn list_paths<'a>(paths: impl Iterator<Item = &'a String>) -> String {
    let joined = paths
        .map(|path| format!("`{path}`"))
        .collect::<Vec<_>>()
        .join(", ");
    if joined.is_empty() {
        "none".into()
    } else {
        joined
    }
}

/// Unstage the candidate after an apply that never reported a result, but only
/// when no unmerged entry is left to lose. Returns the index state to report.
fn release_index(process_env: &orbit_common::Env, workspace: &Path) -> String {
    match read_unmerged_stages(process_env, workspace) {
        Ok(unmerged) if unmerged.is_empty() => match reset_index(process_env, workspace) {
            Ok(()) => "The staged candidate was unstaged; the successor index matches HEAD.".into(),
            Err(error) => format!(
                "Unstaging the candidate failed ({error}); the successor index still holds it \
                 staged and `git reset` in it clears that."
            ),
        },
        Ok(unmerged) => format!(
            "The successor index keeps unmerged entries for {}: no side was saved, so they are \
             the only copy and were not reset.",
            list_paths(unmerged.keys())
        ),
        Err(error) => format!(
            "The successor index was left as Git wrote it (its unmerged entries could not be \
             read: {error})."
        ),
    }
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

/// Save the sides of every unmarked conflict, recording each durable file in
/// `written` so a failure can still name what survives.
fn preserve_unmarked_conflict_sides(
    process_env: &orbit_common::Env,
    workspace: &Path,
    patch_sha256: &str,
    pre_apply_tree: &str,
    unmerged: &BTreeMap<String, UnmergedStages>,
    written: &mut Vec<String>,
) -> Result<Vec<RetainedConflictSide>, OrbitError> {
    let short_digest = &patch_sha256[..patch_sha256.len().min(12)];
    let conflict_root = format!("{CONFLICT_SIDES_DIR}/{short_digest}");
    let mut preserved = Vec::new();
    for (path, stages) in unmerged {
        let side = preserve_path(
            process_env,
            workspace,
            &conflict_root,
            pre_apply_tree,
            path,
            stages,
            written,
        )
        .map_err(|error| OrbitError::Execution(format!("`{path}`: {error}")))?;
        preserved.extend(side);
    }
    Ok(preserved)
}

fn preserve_path(
    process_env: &orbit_common::Env,
    workspace: &Path,
    conflict_root: &str,
    pre_apply_tree: &str,
    path: &str,
    [stage1, stage2, stage3]: &UnmergedStages,
    written: &mut Vec<String>,
) -> Result<Option<RetainedConflictSide>, OrbitError> {
    let Some(retained_oid) = stage3.as_deref() else {
        return Ok(None);
    };
    validate_safe_relative_path(path)?;
    let blob = |oid: &str| git_output_bytes(process_env, workspace, &["cat-file", "blob", oid]);
    let current_bytes = stage2.as_deref().map(blob).transpose()?;
    let working_bytes = read_optional_file(workspace, path)?;
    let working_matches_current = current_bytes
        .as_ref()
        .is_some_and(|ours| working_bytes.as_ref() == Some(ours));
    let lacks_markers = match working_bytes.as_deref() {
        Some(bytes) if !working_matches_current => {
            let configured_width =
                configured_conflict_marker_size(process_env, workspace, pre_apply_tree, path)?;
            !has_conflict_markers(bytes, configured_width)
        }
        _ => true,
    };
    if !working_matches_current && !lacks_markers {
        return Ok(None);
    }

    let mut save = |side: &str, bytes: &[u8]| -> Result<String, OrbitError> {
        let relative = write_conflict_side(workspace, conflict_root, side, path, bytes)?;
        written.push(relative.clone());
        Ok(relative)
    };
    let retained_path = save("retained", &blob(retained_oid)?)?;
    let current_path = current_bytes
        .as_deref()
        .map(|bytes| save("current", bytes))
        .transpose()?;
    let base_path = stage1
        .as_deref()
        .map(|oid| save("base", &blob(oid)?))
        .transpose()?;
    Ok(Some(RetainedConflictSide {
        path: path.to_owned(),
        retained_path,
        current_path,
        base_path,
    }))
}

/// Read `conflict-marker-size` from `pre_apply_tree` (the merge-time attributes
/// before `git apply --3way` writes patch edits) using Git's signed 32-bit
/// `int` range so out-of-range values fall back to the default marker width.
fn configured_conflict_marker_size(
    process_env: &orbit_common::Env,
    workspace: &Path,
    pre_apply_tree: &str,
    path: &str,
) -> Result<Option<usize>, OrbitError> {
    let args = [
        "check-attr",
        "--source",
        pre_apply_tree,
        "-z",
        "conflict-marker-size",
        "--",
        path,
    ];
    let outcome = git_run_bytes(process_env, workspace, &args, None)?;
    if outcome.timed_out {
        return Err(git_timeout_error(
            workspace,
            &args,
            outcome.timeout_ms,
            &outcome.stderr,
        ));
    }
    if outcome.success {
        return Ok(parse_conflict_marker_size(&outcome.stdout));
    }
    // Git < 2.40 lacks `check-attr --source`; fall back to a scratch index so
    // pre-apply attributes still win over any `.gitattributes` applied later.
    let raw = configured_conflict_marker_size_from_scratch_index(
        process_env,
        workspace,
        pre_apply_tree,
        path,
    )?;
    Ok(parse_conflict_marker_size(&raw))
}

fn configured_conflict_marker_size_from_scratch_index(
    process_env: &orbit_common::Env,
    workspace: &Path,
    pre_apply_tree: &str,
    path: &str,
) -> Result<Vec<u8>, OrbitError> {
    let raw_git_path = git_output(
        process_env,
        workspace,
        &[
            "rev-parse",
            "--git-path",
            &format!("orbit-retained-attr-index-{}", std::process::id()),
        ],
    )?;
    let scratch_path = if Path::new(&raw_git_path).is_absolute() {
        std::path::PathBuf::from(raw_git_path)
    } else {
        workspace.join(raw_git_path)
    };
    let index_env = scratch_path.to_string_lossy().into_owned();
    let env = [("GIT_INDEX_FILE", index_env.as_str())];
    let result = (|| {
        git_output_with_env(process_env, workspace, &["read-tree", pre_apply_tree], &env)?;
        git_output_bytes_with_env(
            process_env,
            workspace,
            &[
                "check-attr",
                "--cached",
                "-z",
                "conflict-marker-size",
                "--",
                path,
            ],
            &env,
        )
    })();
    if let Err(error) = std::fs::remove_file(&scratch_path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(index = %scratch_path.display(), %error, "scratch attr index left behind");
    }
    result
}

fn parse_conflict_marker_size(raw: &[u8]) -> Option<usize> {
    let mut fields = raw.split(|byte| *byte == 0);
    let _attr_path = fields.next()?;
    let _attr_name = fields.next()?;
    let value = std::str::from_utf8(fields.next()?).ok()?;
    value
        .parse::<i32>()
        .ok()
        .filter(|width| *width > 0)
        .and_then(|width| usize::try_from(width).ok())
}

/// Read `relative_path` beneath `workspace` only when every traversed component
/// is a real directory and the leaf is a regular file. Symlinks, directories
/// and missing paths cannot hold inline merge markers and return `Ok(None)`
/// without following symlinks.
fn read_optional_file(
    workspace: &Path,
    relative_path: &str,
) -> Result<Option<Vec<u8>>, OrbitError> {
    let mut current = workspace.to_path_buf();
    let mut leaf_is_file = false;
    for component in Path::new(relative_path).components() {
        current.push(component);
        let metadata = match std::fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(None);
            }
            Err(error) => {
                return Err(OrbitError::Io(format!(
                    "read conflicting file '{}': {error}",
                    current.display()
                )));
            }
        };
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            return Ok(None);
        }
        leaf_is_file = file_type.is_file();
    }
    if !leaf_is_file {
        return Ok(None);
    }
    match std::fs::read(&current) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(OrbitError::Io(format!(
            "read conflicting file '{}': {error}",
            current.display()
        ))),
    }
}

fn has_conflict_markers(bytes: &[u8], configured_width: Option<usize>) -> bool {
    // Track per-width marker progress (`false` = saw opening `<`, `true` = also
    // saw matching `=` separator) so custom marker sizes match consistently
    // without tripping on literal 7-char underlines inside wider conflict hunks.
    let mut active_widths: BTreeMap<usize, bool> = BTreeMap::new();
    for raw_line in bytes.split(|byte| *byte == b'\n') {
        let line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
        if let Some(width) =
            marker_run(line, b'<', true).filter(|width| width_matches(*width, configured_width))
        {
            active_widths.entry(width).or_insert(false);
        } else if let Some(width) =
            marker_run(line, b'=', false).filter(|width| width_matches(*width, configured_width))
        {
            if let Some(saw_separator) = active_widths.get_mut(&width) {
                *saw_separator = true;
            }
        } else if let Some(width) =
            marker_run(line, b'>', true).filter(|width| width_matches(*width, configured_width))
            && active_widths.get(&width).copied() == Some(true)
        {
            return true;
        }
    }
    false
}

fn marker_run(line: &[u8], marker: u8, allow_label: bool) -> Option<usize> {
    let width = line.iter().take_while(|&&byte| byte == marker).count();
    if width == 0 {
        return None;
    }
    let rest = &line[width..];
    if rest.is_empty() || (allow_label && rest.starts_with(b" ")) {
        Some(width)
    } else {
        None
    }
}

fn width_matches(width: usize, configured_width: Option<usize>) -> bool {
    match configured_width {
        Some(expected) => width == expected,
        None => width >= 7,
    }
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
