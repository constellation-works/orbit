use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::time::Duration;

use orbit_common::process::build_budget::{BuildBudgetWaits, read_waits};
use orbit_common::process::identity::process_start_identity_token;
use orbit_common::process::stopped_descendants::{StoppedDescendant, stopped_descendant_threshold};
use orbit_common::security::redaction::PatternRedactor;
use orbit_types::workflow::activity_job::V2AuditEventKind;
use serde_json::Value;

use crate::activity_job::cli_runner::plugin_broker::RunPluginBroker;
use crate::activity_job::cli_runner::supervisor::CapturedOutput;
use crate::activity_job::dispatcher::{ResolvedCliExecutor, ResolvedSandbox};
use crate::activity_job::workspace::WorktreeBoundaryGuard;

use super::super::super::super::dispatcher::DispatchError;
use super::super::super::envelope::task_id_from_input;
use super::super::super::spawn::SpawnError;
use super::super::super::stdout_preview::{
    PROGRESS_MESSAGE_LIMIT_BYTES, bounded_assistant_message,
};
use super::super::super::supervisor::{
    OutputProgress, ProgressReporter, SpawnTraceContext, SpawnWithTimeoutRequest,
    StoppedDescendantReporter, spawn_for_supervision, spawn_with_timeout,
};
use super::super::antigravity_usage::AntigravityUsageRequest;
use super::super::environment::codex_home;
use super::super::evidence::emit_build_waits;
use super::super::grok_usage::GrokUsageRequest;
use super::super::limit::UsageWindowContext;
use super::CliDispatchContext;

/// How often a running provider's stdout is sampled for progress. Bounds the
/// staleness of `last_activity_at` and the progress rows one invocation writes.
const PROVIDER_PROGRESS_INTERVAL: Duration = Duration::from_secs(10);

pub(super) struct CliProviderInvocation<'a> {
    pub(super) context: CliDispatchContext<'a>,
    pub(super) provider: &'a String,
    pub(super) resolved_program: &'a str,
    pub(super) subprocess_args: &'a [String],
    pub(super) child_env: &'a [(String, String)],
    pub(super) subprocess_cwd: &'a Option<std::path::PathBuf>,
    pub(super) subprocess_cwd_string: &'a Option<String>,
    pub(super) sandbox: Option<&'a ResolvedSandbox>,
    pub(super) stdin_bytes: &'a [u8],
    pub(super) wall_clock_timeout: Duration,
    pub(super) timeout_seconds: u64,
    pub(super) stall_window: Option<Duration>,
    pub(super) build_wait_directory: &'a tempfile::TempDir,
    pub(super) redaction: &'a PatternRedactor,
    pub(super) cli_executor: &'a ResolvedCliExecutor,
    pub(super) plugin_broker: RunPluginBroker,
    pub(super) worktree_boundary: &'a mut Option<WorktreeBoundaryGuard>,
    pub(super) task_ids: &'a [String],
}

pub(super) struct ProviderProcessExit {
    pub(super) stdout: CapturedOutput,
    pub(super) stderr: CapturedOutput,
    pub(super) exit_code: Option<i32>,
    pub(super) duration: Duration,
    pub(super) timed_out: bool,
    pub(super) stalled: bool,
    pub(super) usage_context: UsageWindowContext,
    pub(super) linux_post_run_guard: Option<orbit_exec::LinuxBwrapPostRunGuard>,
}

