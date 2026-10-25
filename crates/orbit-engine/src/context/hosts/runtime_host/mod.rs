//! The unified capability boundary between the engine and its runtime.

use orbit_common::OrbitError;
use orbit_store::contracts::JobRunStepParams;
use orbit_store::contracts::{InvocationQuery, InvocationRecord, KeptClaimCandidate};
use orbit_tools::{FsAuditLogger, ToolContext};
use orbit_types::identity::AgentModelPair;
use orbit_types::policy::Role;
use orbit_types::record::OrbitEvent;
use orbit_types::task::{
    ContextWideningStep, ExternalRef, Task, TaskArtifact, TaskComment, TaskHistoryEntry,
    TaskPriority, TaskStatus,
};
use orbit_types::telemetry::ProviderLimitObservation;
use orbit_types::workflow::{
    DeferralProbe, JobRun, JobRunStartOutcome, JobRunState, NamespaceProbe, PipelineState,
    TagInstruction,
};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

use crate::activity_job::{
    DispatchError, DispatchInvocationTrace, ResolvedCliExecutor, ResolvedSandbox,
    ResolvedShellExecutor,
};

use super::{
    ClaimExecutionContext, CompletionObservationAdmission, CrewConfig, FinalRecoveryAdmission,
    FinalRecoveryAdmissionRequest, FinalRecoveryApplication, FinalRecoveryApplied,
    HandoffLandingContext, HandoffLandingUpdate, PluginBrokerHandle, PluginBrokerRun,
    PrAutoFixAdmission, PrAutoFixRequest, PrConfig, RebaseRecoveryAttemptScope,
    ResolvedActivityTools, ReviewLandingRequest, ReviewReleaseRequest,
    ReviewReportCorrectionRequest, ReviewerInvocationRequest, ScratchGcReport, SteeringGateReading,
    SteeringRoundAdmission, StepRecoveryAdmission, StepRecoveryDecisionRead,
    StepRecoveryDecisionRequest, StepRecoveryDecisionSlot, TaskActivityUpdate,
    TaskAutomationUpdate, WorktreeGcTaskLookup,
};

mod dispatch;
mod environment;
mod lifecycle;
mod recovery;
mod review;

/// The single capability boundary between the job executor and its runtime.
///
/// Deterministic actions, task/run persistence, environment resolution, agent
/// dispatch, and audit/checkpoint hooks all cross this boundary exactly once.
///
/// Every method is required, so a new capability fails to compile until the
/// production host answers it. Test doubles implement
/// `test_support::NullRuntimeHost` instead, which answers each capability the
/// way a host without it does.
pub trait RuntimeHost: Send + Sync {
    lifecycle::lifecycle_methods!();
    review::review_methods!();
    environment::environment_methods!();
    dispatch::dispatch_methods!();
    recovery::recovery_methods!();
}
