//! Completion contracts, invocation telemetry and output projection.

use super::super::super::envelope::{
    parse_cli_invocation_trace_from, parse_cli_response_result_from,
};
use super::super::super::response_diagnostics::{
    bounded_diagnostic, completion_diagnostic, response_diagnostic,
};
use super::super::super::spawn_diagnostics::linux_bwrap_failed_write_diagnostic;
use super::super::super::stdout_preview::{
    BoundedMessage, FINAL_MESSAGE_LIMIT_BYTES, STDOUT_TEXT_PREVIEW_LIMIT_BYTES, StdoutTextPreview,
    bounded_assistant_message, bounded_redacted_text, stdout_text_preview,
};
use super::super::limit::{record_limit, record_usage_windows, structured_provider_limit};
use super::ProviderExit;
use super::frames::{
    provider_reported, provider_reported_failure, structured_provider_auth_error,
    structured_provider_refusal,
};
use super::{failure, frames};
use crate::activity_job::dispatcher::{DispatchError, DispatchInvocationTrace, DispatchOutcome};
use orbit_agent::{
    ParsedStdout, antigravity_background_task_diagnostic, antigravity_terminal_error_diagnostic,
    assign_recorded_tool_calls, is_missing_envelope_violation, is_success_carrying_error_violation,
    latest_assistant_message, muse_terminal_failure_diagnostic, normalize_cli_stdout,
    project_cli_response, provider_authentication_failure, provider_capacity_exhausted,
    provider_content_refusal, provider_invocation_diagnostic, provider_long_prompt_requests,
    provider_usage_limit,
};
use orbit_common::security::redaction::redact_all_json;
use serde_json::Value;
use std::time::Duration;

/// Inclusive provider-budget boundary, independent of envelope policy flags.
pub(in super::super) fn print_budget_exhausted(
    exit_success: bool,
    envelope_missing: bool,
    budget: Option<Duration>,
    elapsed: Duration,
) -> bool {
    exit_success && envelope_missing && budget.is_some_and(|budget| elapsed >= budget)
}

/// Did the provider say why an exit 0 ended without an envelope, in text no
/// failure frame carries [ORB-22427]? A corrected reply cannot change any of
/// these, so they keep their own failure.
fn provider_reported_ending(
    provider: &str,
    stdout: &[u8],
    stderr: &str,
    duration: Duration,
) -> bool {
    let terminal_error = antigravity_terminal_error_diagnostic(provider, stdout);
    terminal_error.is_some()
        || antigravity_background_task_diagnostic(provider, stdout, duration, str::to_string)
            .is_some()
        || [
            provider_authentication_failure as fn(&str) -> bool,
            provider_usage_limit,
            provider_capacity_exhausted,
            provider_content_refusal,
        ]
        .into_iter()
        .any(|matches| provider_reported(provider, stdout, stderr, None, matches).is_some())
}

/// Is `message` a provider's own frame rather than anything the agent wrote?
/// The assistant-message reader falls back to the whole document when a wrapper
/// holds no answer [ORB-22427].
fn is_provider_frame(message: &str) -> bool {
    serde_json::from_str::<Value>(message).is_ok_and(|value| value.get("type").is_some())
}

/// [ORB-23898] The terminal assistant message of a truncated capture, when the
/// answer projection holds no response envelope yet that message does.
///
/// A truncated capture keeps only a head and a tail, and the answer projection
/// reads one terminal frame. When that frame is absent from the kept bytes, the
/// projection is empty even though the newest assistant message in the tail
/// carries the agent's envelope. Returns that message only if it holds an
/// envelope frame; the caller validates it like any other envelope (frame,
/// status/error consistency, exit alignment), so the recovery admits nothing
/// the normal parser would refuse.
///
/// [ORB-23924] A retained Claude `result` is that terminal even when its
/// `result` text is null or empty. The assistant-message reader skips such a
/// frame and would otherwise return an earlier assistant envelope. Do not
/// replace the retained terminal with that earlier text. Error, refusal, and
/// timeout endings stay on the projection the caller already reads.
fn truncated_terminal_envelope_message(
    provider: &str,
    protocol_bytes: &[u8],
    answer_text: &str,
) -> Option<String> {
    if ParsedStdout::parse(answer_text)
        .peek_response_envelope()
        .is_some()
    {
        return None;
    }
    if retained_claude_terminal_result(provider, answer_text) {
        return None;
    }
    let message = latest_assistant_message(provider, protocol_bytes)?;
    ParsedStdout::parse(&message)
        .peek_response_envelope()
        .is_some()
        .then_some(message)
}

