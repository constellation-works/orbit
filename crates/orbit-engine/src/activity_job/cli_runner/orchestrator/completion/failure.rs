//! Ordered completion failure diagnostics.

use super::super::super::response_diagnostics::{
    bounded_diagnostic, declared_failure_code, declared_failure_diagnostic,
    with_sandbox_write_attribution,
};
use super::super::super::spawn_diagnostics::{
    copilot_model_unavailable_diagnostic, macos_keychain_auth_diagnostic,
    macos_sandbox_apply_failure_diagnostic,
};
use super::super::super::supervisor::CapturedOutput;
use super::super::limit::reported_limit;
use super::frames::provider_reported;
use crate::activity_job::dispatcher::ResolvedSandbox;
use orbit_agent::{
    DeclaredResponseFailure, antigravity_background_task_diagnostic,
    antigravity_print_timeout_diagnostic, antigravity_terminal_error_diagnostic,
    provider_authentication_failure, provider_capacity_exhausted, provider_content_refusal,
    provider_invocation_diagnostic, provider_usage_limit,
};
use orbit_common::security::redaction::PatternRedactor;
use orbit_types::workflow::activity_job::AgentLoopSpec;
use orbit_types::workflow::{
    CLI_STDERR_TAIL_SEPARATOR, ProviderFailureClass, ProviderLimitFailure, STALLED_MARKER,
    TRANSIENT_FAILURE_MARKER, provider_failure_text, with_failed_account,
};
use serde_json::Value;
use std::time::Duration;

pub(super) struct CompletionFailureInput<'a> {
    pub(super) provider: &'a str,
    pub(super) spec: &'a AgentLoopSpec,
    pub(super) input: &'a Value,
    pub(super) stdout: &'a CapturedOutput,
    pub(super) stderr: &'a CapturedOutput,
    pub(super) redaction: &'a PatternRedactor,
    pub(super) sandbox: Option<&'a ResolvedSandbox>,
    pub(super) trace_stdout_text: &'a str,
    pub(super) exit_success: bool,
    pub(super) exit_code: Option<i32>,
    pub(super) stalled: bool,
    pub(super) timed_out: bool,
    pub(super) stall_window: Duration,
    pub(super) timeout_seconds: u64,
    pub(super) duration: Duration,
    pub(super) provider_auth_error: Option<String>,
    pub(super) provider_refusal: Option<String>,
    pub(super) provider_limit: Option<String>,
    pub(super) structured_output_exhaustion: Option<String>,
    pub(super) muse_terminal_failure: Option<String>,
    pub(super) sandbox_write_diagnostic: &'a Option<String>,
    pub(super) print_timeout: Option<Duration>,
    pub(super) print_timeout_reached: bool,
    pub(super) envelope_status: &'a Option<String>,
    pub(super) declared_failure: &'a Option<DeclaredResponseFailure>,
    pub(super) response_envelope_error: &'a Option<String>,
    pub(super) completion_envelope_error: &'a Option<String>,
    pub(super) completion_protocol_violation: bool,
}

pub(super) struct CompletionFailure {
    pub(super) message: Option<String>,
    pub(super) failure_class: Option<ProviderFailureClass>,
    pub(super) failure_code: Option<orbit_types::workflow::StepFailureCode>,
    pub(super) observed_limit: Option<(ProviderLimitFailure, String)>,
}

