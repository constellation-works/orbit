//! Durable step-recovery decisions [ORB-14152].
//!
//! A `step_failure_recovery` invocation decides whether the executor makes its
//! single post-recovery attempt by writing a small JSON file, not by what its
//! final response says. Before dispatch the host allocates the file's path
//! under the assigned worktree's run-local scratch directory
//! (`.orbit/tmp/step-recovery/`), which the recovery leaf's sandbox already
//! makes writable, and binds it to the run, failed step, failed attempt and a
//! fresh nonce. After the invocation completes the host reads the file back
//! without following links and accepts it only when every binding matches.
//!
//! The file is evidence the leaf authored, so its location confers no
//! authority of its own. What makes it this invocation's decision is the
//! nonce: it exists only in the engine's memory and the leaf's input, so
//! nothing written before the allocation, or for another invocation, can
//! carry it.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_engine::{
    STEP_RECOVERY_DECISION_SCHEMA_VERSION, StepRecoveryDecisionRead, StepRecoveryDecisionRequest,
    StepRecoveryDecisionSlot, StepRecoveryVerdict,
};
use serde::Deserialize;

/// Directories from the worktree root to the slot directory.
const SLOT_COMPONENTS: [&str; 3] = [".orbit", "tmp", "step-recovery"];
/// A decision is a handful of short fields; anything larger is not one.
const MAX_DECISION_BYTES: u64 = 16 * 1024;
/// Longest step-id or run-id fragment kept in the file name.
const MAX_NAME_FRAGMENT: usize = 48;

/// The file a recovery invocation writes. Unknown fields are refused so a
/// near-miss shape fails closed instead of being half-read.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionFile {
    schema_version: u32,
    run_id: String,
    failed_step_id: String,
    attempt: u32,
    nonce: String,
    decision: Verdict,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Verdict {
    Retry,
    NotRecovered,
}

/// Allocate a fresh decision path for one recovery invocation.
///
/// The slot directory is created one component at a time beneath the
/// canonical worktree root, and a component that exists as a link or a
/// non-directory is refused rather than followed, so the slot cannot be
/// redirected out of the worktree.
pub(crate) fn allocate(
    request: &StepRecoveryDecisionRequest,
) -> Result<StepRecoveryDecisionSlot, OrbitError> {
    let requested = Path::new(&request.workspace_path);
    if !requested.is_absolute() {
        return Err(OrbitError::InvalidInput(format!(
            "step-recovery decision needs an absolute worktree path, got '{}'",
            request.workspace_path
        )));
    }
    let workspace_root = fs::canonicalize(requested).map_err(|error| {
        OrbitError::Io(format!(
            "resolve step-recovery worktree '{}': {error}",
            requested.display()
        ))
    })?;
    if !workspace_root.is_dir() {
        return Err(OrbitError::InvalidInput(format!(
            "step-recovery worktree '{}' is not a directory",
            workspace_root.display()
        )));
    }
    let mut dir = workspace_root.clone();
    for component in SLOT_COMPONENTS {
        dir.push(component);
        ensure_real_dir(&dir)?;
    }
    let nonce = fresh_nonce()?;
    let path = dir.join(format!(
        "{}-{}-a{}-{nonce}.json",
        name_fragment(&request.run_id),
        name_fragment(&request.failed_step_id),
        request.attempt,
    ));
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Ok(_) => {
            return Err(OrbitError::Io(format!(
                "step-recovery decision path '{}' already exists",
                path.display()
            )));
        }
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "inspect step-recovery decision path '{}': {error}",
                path.display()
            )));
        }
    }
    Ok(StepRecoveryDecisionSlot {
        run_id: request.run_id.clone(),
        failed_step_id: request.failed_step_id.clone(),
        attempt: request.attempt,
        nonce,
        workspace_root,
        path,
    })
}

