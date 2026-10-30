//! Condition triggers [ORB-19990]: the CEL environment for `task` and
//! `workspace` scopes, the load-time checker, and the pure evaluation behind
//! `orbit automation explain`; and the typed wait environment and load-time
//! checks of a job's auto-task steps [ORB-20157]. Nothing here fires.

mod ast;
mod check;
mod evaluate;
mod job_wait;
mod schema;
mod typing;

pub use check::{
    CheckedCondition, CheckedExpression, ConditionCheckError, ConditionReads,
    MAX_COMPREHENSION_NESTING, MAX_CONDITION_NODES, MAX_CONDITION_SOURCE_BYTES, TaskPrefilter,
    check_condition,
};
pub use evaluate::{
    BoardFacts, ClauseNode, ConditionEvaluator, ConditionFacts, OperandValue, OriginFacts,
    SelfFacts, TaskFacts, TopLevelClause, Truth,
};
pub use job_wait::{
    CheckedAutoTaskStep, CheckedJobExpiry, CheckedJobWaits, MAX_AUTO_TASK_STEPS,
    MAX_JOB_EXPIRY_HOURS, OuterBound, READINESS_RESULT_CONTRACT, RESULT_CONTRACTS, check_job_waits,
};
pub use schema::{AUTO_TASK_STEP_STATUSES, ConditionCarrier};
