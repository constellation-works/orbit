//! The one workspace scan behind every agent scorecard: each task with
//! activity in the last 90 days (envelope, history and comments), the
//! implementation job runs that name those tasks, the review gate's
//! settlement comments, the finding-verification verdicts on review-filed
//! findings, the typed `regression_from` links, how each task-pilot
//! assessment's warnings settled and, from the codebase scan, each pilot
//! assessment of a done task graded against its landed diff. The
//! scan keeps the records; [`super::attribution`] decides who each problem
//! counts against, and the roles only slice the result by window.

#[allow(unused_imports)]
use std::collections::{BTreeMap, BTreeSet, HashMap};

#[allow(unused_imports)]
use chrono::{DateTime, Duration, Utc};
#[allow(unused_imports)]
use orbit_common::OrbitError;
#[allow(unused_imports)]
use orbit_store::contracts::{InvocationQuery, JobRunQuery};
#[allow(unused_imports)]
use orbit_types::task::{
    GITHUB_PR_EXTERNAL_REF_SYSTEM, RegressionClassification, Task, TaskComment, TaskComplexity,
    TaskHistoryEntry, TaskOrigin, TaskRelationType, TaskStatus,
};
#[allow(unused_imports)]
use orbit_types::workflow::ReviewVerdict;
#[allow(unused_imports)]
use serde_json::Value;

#[allow(unused_imports)]
use super::attribution::{self, Charge, Credit};
#[allow(unused_imports)]
use super::candidates::Candidates;
#[allow(unused_imports)]
use super::records::{
    Cause, block_cause, blocked_pr, bounded, cites_criteria, is_operator_requeue, landed,
    landing_pr, restart_cause, run_spans, spec_blocker_kind, spec_documents,
};
#[allow(unused_imports)]
use super::roles::pilot_warnings::{Warnings, settle};
#[allow(unused_imports)]
use super::roles::pilots::{Graded, grade_all};
#[allow(unused_imports)]
use crate::OrbitRuntime;
#[allow(unused_imports)]
use crate::application::codebase::{
    PilotEvidence, PilotRuns, failed_claim_settlements_of, pilot_evidence,
};
#[allow(unused_imports)]
use crate::application::task::{
    FindingSeverity, FindingVerdict, is_review_finding, latest_verification,
};

mod attempts;
mod model;
mod review;
mod scan;
mod task;

#[allow(unused_imports)]
pub(crate) use attempts::*;
#[allow(unused_imports)]
pub(crate) use model::*;
pub use model::{AgentFacts, Problem};
#[allow(unused_imports)]
pub(crate) use review::*;
#[allow(unused_imports)]
pub(crate) use scan::*;
#[allow(unused_imports)]
pub(crate) use task::*;
