//! The implementer's deferred-sandbox handoff, as the engine enforces it
//! [ORB-15287]: Bubblewrap deferrals on Linux, Seatbelt ones on macOS
//! [ORB-15409].
//!
//! [`accept_implementer_deferral`] runs at the implement step boundary. The
//! implementer names only the validation summary its gate wrote; Orbit derives
//! the [`DeferredSandboxValidation`] from it, checks it against the gates this
//! host names ([`RuntimeHost::validation_gates`]: on a claimed leaf, the
//! owner's gates its claim captured [ORB-15622]) and the step's pinned base,
//! and runs the deferring sandbox's probe in the implementer's lane itself
//! [ORB-15516]. The derived record
//! replaces the handoff in the step's output, and an owner run keeps it on the
//! task as [`DEFERRED_SANDBOX_ARTIFACT`]: a resumed candidate skips the
//! implementer, so the obligation must outlive the run that created it. A
//! later implementation that deferred nothing clears it. A claimed leaf
//! always runs its implementer, so its validation reads this attempt's output
//! instead.
//!
//! [`deferred_replays`] and [`claimed_deferred_replays`] name the commands the
//! validation steps replay outside any agent sandbox after the required ones,
//! each re-checked against this host's gates.

use std::io::Read;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_types::workflow::{
    DEFERRED_SANDBOX_ARTIFACT, DeferredSandboxArtifact, DeferredSandboxValidation,
    HostValidationGates, deferral_handoff,
};
use serde_json::Value;

use crate::context::RuntimeHost;

/// The checkout directory a handed-off gate summary must be in: the
/// implementer's scratch.
const SUMMARY_DIR: &str = ".orbit/tmp";
/// Larger than any summary a gate writes; a bigger file is not one.
const MAX_SUMMARY_BYTES: u64 = 1024 * 1024;

/// The lane the implementer ran in, where Orbit runs the deferral's probe.
pub(crate) struct ImplementerLane<'a> {
    /// The provider that ran the implementer; `None` for a deterministic one.
    pub(crate) provider: Option<&'a str>,
    /// The step's filesystem profile.
    pub(crate) fs_profile: Option<&'a str>,
}

/// `commands`, then each of `replays` it does not already hold.
pub(super) fn with_replays(mut commands: Vec<String>, replays: &[String]) -> Vec<String> {
    for replay in replays {
        if !commands
            .iter()
            .any(|command| command.trim() == replay.trim())
        {
            commands.push(replay.trim().to_string());
        }
    }
    commands
}

/// Derive a successful implementer step's deferred sandbox validation from
/// the gate summary it names and, on an owner run, keep it on the task.
/// `Ok(Some)` is the derived record, which replaces the handoff in the step's
/// output; `Err` is the step's failure message.
pub(crate) fn accept_implementer_deferral<H: RuntimeHost + ?Sized>(
    host: &H,
    run_id: &str,
    input: &Value,
    output: &Value,
    lane: &ImplementerLane<'_>,
) -> Result<Option<DeferredSandboxValidation>, String> {
    let record = derive(host, input, output, lane).map_err(|reason| {
        format!(
            "implementer output rejected before delivery: `deferred_sandbox_validation` \
             {reason}. Hand off only `{{\"summary\": \"<path>\"}}` for a passing \
             affected-test gate whose sole gap is one sandbox's notices: Bubblewrap \
             `DEFERRED:` notices on Linux, or Seatbelt `SKIP: sandbox-exec cannot apply a \
             profile:` notices on macOS, in a lane that refuses that sandbox; otherwise fix \
             the gate or report the validation blocker."
        )
    })?;
    if input.get("claimed").and_then(Value::as_bool) == Some(true) {
        return Ok(record);
    }
    let Some(task_id) = input.get("task_id").and_then(Value::as_str) else {
        return match record {
            Some(_) => Err(
                "implementer output rejected before delivery: a deferred sandbox validation \
                 needs the step's `task_id` to keep it for the native replay"
                    .to_string(),
            ),
            None => Ok(None),
        };
    };
    keep(host, run_id, task_id, record.clone()).map_err(|error| {
        format!(
            "could not keep {task_id}'s deferred sandbox validation for the native replay: \
             {error}"
        )
    })?;
    Ok(record)
}

/// The record the handoff's gate summary and Orbit's own probe derive, or
/// `None` when the implementer deferred nothing.
fn derive<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    output: &Value,
    lane: &ImplementerLane<'_>,
) -> Result<Option<DeferredSandboxValidation>, String> {
    let Some(handoff) = deferral_handoff(output)? else {
        return Ok(None);
    };
    let checkout = input
        .get("workspace_path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| {
            "cannot be derived: the step names no `workspace_path` to read the gate summary \
             from"
                .to_string()
        })?;
    let summary = read_summary(&checkout, &handoff.summary).map_err(|reason| {
        format!(
            "names the gate summary `{}`, which {reason}",
            handoff.summary
        )
    })?;
    let pending = handoff
        .derive(
            &summary,
            &host.validation_gates(),
            input.get("base_sha").and_then(Value::as_str),
        )
        .map_err(|reason| {
            format!(
                "names the gate summary `{}`, which {reason}",
                handoff.summary
            )
        })?;
    let probe =
        host.run_deferral_probe(&pending.probe(), lane.provider, lane.fs_profile, &checkout);
    pending.complete(probe).map(Some)
}