/// Read back and verify the decision written to `slot`.
///
/// `Ok(Absent)` only when the slot directory is intact and nothing is at the
/// path. A link anywhere on the path, a non-regular or oversized file, an
/// unparseable body, or a binding for another invocation is `Invalid`. An I/O
/// failure is an error, never a decision.
pub(crate) fn read(
    slot: &StepRecoveryDecisionSlot,
) -> Result<StepRecoveryDecisionRead, OrbitError> {
    let invalid = |diagnostic: String| Ok(StepRecoveryDecisionRead::Invalid { diagnostic });
    let expected_dir = slot_dir(&slot.workspace_root);
    if slot.path.parent() != Some(expected_dir.as_path()) {
        return invalid(format!(
            "decision path '{}' is not in the slot directory '{}'",
            slot.path.display(),
            expected_dir.display()
        ));
    }
    let mut dir = slot.workspace_root.clone();
    for component in SLOT_COMPONENTS {
        dir.push(component);
        match fs::symlink_metadata(&dir) {
            Ok(metadata) if metadata.file_type().is_dir() => {}
            Ok(_) => {
                return invalid(format!(
                    "'{}' was replaced by a link or non-directory",
                    dir.display()
                ));
            }
            Err(error) => {
                return Err(OrbitError::Io(format!(
                    "inspect step-recovery slot directory '{}': {error}",
                    dir.display()
                )));
            }
        }
    }
    let before = match fs::symlink_metadata(&slot.path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(StepRecoveryDecisionRead::Absent);
        }
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "inspect step-recovery decision '{}': {error}",
                slot.path.display()
            )));
        }
        Ok(metadata) => metadata,
    };
    if !before.file_type().is_file() {
        return invalid("the decision is a link or not a regular file".to_string());
    }
    let mut file = match orbit_common::fs::open_read_only_no_follow(&slot.path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => {
            return invalid(format!("the decision path is unsafe: {error}"));
        }
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "open step-recovery decision '{}': {error}",
                slot.path.display()
            )));
        }
    };
    let opened = file.metadata().map_err(|error| {
        OrbitError::Io(format!(
            "inspect opened step-recovery decision '{}': {error}",
            slot.path.display()
        ))
    })?;
    if !opened.is_file() || !same_file(&before, &opened) {
        return invalid("the decision was replaced while it was being opened".to_string());
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_DECISION_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            OrbitError::Io(format!(
                "read step-recovery decision '{}': {error}",
                slot.path.display()
            ))
        })?;
    if bytes.len() as u64 > MAX_DECISION_BYTES {
        return invalid(format!("the decision exceeds {MAX_DECISION_BYTES} bytes"));
    }
    if bytes.ends_with(b"\\n") {
        return invalid("the decision ends with a literal backslash-n suffix".to_string());
    }
    let decision: DecisionFile = match serde_json::from_slice(&bytes) {
        Ok(decision) => decision,
        Err(error) => return invalid(format!("the decision is malformed: {error}")),
    };
    if decision.schema_version != STEP_RECOVERY_DECISION_SCHEMA_VERSION {
        return invalid(format!(
            "unsupported decision schema_version {}",
            decision.schema_version
        ));
    }
    for (field, matches) in [
        ("run_id", decision.run_id == slot.run_id),
        (
            "failed_step_id",
            decision.failed_step_id == slot.failed_step_id,
        ),
        ("attempt", decision.attempt == slot.attempt),
        ("nonce", decision.nonce == slot.nonce),
    ] {
        if !matches {
            return invalid(format!(
                "the decision's {field} does not name this recovery invocation"
            ));
        }
    }
    Ok(StepRecoveryDecisionRead::Verified {
        verdict: match decision.decision {
            Verdict::Retry => StepRecoveryVerdict::Retry,
            Verdict::NotRecovered => StepRecoveryVerdict::NotRecovered,
        },
        reason: decision.reason.filter(|reason| !reason.trim().is_empty()),
    })
}

/// Create `dir` if absent; refuse it if it exists as anything but a real
/// directory.
fn ensure_real_dir(dir: &Path) -> Result<(), OrbitError> {
    let refuse = || {
        Err(OrbitError::InvalidInput(format!(
            "step-recovery slot directory '{}' is a link or not a directory",
            dir.display()
        )))
    };
    match fs::symlink_metadata(dir) {
        Ok(metadata) if metadata.file_type().is_dir() => Ok(()),
        Ok(_) => refuse(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => match fs::create_dir(dir) {
            Ok(()) => Ok(()),
            // A concurrent creator wins the race; recheck what it made.
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                match fs::symlink_metadata(dir) {
                    Ok(metadata) if metadata.file_type().is_dir() => Ok(()),
                    _ => refuse(),
                }
            }
            Err(error) => Err(OrbitError::Io(format!(
                "create step-recovery slot directory '{}': {error}",
                dir.display()
            ))),
        },
        Err(error) => Err(OrbitError::Io(format!(
            "inspect step-recovery slot directory '{}': {error}",
            dir.display()
        ))),
    }
}

fn fresh_nonce() -> Result<String, OrbitError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| {
        OrbitError::Execution(format!("draw step-recovery decision nonce: {error}"))
    })?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
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
fn same_file(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
fn same_file(_left: &fs::Metadata, _right: &fs::Metadata) -> bool {
    true
}

/// The slot directory beneath a worktree root.
fn slot_dir(workspace_root: &Path) -> PathBuf {
    SLOT_COMPONENTS
        .iter()
        .fold(workspace_root.to_path_buf(), |dir, component| {
            dir.join(component)
        })
}
