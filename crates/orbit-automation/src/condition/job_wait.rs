//! Auto-task job steps [ORB-20157]: the load-time checks of a job's
//! `auto-task:` steps and their CEL fields
//! (`docs/design/activity-job/specs/auto-task-step.md` §2-§4). Phase 1
//! defines and explains these steps; nothing here evaluates a wait, and no
//! binary runs such a job yet.

use std::collections::{BTreeMap, BTreeSet};

use cel::common::ast::Expr;
use chrono::{DateTime, Duration, Utc};
use orbit_types::workflow::{JobScheduleState, JobV2, JobV2Step, JobV2StepBody};

use super::ast::{path_of, string_literal};
use super::check::{CheckedExpression, ConditionCheckError, ConditionReads, check_expression};
use super::schema::{Environment, WaitClause, WaitScope};

/// Most `auto-task:` steps one job may declare.
pub const MAX_AUTO_TASK_STEPS: usize = 32;
/// Longest outer bound `spec.expire_when` may set, in hours after admission.
pub const MAX_JOB_EXPIRY_HOURS: i64 = 7 * 24;
/// The typed result contract a release-readiness step requires. A job with a
/// step requiring it pins its source revision at admission.
pub const READINESS_RESULT_CONTRACT: &str = "release-readiness/v1";
/// Result contracts `require_result` may name.
pub const RESULT_CONTRACTS: &[&str] = &[READINESS_RESULT_CONTRACT];

const JOB_EXPIRY_CLAUSE: &str = "spec.expire_when";
const JOB_EXPIRY_SHAPES: &str = "`now >= job.started_at + duration('<duration>')` or \
     `now >= timestamp('<RFC3339>')`, at most 168h after admission";

/// A job's auto-task steps after every load-time check. Empty for a job
/// without them.
#[derive(Debug, Clone, Default)]
pub struct CheckedJobWaits {
    /// The outer bound; present exactly when the job has auto-task steps.
    pub expiry: Option<CheckedJobExpiry>,
    pub steps: Vec<CheckedAutoTaskStep>,
    /// Whether the job pins `job.source_revision` at admission, which a
    /// `release-readiness/v1` result requirement implies.
    pub pinned_revision: bool,
}

/// The checked `spec.expire_when`.
#[derive(Debug, Clone)]
pub struct CheckedJobExpiry {
    pub expression: CheckedExpression,
    pub bound: OuterBound,
}

/// The finite deadline `spec.expire_when` sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OuterBound {
    /// `now >= timestamp('<RFC3339>')`.
    At(DateTime<Utc>),
    /// `now >= job.started_at + duration('<duration>')`.
    AfterStart(Duration),
}

/// One checked `auto-task:` step.
#[derive(Debug, Clone)]
pub struct CheckedAutoTaskStep {
    pub id: String,
    /// The auto-task definition the step names.
    pub auto_task: String,
    /// The nearest enclosing `parallel` step, when this step is a branch.
    pub parallel: Option<String>,
    /// The previous sibling `self.prev` observes; `None` for a first step
    /// or a parallel branch.
    pub prev: Option<String>,
    /// The existing one-shot guard: false skips the step, which is never
    /// accepted evidence.
    pub when: Option<String>,
    pub wait_until: Option<CheckedExpression>,
    pub expire_when: Option<CheckedExpression>,
    pub reuse_when: Option<CheckedExpression>,
    pub require_result: Option<String>,
    /// What the step's expressions read.
    pub reads: ConditionReads,
}

