//! The implementer's deferred-sandbox handoff, as the engine enforces it
//! [ORB-15287].
//!
//! [`accept_implementer_deferral`] runs at the implement step boundary. It
//! checks an implementer's [`DeferredSandboxValidation`] against the gates this
//! host names and the step's pinned base, and an owner run keeps the accepted
//! record on the task as [`DEFERRED_SANDBOX_ARTIFACT`]: a resumed candidate
//! skips the implementer, so the obligation must outlive the run that created
//! it. A later implementation that deferred nothing clears it. A claimed leaf
//! always runs its implementer, so its validation reads this attempt's output
//! instead.
//!
//! [`deferred_replays`] and [`claimed_deferred_replays`] name the commands the
//! validation steps replay outside any agent sandbox after the required ones,
//! each re-checked against this host's gates.

use orbit_common::OrbitError;
use orbit_types::workflow::{
    DEFERRED_SANDBOX_ARTIFACT, DeferredSandboxArtifact, DeferredSandboxValidation,
    deferred_sandbox_validation,
};
use serde_json::Value;

use crate::context::RuntimeHost;

/// The gates whose deferrals an implementer may hand off: the required
/// validation commands, then the review baseline commands.
fn deferral_gates<H: RuntimeHost + ?Sized>(host: &H) -> Vec<String> {
    with_replays(
        host.required_validation_commands(),
        &host.review_baseline_commands(),
    )
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

/// Check a successful implementer step's `deferred_sandbox_validation` and,
/// on an owner run, keep it on the task. `Err` is the step's failure message.
pub(crate) fn accept_implementer_deferral<H: RuntimeHost + ?Sized>(
    host: &H,
    run_id: &str,
    input: &Value,
    output: &Value,
) -> Result<(), String> {
    let record = deferred_sandbox_validation(
        output,
        &deferral_gates(host),
        input.get("base_sha").and_then(Value::as_str),
    )
    .map_err(|reason| {
        format!(
            "implementer output rejected before delivery: `deferred_sandbox_validation` \
             {reason}. Hand off only a passing affected-test gate whose sole gap is \
             Bubblewrap `DEFERRED:` notices backed by a failing namespace probe; otherwise \
             fix the gate or report the validation blocker."
        )
    })?;
    if input.get("claimed").and_then(Value::as_bool) == Some(true) {
        return Ok(());
    }
    let Some(task_id) = input.get("task_id").and_then(Value::as_str) else {
        return match record {
            Some(_) => Err(
                "implementer output rejected before delivery: a deferred sandbox validation \
                 needs the step's `task_id` to keep it for the native replay"
                    .to_string(),
            ),
            None => Ok(()),
        };
    };
    keep(host, run_id, task_id, record).map_err(|error| {
        format!(
            "could not keep {task_id}'s deferred sandbox validation for the native replay: \
             {error}"
        )
    })
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
    let gates = deferral_gates(host);
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
    Ok(vec![replayable(&record, &deferral_gates(host))?])
}

/// The record's command, while it is still one of this host's gates.
fn replayable(record: &DeferredSandboxValidation, gates: &[String]) -> Result<String, OrbitError> {
    if record.names_gate(gates) {
        return Ok(record.command.trim().to_string());
    }
    Err(OrbitError::PolicyDenied(format!(
        "deferred sandbox validation names `{}`, which is not one of this host's validation \
         gates, so its deferred coverage cannot be replayed",
        record.command.trim()
    )))
}
