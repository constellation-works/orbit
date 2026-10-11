//! Audit and provider-output evidence for dispatch outcomes.
use super::super::super::audit_writer::V2AuditWriter;
use super::super::super::dispatcher::{DispatchError, DispatchOutcome};
use orbit_common::process::build_budget::BuildBudgetWaits;
use orbit_exec::LinuxBwrapScratchRemoval;
use orbit_types::workflow::activity_job::V2AuditEventKind;
use serde_json::Value;

/// A denyModify match in run scratch is removed rather than failing the run;
/// each removal stays visible as a warning and a denied modify in the audit.
pub(super) fn record_scratch_removals(
    audit: &V2AuditWriter,
    provider: &str,
    profile: &str,
    removed: &[LinuxBwrapScratchRemoval],
) {
    for removal in removed {
        tracing::warn!(
            provider,
            path = %removal.path.display(),
            rule = %removal.rule,
            "removed a path forbidden by denyModify that the provider created in run scratch"
        );
        audit.emit_lossy(V2AuditEventKind::FsCallDenied {
            profile: profile.to_string(),
            op: "modify".to_string(),
            path: removal.path.display().to_string(),
            allowed: false,
            matched_rule: format!("!{}", removal.rule),
        });
    }
}

pub(super) fn emit_build_waits(audit: &V2AuditWriter, provider: &str, waits: &BuildBudgetWaits) {
    audit.emit_lossy(V2AuditEventKind::CliInvocationBuildBudget {
        provider: provider.to_string(),
        count: waits.count,
        total_ms: waits.total_ms,
        longest_ms: waits.longest_ms,
        queued_wall_ms: waits.queued_wall_ms,
        deadline_extension_ms: waits.deadline_extension_ms,
    });
}

/// Keep a worktree-integrity failure as the step error, and cite the stored
/// provider output on every failure that follows a post-run check.
pub(super) fn step_error_after_provider_evidence(
    completion: Result<DispatchOutcome, DispatchError>,
    post_run_error: Option<DispatchError>,
    stdout_blob_ref: &str,
    stderr_blob_ref: &str,
) -> Result<DispatchOutcome, DispatchError> {
    let Some(post_run_error) = post_run_error else {
        return completion;
    };
    let cited_post_run = cite_output_evidence(post_run_error, stdout_blob_ref, stderr_blob_ref);
    let error = match completion {
        Err(DispatchError::WorktreeIntegrity { code, diagnostic }) => {
            DispatchError::WorktreeIntegrity {
                code,
                diagnostic: cite_integrity_diagnostic(
                    &diagnostic,
                    stdout_blob_ref,
                    stderr_blob_ref,
                ),
            }
        }
        Err(error) => cite_output_evidence(error, stdout_blob_ref, stderr_blob_ref),
        Ok(_) => cited_post_run,
    };
    Err(error)
}

/// Attach blob refs when `error` carries a message this layer can extend.
/// Other variants fall back to a permanent error that still names the blobs,
/// so a post-run failure cannot return without those references.
fn cite_output_evidence(
    error: DispatchError,
    stdout_blob_ref: &str,
    stderr_blob_ref: &str,
) -> DispatchError {
    let cite = output_blob_cite(stdout_blob_ref, stderr_blob_ref);
    match error {
        DispatchError::CliInvocationFailed(message) => {
            DispatchError::CliInvocationFailed(format!("{message} ({cite})"))
        }
        DispatchError::CliInvocationPermanent(message) => {
            DispatchError::CliInvocationPermanent(format!("{message} ({cite})"))
        }
        DispatchError::JobExecution(message) => {
            DispatchError::JobExecution(format!("{message} ({cite})"))
        }
        DispatchError::GitTimeout {
            operation,
            root,
            timeout_ms,
            diagnostic,
        } => DispatchError::GitTimeout {
            operation,
            root,
            timeout_ms,
            diagnostic: format!("{diagnostic} ({cite})"),
        },
        DispatchError::WorktreeIntegrity { code, diagnostic } => DispatchError::WorktreeIntegrity {
            code,
            diagnostic: cite_integrity_diagnostic(&diagnostic, stdout_blob_ref, stderr_blob_ref),
        },
        other => DispatchError::CliInvocationPermanent(format!("{other} ({cite})")),
    }
}

fn cite_integrity_diagnostic(
    diagnostic: &str,
    stdout_blob_ref: &str,
    stderr_blob_ref: &str,
) -> String {
    let Ok(mut value) = serde_json::from_str::<Value>(diagnostic) else {
        return format!(
            "{diagnostic} ({})",
            output_blob_cite(stdout_blob_ref, stderr_blob_ref)
        );
    };
    let Some(object) = value.as_object_mut() else {
        return format!(
            "{diagnostic} ({})",
            output_blob_cite(stdout_blob_ref, stderr_blob_ref)
        );
    };
    object.insert(
        "stdout_blob_ref".to_string(),
        Value::String(stdout_blob_ref.to_string()),
    );
    object.insert(
        "stderr_blob_ref".to_string(),
        Value::String(stderr_blob_ref.to_string()),
    );
    value.to_string()
}

fn output_blob_cite(stdout_blob_ref: &str, stderr_blob_ref: &str) -> String {
    format!("stdout_blob_ref={stdout_blob_ref}, stderr_blob_ref={stderr_blob_ref}")
}