/// Check `job`'s auto-task steps: the disabled state, the step count and
/// placement, the outer bound against `now`, each CEL field in its typed
/// scope, and every `steps['<id>']` reference. A job without auto-task steps
/// passes unless it declares `spec.expire_when`.
pub fn check_job_waits(
    job: &JobV2,
    now: DateTime<Utc>,
) -> Result<CheckedJobWaits, ConditionCheckError> {
    let auto_task_steps = job.auto_task_steps();
    if auto_task_steps.is_empty() {
        if job.expire_when.is_some() {
            return Err(refusal(
                JOB_EXPIRY_CLAUSE,
                "is admitted only on a job with a `target: auto-task:<name>` step",
            ));
        }
        return Ok(CheckedJobWaits::default());
    }
    if job.state == JobScheduleState::Enabled {
        return Err(refusal(
            "spec.state",
            "a job with auto-task steps must stay `disabled`: this Orbit defines and explains \
             auto-task steps but cannot run them yet",
        ));
    }
    if auto_task_steps.len() > MAX_AUTO_TASK_STEPS {
        return Err(refusal(
            "spec.steps",
            format!(
                "declares {} auto-task steps; a job admits at most {MAX_AUTO_TASK_STEPS}",
                auto_task_steps.len()
            ),
        ));
    }
    for step in &auto_task_steps {
        if let Some(contract) = step.require_result.as_deref()
            && !RESULT_CONTRACTS.contains(&contract)
        {
            return Err(refusal(
                &step_clause(&step.id, "require_result"),
                format!(
                    "unknown result contract `{contract}`; registered contracts are {}",
                    RESULT_CONTRACTS.join(", ")
                ),
            ));
        }
    }
    let pinned_revision = auto_task_steps
        .iter()
        .any(|step| step.require_result.as_deref() == Some(READINESS_RESULT_CONTRACT));
    let expiry = check_job_expiry(job.expire_when.as_deref(), now, pinned_revision)?;

    let sites = step_sites(job);
    let positions = sites
        .iter()
        .rev()
        .map(|site| (site.step.id.as_str(), site))
        .collect::<BTreeMap<_, _>>();
    let mut steps = Vec::new();
    for site in &sites {
        let Some(auto_task) = site.step.auto_task_name() else {
            continue;
        };
        if site.dynamic {
            return Err(refusal(
                &step_clause(&site.step.id, "target"),
                "an auto-task target inside a loop or fan_out is not admitted: its invocation \
                 identity is not keyed yet; move the step to a sequential or parallel list",
            ));
        }
        steps.push(check_step(site, auto_task, &positions, pinned_revision)?);
    }
    Ok(CheckedJobWaits {
        expiry: Some(expiry),
        steps,
        pinned_revision,
    })
}

fn check_step(
    site: &StepSite<'_>,
    auto_task: &str,
    positions: &BTreeMap<&str, &StepSite<'_>>,
    pinned_revision: bool,
) -> Result<CheckedAutoTaskStep, ConditionCheckError> {
    let step = site.step;
    let refused_steps = positions
        .iter()
        .filter(|(id, _)| !site.visible.contains(*id))
        .map(|(id, other)| ((*id).to_owned(), site.refusal(other)))
        .collect();
    let mut scope = WaitScope {
        clause: WaitClause::StepWait,
        prev: site.prev.map(str::to_owned),
        visible_steps: site.visible.iter().map(|id| (*id).to_owned()).collect(),
        refused_steps,
        pinned_revision,
        readiness_result: step.require_result.as_deref() == Some(READINESS_RESULT_CONTRACT),
    };
    let mut reads = ConditionReads::default();
    let mut check = |field: &str, source: Option<&String>, scope: &WaitScope| {
        source
            .map(|source| {
                check_expression(
                    source,
                    &step_clause(&step.id, field),
                    Environment::JobWait(scope),
                    &mut reads,
                )
            })
            .transpose()
    };
    let wait_until = check("wait_until", step.wait_until.as_ref(), &scope)?;
    let expire_when = check("expire_when", step.expire_when.as_ref(), &scope)?;
    scope.clause = WaitClause::StepReuse;
    let reuse_when = check("reuse_when", step.reuse_when.as_ref(), &scope)?;
    Ok(CheckedAutoTaskStep {
        id: step.id.clone(),
        auto_task: auto_task.to_owned(),
        parallel: site.parallel.map(str::to_owned),
        prev: site.prev.map(str::to_owned),
        when: step.when.clone(),
        wait_until,
        expire_when,
        reuse_when,
        require_result: step.require_result.clone(),
        reads,
    })
}