/// Claude's answer projection is the last `result` line when the retained
/// stream has one, and empty when it does not. A parsed `result` object is a
/// terminal the turn already ended on.
fn retained_claude_terminal_result(provider: &str, answer_text: &str) -> bool {
    if provider != "claude" {
        return false;
    }
    let text = answer_text.trim();
    if text.is_empty() {
        return false;
    }
    serde_json::from_str::<Value>(text)
        .ok()
        .is_some_and(|value| value.get("type").and_then(Value::as_str) == Some("result"))
}

/// Names this projection writes into step output.
///
/// An agent `result` cannot set one of these or have it listed in
/// `response_result_fields`. The drop below and every write share this slice,
/// so a new Orbit key cannot be offered as the agent's answer.
const ORBIT_OWNED_STEP_OUTPUT_KEYS: &[&str] = &[
    "build_budget_waits",
    "timed_out",
    "stalled",
    super::super::continuation::REQUEST_OUTPUT_KEY,
    super::super::response_correction::REQUEST_OUTPUT_KEY,
    "print_timeout_ms",
    "effective_deadline_ms",
    "response_result_fields",
    "final_message",
    "final_message_truncated",
    "final_message_bytes",
    "provider",
    "argv_redacted",
    "stdin_blob_ref",
    "stdout_blob_ref",
    "stderr_blob_ref",
    "exit_code",
    "duration_ms",
    "response_envelope_required",
    "response_envelope_valid",
    "response_envelope_status",
    "response_envelope_error",
    "completion_envelope_required",
    "completion_envelope_satisfied",
    "completion_envelope_error",
    "sandbox_write_diagnostic",
    "stdout_text",
    "stdout_text_truncated",
    "stdout_text_original_bytes",
    "stdout_text_preview_bytes",
    "stdout_text_preview_limit_bytes",
    "stdout_text_captured_bytes",
    "stdout_capture_truncated",
    "stdout_capture_limit_bytes",
    "stderr_original_bytes",
    "stderr_captured_bytes",
    "stderr_capture_truncated",
    "stderr_capture_limit_bytes",
];

/// Record one Orbit-owned step-output field, replacing any agent value.
#[track_caller]
fn insert_orbit_owned_output(
    output: &mut serde_json::Map<String, Value>,
    key: &'static str,
    value: Value,
) {
    debug_assert!(
        ORBIT_OWNED_STEP_OUTPUT_KEYS.contains(&key),
        "step-output key `{key}` is written without being reserved"
    );
    output.insert(key.to_string(), value);
}