pub(super) fn completion_failure(input: CompletionFailureInput<'_>) -> CompletionFailure {
    let CompletionFailureInput {
        provider,
        spec,
        input,
        stdout,
        stderr,
        redaction,
        sandbox,
        trace_stdout_text,
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
        sandbox_write_diagnostic,
        print_timeout,
        print_timeout_reached,
        envelope_status,
        declared_failure,
        response_envelope_error,
        completion_envelope_error,
        completion_protocol_violation,
    } = input;
    // [ORB-14695] The usage limit the failure reports, with the provider's
    // own words, for the host's provider-limit store.
    let mut failure_class = None;
    let mut observed_limit: Option<(ProviderLimitFailure, String)> = None;
    let mut limit_failure = |text: &str, prefix: &str| {
        let reported = bounded_diagnostic(text, redaction);
        let limit = reported_limit(provider, text, stdout.protocol_bytes(), chrono::Utc::now());
        let message = limit.text(
            provider,
            &format!("{prefix}{provider} provider reported a usage limit: {reported}"),
        );
        observed_limit = Some((limit, reported));
        message
    };
    // [ORB-15243] An exit 0 with no envelope whose stream last reported a
    // background task still running: the agent ended its turn without waiting
    // for work it started. Only a step that already fails for the missing
    // envelope is reworded; the exit code and a persisted summary are no
    // evidence either way.
    let background_task_diagnostic = (exit_success
        && completion_envelope_error.is_some()
        && (spec.require_completion_envelope || spec.require_response_envelope))
        .then(|| {
            antigravity_background_task_diagnostic(
                provider,
                stdout.protocol_bytes(),
                duration,
                |text| bounded_diagnostic(text, redaction),
            )
        })
        .flatten();
    let message = if stalled {
        Some(format!(
            "{TRANSIENT_FAILURE_MARKER} {STALLED_MARKER} provider process produced no output and used no CPU for {}s",
            stall_window.as_secs()
        ))
    } else if timed_out {
        Some(format!(
            "cli subprocess exceeded {}s wall-clock timeout",
            timeout_seconds
        ))
    } else if let Some(diagnostic) = provider_auth_error {
        failure_class = Some(ProviderFailureClass::Unavailable);
        Some(provider_failure_text(
            ProviderFailureClass::Unavailable,
            provider,
            &bounded_diagnostic(&diagnostic, redaction),
        ))
    } else if let Some(refusal) = provider_refusal {
        failure_class = Some(ProviderFailureClass::Refusal);
        Some(provider_failure_text(
            ProviderFailureClass::Refusal,
            provider,
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
                    provider,
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
                        copilot_model_unavailable_diagnostic(provider, crew, stderr_text.as_ref())
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
                    provider,
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
                provider_invocation_diagnostic(trace_stdout_text, stderr_text.as_ref())
                    .map(|diagnostic| bounded_diagnostic(&diagnostic, redaction))
            })
            .or_else(|| {
                structured_output_exhaustion
                    .as_ref()
                    .map(|diagnostic| format!("{} {diagnostic}", exit_message()))
            })
            // Antigravity writes terminal `ERROR` on stdout and often
            // leaves stderr empty. Read the raw capture: normalization
            // drops failed terminals so they cannot satisfy completion.
            // [ORB-11337]
            .or_else(|| {
                antigravity_terminal_error_diagnostic(provider, stdout.protocol_bytes()).map(
                    |diagnostic| {
                        format!(
                            "{} {}",
                            exit_message(),
                            bounded_diagnostic(&diagnostic, redaction)
                        )
                    },
                )
            })
            // [ORB-16597] Muse likewise writes its failure reason only in
            // its terminal frame and often leaves stderr empty.
            .or_else(|| {
                muse_terminal_failure.as_ref().map(|diagnostic| {
                    format!(
                        "{} {}",
                        exit_message(),
                        bounded_diagnostic(diagnostic, redaction)
                    )
                })
            });
        let generic_diagnostic = diagnostic.is_none();
        let diagnostic = diagnostic.unwrap_or_else(exit_message);
        // [ORB-13941] A provider that could not authenticate is unusable on
        // this host, not a failed attempt at the work. Only text the provider
        // wrote about itself is read, never the agent's transcript; the typed
        // marker lets a pull drain release the claim instead of failing it.
        let terminal_error =
            antigravity_terminal_error_diagnostic(provider, stdout.protocol_bytes());
        if provider_authentication_failure(&stderr_text)
            || terminal_error
                .as_deref()
                .is_some_and(provider_authentication_failure)
            || muse_terminal_failure
                .as_deref()
                .is_some_and(provider_authentication_failure)
        {
            failure_class = Some(ProviderFailureClass::Unavailable);
            Some(provider_failure_text(
                ProviderFailureClass::Unavailable,
                provider,
                &diagnostic,
            ))
        } else if let Some(limit) = provider_reported(
            provider,
            stdout.protocol_bytes(),
            stderr_text.as_ref(),
            terminal_error.as_deref(),
            provider_usage_limit,
        ) {
            // [ORB-14695] Nor can either outlast an account's usage limit:
            // the limit holds until the reset the provider reported.
            Some(limit_failure(&limit, &format!("{diagnostic}: ")))
        } else if let Some(capacity) = provider_reported(
            provider,
            stdout.protocol_bytes(),
            stderr_text.as_ref(),
            terminal_error.as_deref(),
            provider_capacity_exhausted,
        ) {
            failure_class = Some(ProviderFailureClass::Capacity);
            // [ORB-14149] Nor can a repair agent, or an immediate rerun of the
            // same model, change a provider's capacity. Read only from a
            // failed exit: a provider that reports capacity mid-turn and then
            // finishes is not unavailable.
            Some(provider_failure_text(
                ProviderFailureClass::Capacity,
                provider,
                &format!(
                    "{diagnostic}: {provider} provider reported the selected model at capacity: {}",
                    bounded_diagnostic(&capacity, redaction)
                ),
            ))
        } else if let Some(refusal) = provider_reported(
            provider,
            stdout.protocol_bytes(),
            stderr_text.as_ref(),
            terminal_error.as_deref(),
            provider_content_refusal,
        ) {
            failure_class = Some(ProviderFailureClass::Refusal);
            // [ORB-14266] Nor does either change the provider's content
            // policy: Codex's content filter ends the turn with its own
            // `error` and `turn.failed` frames and a failed exit.
            Some(provider_failure_text(
                ProviderFailureClass::Refusal,
                provider,
                &format!(
                    "{diagnostic}: {provider} provider refused the request: {}",
                    bounded_diagnostic(&refusal, redaction)
                ),
            ))
        } else if generic_diagnostic {
            // Context only: classify the unchanged exit message, never words,
            // typed markers or check-repair phrases a provider happens to
            // quote on stderr (`failure_classification_text`).
            let mut tail: Vec<_> = stderr_text
                .lines()
                .rev()
                .filter(|line| !line.trim().is_empty())
                .take(4)
                .collect();
            tail.reverse();
            let tail = bounded_diagnostic(&tail.join("\n"), redaction);
            Some(if tail.is_empty() {
                diagnostic
            } else {
                format!("{diagnostic}{CLI_STDERR_TAIL_SEPARATOR}{tail}")
            })
        } else {
            Some(diagnostic)
        }
    } else if let Some(diagnostic) = structured_output_exhaustion {
        Some(with_sandbox_write_attribution(
            diagnostic,
            sandbox_write_diagnostic.as_deref(),
        ))
    } else if let Some(budget) = print_timeout.filter(|_| print_timeout_reached) {
        Some(with_sandbox_write_attribution(
            format!(
                "{TRANSIENT_FAILURE_MARKER} {}",
                antigravity_print_timeout_diagnostic(budget, duration),
            ),
            sandbox_write_diagnostic.as_deref(),
        ))
    } else if let Some(diagnostic) = background_task_diagnostic {
        Some(with_sandbox_write_attribution(
            diagnostic,
            sandbox_write_diagnostic.as_deref(),
        ))
    } else if let Some(diagnostic) = muse_terminal_failure {
        // [ORB-16597] Exit 0 with a non-completed terminal: the provider's
        // own reason, not the generic missing-envelope diagnostic.
        Some(with_sandbox_write_attribution(
            bounded_diagnostic(&diagnostic, redaction),
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
    // [ORB-17288] A crew that sets `env` is its own provider account: its
    // provider failure names that account, so the hold or exclusion it
    // places stays on that crew.
    let account = spec.crew_account.as_deref();
    let message = match account {
        Some(account) => message.map(|message| with_failed_account(&message, account)),
        None => message,
    };

    if observed_limit.is_some() {
        failure_class = Some(ProviderFailureClass::Limit);
    }
    CompletionFailure {
        message,
        failure_class,
        failure_code: declared_failure_code(declared_failure.as_ref()),
        observed_limit,
    }
}