/// Check the outer bound: present, one of the two restricted shapes, and
/// finite within seven days of admission. An absolute deadline is measured
/// from `now`, which no later admission precedes.
fn check_job_expiry(
    source: Option<&str>,
    now: DateTime<Utc>,
    pinned_revision: bool,
) -> Result<CheckedJobExpiry, ConditionCheckError> {
    let source = source.ok_or_else(|| {
        refusal(
            JOB_EXPIRY_CLAUSE,
            format!("a job with auto-task steps needs a finite outer bound: {JOB_EXPIRY_SHAPES}"),
        )
    })?;
    let scope = WaitScope {
        clause: WaitClause::JobExpiry,
        prev: None,
        visible_steps: BTreeSet::new(),
        refused_steps: BTreeMap::new(),
        pinned_revision,
        readiness_result: false,
    };
    let expression = check_expression(
        source,
        JOB_EXPIRY_CLAUSE,
        Environment::JobWait(&scope),
        &mut ConditionReads::default(),
    )?;
    let at_start = |message: String| ConditionCheckError {
        clause: JOB_EXPIRY_CLAUSE.to_owned(),
        position: Some((1, 1)),
        message,
    };
    let bound = outer_bound(&expression).ok_or_else(|| {
        at_start(format!(
            "is not a finite outer bound; write {JOB_EXPIRY_SHAPES}"
        ))
    })?;
    let limit = Duration::hours(MAX_JOB_EXPIRY_HOURS);
    match bound {
        OuterBound::AfterStart(duration) if duration <= Duration::zero() => {
            return Err(at_start(
                "the duration after job.started_at must be positive".into(),
            ));
        }
        OuterBound::AfterStart(duration) if duration > limit => {
            return Err(at_start(format!(
                "the duration after job.started_at is {}h; the limit is {MAX_JOB_EXPIRY_HOURS}h",
                duration.num_hours()
            )));
        }
        OuterBound::At(deadline) if deadline > now + limit => {
            return Err(at_start(format!(
                "deadline {} is more than {MAX_JOB_EXPIRY_HOURS}h after now, so a job admitted \
                 now could wait past the limit",
                deadline.to_rfc3339()
            )));
        }
        _ => {}
    }
    Ok(CheckedJobExpiry { expression, bound })
}

