//! Projection of a finished provider subprocess: response and completion
//! envelopes, invocation trace, failure diagnostics, and the step output.

use std::path::{Path, PathBuf};
use std::time::Duration;

use orbit_agent::{
    ParsedStdout, antigravity_print_timeout_diagnostic, antigravity_terminal_error_diagnostic,
    normalize_cli_stdout, project_cli_response, provider_authentication_failure,
    provider_capacity_exhausted, provider_content_refusal, provider_invocation_diagnostic,
    provider_usage_limit,
};
use orbit_common::process::build_budget::BuildBudgetWaits;
use orbit_common::security::redaction::{PatternRedactor, redact_all_json};
use orbit_types::workflow::activity_job::AgentLoopSpec;
use orbit_types::workflow::{ProviderFailureClass, ProviderLimitFailure, provider_failure_text};
use serde_json::Value;

use crate::context::RuntimeHost;

use super::super::super::dispatcher::{
    DispatchError, DispatchInvocationTrace, DispatchOutcome, ResolvedSandbox,
};
use super::super::super::workspace::WorktreeBoundaryGuard;
use super::super::envelope::{parse_cli_invocation_trace_from, parse_cli_response_result_from};
use super::super::response_diagnostics::{
    bounded_diagnostic, completion_diagnostic, declared_failure_diagnostic, response_diagnostic,
    with_sandbox_write_attribution,
};
use super::super::spawn_diagnostics::{
    copilot_model_unavailable_diagnostic, linux_bwrap_failed_write_diagnostic,
    macos_keychain_auth_diagnostic, macos_sandbox_apply_failure_diagnostic,
};
use super::super::stdout_preview::{
    BoundedMessage, FINAL_MESSAGE_LIMIT_BYTES, STDOUT_TEXT_PREVIEW_LIMIT_BYTES, StdoutTextPreview,
    bounded_assistant_message, stdout_text_preview,
};
use super::super::supervisor::CapturedOutput;
use super::limit::{record_limit, record_usage_windows, reported_limit, structured_provider_limit};

/// Everything a provider subprocess left behind once it exited, plus the
/// run state its completion projection reads.
pub(super) struct ProviderExit<'a> {
    pub(super) host: &'a dyn RuntimeHost,
    pub(super) spec: &'a AgentLoopSpec,
    pub(super) input: &'a Value,
    pub(super) run_id: &'a str,
    pub(super) provider: String,
    pub(super) model: Option<String>,
    pub(super) task_ids: &'a [String],
    pub(super) worktree_boundary: Option<WorktreeBoundaryGuard>,
    pub(super) sandbox: Option<&'a ResolvedSandbox>,
    pub(super) subprocess_cwd: Option<&'a Path>,
    pub(super) redaction: &'a PatternRedactor,
    pub(super) timeout_seconds: u64,
    pub(super) argv_redacted: Vec<String>,
    pub(super) stdin_blob_ref: String,
    pub(super) stdout_blob_ref: String,
    pub(super) stderr_blob_ref: String,
    pub(super) stdout: CapturedOutput,
    pub(super) stderr: CapturedOutput,
    pub(super) exit_code: Option<i32>,
    pub(super) duration: Duration,
    pub(super) timed_out: bool,
    /// The provider-side time budget Orbit injected into argv, if the
    /// provider has one. [ORB-14683]
    pub(super) print_timeout: Option<Duration>,
    /// The `CODEX_HOME` a Codex child ran with, where its session rollout
    /// holds the usage windows. [ORB-14696]
    pub(super) codex_home: Option<PathBuf>,
    pub(super) build_budget_waits: BuildBudgetWaits,
}

