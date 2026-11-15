//! Authenticated resolution intents for a stopped rebase [F2026-11-277].
//!
//! A modify/delete conflict can be resolved correctly by keeping one side
//! unchanged, and that resolution leaves the worktree exactly as Git left
//! the stop: content alone cannot tell it from a conflict nobody touched.
//! The `pr_conflict_recovery` leaf therefore states such a resolution
//! explicitly, per path, in a file the host allocates for that one
//! invocation, beneath the assigned worktree's scratch directory
//! (`.orbit/tmp/rebase-resolution/`). Before dispatch the engine binds the
//! file to the run, failed step, recovery attempt, pinned target base, task
//! set and a fresh nonce, and names it in the leaf's input. The boundary
//! guard admits that slot with the stopped rebase, reads the file back
//! without following links after the provider exits, and accepts it only
//! when every binding matches. What the file asks for is then checked
//! against the index stages and the worktree before any path is adopted.
//!
//! The location confers no authority: the nonce exists only in the engine's
//! memory and the leaf's input, so nothing written before the allocation, or
//! for another invocation, run, base or task, can carry it. Without a
//! verified intent, content equal to a side is still the untouched stop.

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use super::super::DispatchError;

/// The intent file's schema version, named in the leaf's input.
pub(crate) const RESOLUTION_INTENT_SCHEMA_VERSION: u32 = 1;
/// Directories from the worktree root to the slot directory.
const SLOT_COMPONENTS: [&str; 3] = [".orbit", "tmp", "rebase-resolution"];
/// An intent names a handful of paths; anything larger is not one.
const MAX_INTENT_BYTES: u64 = 64 * 1024;
/// Longest step-id or run-id fragment kept in the file name.
const MAX_NAME_FRAGMENT: usize = 48;
/// The nonce is 16 random bytes, hex-encoded.
const NONCE_BYTES: usize = 16;

/// How the leaf resolved one modify/delete conflict path. During a rebase
/// the target base is index stage 2 and the candidate pick is stage 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::activity_job::workspace) enum ConflictResolution {
    /// Keep the target base side's blob (stage 2) unchanged.
    KeepTargetBase,
    /// Keep the candidate side's blob (stage 3) unchanged.
    KeepCandidate,
    /// Resolve the conflict by deleting the path.
    Delete,
}

impl ConflictResolution {
    /// The index stage whose blob the worktree must hold, if any.
    pub(in crate::activity_job::workspace) fn kept_stage(self) -> Option<u8> {
        match self {
            Self::KeepTargetBase => Some(2),
            Self::KeepCandidate => Some(3),
            Self::Delete => None,
        }
    }
}

/// One verified per-path resolution.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::activity_job::workspace) struct ResolutionIntent {
    pub(in crate::activity_job::workspace) path: String,
    pub(in crate::activity_job::workspace) resolution: ConflictResolution,
}

/// The file a recovery leaf writes. Unknown fields are refused so a
/// near-miss shape fails closed instead of being half-read.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IntentFile {
    schema_version: u32,
    run_id: String,
    failed_step_id: String,
    attempt: u64,
    nonce: String,
    target_base_sha: String,
    task_ids: Vec<String>,
    resolutions: Vec<ResolutionIntent>,
}

/// Allocate this invocation's intent slot and return the binding to name in
/// the leaf's input as `resolution_intent`. `None` when the input names no
/// existing absolute worktree to hold it; the host then adopts no intents,
/// and the boundary guard still judges the worktree itself.
pub(crate) fn allocate_resolution_intent(
    input: &Value,
    run_id: &str,
    failed_step_id: &str,
    attempt: u32,
) -> Result<Option<Value>, DispatchError> {
    let Some(workspace) = input
        .get("workspace_path")
        .and_then(Value::as_str)
        .map(Path::new)
        .filter(|path| path.is_absolute())
    else {
        return Ok(None);
    };
    let Some(target_base_sha) = input.get("target_base_sha").and_then(Value::as_str) else {
        return Ok(None);
    };
    let Some(root) = fs::canonicalize(workspace)
        .ok()
        .filter(|root| root.is_dir())
    else {
        return Ok(None);
    };
    let dir = orbit_common::fs::io::create_private_dir_chain(
        &root,
        &SLOT_COMPONENTS.iter().collect::<PathBuf>(),
    )
    .map_err(|error| intent_error(&error.to_string()))?;
    let nonce = orbit_common::security::token::random_hex(NONCE_BYTES)
        .map_err(|error| intent_error(&error.to_string()))?;
    let path = dir.join(slot_file_name(
        run_id,
        failed_step_id,
        attempt.into(),
        &nonce,
    ));
    if fs::symlink_metadata(&path).is_ok() {
        return Err(intent_error(&format!(
            "the slot '{}' already exists",
            path.display()
        )));
    }
    Ok(Some(serde_json::json!({
        "path": path.display().to_string(),
        "schema_version": RESOLUTION_INTENT_SCHEMA_VERSION,
        "run_id": run_id,
        "failed_step_id": failed_step_id,
        "attempt": attempt,
        "nonce": nonce,
        "target_base_sha": target_base_sha,
        "task_ids": recovery_task_ids(input),
    })))
}