/// The deadline of `now >= timestamp('<t>')` or
/// `now >= job.started_at + duration('<d>')`; `None` for any other shape.
fn outer_bound(expression: &CheckedExpression) -> Option<OuterBound> {
    let Expr::Call(call) = &expression.ast.expr else {
        return None;
    };
    let [now, deadline] = call.args.as_slice() else {
        return None;
    };
    if call.target.is_some() || call.func_name != "_>=_" || path_of(now).as_deref() != Some("now") {
        return None;
    }
    let Expr::Call(deadline_call) = &deadline.expr else {
        return None;
    };
    match (
        deadline_call.func_name.as_str(),
        deadline_call.args.as_slice(),
    ) {
        ("timestamp", [literal]) if string_literal(literal).is_some() => {
            match cel::Value::resolve(deadline, &cel::Context::default()).ok()? {
                cel::Value::Timestamp(at) => Some(OuterBound::At(at.with_timezone(&Utc))),
                _ => None,
            }
        }
        ("_+_", [start, duration]) if path_of(start).as_deref() == Some("job.started_at") => {
            let Expr::Call(duration_call) = &duration.expr else {
                return None;
            };
            let [literal] = duration_call.args.as_slice() else {
                return None;
            };
            if duration_call.func_name != "duration" || string_literal(literal).is_none() {
                return None;
            }
            match cel::Value::resolve(duration, &cel::Context::default()).ok()? {
                cel::Value::Duration(duration) => Some(OuterBound::AfterStart(duration)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// One step's place in the job's structure.
struct StepSite<'a> {
    step: &'a JobV2Step,
    /// File-order position.
    index: usize,
    prev: Option<&'a str>,
    parallel: Option<&'a str>,
    /// Inside a loop or fan_out body, where a step runs once per item.
    dynamic: bool,
    ancestors: Vec<&'a str>,
    /// Steps settled before this one starts: earlier siblings in each
    /// enclosing list and the branches of earlier joined parallel blocks.
    visible: BTreeSet<&'a str>,
    /// Steps in the parallel branches beside this one's.
    beside: BTreeSet<&'a str>,
}

impl StepSite<'_> {
    /// Why this step may not read `other`.
    fn refusal(&self, other: &StepSite<'_>) -> String {
        let id = other.step.id.as_str();
        if id == self.step.id {
            "a step cannot read its own observation; read self instead".into()
        } else if self.ancestors.contains(&id) {
            format!("`{id}` encloses this step and has not settled while it waits")
        } else if other.dynamic {
            format!("`{id}` runs inside a loop or fan_out, which has no single observation")
        } else if self.beside.contains(id) {
            format!(
                "`{id}` runs in a parallel branch beside this one; a branch reads only steps \
                 settled before its parallel block or earlier in its own branch"
            )
        } else if other.index > self.index {
            format!("`{id}` comes after this step; steps[] reads only earlier steps")
        } else {
            format!("`{id}` has not settled when this step waits")
        }
    }
}

#[derive(Clone, Default)]
struct WalkContext<'a> {
    visible: BTreeSet<&'a str>,
    ancestors: Vec<&'a str>,
    parallel: Option<&'a str>,
    dynamic: bool,
    beside: BTreeSet<&'a str>,
}

/// Every step of `job` in file order with its structural context.
fn step_sites(job: &JobV2) -> Vec<StepSite<'_>> {
    let mut sites = Vec::new();
    walk(&job.steps, WalkContext::default(), &mut sites);
    sites
}

fn walk<'a>(steps: &'a [JobV2Step], context: WalkContext<'a>, sites: &mut Vec<StepSite<'a>>) {
    let mut visible = context.visible.clone();
    for (position, step) in steps.iter().enumerate() {
        sites.push(StepSite {
            step,
            index: sites.len(),
            prev: position
                .checked_sub(1)
                .map(|previous| steps[previous].id.as_str()),
            parallel: context.parallel,
            dynamic: context.dynamic,
            ancestors: context.ancestors.clone(),
            visible: visible.clone(),
            beside: context.beside.clone(),
        });
        let mut ancestors = context.ancestors.clone();
        ancestors.push(&step.id);
        let nested = WalkContext {
            visible: visible.clone(),
            ancestors,
            ..context.clone()
        };
        match &step.body {
            JobV2StepBody::Parallel { parallel } => {
                for (branch_index, branch) in parallel.branches.iter().enumerate() {
                    let mut beside = context.beside.clone();
                    for (other_index, other) in parallel.branches.iter().enumerate() {
                        if other_index != branch_index {
                            collect_ids(other, &mut beside);
                        }
                    }
                    walk(
                        std::slice::from_ref(branch),
                        WalkContext {
                            parallel: Some(&step.id),
                            beside,
                            ..nested.clone()
                        },
                        sites,
                    );
                }
            }
            JobV2StepBody::FanOut { fan_out, .. } => walk(
                std::slice::from_ref(&*fan_out.worker),
                WalkContext {
                    dynamic: true,
                    ..nested
                },
                sites,
            ),
            JobV2StepBody::Loop { loop_ } => walk(
                &loop_.steps,
                WalkContext {
                    dynamic: true,
                    ..nested
                },
                sites,
            ),
            JobV2StepBody::TargetRef(_) | JobV2StepBody::Target(_) => {}
        }
        visible.insert(&step.id);
        collect_settled(step, &mut visible);
    }
}

/// `step` and every step nested in it.
fn collect_ids<'a>(step: &'a JobV2Step, ids: &mut BTreeSet<&'a str>) {
    ids.insert(&step.id);
    match &step.body {
        JobV2StepBody::Parallel { parallel } => {
            for branch in &parallel.branches {
                collect_ids(branch, ids);
            }
        }
        JobV2StepBody::FanOut { fan_out, .. } => collect_ids(&fan_out.worker, ids),
        JobV2StepBody::Loop { loop_ } => {
            for nested in &loop_.steps {
                collect_ids(nested, ids);
            }
        }
        JobV2StepBody::TargetRef(_) | JobV2StepBody::Target(_) => {}
    }
}

/// The steps a settled `step` exposes to later ones: the branches of a
/// joined parallel block, recursively. A loop or fan_out body has no single
/// observation per step, so it exposes only its container.
fn collect_settled<'a>(step: &'a JobV2Step, visible: &mut BTreeSet<&'a str>) {
    if let JobV2StepBody::Parallel { parallel } = &step.body {
        for branch in &parallel.branches {
            visible.insert(&branch.id);
            collect_settled(branch, visible);
        }
    }
}

fn step_clause(id: &str, field: &str) -> String {
    format!("spec.steps.{id}.{field}")
}

fn refusal(clause: &str, message: impl Into<String>) -> ConditionCheckError {
    ConditionCheckError {
        clause: clause.to_owned(),
        position: None,
        message: message.into(),
    }
}
