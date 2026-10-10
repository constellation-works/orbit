use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::tool_allowlist::ActivityToolPolicyMode;

/// Schema version for the §7 v2 audit envelope. Per §12 Q10 resolution,
/// versioning is PER EVENT TYPE — each variant of `V2AuditEventKind` can be
/// versioned independently. This constant is the envelope schema itself.
pub const AUDIT_ENVELOPE_SCHEMA_VERSION: u32 = 1;

pub const V2_EVENT_TYPE_FS_CALL_DENIED: &str = "fs.call.denied";
pub const V2_EVENT_TYPE_TOOL_DENIED: &str = "tool.denied";
pub const V2_EVENT_TYPE_STEP_DENIED: &str = "step.denied";
pub const V2_DENIAL_EVENT_TYPES: &[&str] = &[
    V2_EVENT_TYPE_FS_CALL_DENIED,
    V2_EVENT_TYPE_TOOL_DENIED,
    V2_EVENT_TYPE_STEP_DENIED,
];

/// Common envelope fields wrapping every v2 audit event (§7).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct V2AuditEnvelope {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub event_type: String,
    pub event_id: String,
    pub ts: DateTime<Utc>,
    pub run_id: String,
    pub agent_identity: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_event_id: Option<String>,
    /// Absolute filesystem path of the workspace that produced this event.
    /// Populated by CLI entry points so persisted v2 audit rows can be
    /// filtered by origin repo.
    /// Absent for smokes and stub hosts that don't carry a workspace identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_path: Option<String>,
}

/// §7 v2 audit event — the envelope plus a type-specific body.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct V2AuditEvent {
    #[serde(flatten)]
    pub envelope: V2AuditEnvelope,
    #[serde(flatten)]
    pub kind: V2AuditEventKind,
}

