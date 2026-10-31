//! Condition triggers: the CEL environment for `task` and `workspace` scopes,
//! the load-time checker, the pure evaluation behind `orbit automation
//! explain`, the edge rules, and the protocol that fires each edge once
//! (`docs/design/condition-triggers/1_scope.md`); and the typed wait
//! environment, load-time checks and pass evaluation of a job's auto-task
//! steps [ORB-20157, ORB-20158].

mod ast;
mod check;
mod evaluate;
mod firing;
mod guard;
mod job_wait;
mod protocol;
mod schema;
mod typing;

pub use check::{
    CheckedCondition, CheckedExpression, CheckedGuard, ConditionCheckError, ConditionReads,
    MAX_COMPREHENSION_NESTING, MAX_CONDITION_NODES, MAX_CONDITION_SOURCE_BYTES, TaskPrefilter,
    check_condition, check_guard,
};
pub use evaluate::{
    BoardFacts, ClauseNode, ConditionEvaluator, ConditionFacts, JobWaitFacts, OperandValue,
    OriginFacts, SelfFacts, StepFacts, TaskFacts, TopLevelClause, Truth,
};
pub use firing::{
    ConditionObservation, PassOutcome, PassSettings, SubjectObservation, advance_pass,
    condition_epoch, continuity_limit, fire_identity, own_task_ids, self_facts,
};
pub use guard::{GUARD_FALSE, GUARD_UNKNOWN, GuardVerdict};
pub use job_wait::{
    CheckedAutoTaskStep, CheckedJobExpiry, CheckedJobWaits, MAX_AUTO_TASK_STEPS,
    MAX_JOB_EXPIRY_HOURS, OuterBound, READINESS_RESULT_CONTRACT, RESULT_CONTRACTS, check_job_waits,
};
pub use protocol::{
    ConditionAction, ConditionDiagnostic, ConditionEvaluation, ConditionHost, evaluate_condition,
};
pub use schema::{AUTO_TASK_STEP_STATUSES, ConditionCarrier};