/// The admitted slot, held only in the boundary guard's memory.
pub(in crate::activity_job::workspace) struct ResolutionIntentSlot {
    root: PathBuf,
    path: PathBuf,
    run_id: String,
    failed_step_id: String,
    attempt: u64,
    nonce: String,
    target_base_sha: String,
    task_ids: Vec<String>,
}

impl ResolutionIntentSlot {
    /// Admit the slot the engine named in `input`, when it names one. It
    /// must describe this run, step and pinned base, sit at its exact name
    /// in the real slot directory of `root`, and be empty before the
    /// provider launches.
    pub(in crate::activity_job::workspace) fn admit(
        input: &Value,
        root: &Path,
        run_id: &str,
        failed_step_id: &str,
        target_base_sha: &str,
    ) -> Result<Option<Self>, String> {
        let Some(binding) = input.get("resolution_intent") else {
            return Ok(None);
        };
        let text = |field: &str| {
            binding
                .get(field)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| format!("the resolution intent slot is missing '{field}'"))
        };
        if binding.get("schema_version").and_then(Value::as_u64)
            != Some(RESOLUTION_INTENT_SCHEMA_VERSION.into())
        {
            return Err("the resolution intent slot has an unsupported schema_version".into());
        }
        let attempt = binding
            .get("attempt")
            .and_then(Value::as_u64)
            .ok_or("the resolution intent slot is missing 'attempt'")?;
        let nonce = text("nonce")?;
        if nonce.len() != NONCE_BYTES * 2
            || !nonce
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("the resolution intent slot nonce is malformed".into());
        }
        let slot = Self {
            root: root.to_path_buf(),
            path: PathBuf::from(text("path")?),
            run_id: text("run_id")?,
            failed_step_id: text("failed_step_id")?,
            attempt,
            nonce,
            target_base_sha: text("target_base_sha")?,
            task_ids: binding
                .get("task_ids")
                .and_then(Value::as_array)
                .ok_or("the resolution intent slot is missing 'task_ids'")?
                .iter()
                .map(|id| id.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()
                .ok_or("the resolution intent slot task_ids are malformed")?,
        };
        if slot.run_id != run_id
            || slot.failed_step_id != failed_step_id
            || slot.target_base_sha != target_base_sha
        {
            return Err(
                "the resolution intent slot does not name this run, step and pinned base".into(),
            );
        }
        let expected = slot.real_slot_dir()?.join(slot_file_name(
            &slot.run_id,
            &slot.failed_step_id,
            slot.attempt,
            &slot.nonce,
        ));
        if slot.path != expected {
            return Err("the resolution intent slot is not at its allocated path".into());
        }
        match fs::symlink_metadata(&slot.path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Some(slot)),
            Ok(_) => Err("the resolution intent slot was written before the provider ran".into()),
            Err(error) => Err(format!("inspect the resolution intent slot: {error}")),
        }
    }

    /// Read back and verify the intents the leaf left. Nothing at the path
    /// is no intent. A link anywhere on the path, a non-regular or oversized
    /// file, an unparseable body, a binding for another invocation, or a
    /// path named twice is refused.
    pub(in crate::activity_job::workspace) fn read(&self) -> Result<Vec<ResolutionIntent>, String> {
        self.real_slot_dir()?;
        let before = match fs::symlink_metadata(&self.path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(format!("inspect the resolution intent: {error}")),
            Ok(metadata) => metadata,
        };
        if !before.file_type().is_file() {
            return Err("the resolution intent is a link or not a regular file".into());
        }
        let mut file = orbit_common::fs::open_read_only_no_follow(&self.path)
            .map_err(|error| format!("open the resolution intent: {error}"))?;
        let opened = file
            .metadata()
            .map_err(|error| format!("inspect the opened resolution intent: {error}"))?;
        if !opened.is_file() || !same_file(&before, &opened) {
            return Err("the resolution intent was replaced while it was being opened".into());
        }
        let mut bytes = Vec::new();
        (&mut file)
            .take(MAX_INTENT_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("read the resolution intent: {error}"))?;
        if bytes.len() as u64 > MAX_INTENT_BYTES {
            return Err(format!(
                "the resolution intent exceeds {MAX_INTENT_BYTES} bytes"
            ));
        }
        let intent: IntentFile = serde_json::from_slice(&bytes)
            .map_err(|error| format!("the resolution intent is malformed: {error}"))?;
        for (field, matches) in [
            (
                "schema_version",
                intent.schema_version == RESOLUTION_INTENT_SCHEMA_VERSION,
            ),
            ("run_id", intent.run_id == self.run_id),
            (
                "failed_step_id",
                intent.failed_step_id == self.failed_step_id,
            ),
            ("attempt", intent.attempt == self.attempt),
            ("nonce", intent.nonce == self.nonce),
            (
                "target_base_sha",
                intent.target_base_sha == self.target_base_sha,
            ),
            ("task_ids", intent.task_ids == self.task_ids),
        ] {
            if !matches {
                return Err(format!(
                    "the resolution intent's {field} does not name this recovery invocation"
                ));
            }
        }
        let mut seen = BTreeSet::new();
        for resolution in &intent.resolutions {
            if !seen.insert(resolution.path.as_str()) {
                return Err(format!(
                    "the resolution intent names '{}' more than once",
                    resolution.path
                ));
            }
        }
        Ok(intent.resolutions)
    }

    /// The slot directory, refused when a component is a link or not a
    /// directory, so the slot cannot be redirected out of the worktree.
    fn real_slot_dir(&self) -> Result<PathBuf, String> {
        let mut dir = self.root.clone();
        for component in SLOT_COMPONENTS {
            dir.push(component);
            match fs::symlink_metadata(&dir) {
                Ok(metadata) if metadata.file_type().is_dir() => {}
                Ok(_) => {
                    return Err(format!(
                        "'{}' was replaced by a link or non-directory",
                        dir.display()
                    ));
                }
                Err(error) => {
                    return Err(format!(
                        "inspect the resolution intent directory '{}': {error}",
                        dir.display()
                    ));
                }
            }
        }
        Ok(dir)
    }
}