/// Decide the step outcome from the provider's exit and its stdout envelope,
/// run the post-provider worktree check, and project the step output.
pub(in super::super) fn project_completion(
    exit: ProviderExit<'_>,
) -> Result<DispatchOutcome, DispatchError> {
    let ProviderExit {
        audit,
        started_at,
        finished_at,
        host,
        spec,
        input,
        run_id,
        provider,
        model,
        agent_execution_id,
        task_ids,
        worktree_boundary,
        sandbox,
        subprocess_cwd,
        redaction,
        timeout_seconds,
        argv_redacted,
        stdin_blob_ref,
        stdout_blob_ref,
        stderr_blob_ref,
        stdout,
        stderr,
        exit_code,
        duration,
        timed_out,
        stalled,
        stall_window,
        print_timeout,
        usage_context,
        build_budget_waits,
    } = exit;

    // Provider output is not the system of record for artifact-backed
    // activities: task state, review threads, git state, and deterministic
    // downstream gates are. Parse response envelopes to project useful fields
    // and diagnostics, but only make them authoritative when the activity
    // explicitly declares that downstream templates require them.
    let exit_success = !timed_out && !stalled && matches!(exit_code, Some(0));
    // Attribution is deliberately not gated on the exit code. An agent that
    // hits a policy-denied write mid-turn can still exit 0 — it narrates the
    // denial and stops without emitting a terminating envelope — and that
    // exit-0 shape is precisely the one that reached an operator with no path
    // and no rule. Deriving the diagnostic here costs a substring scan of
    // stderr (the helper skips any line without an EROFS marker) and keeps the
    // Orbit-owned attribution available to every failure branch below.
    let sandbox_write_diagnostic = match sandbox {
        Some(sandbox) if sandbox.kind == orbit_types::workflow::ExecutorSandboxKind::LinuxBwrap => {
            linux_bwrap_failed_write_diagnostic(
                &sandbox.fs_profile,
                stderr.protocol_bytes(),
                subprocess_cwd,
            )
            .map_err(|error| DispatchError::CliInvocationPermanent(error.to_string()))?
            .map(|diagnostic| bounded_diagnostic(&diagnostic, redaction))
        }
        _ => None,
    };
    // A truncated capture retains the final complete JSONL events separately
    // from its diagnostic prefix. Protocol parsing must use that tail so a
    // verbose provider's final Orbit envelope remains authoritative.
    //
    // Keep invocation telemetry distinct from answer projection. Provider
    // JSONL carries usage, tool traffic, failures, reasoning, and command
    // output; those frames belong in the trace and diagnostics, but only
    // provider-attributed assistant answer content may supply Orbit response
    // fields or completion status. [ORB-10946] [ORB-11348]
    let trace_stdout = normalize_cli_stdout(&provider, stdout.protocol_bytes());
    let answer_stdout = project_cli_response(&provider, stdout.protocol_bytes());
    let answer_text = String::from_utf8_lossy(answer_stdout.as_ref());
    let trace_stdout_text = String::from_utf8_lossy(trace_stdout.as_ref());
    let answer_parsed = ParsedStdout::parse(answer_text.as_ref());
    // [ORB-23898] Envelope decisions read the recovered terminal message when a
    // truncated capture lost the answer's envelope frame; trace reads keep the
    // answer projection, which carries the usage frames.
    let recovered_message = stdout
        .truncated()
        .then(|| {
            truncated_terminal_envelope_message(
                &provider,
                stdout.protocol_bytes(),
                answer_text.as_ref(),
            )
        })
        .flatten();
    let envelope_parsed =
        ParsedStdout::parse(recovered_message.as_deref().unwrap_or(answer_text.as_ref()));
    let declared_failure = envelope_parsed.peek_declared_response_failure();
    let envelope_status = declared_failure
        .as_ref()
        .map(|failure| failure.status.clone())
        .or_else(|| envelope_parsed.peek_response_status());
    // The operator-facing preview stays on the *raw* capture: normalization
    // drops the session control plane, and that is where a provider puts the
    // policy and authentication failures an operator needs to see.
    let raw_stdout_text = String::from_utf8_lossy(stdout.protocol_bytes());
    let stderr_text = String::from_utf8_lossy(stderr.protocol_bytes());
    let stdout_preview =
        stdout_text_preview(raw_stdout_text.as_ref(), redaction, stdout.truncated());
    let parsed_result = exit_success.then(|| {
        parse_cli_response_result_from(
            &envelope_parsed,
            stderr.protocol_bytes(),
            exit_code,
            duration.as_millis() as u64,
            true,
        )
    });
    let response_envelope_valid = matches!(parsed_result.as_ref(), Some(Ok(_)));
    let response_envelope_error = parsed_result
        .as_ref()
        .and_then(|result| result.as_ref().err())
        .map(|error| response_diagnostic(error, redaction));
    // [ORB-10449]: the step-completion protocol check. Content-blind
    // by construction — `response_envelope_protocol_check` reads the envelope
    // frame and never `result`/`error`, so this asks only "did the invocation
    // run its contract to the end", never "do we believe what it said". An
    // agent that declares `status: "failed"` passes this frame check; one that
    // yielded mid-turn emitted no envelope and does not.
    //
    // Only meaningful on an otherwise-clean exit: a timeout or nonzero exit
    // already fails the step with a more specific message.
    let completion_envelope_error = exit_success
        .then(|| envelope_parsed.response_envelope_protocol_check())
        .and_then(Result::err)
        .map(|error| completion_diagnostic(&error.to_string(), redaction));
    let completion_protocol_violation =
        spec.require_completion_envelope && completion_envelope_error.is_some();
    // [ORB-14683] An exit 0 with no envelope that ran for the whole provider
    // print-timeout is a spent budget, not a yielded agent. The elapsed time is
    // the only evidence: `agy` ends at its budget with a `SUCCESS` wrapper of
    // progress text. It fails whatever the activity's envelope
    // flags say, because the run was cut off rather than finished, and it is
    // reported as a timeout like the wall-clock kill it stands in for.
    // [ORB-15206]
    let print_timeout_reached = print_budget_exhausted(
        exit_success,
        completion_envelope_error.is_some(),
        print_timeout,
        duration,
    );
    // [ORB-10733] Protocol termination and control-plane outcome are distinct:
    // all recognized status tokens finish the frame, but a required completion
    // contract cannot checkpoint an explicit failed/timeout outcome. Only the
    // token controls this; the envelope's result/error payload stays advisory.
    let completion_status_failure = spec.require_completion_envelope
        && completion_envelope_error.is_none()
        && matches!(envelope_status.as_deref(), Some("failed") | Some("timeout"));
    let provider_auth_error = structured_provider_auth_error(&provider, stdout.protocol_bytes());
    // [ORB-14266] A provider's terminal refusal frame ends the turn whatever
    // the exit code, so it fails the invocation like an authentication error.
    let provider_refusal = structured_provider_refusal(&provider, stdout.protocol_bytes());
    // [ORB-14695] So does Claude's terminal error result for a usage limit.
    let provider_limit = structured_provider_limit(&provider, stdout.protocol_bytes());
    let structured_output_exhaustion = (!stdout.truncated())
        .then(|| frames::claude_structured_output_exhaustion(&provider, stdout.protocol_bytes()))
        .flatten()
        .map(|diagnostic| bounded_diagnostic(&diagnostic, redaction));
    // [ORB-16597] A non-completed Muse terminal's own failure reason, for
    // the operator diagnostic on either exit path. Classification of that
    // reason stays with the shared readers above and below.
    let muse_terminal_failure =
        muse_terminal_failure_diagnostic(&provider, stdout.protocol_bytes());
    // Two orthogonal contracts. `require_completion_envelope` gates step
    // completion and its status outcome (above); `require_response_envelope` additionally gates the
    // envelope's *content* for activities whose downstream templates consume it
    // (ADR-0224 / L-0087) — outside that opt-in, parsing stays advisory.
    let success = exit_success
        && provider_auth_error.is_none()
        && provider_refusal.is_none()
        && provider_limit.is_none()
        && structured_output_exhaustion.is_none()
        && muse_terminal_failure.is_none()
        && !completion_protocol_violation
        && !print_timeout_reached
        && !completion_status_failure
        && (!spec.require_response_envelope || response_envelope_valid);

    // [ORB-21837] The typed unfinished-work signal, read only when the
    // declared `failed` token is the step's sole failure: a clean exit with a
    // complete envelope and no provider, limit, refusal or budget failure.
    // The continuation loop in the dispatch entry decides admission.
    let continuation_request = (spec.incomplete_continuation.is_some()
        && completion_status_failure
        && exit_success
        && provider_auth_error.is_none()
        && provider_refusal.is_none()
        && provider_limit.is_none()
        && structured_output_exhaustion.is_none()
        && muse_terminal_failure.is_none()
        && !print_timeout_reached)
        .then(|| {
            declared_failure
                .as_ref()
                .and_then(|failure| failure.continuation.clone())
        })
        .flatten();

    // [ORB-22426] [ORB-22427] The reply the parser rejected, read only when that
    // rejection is the step's sole failure. `parsed_result` exists only for a
    // clean exit. Any failure frame the provider wrote, even one no reader above
    // recognizes, rules it out: the turn may have ended for that reason rather
    // than for its formatting. The correction loop in the dispatch entry decides
    // admission.
    let parse_rejection = (spec.response_correction.is_some()
        && spec.require_response_envelope
        && provider_auth_error.is_none()
        && provider_refusal.is_none()
        && provider_limit.is_none()
        && muse_terminal_failure.is_none()
        && !print_timeout_reached
        && !completion_status_failure
        && !provider_reported_failure(&provider, stdout.protocol_bytes()))
    .then(|| {
        parsed_result
            .as_ref()
            .and_then(|result| result.as_ref().err())
    })
    .flatten();
    let mut response_correction_request = parse_rejection
        .and_then(|error| {
            if is_success_carrying_error_violation(error) {
                if completion_protocol_violation {
                    return None;
                }
                let reply = envelope_parsed.peek_response_envelope()?.to_string();
                let reply =
                    bounded_redacted_text(&reply, redaction, false, FINAL_MESSAGE_LIMIT_BYTES);
                return Some((
                    super::super::response_correction::CONTRADICTORY_REPLY,
                    reply.text,
                    reply.truncated,
                ));
            }
            // The generic refusal alone is not enough: the reply below must be what
            // ended the turn, not what a lost capture, a denied write or a provider
            // failure left behind.
            let clean_omission = is_missing_envelope_violation(error)
                && !stdout.truncated()
                && sandbox_write_diagnostic.is_none()
                && !provider_reported_ending(
                    &provider,
                    stdout.protocol_bytes(),
                    &stderr_text,
                    duration,
                );
            if !clean_omission {
                return None;
            }
            let reply = bounded_assistant_message(
                &provider,
                stdout.protocol_bytes(),
                redaction,
                FINAL_MESSAGE_LIMIT_BYTES,
            )
            .filter(|reply| !is_provider_frame(&reply.text))?;
            Some((
                super::super::response_correction::MISSING_ENVELOPE,
                reply.text,
                reply.truncated,
            ))
        })
        .map(|(rejection, reply, truncated)| {
            serde_json::json!({
                "rejection": rejection,
                "rejected_reply": reply,
                "rejected_reply_truncated": truncated,
            })
        });

    // A failed process remains failed. Only this provider-owned formatting
    // terminal may ask the existing read-only loop for a fresh answer. Do not
    // project prior StructuredOutput tool arguments, or treat a partial stream
    // as proof that nothing else ended the invocation.
    if spec.response_correction.is_some()
        && spec.require_response_envelope
        && exit_code == Some(1)
        && !timed_out
        && !stalled
        && !stderr.truncated()
        && !stderr_text.to_ascii_lowercase().contains("api error")
        && sandbox_write_diagnostic.is_none()
        && provider_auth_error.is_none()
        && provider_refusal.is_none()
        && provider_limit.is_none()
        && !completion_status_failure
        && let Some(diagnostic) = structured_output_exhaustion.as_ref()
        && !provider_reported_ending(&provider, stdout.protocol_bytes(), &stderr_text, duration)
        && provider_invocation_diagnostic(&trace_stdout_text, &stderr_text).is_none()
    {
        response_correction_request = Some(serde_json::json!({
            "rejection": super::super::response_correction::STRUCTURED_OUTPUT_EXHAUSTED,
            "rejected_reply": diagnostic,
            // There is no accepted answer to copy: the terminal diagnostic is
            // all this request carries, and the whole result must be supplied.
            "rejected_reply_truncated": true,
        }));
    }

    // A conflict-recovery provider repairs files only. Once its process has
    // satisfied the activity completion contract, the host-side boundary
    // independently revalidates live ownership, stages the resolved conflict
    // set with every companion edit, and continues the checkpointed rebase.
    // No response result field is consulted. Ordinary providers retain the
    // same post-run integrity check, and an implementer's or recovery
    // agent's changed paths widen its task's selectors.
    if let Some(boundary) = worktree_boundary {
        boundary.verify_after_provider(
            host,
            success,
            input.get("failed_step_id").and_then(Value::as_str),
            task_ids,
        )?;
    }

    let trace = if trace_stdout.as_ref() == answer_stdout.as_ref() {
        parse_cli_invocation_trace_from(
            &answer_parsed,
            stderr.protocol_bytes(),
            exit_code,
            duration.as_millis() as u64,
            success,
        )
    } else {
        let trace_parsed = ParsedStdout::parse(trace_stdout_text.as_ref());
        parse_cli_invocation_trace_from(
            &trace_parsed,
            stderr.protocol_bytes(),
            exit_code,
            duration.as_millis() as u64,
            success,
        )
    }
    .or_else(|| {
        spec.response_correction.map(|_| {
            // A provider formatting failure has no valid Orbit envelope, but
            // its invocation still belongs in the correction's trace history.
            ParsedStdout::parse(trace_stdout_text.as_ref())
                .cli_invocation_trace(duration.as_millis() as u64)
        })
    });
    // [ORB-19680] Prompt-size price tiers key on each request's own prompt,
    // which the totals above cannot show. Codex identifies its rollout from
    // thread.started and turn.started, which can live in the redacted retained
    // prefix after truncation. The collector excludes earlier rollout turns
    // before assigning requests to this invocation; pricing adds their premiums
    // to this invocation's totals. Answer projection stays on protocol_bytes().
    // Usage stays on the reduced document. Claude's and Muse's reductions
    // drop the tool frames, and Grok's streaming-json reduction keeps only
    // the end event, so the calls are read from the raw capture.
    let trace = trace.map(|mut trace| {
        assign_recorded_tool_calls(&provider, stdout.protocol_bytes(), &mut trace);
        // [ORB-20572] Every reading above is of the retained suffix, whose
        // calls are numbered from its first complete line. A capture that
        // dropped its middle must not pass that suffix off as the whole trace.
        trace.tool_calls_incomplete = stdout.truncated();
        trace.long_prompt_requests = provider_long_prompt_requests(
            &provider,
            stdout.bytes(),
            !stdout.truncated(),
            usage_context.codex_home.as_deref(),
            trace.provider_model.as_deref().or(model.as_deref()),
            chrono::Utc::now(),
        );
        trace
    });
    let failure::CompletionFailure {
        message,
        failure_class,
        failure_code,
        observed_limit,
    } = failure::completion_failure(failure::CompletionFailureInput {
        provider: &provider,
        spec,
        input,
        stdout: &stdout,
        stderr: &stderr,
        redaction,
        sandbox,
        trace_stdout_text: trace_stdout_text.as_ref(),
        exit_success,
        exit_code,
        stalled,
        timed_out,
        stall_window,
        timeout_seconds,
        duration,
        provider_auth_error,
        provider_refusal,
        provider_limit,
        structured_output_exhaustion,
        muse_terminal_failure,
        sandbox_write_diagnostic: &sandbox_write_diagnostic,
        print_timeout,
        print_timeout_reached,
        envelope_status: &envelope_status,
        declared_failure: &declared_failure,
        response_envelope_error: &response_envelope_error,
        completion_envelope_error: &completion_envelope_error,
        completion_protocol_violation,
    });
    let account = spec.crew_account.as_deref();
    audit.emit_lossy(
        orbit_types::workflow::V2AuditEventKind::ProviderInvocationObserved {
            provider: provider.clone(),
            crew: spec.crew_name.clone(),
            account: spec.crew_account.clone(),
            started_at,
            finished_at,
            success,
            failure_class,
        },
    );
    // [ORB-14696] The windows first, so a limit failure observed in the same
    // run is the newer observation of its window.
    let crew = input.get("crew").and_then(Value::as_str);
    record_usage_windows(
        host,
        &provider,
        stdout.bytes(),
        &usage_context,
        run_id,
        crew,
        account,
    );
    if let Some((limit, detail)) = &observed_limit {
        record_limit(host, &provider, limit, detail, run_id, crew, account);
    }

    let StdoutTextPreview {
        text: stdout_text,
        truncated: stdout_text_truncated,
        preview_bytes: stdout_text_preview_bytes,
    } = stdout_preview;
    // [ORB-13899] What the agent last said, for an operator reading the run.
    // Taken from the answer projection, so it never quotes tool traffic.
    let final_message = bounded_assistant_message(
        &provider,
        stdout.protocol_bytes(),
        redaction,
        FINAL_MESSAGE_LIMIT_BYTES,
    );
    let mut output = parsed_result.and_then(Result::ok).unwrap_or_default();
    // Drop reserved names before listing the agent's keys. A name this
    // invocation does not write (no continuation or correction request) is
    // still not the agent's answer.
    output.retain(|key, _| !ORBIT_OWNED_STEP_OUTPUT_KEYS.contains(&key.as_str()));
    // The envelope `result` keys that are not invocation metadata, so a
    // reader can tell the agent's answer from Orbit's record of the run.
    let response_result_fields = if response_envelope_valid {
        serde_json::json!(output.keys().collect::<Vec<_>>())
    } else {
        Value::Null
    };
    // These are supervisor observations, so an agent result cannot override
    // them or make a spent provider budget appear complete.
    insert_orbit_owned_output(
        &mut output,
        "build_budget_waits",
        serde_json::json!(build_budget_waits),
    );
    insert_orbit_owned_output(
        &mut output,
        "timed_out",
        Value::Bool(timed_out || print_timeout_reached),
    );
    insert_orbit_owned_output(&mut output, "stalled", Value::Bool(stalled));
    if spec.incomplete_continuation.is_some() {
        insert_orbit_owned_output(
            &mut output,
            super::super::continuation::REQUEST_OUTPUT_KEY,
            continuation_request.map_or(
                Value::Null,
                |request| serde_json::json!({ "remaining_work": request.remaining_work }),
            ),
        );
    }
    if spec.response_correction.is_some() {
        insert_orbit_owned_output(
            &mut output,
            super::super::response_correction::REQUEST_OUTPUT_KEY,
            response_correction_request.unwrap_or(Value::Null),
        );
    }
    insert_orbit_owned_output(
        &mut output,
        "print_timeout_ms",
        print_timeout.map_or(Value::Null, |budget| {
            serde_json::json!(budget.as_millis() as u64)
        }),
    );
    insert_orbit_owned_output(
        &mut output,
        "effective_deadline_ms",
        serde_json::json!(
            timeout_seconds
                .saturating_mul(1000)
                .saturating_add(build_budget_waits.deadline_extension_ms)
        ),
    );
    let (final_message_text, final_message_truncated, final_message_bytes) = match final_message {
        Some(BoundedMessage {
            text,
            truncated,
            original_bytes,
        }) => (
            Value::String(text),
            truncated,
            serde_json::json!(original_bytes),
        ),
        None => (Value::Null, false, Value::Null),
    };
    for (key, value) in [
        ("response_result_fields", response_result_fields),
        ("final_message", final_message_text),
        (
            "final_message_truncated",
            Value::Bool(final_message_truncated),
        ),
        ("final_message_bytes", final_message_bytes),
        ("provider", Value::String(provider.clone())),
        ("argv_redacted", serde_json::json!(argv_redacted)),
        ("stdin_blob_ref", Value::String(stdin_blob_ref.clone())),
        ("stdout_blob_ref", Value::String(stdout_blob_ref.clone())),
        ("stderr_blob_ref", Value::String(stderr_blob_ref.clone())),
        ("exit_code", serde_json::json!(exit_code)),
        (
            "duration_ms",
            serde_json::json!(duration.as_millis() as u64),
        ),
        (
            "response_envelope_required",
            Value::Bool(spec.require_response_envelope),
        ),
        (
            "response_envelope_valid",
            Value::Bool(response_envelope_valid),
        ),
        (
            "response_envelope_status",
            envelope_status.map_or(Value::Null, Value::String),
        ),
        (
            "response_envelope_error",
            response_envelope_error.map_or(Value::Null, Value::String),
        ),
        (
            "completion_envelope_required",
            Value::Bool(spec.require_completion_envelope),
        ),
        (
            "completion_envelope_satisfied",
            Value::Bool(!exit_success || completion_envelope_error.is_none()),
        ),
        (
            "completion_envelope_error",
            completion_envelope_error.map_or(Value::Null, Value::String),
        ),
        (
            "sandbox_write_diagnostic",
            sandbox_write_diagnostic.map_or(Value::Null, Value::String),
        ),
        ("stdout_text", Value::String(stdout_text)),
        (
            "stdout_text_truncated",
            Value::Bool(stdout.truncated() || stdout_text_truncated),
        ),
        (
            "stdout_text_original_bytes",
            serde_json::json!(stdout.observed_bytes()),
        ),
        (
            "stdout_text_preview_bytes",
            serde_json::json!(stdout_text_preview_bytes),
        ),
        (
            "stdout_text_preview_limit_bytes",
            serde_json::json!(STDOUT_TEXT_PREVIEW_LIMIT_BYTES),
        ),
        (
            "stdout_text_captured_bytes",
            serde_json::json!(stdout.bytes().len()),
        ),
        ("stdout_capture_truncated", Value::Bool(stdout.truncated())),
        (
            "stdout_capture_limit_bytes",
            serde_json::json!(stdout.capture_limit_bytes()),
        ),
        (
            "stderr_original_bytes",
            serde_json::json!(stderr.observed_bytes()),
        ),
        (
            "stderr_captured_bytes",
            serde_json::json!(stderr.bytes().len()),
        ),
        ("stderr_capture_truncated", Value::Bool(stderr.truncated())),
        (
            "stderr_capture_limit_bytes",
            serde_json::json!(stderr.capture_limit_bytes()),
        ),
    ] {
        insert_orbit_owned_output(&mut output, key, value);
    }

    Ok(DispatchOutcome {
        success,
        // Results become downstream step input and durable pipeline state.
        // Scrub the parsed fields as well as the bounded stdout preview.
        output: redact_all_json(Value::Object(output)),
        message,
        failure_code,
        invocation: trace.map(|trace| DispatchInvocationTrace {
            provider,
            model,
            agent_execution_id,
            account: spec.crew_account.clone(),
            trace,
        }),
    })
}
