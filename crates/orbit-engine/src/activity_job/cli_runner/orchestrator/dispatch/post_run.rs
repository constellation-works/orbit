use std::time::Duration;

use orbit_common::process::build_budget::read_waits;
use orbit_common::security::redaction::PatternRedactor;
use orbit_types::workflow::activity_job::V2AuditEventKind;

use crate::activity_job::cli_runner::image_input::StagedImages;
use crate::activity_job::cli_runner::inspection::SourceInspection;
use crate::activity_job::dispatcher::{DispatchOutcome, ResolvedSandbox};
use crate::activity_job::workspace::WorktreeBoundaryGuard;

use super::super::super::super::dispatcher::DispatchError;
use super::super::completion::{ProviderExit, project_completion};
use super::super::evidence::{
    emit_build_waits, record_scratch_removals, step_error_after_provider_evidence,
};
use super::{CliDispatchContext, subprocess};

pub(super) struct CliProviderCompletion<'a> {
    pub(super) context: CliDispatchContext<'a>,
    pub(super) provider: String,
    pub(super) model: Option<String>,
    pub(super) task_ids: &'a [String],
    pub(super) worktree_boundary: Option<WorktreeBoundaryGuard>,
    pub(super) sandbox: Option<&'a ResolvedSandbox>,
    pub(super) subprocess_cwd: Option<&'a std::path::Path>,
    pub(super) redaction: &'a PatternRedactor,
    pub(super) timeout_seconds: u64,
    pub(super) argv_redacted: Vec<String>,
    pub(super) stdin_blob_ref: Option<String>,
    pub(super) print_timeout: Option<Duration>,
    pub(super) stall_window: Option<Duration>,
    pub(super) inspection: Option<&'a SourceInspection>,
    pub(super) staged_images: Option<&'a StagedImages>,
    pub(super) scratch_dir: Option<&'a std::path::Path>,
    pub(super) build_wait_directory: &'a tempfile::TempDir,
    pub(super) process_exit: subprocess::ProviderProcessExit,
}

pub(super) fn finish_cli_dispatch(
    completion: CliProviderCompletion<'_>,
) -> Result<DispatchOutcome, DispatchError> {
    let CliProviderCompletion {
        context,
        provider,
        model,
        task_ids,
        worktree_boundary,
        sandbox,
        subprocess_cwd,
        redaction,
        timeout_seconds,
        argv_redacted,
        stdin_blob_ref,
        print_timeout,
        stall_window,
        inspection,
        staged_images,
        scratch_dir,
        build_wait_directory,
        process_exit,
    } = completion;
    let CliDispatchContext {
        host,
        spec,
        step_id,
        activity_name,
        input,
        run_id,
        audit,
        ..
    } = context;
    let subprocess::ProviderProcessExit {
        stdout,
        stderr,
        exit_code,
        duration,
        timed_out,
        stalled,
        usage_context,
        linux_post_run_guard,
    } = process_exit;
    // A post-run check fails the step, but the run trail must still hold the
    // provider's output. Refresh before those writes: a sandboxed provider may
    // have opened the granted SQLite/WAL files while this worker kept its
    // pre-spawn handles, and every clone of that store shares the connection.
    // The swap happens only after the replacement is open and writable, so a
    // failure before it leaves the previous connection and a failure after it
    // leaves the new one. Blobs and `CliInvocationFinished` are recorded on
    // whichever connection remains, then inspection, the Linux guard, and the
    // worktree boundary run. Integrity failures keep that classification.
    let refresh_error = host
        .refresh_persistence_after_cli_provider()
        .err()
        .map(|error| {
            DispatchError::CliInvocationPermanent(format!(
                "refresh durable store after provider `{provider}` exited: {error}"
            ))
        });

    let stdout_blob_ref = audit.write_blob(stdout.bytes());
    let stderr_blob_ref = audit.write_blob(stderr.bytes());
    let build_budget_waits = read_waits(
        build_wait_directory.path(),
        timeout_seconds.saturating_mul(1000),
    );
    emit_build_waits(audit, &provider, &build_budget_waits);
    audit.emit_lossy(V2AuditEventKind::CliInvocationFinished {
        provider: provider.clone(),
        exit_code,
        duration_ms: duration.as_millis() as u64,
        stdout_blob_ref: stdout_blob_ref.clone(),
        stderr_blob_ref: stderr_blob_ref.clone(),
        harness_version: None,
        timed_out,
    });

    let inspection_error = inspection.and_then(|snapshot| snapshot.verify().err());
    let guard_error =
        linux_post_run_guard
            .as_ref()
            .and_then(|guard| match guard.verify(scratch_dir) {
                Ok(removed) => {
                    record_scratch_removals(audit, &provider, guard.profile(), &removed);
                    None
                }
                Err(error) => Some(DispatchError::CliInvocationPermanent(error.to_string())),
            });
    let post_run_error = inspection_error.or(guard_error).or(refresh_error);

    let canary_provider = staged_images.is_some().then(|| provider.clone());
    let mut completion = project_completion(ProviderExit {
        host,
        spec,
        input,
        run_id,
        provider,
        model,
        task_ids,
        worktree_boundary,
        sandbox,
        subprocess_cwd,
        redaction,
        timeout_seconds,
        argv_redacted,
        // A blob that could not be stored has no ref to cite.
        stdin_blob_ref: stdin_blob_ref.unwrap_or_default(),
        stdout_blob_ref: stdout_blob_ref.clone().unwrap_or_default(),
        stderr_blob_ref: stderr_blob_ref.clone().unwrap_or_default(),
        stdout,
        stderr,
        exit_code,
        duration,
        timed_out,
        stalled,
        stall_window: stall_window.unwrap_or_default(),
        print_timeout,
        usage_context,
        build_budget_waits,
    });
    if let (Some(staged), Some(provider), Ok(outcome)) =
        (staged_images, canary_provider, completion.as_mut())
    {
        staged.verify(
            outcome,
            audit,
            step_id,
            activity_name,
            &provider,
            input.get("crew").and_then(serde_json::Value::as_str),
        );
    }
    step_error_after_provider_evidence(
        completion,
        post_run_error,
        stdout_blob_ref.as_deref().unwrap_or_default(),
        stderr_blob_ref.as_deref().unwrap_or_default(),
    )
}
