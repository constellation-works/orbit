//! Condition triggers [ORB-19990]: the CEL environment for `task` and
//! `workspace` scopes, the load-time checker, and the pure evaluation behind
//! `orbit automation explain`. Nothing here fires.

mod ast;
mod check;
mod evaluate;
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
pub use schema::ConditionCarrier;
