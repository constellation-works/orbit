//! One attempt of a `target: script:<name>` step (script steps §4–§5).
//!
//! The host checks the grant, snapshots the script and runs it under the
//! script profile ([`RuntimeHost::run_script_step`]). This side decides
//! what the answer means for the step and records it: valid stdout JSON is
//! the step's output; invalid output fails without retry; a nonzero exit,
//! a signal, a timeout or a scratch breach is `Ok(success = false)` with
//! `{exit_code, signal}` as output, which the retry loop retries per the
//! step's `retry`; a refusal is a non-retryable error. Every attempt,
//! refusals included, leaves one `script.execution` audit event.

use orbit_types::workflow::activity_job::{
    SCRIPT_REF_PREFIX, ScriptExecutionRecord, TargetRef, script_failure,
};

use super::*;
use crate::activity_job::script_stdout_output;
use crate::context::{
    ScriptStepEnd, ScriptStepOutputs, ScriptStepRan, ScriptStepRequest, ScriptStepRun,
};

/// Lines of the log tail a failed attempt's message carries.
const FAILURE_LOG_TAIL_LINES: usize = 20;
/// Bytes of that tail, at most.
const FAILURE_LOG_TAIL_BYTES: usize = 4 * 1024;

pub(super) fn run_script_target(
    step: &JobV2Step,
    target: &TargetRef,
    ctx: &ExecCtx<'_>,
) -> Result<StepOutcome, DispatchError> {
    let script = target
        .target
        .strip_prefix(SCRIPT_REF_PREFIX)
        .unwrap_or(&target.target)
        .to_string();
    let input = {
        let tctx = ctx.template_ctx();
        render_input(target.default_input.as_ref(), &ctx.input, &tctx, None)?
    };
    let request = ScriptStepRequest {
        run_id: ctx.run_id.clone(),
        step_id: step.id.clone(),
        script: script.clone(),
        input,
        workspace_path: ctx
            .input
            .get("workspace_path")
            .and_then(Value::as_str)
            .filter(|path| !path.trim().is_empty())
            .map(str::to_string),
        timeout_seconds: (target.timeout_seconds > 0).then_some(target.timeout_seconds),
    };
    match ctx.host.run_script_step(&request) {
        ScriptStepRun::Refused {
            code,
            message,
            mut record,
        } => {
            record.outcome = "refused".to_string();
            record.refusal_code = Some(code.to_string());
            record.message = Some(message.clone());
            emit_script_execution(ctx, &step.id, *record);
            Err(DispatchError::ScriptRefused {
                code,
                step_id: step.id.clone(),
                script,
                message,
            })
        }
        ScriptStepRun::Ran(ran) => settle_script_run(step, &script, *ran, ctx),
    }
}