/// The summary at checkout-relative `path`, which must resolve inside the
/// checkout's [`SUMMARY_DIR`].
fn read_summary(checkout: &Path, path: &str) -> Result<Vec<u8>, String> {
    let scratch = checkout
        .join(SUMMARY_DIR)
        .canonicalize()
        .map_err(|error| format!("cannot be read: the checkout has no `{SUMMARY_DIR}`: {error}"))?;
    let resolved = checkout
        .join(path)
        .canonicalize()
        .map_err(|error| format!("cannot be read: {error}"))?;
    if !resolved.starts_with(&scratch) || !resolved.is_file() {
        return Err(format!("is not a file in the checkout's `{SUMMARY_DIR}`"));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(&resolved)
        .and_then(|file| file.take(MAX_SUMMARY_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|error| format!("cannot be read: {error}"))?;
    if bytes.len() as u64 > MAX_SUMMARY_BYTES {
        return Err(format!("is larger than {MAX_SUMMARY_BYTES} bytes"));
    }
    Ok(bytes)
}

/// Write the task's [`DEFERRED_SANDBOX_ARTIFACT`], or clear one an earlier
/// implementation left when this one deferred nothing.
fn keep<H: RuntimeHost + ?Sized>(
    host: &H,
    run_id: &str,
    task_id: &str,
    deferred: Option<DeferredSandboxValidation>,
) -> Result<(), OrbitError> {
    if deferred.is_none() && kept(host, task_id)?.is_none() {
        return Ok(());
    }
    let content = serde_json::to_vec(&DeferredSandboxArtifact {
        schema_version: 1,
        run_id: run_id.to_string(),
        deferred,
    })
    .map_err(|error| {
        OrbitError::Execution(format!("encode {DEFERRED_SANDBOX_ARTIFACT}: {error}"))
    })?;
    host.attach_task_validation_log(task_id, run_id, DEFERRED_SANDBOX_ARTIFACT, content)
}

/// The task's kept record artifact, if it has one.
fn kept<H: RuntimeHost + ?Sized>(
    host: &H,
    task_id: &str,
) -> Result<Option<DeferredSandboxArtifact>, OrbitError> {
    let Some(artifact) = host
        .get_task_artifacts(task_id)?
        .into_iter()
        .find(|artifact| artifact.path == DEFERRED_SANDBOX_ARTIFACT)
    else {
        return Ok(None);
    };
    serde_json::from_slice::<DeferredSandboxArtifact>(&artifact.content)
        .ok()
        .filter(|kept| kept.schema_version == 1)
        .map(Some)
        .ok_or_else(|| {
            OrbitError::PolicyDenied(format!(
                "{task_id}'s {DEFERRED_SANDBOX_ARTIFACT} is not a schema 1 record, so its \
                 deferred sandbox coverage cannot be replayed"
            ))
        })
}

/// The commands an owner's validation replays for `task_ids`: each task's
/// kept deferral.
pub(super) fn deferred_replays<H: RuntimeHost + ?Sized>(
    host: &H,
    task_ids: &[String],
) -> Result<Vec<String>, OrbitError> {
    let gates = host.validation_gates();
    let mut replays = Vec::new();
    for task_id in task_ids {
        if let Some(record) = kept(host, task_id)?.and_then(|kept| kept.deferred) {
            replays = with_replays(replays, &[replayable(&record, &gates)?]);
        }
    }
    Ok(replays)
}

/// The command a claimed leaf's validation replays: this attempt's
/// implementer output, handed in as `implementation`.
pub(super) fn claimed_deferred_replays<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Vec<String>, OrbitError> {
    let Some(value) = input
        .get("implementation")
        .and_then(|output| output.get("deferred_sandbox_validation"))
        .filter(|value| !value.is_null())
    else {
        return Ok(Vec::new());
    };
    let record: DeferredSandboxValidation =
        serde_json::from_value(value.clone()).map_err(|error| {
            OrbitError::PolicyDenied(format!(
                "the implementer's deferred sandbox validation cannot be replayed: {error}"
            ))
        })?;
    Ok(vec![replayable(&record, &host.validation_gates())?])
}

/// The record's command, while it is still one of this host's gates.
fn replayable(
    record: &DeferredSandboxValidation,
    gates: &HostValidationGates,
) -> Result<String, OrbitError> {
    if record.names_gate(&gates.gates.deferrable()) {
        return Ok(record.command.trim().to_string());
    }
    Err(OrbitError::PolicyDenied(format!(
        "deferred sandbox validation names `{}`, which is not one of this host's validation \
         gates, so its deferred coverage cannot be replayed; the gates checked are {}",
        record.command.trim(),
        gates.describe_deferrable()
    )))
}