/// Decide the step outcome from the provider's exit and its stdout envelope,
/// run the post-provider worktree check, and project the step output.
pub(super) fn project_completion(exit: ProviderExit<'_>) -> Result<DispatchOutcome, DispatchError> {
    let ProviderExit {
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
        stdin_blob_ref,
        stdout_blob_ref,
        stderr_blob_ref,
        stdout,
        stderr,
        exit_code,
        duration,
        timed_out,
        print_timeout,
        codex_home,
        build_budget_waits,
    } = exit;

    // Provider output is not the system of record for artifact-backed
    // activities: task state, review threads, git state, and deterministic
    // downstream gates are. Parse response envelopes to project useful fields
    // and diagnostics, but only make them authoritative when the activity
    // explicitly declares that downstream templates require them.
    let exit_success = !timed_out && matches!(exit_code, Some(0));
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
    let declared_failure = answer_parsed.peek_declared_response_failure();
    let envelope_status = declared_failure
        .as_ref()
        .map(|failure| failure.status.clone())
        .or_else(|| answer_parsed.peek_response_status());
    // The operator-facing preview stays on the *raw* capture: normalization
    // drops the session control plane, and that is where a provider puts the
    // policy and authentication failures an operator needs to see.
    let raw_stdout_text = String::from_utf8_lossy(stdout.protocol_bytes());
    let stdout_preview =
        stdout_text_preview(raw_stdout_text.as_ref(), redaction, stdout.truncated());
    let parsed_result = exit_success.then(|| {
        parse_cli_response_result_from(
            &answer_parsed,
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
        .then(|| answer_parsed.response_envelope_protocol_check())
        .and_then(Result::err)
        .map(|error| completion_diagnostic(&error.to_string(), redaction));
    let completion_protocol_violation =
        spec.require_completion_envelope && completion_envelope_error.is_some();
    // [ORB-14683] An exit 0 with no envelope that ran for the whole provider
    // print-timeout is a spent budget, not a yielded agent. The elapsed time is
    // the only evidence: `agy` ends at its budget with a `SUCCESS` wrapper of
    // progress text. Orbit's spawn clock starts no later than the provider's, so
    // an earlier exit cannot match. It fails whatever the activity's envelope
    // flags say, because the run was cut off rather than finished.
    let print_timeout_reached = completion_envelope_error.is_some()
        && print_timeout.is_some_and(|budget| duration >= budget);
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
    // Two orthogonal contracts. `require_completion_envelope` gates step
    // completion and its status outcome (above); `require_response_envelope` additionally gates the
    // envelope's *content* for activities whose downstream templates consume it
    // (ADR-0224 / L-0087) — outside that opt-in, parsing stays advisory.
    let success = exit_success
        && provider_auth_error.is_none()
        && provider_refusal.is_none()
        && provider_limit.is_none()
        && !completion_protocol_violation
        && !print_timeout_reached
        && !completion_status_failure
        && (!spec.require_response_envelope || response_envelope_valid);

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
    };
    // [ORB-14695] The usage limit the failure reports, with the provider's
    // own words, for the host's provider-limit store.
    let mut observed_limit: Option<(ProviderLimitFailure, String)> = None;
    let mut limit_failure = |text: &str, prefix: &str| {
        let reported = bounded_diagnostic(text, redaction);
        let limit = reported_limit(&provider, text, stdout.protocol_bytes(), chrono::Utc::now());
        let message = limit.text(
            &provider,
            &format!("{prefix}{provider} provider reported a usage limit: {reported}"),
        );
        observed_limit = Some((limit, reported));
        message
    };
    let message = if timed_out {
        Some(format!(
            "cli subprocess exceeded {}s wall-clock timeout",
            timeout_seconds
        ))
    } else if let Some(diagnostic) = provider_auth_error {
        Some(provider_failure_text(
            ProviderFailureClass::Unavailable,
            &provider,
            &bounded_diagnostic(&diagnostic, redaction),
        ))
    } else if let Some(refusal) = provider_refusal {
        Some(provider_failure_text(
            ProviderFailureClass::Refusal,
            &provider,
            &format!(
                "{provider} provider refused the request: {}",
                bounded_diagnostic(&refusal, redaction)
            ),
        ))
    } else if let Some(limit) = provider_limit.as_deref() {
        Some(limit_failure(limit, ""))
    } else if !exit_success {
        let stderr_text = String::from_utf8_lossy(stderr.protocol_bytes());
        let exit_message = || match exit_code {
            Some(code) => format!("cli subprocess exited with code {code}"),
            // No exit status means the OS ended the process with a signal.
            None => {
                "cli subprocess was terminated by a signal and reported no exit code".to_string()
            }
        };
        let diagnostic = sandbox_write_diagnostic
            .clone()
            // Ordered first among the provider-output diagnostics: when
            // sandbox-exec itself could not apply the profile the provider
            // never ran, so no marker any later branch keys on can be
            // genuine. [DANI-10509]
            .or_else(|| {
                macos_sandbox_apply_failure_diagnostic(
                    &provider,
                    sandbox,
                    exit_code,
                    stderr_text.as_ref(),
                )
                .map(|diagnostic| format!("{} {diagnostic}", exit_message()))
            })
            // Copilot reports an unavailable explicit model only on
            // stderr. Join it to the resolved crew before the generic
            // exit-code path loses the configuration source.
            .or_else(|| {
                input
                    .get("crew")
                    .and_then(Value::as_str)
                    .and_then(|crew| {
                        copilot_model_unavailable_diagnostic(&provider, crew, stderr_text.as_ref())
                    })
                    .map(|diagnostic| {
                        format!(
                            "{} {}",
                            exit_message(),
                            bounded_diagnostic(&diagnostic, redaction)
                        )
                    })
            })
            // A Keychain-backed provider login reads as "expired" whether it
            // really expired or the sandbox hid the credential. Orbit
            // compiled the profile, so it is the layer that can say which
            // one this was — and the provider's own message cannot.
            // [ORB-10929]
            .or_else(|| {
                macos_keychain_auth_diagnostic(
                    &provider,
                    sandbox,
                    &format!("{trace_stdout_text}\n{stderr_text}"),
                )
                .map(|diagnostic| format!("{} {diagnostic}", exit_message()))
            })
            // [ORB-10746] A bare exit code cannot distinguish "this CLI
            // has no --json-schema" from "the provider rejected Orbit's
            // schema" from any other nonzero exit, and the first two are
            // configuration faults an operator can act on immediately.
            .or_else(|| {
                provider_invocation_diagnostic(trace_stdout_text.as_ref(), stderr_text.as_ref())
                    .map(|diagnostic| bounded_diagnostic(&diagnostic, redaction))
            })
            // Antigravity writes terminal `ERROR` on stdout and often
            // leaves stderr empty. Read the raw capture: normalization
            // drops failed terminals so they cannot satisfy completion.
            // [ORB-11337]
            .or_else(|| {
                antigravity_terminal_error_diagnostic(&provider, stdout.protocol_bytes()).map(
                    |diagnostic| {
                        format!(
                            "{} {}",
                            exit_message(),
                            bounded_diagnostic(&diagnostic, redaction)
                        )
                    },
                )
            })
            .unwrap_or_else(exit_message);
        // [ORB-13941] A provider that could not authenticate is unusable on
        // this host, not a failed attempt at the work. Only text the provider
        // wrote about itself is read, never the agent's transcript; the typed
        // marker lets a pull drain release the claim instead of failing it.
        let terminal_error =
            antigravity_terminal_error_diagnostic(&provider, stdout.protocol_bytes());
        if provider_authentication_failure(&stderr_text)
            || terminal_error
                .as_deref()
                .is_some_and(provider_authentication_failure)
        {
            Some(provider_failure_text(
                ProviderFailureClass::Unavailable,
                &provider,
                &diagnostic,
            ))
        } else if let Some(limit) = provider_reported(
            &provider,
            stdout.protocol_bytes(),
            stderr_text.as_ref(),
            terminal_error.as_deref(),
            provider_usage_limit,
        ) {
            // [ORB-14695] Nor can either outlast an account's usage limit:
            // the limit holds until the reset the provider reported.
            Some(limit_failure(&limit, &format!("{diagnostic}: ")))
        } else if let Some(capacity) = provider_reported(
            &provider,
            stdout.protocol_bytes(),
            stderr_text.as_ref(),
            terminal_error.as_deref(),
            provider_capacity_exhausted,
        ) {
            // [ORB-14149] Nor can a repair agent, or an immediate rerun of the
            // same model, change a provider's capacity. Read only from a
            // failed exit: a provider that reports capacity mid-turn and then
            // finishes is not unavailable.
            Some(provider_failure_text(
                ProviderFailureClass::Capacity,
                &provider,
                &format!(
                    "{diagnostic}: {provider} provider reported the selected model at capacity: {}",
                    bounded_diagnostic(&capacity, redaction)
                ),
            ))
        } else if let Some(refusal) = provider_reported(
            &provider,
            stdout.protocol_bytes(),
            stderr_text.as_ref(),
            terminal_error.as_deref(),
            provider_content_refusal,
        ) {
            // [ORB-14266] Nor does either change the provider's content
            // policy: Codex's content filter ends the turn with its own
            // `error` and `turn.failed` frames and a failed exit.
            Some(provider_failure_text(
                ProviderFailureClass::Refusal,
                &provider,
                &format!(
                    "{diagnostic}: {provider} provider refused the request: {}",
                    bounded_diagnostic(&refusal, redaction)
                ),
            ))
        } else {
            Some(diagnostic)
        }
    } else if let Some(budget) = print_timeout.filter(|_| print_timeout_reached) {
        Some(with_sandbox_write_attribution(
            antigravity_print_timeout_diagnostic(budget, duration),
            sandbox_write_diagnostic.as_deref(),
        ))
    } else if (spec.require_completion_envelope || spec.require_response_envelope)
        && matches!(envelope_status.as_deref(), Some("failed") | Some("timeout"))
    {
        Some(with_sandbox_write_attribution(
            declared_failure_diagnostic(
                envelope_status.as_deref().unwrap_or("unknown"),
                declared_failure.as_ref(),
                redaction,
            ),
            sandbox_write_diagnostic.as_deref(),
        ))
    } else if spec.require_response_envelope
        && let Some(error) = response_envelope_error.clone()
    {
        Some(with_sandbox_write_attribution(
            error,
            sandbox_write_diagnostic.as_deref(),
        ))
    } else if completion_protocol_violation {
        // Ordered last on purpose: an activity that opted into the content
        // contract already produced a strictly more specific diagnostic above,
        // and the two conditions largely overlap. This branch is what the
        // remaining activities — the ones that only ever had the advisory
        // parse — now report instead of silently checkpointing success.
        completion_envelope_error
            .clone()
            .map(|error| with_sandbox_write_attribution(error, sandbox_write_diagnostic.as_deref()))
    } else {
        None
    };

    // [ORB-14696] The windows first, so a limit failure observed in the same
    // run is the newer observation of its window.
    let crew = input.get("crew").and_then(Value::as_str);
    record_usage_windows(
        host,
        &provider,
        stdout.bytes(),
        codex_home.as_deref(),
        run_id,
        crew,
    );
    if let Some((limit, detail)) = &observed_limit {
        record_limit(host, &provider, limit, detail, run_id, crew);
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
    // The envelope `result` keys, recorded before Orbit's own fields join
    // them, so a reader can tell the agent's answer from invocation metadata.
    let response_result_fields = if response_envelope_valid {
        serde_json::json!(output.keys().collect::<Vec<_>>())
    } else {
        Value::Null
    };
    output.insert(
        "build_budget_waits".to_string(),
        serde_json::json!(build_budget_waits),
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
        ("timed_out", Value::Bool(timed_out)),
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
        output.entry(key.to_string()).or_insert(value);
    }

    Ok(DispatchOutcome {
        success,
        // Results become downstream step input and durable pipeline state.
        // Scrub the parsed fields as well as the bounded stdout preview.
        output: redact_all_json(Value::Object(output)),
        message,
        invocation: trace.map(|trace| DispatchInvocationTrace {
            provider,
            model,
            trace,
        }),
    })
}

/// Stdout's JSON frames, for the provider-owned failure readers below.
pub(super) fn stdout_frames(stdout: &[u8]) -> impl Iterator<Item = Value> + '_ {
    serde_json::Deserializer::from_slice(stdout)
        .into_iter::<Value>()
        .filter_map(Result::ok)
}

/// The failure a provider wrote in its own control-plane `frame`, never
/// assistant or tool text, and never an Orbit envelope, which describes the
/// work rather than the provider.
pub(super) fn provider_failure<'a>(provider: &str, frame: &'a Value) -> Option<&'a Value> {
    if frame.get("schemaVersion").is_some() {
        return None;
    }
    match provider {
        "claude"
            if frame.get("is_error").and_then(Value::as_bool) == Some(true)
                && matches!(
                    frame.get("type").and_then(Value::as_str),
                    None | Some("result")
                ) =>
        {
            Some(frame)
        }
        "codex"
            if matches!(
                frame.get("type").and_then(Value::as_str),
                Some("error" | "turn.failed")
            ) =>
        {
            Some(frame.get("error").unwrap_or(frame))
        }
        "grok" | "gemini"
            if matches!(
                frame.get("type").and_then(Value::as_str),
                None | Some("error")
            ) =>
        {
            frame.get("error")
        }
        _ => None,
    }
}