pub(super) fn invoke_cli_provider(
    invocation: CliProviderInvocation<'_>,
) -> Result<ProviderProcessExit, DispatchError> {
    let CliProviderInvocation {
        context,
        provider,
        resolved_program,
        subprocess_args,
        child_env,
        subprocess_cwd,
        subprocess_cwd_string,
        sandbox,
        stdin_bytes,
        wall_clock_timeout,
        timeout_seconds,
        stall_window,
        build_wait_directory,
        redaction,
        cli_executor,
        plugin_broker,
        worktree_boundary,
        task_ids,
    } = invocation;
    let CliDispatchContext {
        host,
        run_id,
        input,
        audit,
        ..
    } = context;
    // [ORB-10496] Record the provider child's PID the moment it exists. Emitted
    // through the same writer, so it is persisted (and therefore readable by
    // `orbit run show` / the run-status API) while the invocation is still
    // running — the only in-flight signal for a ship-pipeline agent step.
    let pid_audit = Arc::clone(audit);
    let pid_provider = provider.clone();
    let on_spawn = move |pid: u32| {
        pid_audit.emit_lossy(V2AuditEventKind::CliInvocationProcess {
            provider: pid_provider.clone(),
            pid,
            pid_start_time: process_start_identity_token(pid),
        });
    };

    // [ORB-13899] Live progress: the newest assistant message and, through
    // the event time, the last moment the child produced output. Silence
    // emits nothing, so a quiet child adds no rows; build-budget waits are
    // recorded only when they change.
    let progress_audit = Arc::clone(audit);
    let progress_provider = provider.clone();
    let reported_bytes = Cell::new(0usize);
    let reported_waits = RefCell::new(BuildBudgetWaits::default());
    let report_progress = |progress: &OutputProgress| {
        let waits = read_waits(
            build_wait_directory.path(),
            timeout_seconds.saturating_mul(1000),
        );
        if waits.count > 0 && *reported_waits.borrow() != waits {
            emit_build_waits(&progress_audit, &progress_provider, &waits);
            *reported_waits.borrow_mut() = waits;
        }
        if progress.observed_bytes == reported_bytes.replace(progress.observed_bytes) {
            return;
        }
        let message = bounded_assistant_message(
            &progress_provider,
            &progress.recent,
            redaction,
            PROGRESS_MESSAGE_LIMIT_BYTES,
        );
        progress_audit.emit_lossy(V2AuditEventKind::CliInvocationActivity {
            provider: progress_provider.clone(),
            observed_bytes: progress.observed_bytes as u64,
            latest_message_truncated: message.as_ref().is_some_and(|message| message.truncated),
            latest_message: message.map(|message| message.text),
        });
    };

    // A descendant the supervisor ended because it stayed stopped: the
    // operator's record of what was killed and why the agent moved on.
    let stopped_audit = Arc::clone(audit);
    let stopped_provider = provider.clone();
    let report_stopped = |stopped: &StoppedDescendant| {
        tracing::warn!(
            target: "orbit.engine.cli_runner",
            provider = %stopped_provider,
            job_run_id = %run_id,
            pid = stopped.pid,
            ended = stopped.ended(),
            "{}",
            stopped.describe()
        );
        stopped_audit.emit_lossy(V2AuditEventKind::CliInvocationStoppedDescendant {
            provider: stopped_provider.clone(),
            pid: stopped.pid,
            pid_start_time: stopped.pid_start_time.clone(),
            command: stopped.command.clone(),
            stopped_ms: u64::try_from(stopped.stopped_for.as_millis()).unwrap_or(u64::MAX),
            ended: stopped.ended(),
            error: stopped.end_error.clone(),
        });
    };

    let usage_context = UsageWindowContext {
        antigravity: (provider == "antigravity").then(|| AntigravityUsageRequest {
            program: resolved_program.to_string(),
            args: cli_executor.args.clone(),
            environment: child_env.to_vec(),
            cwd: subprocess_cwd.clone(),
        }),
        codex_home: (provider == "codex")
            .then(|| codex_home(child_env))
            .flatten(),
        grok: (provider == "grok")
            .then(|| GrokUsageRequest::from_env(child_env))
            .flatten(),
    };
    // A managed Linux Bubblewrap launch snapshots the write-policy gaps its
    // mounts cannot cover while compiling those mounts; take that snapshot off
    // the spawned child rather than walking the worktree a second time.
    let mut linux_post_run_guard = None;
    let spawn_result = spawn_for_supervision(
        resolved_program,
        subprocess_args,
        child_env,
        subprocess_cwd.as_deref(),
        sandbox,
        provider,
    )
    .and_then(|mut spawned| {
        let registered = host
            .register_worker_process(spawned.child.pid())
            .and_then(|()| {
                if sandbox.is_some_and(|sandbox| {
                    sandbox.kind == orbit_types::workflow::ExecutorSandboxKind::LinuxBwrap
                }) {
                    host.register_worker_pid_namespace(spawned.child.pid())
                } else {
                    Ok(())
                }
            });
        if let Err(error) = registered {
            return Err(SpawnError::permanent(error.to_string()));
        }
        plugin_broker.bind_sandbox(spawned.child.pid());
        linux_post_run_guard = spawned.take_linux_post_run_guard();
        spawn_with_timeout(SpawnWithTimeoutRequest {
            program: resolved_program,
            args: subprocess_args,
            stdin_bytes,
            env: child_env,
            cwd: subprocess_cwd.as_deref(),
            timeout: wall_clock_timeout,
            sandbox,
            trace: SpawnTraceContext {
                provider,
                job_run_id: run_id,
                task_id: task_id_from_input(input),
                cwd: subprocess_cwd_string.as_deref(),
            },
            output_capture_limit: None,
            on_spawn: Some(&on_spawn),
            on_progress: Some(ProgressReporter {
                interval: PROVIDER_PROGRESS_INTERVAL,
                report: &report_progress,
            }),
            stopped_descendants: Some(StoppedDescendantReporter {
                threshold: stopped_descendant_threshold(),
                report: &report_stopped,
            }),
            spawned_child: Some(spawned),
            stall_window,
        })
    });
    // The provider has exited, timed out, or failed to start: nothing in its
    // sandbox may reach the broker any longer.
    drop(plugin_broker);

    let (stdout, stderr, exit_code, duration, timed_out, stalled) = match spawn_result {
        Ok(result) => result,
        Err(err) => {
            if let Some(boundary) = worktree_boundary.take() {
                boundary.verify_after_provider(
                    host,
                    false,
                    input.get("failed_step_id").and_then(Value::as_str),
                    task_ids,
                )?;
            }
            // Spawn-layer classification (ORB-10006): executable missing /
            // permission denied fail fast; resource exhaustion (EAGAIN,
            // ENOMEM, ...) and other transient host failures stay retryable
            // at the step layer.
            return Err(if err.permanent {
                DispatchError::CliInvocationPermanent(err.message)
            } else {
                DispatchError::CliInvocationFailed(err.message)
            });
        }
    };

    Ok(ProviderProcessExit {
        stdout,
        stderr,
        exit_code,
        duration,
        timed_out,
        stalled,
        usage_context,
        linux_post_run_guard,
    })
}