/// Event-type discriminator (§7). The v2 layer emits run.*, step.*,
/// activity.*, construct-level (parallel / fan_out / loop), and tool.denied
/// events. Loop-engine http.* and tool.call.* events continue to be emitted
/// by the loop engine and are referenced via `parent_event_id` from Activity
/// events.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "body_kind", rename_all = "snake_case")]
pub enum V2AuditEventKind {
    RunStarted {
        job_name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        retry_source_run_id: Option<String>,
    },
    RunFinished {
        outcome: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_message: Option<String>,
    },
    RunCancelled {
        actor: String,
        source: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        previous_state: String,
        final_state: String,
    },
    StepStarted {
        step_id: String,
    },
    StepFinished {
        step_id: String,
        outcome: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_message: Option<String>,
    },
    StepSkipped {
        step_id: String,
        reason: String,
    },
    StepRetry {
        step_id: String,
        attempt: u32,
        next_backoff_ms: u64,
    },
    StepRecoveryAttempted {
        step_id: String,
        recovery_activity: String,
        recovery_succeeded: bool,
        /// Absent on historical events and successful attempts.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failure_phase: Option<String>,
        /// Bounded and redacted independently of the original step error.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_message: Option<String>,
        /// Bounded, redacted output reported by the recovery dispatch.
        /// Absent on historical events or when dispatch never returned.
        /// Advisory diagnostics only: nothing reads it to admit a retry.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<serde_json::Value>,
        /// [ORB-14152] The host's reading of the decision file the activity
        /// wrote. Absent on historical events, unsuccessful dispatches, and
        /// recoveries that ran without a decision slot.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decision: Option<StepRecoveryDecisionRecord>,
    },
    /// The failed step's single re-attempt after recovery completed.
    StepPostRecoveryAttempt {
        step_id: String,
        recovery_activity: String,
        outcome: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_message: Option<String>,
        /// Bounded, redacted output of the re-attempted step, if it returned.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<serde_json::Value>,
    },
    /// [ORB-13907] The job-level final recovery hook ran, or was skipped, for
    /// a failed top-level step.
    FinalRecoveryAttempted {
        step_id: String,
        final_recovery_activity: String,
        /// `skipped`, `resume`, `settled`, or `escalated`.
        outcome: String,
        /// The decision acted on; absent when skipped.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decision: Option<String>,
        /// Why it was skipped, or what it settled or escalated; bounded and
        /// redacted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    StepDenied {
        step_id: String,
        reason: String,
    },
    StepJoin {
        step_id: String,
        mode: String,
        branch_outcomes: Vec<BranchOutcome>,
    },
    FanoutDispatched {
        step_id: String,
        worker_count: u32,
    },
    WorkerState {
        step_id: String,
        worker_index: u32,
        state: String,
    },
    FaninJoined {
        step_id: String,
        collected: u32,
        failed: u32,
    },
    LoopIterationStart {
        step_id: String,
        iteration: u32,
    },
    LoopIterationEnd {
        step_id: String,
        iteration: u32,
        broke: bool,
    },
    LoopDidNotConverge {
        step_id: String,
        max_iterations: u32,
    },
    ActivityStarted {
        activity_name: String,
        activity_type: String,
    },
    ActivityFinished {
        activity_name: String,
        outcome: String,
    },
    FsCallRequest {
        profile: String,
        op: String,
        path: String,
        allowed: bool,
        matched_rule: String,
    },
    FsCallResult {
        profile: String,
        op: String,
        path: String,
        allowed: bool,
        matched_rule: String,
    },
    FsCallDenied {
        profile: String,
        op: String,
        path: String,
        allowed: bool,
        matched_rule: String,
    },
    ToolDenied {
        tool_name: String,
        reason: String,
    },
    /// §6 harness-delegated allowlist advisory. Emitted once per CLI backend
    /// invocation when the declared `tools:` list is passed through to the
    /// provider harness (Orbit does not enforce it in CLI mode).
    ToolAllowlistHarnessDelegated {
        provider: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        /// Every task whose requirements contributed to this dispatch.
        #[serde(default)]
        task_ids: Vec<String>,
        /// Task-scoped exact names requested at admission.
        #[serde(default)]
        requested_tools: Vec<String>,
        /// Deduplicated activity baseline plus task requirements.
        #[serde(default)]
        effective_tools: Vec<String>,
        /// Compatibility projection of `effective_tools`.
        tools: Vec<String>,
        /// Policy mode the activity declared (`allow` or `deny`). Absent on
        /// records written before deny mode existed, which were all
        /// allowlist runs. [ORB-13315]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_policy: Option<ActivityToolPolicyMode>,
        /// A deny-mode activity's `tool_disallow_list`; absent in allow mode.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_disallow_list: Option<Vec<String>>,
        /// Required tools a non-implementer dropped because its disallow list
        /// covers them. Each entry is the admission note. Empty, and omitted
        /// from older records, when nothing was dropped. [ORB-15162]
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        omitted_requirement_notes: Vec<String>,
    },
    /// [ORB-11354] An operator-admitted provider subprocess is about to run
    /// **outside** the executor's filesystem sandbox.
    ///
    /// Emitted immediately before the invocation starts, and only for the one
    /// activity that may run this way, so the run trail names who authorized an
    /// unsandboxed process and against which checkout — rather than leaving the
    /// absence of a sandbox to be inferred from a missing field on
    /// `cli.invocation.started`.
    TrustedHostExecutionAdmitted {
        provider: String,
        activity_name: String,
        /// Attribution label of the operator recorded in the run's admission.
        authorized_by: String,
        /// How the authorization chokepoint resolved that operator.
        authorizer_provenance: String,
        /// Caller label forwarded by an SSH-originated session, absent for a
        /// local admission. Attribution only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caller_machine_id: Option<String>,
        /// RFC 3339 timestamp the admission was stamped.
        authorized_at: String,
        /// Canonical checkout the invocation was admitted against.
        workspace_path: String,
        /// Canonical working directory of the provider subprocess.
        cwd: String,
    },
    /// §7.6 — CLI backend subprocess starting. Emitted after redaction has been
    /// applied to `argv`; the stdin blob is already written and hashed by the
    /// time this event fires.
    CliInvocationStarted {
        provider: String,
        argv_redacted: Vec<String>,
        stdin_blob_ref: Option<String>,
        model: Option<String>,
        cwd: Option<String>,
        wall_clock_timeout_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sandbox_backend: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sandbox_trusted_wrapper: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sandbox_probe_outcome: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sandbox_write_enforcement: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sandbox_read_enforcement: Option<String>,
    },
    /// [ORB-10496] The CLI backend subprocess exists. Emitted once, immediately
    /// after spawn and before the wall-clock supervision loop, so the provider
    /// child is observable *while it runs* rather than only in retrospect.
    ///
    /// Pairs with `cli.invocation.finished` within the same step: an unpaired
    /// process event means the child had not exited when the trail was last
    /// written, and `pid` (guarded against PID reuse by `pid_start_time`) can be
    /// probed for liveness. This is the only channel that sees ship-pipeline
    /// `agent_implement` agents — the Worker-daemon run store behind
    /// `agent_run_list` does not observe them.
    CliInvocationProcess {
        provider: String,
        pid: u32,
        /// Versioned process-start identity token for `pid`, when it could be
        /// probed. Absent when the host cannot run the probe (non-Unix, or a
        /// sandbox that blocks `ps`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pid_start_time: Option<String>,
    },
    /// [ORB-13899] The provider child produced output since the previous
    /// observation. Emitted at most once per supervision interval while the
    /// child runs, and only when its stdout grew, so the event time is the
    /// run's last observed activity.
    ///
    /// Pairs with `cli.invocation.process` the same way
    /// `cli.invocation.finished` does. `latest_message` is the newest
    /// assistant message Orbit could read from the output tail — bounded and
    /// redacted — and absent when the tail carried none.
    CliInvocationActivity {
        provider: String,
        /// Bytes the child has written to stdout so far.
        observed_bytes: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        latest_message: Option<String>,
        /// Whether `latest_message` was cut to its bound.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        latest_message_truncated: bool,
    },
    /// A descendant of the provider child stayed stopped (state `T`) past the
    /// supervisor's threshold, so the supervisor sent it `SIGKILL` and the
    /// child's wait on it returns instead of holding the step until its wall
    /// clock ends. Never the child itself.
    ///
    /// Pairs with `cli.invocation.process` the same way
    /// `cli.invocation.finished` does. `ended` is false when the signal could
    /// not be delivered; `error` then says why. `command` is bounded and
    /// redacted, and `stopped_ms` counts from the first sample that saw the
    /// descendant stopped.
    CliInvocationStoppedDescendant {
        provider: String,
        pid: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pid_start_time: Option<String>,
        command: String,
        stopped_ms: u64,
        ended: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Admission waits observed for a provider invocation. Cumulative within
    /// the invocation; overlapping intervals earn deadline credit only once.
    CliInvocationBuildBudget {
        provider: String,
        count: u64,
        total_ms: u64,
        longest_ms: u64,
        queued_wall_ms: u64,
        deadline_extension_ms: u64,
    },
    /// §7.6 — CLI backend subprocess finished (either naturally or by
    /// wall-clock timeout). `timed_out == true` iff the subprocess was killed
    /// because it exceeded `wall_clock_timeout_ms`.
    CliInvocationFinished {
        provider: String,
        exit_code: Option<i32>,
        duration_ms: u64,
        stdout_blob_ref: Option<String>,
        stderr_blob_ref: Option<String>,
        harness_version: Option<String>,
        timed_out: bool,
    },
    /// [ORB-10367] A telemetry write failed (e.g. persisting an invocation
    /// trace). The run continues — telemetry is observability, not
    /// correctness — but the failure is recorded on the run so the gap in
    /// the telemetry is visible rather than silent.
    TelemetryPersistFailed {
        component: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step_id: Option<String>,
        error: String,
    },
}

impl V2AuditEventKind {
    pub fn event_type(&self) -> &'static str {
        match self {
            V2AuditEventKind::RunStarted { .. } => "run.started",
            V2AuditEventKind::RunFinished { .. } => "run.finished",
            V2AuditEventKind::RunCancelled { .. } => "run.cancelled",
            V2AuditEventKind::StepStarted { .. } => "step.started",
            V2AuditEventKind::StepFinished { .. } => "step.finished",
            V2AuditEventKind::StepSkipped { .. } => "step.skipped",
            V2AuditEventKind::StepRetry { .. } => "step.retry",
            V2AuditEventKind::StepRecoveryAttempted { .. } => "step.recovery_attempted",
            V2AuditEventKind::StepPostRecoveryAttempt { .. } => "step.post_recovery_attempt",
            V2AuditEventKind::FinalRecoveryAttempted { .. } => "job.final_recovery_attempted",
            V2AuditEventKind::StepDenied { .. } => V2_EVENT_TYPE_STEP_DENIED,
            V2AuditEventKind::StepJoin { .. } => "step.join",
            V2AuditEventKind::FanoutDispatched { .. } => "fanout.dispatched",
            V2AuditEventKind::WorkerState { .. } => "worker.state",
            V2AuditEventKind::FaninJoined { .. } => "fanin.joined",
            V2AuditEventKind::LoopIterationStart { .. } => "loop.iteration.start",
            V2AuditEventKind::LoopIterationEnd { .. } => "loop.iteration.end",
            V2AuditEventKind::LoopDidNotConverge { .. } => "loop.did_not_converge",
            V2AuditEventKind::ActivityStarted { .. } => "activity.started",
            V2AuditEventKind::ActivityFinished { .. } => "activity.finished",
            V2AuditEventKind::FsCallRequest { .. } => "fs.call.request",
            V2AuditEventKind::FsCallResult { .. } => "fs.call.result",
            V2AuditEventKind::FsCallDenied { .. } => V2_EVENT_TYPE_FS_CALL_DENIED,
            V2AuditEventKind::ToolDenied { .. } => V2_EVENT_TYPE_TOOL_DENIED,
            V2AuditEventKind::ToolAllowlistHarnessDelegated { .. } => {
                "tool_allowlist.harness_delegated"
            }
            V2AuditEventKind::TrustedHostExecutionAdmitted { .. } => {
                "trusted_host.execution_admitted"
            }
            V2AuditEventKind::CliInvocationStarted { .. } => "cli.invocation.started",
            V2AuditEventKind::CliInvocationProcess { .. } => "cli.invocation.process",
            V2AuditEventKind::CliInvocationActivity { .. } => "cli.invocation.activity",
            V2AuditEventKind::CliInvocationStoppedDescendant { .. } => {
                "cli.invocation.stopped_descendant"
            }
            V2AuditEventKind::CliInvocationFinished { .. } => "cli.invocation.finished",
            V2AuditEventKind::CliInvocationBuildBudget { .. } => "cli.invocation.build_budget",
            V2AuditEventKind::TelemetryPersistFailed { .. } => "telemetry.persist_failed",
        }
    }
}

/// How the executor read a recovery invocation's durable decision file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct StepRecoveryDecisionRecord {
    /// `verified`, `absent` (nothing written), `invalid` (present but not a
    /// decision for this invocation) or `unavailable` (could not be read).
    pub status: String,
    /// `retry` or `not_recovered`; present only when `verified`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    /// Whether the executor admitted its single post-recovery attempt.
    pub retry_admitted: bool,
    /// The decision's bounded, redacted reason, or why it was refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BranchOutcome {
    pub branch_id: String,
    pub outcome: String,
}