/// Read only provider-owned failure frames, never assistant or tool text.
/// Claude can emit an error result even with exit 0; the wrapper must still
/// fail the invocation rather than allowing an earlier answer to succeed.
fn structured_provider_auth_error(provider: &str, stdout: &[u8]) -> Option<String> {
    stdout_frames(stdout).find_map(|frame| {
        let failure = provider_failure(provider, &frame)?;
        let status = [failure, &frame].into_iter().find_map(|fields| {
            [
                "api_error_status",
                "status",
                "status_code",
                "http_status",
                "code",
            ]
            .iter()
            .find_map(|key| {
                let value = fields.get(*key)?;
                let status = value.as_u64().or_else(|| value.as_str()?.parse().ok())?;
                matches!(status, 401 | 403).then_some(status)
            })
        });
        let message = failure.as_str().or_else(|| {
            ["message", "result", "type", "status", "code"]
                .iter()
                .filter_map(|key| failure.get(*key).and_then(Value::as_str))
                .find(|text| provider_authentication_failure(text))
        });
        if status.is_none() && !message.is_some_and(provider_authentication_failure) {
            return None;
        }
        let status = status
            .map(|status| format!(" (HTTP {status})"))
            .unwrap_or_default();
        Some(format!(
            "{provider} provider authentication failure{status}: {}",
            message.unwrap_or("provider credentials were rejected")
        ))
    })
}

