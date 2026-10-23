//! The one workspace scan behind every agent scorecard: each task with
//! activity in the last 90 days (envelope, history and comments), the
//! implementation job runs that name those tasks, the review gate's
//! settlement comments, the finding-verification verdicts on review-filed
//! findings, the typed `regression_from` links and, from the codebase scan,
//! each pilot assessment of a done task graded against its landed diff. The
//! scan keeps the records; [`super::attribution`] decides who each problem
//! counts against, and the roles only slice the result by window.

mod attempts;
mod model;
mod review;
mod scan;
mod task;

pub use model::{AgentFacts, Problem};
pub(super) use model::{
    Attempt, Finding, FindingState, Record, RegressionLink, RejectOutcome, Review, TaskFacts,
    filing_attempt,
};
pub(super) use scan::{SCAN_DAYS, scan};