/// The task set the recovery input names, in its order.
fn recovery_task_ids(input: &Value) -> Vec<String> {
    match input.get("task_ids").and_then(Value::as_array) {
        Some(ids) => ids
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        None => input
            .get("task_id")
            .and_then(Value::as_str)
            .map(|id| vec![id.to_string()])
            .unwrap_or_default(),
    }
}

fn slot_file_name(run_id: &str, failed_step_id: &str, attempt: u64, nonce: &str) -> String {
    format!(
        "{}-{}-a{attempt}-{nonce}.json",
        name_fragment(run_id),
        name_fragment(failed_step_id),
    )
}

/// A readable, filesystem-safe fragment of an identifier for the file name.
/// The binding check uses the full identifiers, never this fragment.
fn name_fragment(value: &str) -> String {
    let fragment: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .take(MAX_NAME_FRAGMENT)
        .collect();
    if fragment.is_empty() {
        "_".to_string()
    } else {
        fragment
    }
}

#[cfg(unix)]
fn same_file(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    (before.dev(), before.ino()) == (after.dev(), after.ino())
}

#[cfg(not(unix))]
fn same_file(_before: &fs::Metadata, _after: &fs::Metadata) -> bool {
    true
}

fn intent_error(detail: &str) -> DispatchError {
    DispatchError::CliInvocationPermanent(format!(
        "allocate the conflict recovery resolution intent: {detail}"
    ))
}