/// [ORB-14266] Claude's terminal `result` frame stopping for a refusal: the
/// provider declined the turn, so no answer before it may stand. Only the
/// provider's own control frame is read, never assistant or tool text.
fn structured_provider_refusal(provider: &str, stdout: &[u8]) -> Option<String> {
    if provider != "claude" {
        return None;
    }
    let frame = stdout_frames(stdout)
        .filter(|frame| frame.get("schemaVersion").is_none())
        .filter(|frame| frame.get("type").and_then(Value::as_str) == Some("result"))
        .last()?;
    let refused = frame.get("stop_reason").and_then(Value::as_str) == Some("refusal")
        || (frame.get("is_error").and_then(Value::as_bool) == Some(true)
            && frame
                .get("result")
                .and_then(Value::as_str)
                .is_some_and(provider_content_refusal));
    refused.then(|| {
        frame
            .get("result")
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .unwrap_or("stop_reason=refusal")
            .to_string()
    })
}

/// The text a failed provider wrote about itself that `matches` — its
/// stderr, its terminal error, or a provider-owned failure frame on stdout.
/// Codex reports an exhausted model [ORB-14149] and a content-filter refusal
/// [ORB-14266] only as `error` and `turn.failed` frames.
fn provider_reported(
    provider: &str,
    stdout: &[u8],
    stderr_text: &str,
    terminal_error: Option<&str>,
    matches: fn(&str) -> bool,
) -> Option<String> {
    let frame_text = || {
        stdout_frames(stdout).find_map(|frame| {
            let failure = provider_failure(provider, &frame)?;
            failure
                .as_str()
                .into_iter()
                .chain(
                    ["message", "result"]
                        .iter()
                        .filter_map(|key| failure.get(*key).and_then(Value::as_str)),
                )
                .find(|text| matches(text))
                .map(str::to_string)
        })
    };
    let line = |text: &str| {
        text.lines()
            .find(|line| matches(line))
            .map(|line| line.trim().to_string())
    };
    line(stderr_text)
        .or_else(|| terminal_error.and_then(line))
        .or_else(frame_text)
}