fn settle_script_run(
    step: &JobV2Step,
    script: &str,
    ran: ScriptStepRan,
    ctx: &ExecCtx<'_>,
) -> Result<StepOutcome, DispatchError> {
    let ScriptStepRan {
        end,
        stdout,
        stdout_over_cap,
        output_schema,
        log,
        mut record,
        outputs,
    } = ran;
    record.log_blob_ref = ctx.audit.write_blob(&log);
    record.stdout_bytes = Some(stdout.len() as u64);
    let (exit_code, signal) = match end {
        ScriptStepEnd::Exited(code) => (Some(code), None),
        ScriptStepEnd::Signaled(signal) => (None, Some(signal)),
        ScriptStepEnd::TimedOut | ScriptStepEnd::ScratchCapExceeded => (None, None),
    };
    record.exit_code = exit_code;
    record.signal = signal;
    record.timed_out = end == ScriptStepEnd::TimedOut;
    record.outcome = match end {
        ScriptStepEnd::Exited(_) => "exited",
        ScriptStepEnd::Signaled(_) => "signaled",
        ScriptStepEnd::TimedOut => "timed_out",
        ScriptStepEnd::ScratchCapExceeded => "scratch_cap_exceeded",
    }
    .to_string();

    if end == ScriptStepEnd::Exited(0) {
        return match script_stdout_output(&stdout, stdout_over_cap, &output_schema) {
            Ok(output) => match outputs {
                ScriptStepOutputs::Rejected { code, message } => {
                    let message = format!("script `{script}` exited 0 but {message}");
                    record.failure_code = Some(code.to_string());
                    record.message = Some(message.clone());
                    emit_script_execution(ctx, &step.id, record);
                    Err(DispatchError::ScriptOutputFiles {
                        code,
                        step_id: step.id.clone(),
                        message,
                    })
                }
                ScriptStepOutputs::Kept(set) => {
                    record.output_set_id = Some(set.set_id.clone());
                    record.output_files = set.files.clone();
                    emit_script_execution(ctx, &step.id, record);
                    record_pipeline_with_files(ctx, &step.id, output.clone(), &set);
                    Ok(StepOutcome {
                        success: true,
                        output,
                        message: None,
                    })
                }
                ScriptStepOutputs::None => {
                    emit_script_execution(ctx, &step.id, record);
                    record_pipeline(ctx, &step.id, output.clone());
                    Ok(StepOutcome {
                        success: true,
                        output,
                        message: None,
                    })
                }
            },
            Err(reason) => {
                record.failure_code = Some(script_failure::OUTPUT_INVALID.to_string());
                record.message = Some(reason.clone());
                emit_script_execution(ctx, &step.id, record);
                Err(DispatchError::ScriptOutputInvalid {
                    step_id: step.id.clone(),
                    script: script.to_string(),
                    message: reason,
                })
            }
        };
    }

    let (failure_code, what) = match end {
        ScriptStepEnd::Exited(code) => (None, format!("exited {code}")),
        ScriptStepEnd::Signaled(signal) => (None, format!("was killed by signal {signal}")),
        ScriptStepEnd::TimedOut => (
            Some(script_failure::TIMEOUT),
            "outlived its timeout and was killed".to_string(),
        ),
        ScriptStepEnd::ScratchCapExceeded => (
            Some(script_failure::SCRATCH_CAP_EXCEEDED),
            "outgrew its scratch cap and was killed".to_string(),
        ),
    };
    let prefix = failure_code
        .map(|code| format!("{code}: "))
        .unwrap_or_default();
    let tail = log_tail(&log);
    let message = if tail.is_empty() {
        format!("{prefix}script `{script}` {what}")
    } else {
        format!("{prefix}script `{script}` {what}; log tail:\n{tail}")
    };
    record.failure_code = failure_code.map(str::to_string);
    record.message = Some(message.clone());
    emit_script_execution(ctx, &step.id, record);
    let output = serde_json::json!({
        "exit_code": exit_code,
        "signal": signal,
        "timed_out": end == ScriptStepEnd::TimedOut,
    });
    record_pipeline(ctx, &step.id, output.clone());
    Ok(StepOutcome {
        success: false,
        output,
        message: Some(message),
    })
}

/// The last lines of a failed attempt's log, bounded.
fn log_tail(log: &[u8]) -> String {
    let text = String::from_utf8_lossy(log);
    let lines: Vec<&str> = text.trim_end().lines().collect();
    let start = lines.len().saturating_sub(FAILURE_LOG_TAIL_LINES);
    let tail = lines[start..].join("\n");
    if tail.len() <= FAILURE_LOG_TAIL_BYTES {
        return tail;
    }
    let mut cut = tail.len() - FAILURE_LOG_TAIL_BYTES;
    while !tail.is_char_boundary(cut) {
        cut += 1;
    }
    tail[cut..].to_string()
}

fn emit_script_execution(ctx: &ExecCtx<'_>, step_id: &str, record: ScriptExecutionRecord) {
    emit_job_event_lossy(
        &ctx.audit,
        ctx.task_id(),
        V2AuditEventKind::ScriptExecution {
            step_id: step_id.to_string(),
            record: Box::new(record),
        },
    );
}
