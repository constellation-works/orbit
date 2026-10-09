//! v2 Job DAG executor — the Phase 3 runtime for `JobV2` assets.
//!
//! Interprets a `JobV2` step tree with first-class `parallel:`, `when:`,
//! `retry:`, `fan_out:/fan_in:`, and `loop:` constructs (design §4). This
//! module is purely additive; it shares only the boolean-expression
//! evaluator (`crate::condition::evaluate_bool_expr`) with v1.
//!
//! ## Concurrency
//! Parallel branches and fan-out workers run under `std::thread::scope`
//! (matching v1's DAG scheduler). No tokio, no async.
//!
//! ## Audit
//! Every construct emits §7 envelope events (`step.*`, `fanout.dispatched`,
//! `worker.state`, `fanin.joined`, `loop.iteration.{start,end}`,
//! `loop.did_not_converge`). The retry wrapper emits `step.retry` between
//! attempts and `step.denied` when a denial bypasses retry.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use orbit_common::process::jitter::JitterRng;
use orbit_types::workflow::activity_job::{
    ActivityV2Spec, AgentLoopSpec, BackoffStrategy, BranchOutcome, FanInSpec, FanOutBlock, JobV2,
    JobV2Step, JobV2StepBody, JoinMode, LoopBlock, ParallelBlock, RetrySpec, TargetStep,
    V2AuditEventKind,
};
use serde_json::Value;

use crate::condition::evaluate_bool_expr;
use crate::template::{self, TemplateContext};

use super::audit_writer::{V2AuditWriter, WriteError};
use super::crew::{apply_resolved_settings, inject_system_crew_input, resolve_crew_settings};
use super::dispatcher::{
    DispatchError, V2DispatchInput, dispatch_v2_activity_without_run_id_injection,
    dispatch_v2_target_activity,
};
use crate::context::RuntimeHost;

mod audit;
mod concurrency;
mod exec_ctx;
mod execute;
mod fan_out;
mod final_recovery;
mod loop_block;
mod parallel;
mod recovery;
mod recovery_commit;
mod recovery_evidence;
mod recovery_observation;
mod reviewer;
mod step;
mod target;
mod templating;
mod validate;

#[cfg(test)]
mod tests;

use self::audit::*;
use self::concurrency::*;
use self::exec_ctx::*;
use self::fan_out::*;
use self::final_recovery::*;
use self::loop_block::*;
use self::parallel::*;
use self::recovery::*;
use self::recovery_evidence::*;
use self::step::*;
use self::target::*;
use self::templating::*;

use self::execute::{fan_in_alias, panic_payload_message};

pub use self::execute::{execute_job_with_resume, resolve_job_catalog_refs_for_execution};
pub use self::validate::{validate_job, validate_job_deterministic_actions};

#[derive(Debug, Clone)]
pub struct JobOutcome {
    pub success: bool,
    /// A settled review awaiting external evidence, without a delivery failure.
    pub evidence_hold: Option<orbit_types::workflow::ReviewEvidenceHold>,
    /// [ORB-14617] A delivery push the forge kept refusing: the run holds at
    /// that step, without a delivery failure, for a later resume.
    pub forge_hold: Option<orbit_types::workflow::ForgeUnavailableHold>,
    pub pipeline: Value,
    pub message: Option<String>,
    /// [ORB-00414] Number of audit-write failures observed during the run.
    /// Non-zero means the audit trail is incomplete (see `degraded_audit`).
    pub audit_failures: u64,
    /// [ORB-00414] True when any audit write failed — retry/recovery/debugging
    /// consumers should treat the trail as incomplete.
    pub degraded_audit: bool,
    /// [ORB-10367] Number of telemetry-persistence failures (invocation
    /// traces) observed during the run. Never affects `success`.
    pub telemetry_failures: u64,
    /// [ORB-10367] True when any telemetry write failed — the run's
    /// invocation/token accounting is incomplete, but its work is not.
    pub degraded_telemetry: bool,
}

#[derive(Debug, Clone)]
struct ResolvedRecoveryActivity {
    name: String,
    spec: ActivityV2Spec,
}
