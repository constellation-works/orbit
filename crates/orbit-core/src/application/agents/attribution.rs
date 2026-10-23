//! Who each problem counts against. Every problem the scan found gets
//! exactly one owning role and the run (or orchestrator) charged, or a Not
//! scored reason; the roles and the attribution strip read these charges, so
//! the strip and the rankings always agree.
//!
//! A rework cause names its owner (spec, crew, or none for infrastructure
//! and unrecorded causes), and a regression's escape names the safeguard
//! that failed. A problem recorded on a task charges the implementation
//! attempt whose span contains it, so a task that moved between crews
//! charges each crew only for its own attempt. A `reject` the next run
//! proved wrong is the reviewer's false reject, not the implementer's
//! review reject.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, SecondsFormat, Utc};
use orbit_types::task::{RegressionEscape, RegressionNature};
use orbit_types::workflow::ReviewVerdict;
use serde_json::{Map, Value, json};

use super::facts::{Problem, Record, RegressionLink, Review, TaskFacts, filing_attempt};
use super::records::{Cause, complexity_weight, label};
use super::roles::pilots::{Graded, costly_charge_detail, wait_charge_detail};
use super::{AgentFacts, AgentRole, MODEL_UNKNOWN};
use crate::application::task::FindingVerdict;

/// What a problem counts against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Owner {
    /// One role: the row named by the charge's crew and model.
    Role(AgentRole),
    /// No agent is at fault, or nothing records which one; the reason is a
    /// wire key such as `infrastructure` or an unscored escape.
    NotScored(&'static str),
}

/// One problem and who it counts against.
#[derive(Debug, Clone)]
pub struct Charge {
    /// The task the problem counts against; for a regression, the culprit.
    pub task_id: String,
    pub problem: Problem,
    pub owner: Owner,
    /// The charged row's crew, or the orchestrator for orchestrators.
    /// `None` when the owning run recorded none, and for Not scored.
    pub crew: Option<String>,
    /// The charged run's model; `None` when it recorded none.
    pub model: Option<String>,
    /// The charged job run, when one is known.
    pub run_id: Option<String>,
    /// When the problem was recorded.
    pub at: DateTime<Utc>,
    /// The time of the owner's unit of work, which decides the window the
    /// problem falls in: the task's creation for orchestrators, the charged
    /// attempt's start for implementers, the settlement for reviewers, and
    /// the problem itself when Not scored.
    pub anchor: DateTime<Utc>,
    pub detail: String,
    /// The rework cause, for rework.
    pub(super) cause: Option<Cause>,
}

/// Landed credit for the implementation run whose candidate landed.
#[derive(Debug, Clone)]
pub struct Credit {
    pub task_id: String,
    pub crew: Option<String>,
    pub model: Option<String>,
    pub run_id: Option<String>,
    /// The landed task's complexity weight.
    pub weight: i64,
    /// When the task was done.
    pub at: Option<DateTime<Utc>>,
    /// The final implementation attempt's start.
    pub anchor: DateTime<Utc>,
}

/// Every problem in `tasks`, every regression against them and every pilot
/// problem of their graded assessments, each with one owner, and the landed
/// credit of each task's final run.
pub(super) fn attribute(
    tasks: &[TaskFacts],
    regressions: &[RegressionLink],
    pilots: &[Graded],
) -> (Vec<Charge>, Vec<Credit>) {
    let mut charges = Vec::new();
    let mut credits = Vec::new();
    for task in tasks {
        for record in &task.records {
            charges.push(record_charge(task, record));
        }
        for review in &task.reviews {
            charges.extend(review_charge(task, review));
            for finding in review.disposed() {
                let charge = Draft {
                    task,
                    problem: Problem::DisposedFinding,
                    at: review.at,
                    detail: format!("review attempt {}: {}", review.attempt_id, finding.line),
                    cause: None,
                };
                charges.push(charge.reviewer(Some(review)));
            }
        }
        if task.landed
            && let Some(attempt) = task
                .final_attempt
                .and_then(|index| task.attempts.get(index))
        {
            credits.push(Credit {
                task_id: task.id.clone(),
                crew: attempt.runner.crew.clone(),
                model: attempt.runner.model.clone(),
                run_id: attempt.runner.run_id.clone(),
                weight: complexity_weight(task.complexity),
                at: task.landed_at,
                anchor: attempt.start,
            });
        }
    }

    let by_id: HashMap<&str, &TaskFacts> =
        tasks.iter().map(|task| (task.id.as_str(), task)).collect();
    for link in regressions {
        // A culprit outside the scan has no work in any window.
        let Some(task) = by_id.get(link.culprit.as_str()) else {
            continue;
        };
        charges.push(regression_charge(task, link));
    }
    for task in tasks {
        let Some((FindingVerdict::Rejected, at, _)) =
            task.filed.as_ref().and_then(|filed| filed.verdict)
        else {
            continue;
        };
        let draft = Draft {
            task,
            problem: Problem::RejectedFinding,
            at,
            detail: "finding verification REJECTED this review-filed finding".to_string(),
            cause: None,
        };
        charges.push(draft.review_job(filing_attempt(&by_id, task)));
    }
    for graded in pilots {
        charges.extend(pilot_charges(graded));
    }
    charges.sort_by(|left, right| {
        left.at
            .cmp(&right.at)
            .then(left.task_id.cmp(&right.task_id))
    });
    (charges, credits)
}

/// A problem on `task` before its owner is decided.
struct Draft<'a> {
    task: &'a TaskFacts,
    problem: Problem,
    at: DateTime<Utc>,
    detail: String,
    cause: Option<Cause>,
}

impl Draft<'_> {
    fn charge(
        self,
        owner: Owner,
        crew: Option<String>,
        model: Option<String>,
        run_id: Option<String>,
        anchor: DateTime<Utc>,
    ) -> Charge {
        Charge {
            task_id: self.task.id.clone(),
            problem: self.problem,
            owner,
            crew,
            model,
            run_id,
            at: self.at,
            anchor,
            detail: self.detail,
            cause: self.cause,
        }
    }

    fn not_scored(self, reason: &'static str) -> Charge {
        let at = self.at;
        self.charge(Owner::NotScored(reason), None, None, None, at)
    }

    /// The task's orchestrator attribution and its recorded model, or Not
    /// scored when it names no orchestrator.
    fn orchestrator(self) -> Charge {
        match self.task.orchestrator.clone() {
            Some(name) => {
                let anchor = self.task.created_at;
                let model = self.task.orchestrator_model.clone();
                self.charge(
                    Owner::Role(AgentRole::Orchestrators),
                    Some(name),
                    model,
                    None,
                    anchor,
                )
            }
            None => self.not_scored("unattributed"),
        }
    }

    /// The implementation attempt at `index`, or Not scored when the task
    /// has no attempt to charge.
    fn implementer(self, index: Option<usize>) -> Charge {
        let Some(attempt) = index.and_then(|index| self.task.attempts.get(index)) else {
            return self.not_scored("no_run");
        };
        let runner = attempt.runner.clone();
        let anchor = attempt.start;
        self.charge(
            Owner::Role(AgentRole::Implementers),
            runner.crew,
            runner.model,
            runner.run_id,
            anchor,
        )
    }

    /// The review job attempt that filed the finding, or Not scored when no
    /// review job is recorded as its filer.
    fn review_job(self, filer: Option<(&TaskFacts, usize)>) -> Charge {
        let Some(attempt) = filer.and_then(|(task, index)| task.attempts.get(index)) else {
            return self.not_scored("no_review");
        };
        let runner = attempt.runner.clone();
        let anchor = attempt.start;
        self.charge(
            Owner::Role(AgentRole::Reviewers),
            runner.crew,
            runner.model,
            runner.run_id,
            anchor,
        )
    }

    /// The reviewer of `review`, or Not scored when no review settled.
    fn reviewer(self, review: Option<&Review>) -> Charge {
        let Some(review) = review else {
            return self.not_scored("no_review");
        };
        let run_id = self
            .task
            .attempt_at(review.at)
            .and_then(|index| self.task.attempts[index].runner.run_id.clone());
        self.charge(
            Owner::Role(AgentRole::Reviewers),
            review.reviewer.crew.clone(),
            review.reviewer.model.clone(),
            run_id,
            review.at,
        )
    }
}

/// The owner of a problem the task's history or comments record.
fn record_charge(task: &TaskFacts, record: &Record) -> Charge {
    let draft = Draft {
        task,
        problem: record.problem,
        at: record.at,
        detail: record.detail.clone(),
        cause: record.cause,
    };
    match record.problem {
        Problem::SpecEdit | Problem::CriteriaBlock | Problem::CriteriaEscalation => {
            draft.orchestrator()
        }
        Problem::RepeatRun
        | Problem::BlockedPr
        | Problem::UnmergedPr
        | Problem::OperatorRequeue => match record.cause {
            Some(Cause::Spec) => draft.orchestrator(),
            Some(Cause::Crew) => {
                let attempt = record.ended.or_else(|| task.attempt_at(record.at));
                draft.implementer(attempt)
            }
            Some(Cause::Infrastructure) => draft.not_scored("infrastructure"),
            Some(Cause::Other) | None => draft.not_scored("no_cause"),
        },
        Problem::FailedFix => {
            let review = task
                .reviews
                .iter()
                .rev()
                .find(|review| review.at <= record.at);
            draft.reviewer(review)
        }
        // Read from settlements, verdicts, relations and pilot grading, never
        // from records.
        Problem::ReviewReject
        | Problem::ReviewFixes
        | Problem::Regression
        | Problem::DisposedFinding
        | Problem::FalseReject
        | Problem::RejectedFinding
        | Problem::IncompleteReview
        | Problem::CostlyMiss
        | Problem::LockWait => draft.not_scored("no_cause"),
    }
}

/// The problem a settled verdict records, if any: a `reject` or
/// `accept_with_fixes` against the implementer whose candidate was reviewed,
/// a false reject against the reviewer instead, and an `incomplete` against
/// the reviewer unless infrastructure caused it.
fn review_charge(task: &TaskFacts, review: &Review) -> Option<Charge> {
    let problem = match review.verdict {
        ReviewVerdict::Reject if review.false_reject() => Problem::FalseReject,
        ReviewVerdict::Reject => Problem::ReviewReject,
        ReviewVerdict::AcceptWithFixes => Problem::ReviewFixes,
        ReviewVerdict::Incomplete => Problem::IncompleteReview,
        ReviewVerdict::Accept => return None,
    };
    let draft = Draft {
        task,
        problem,
        at: review.at,
        detail: format!(
            "review attempt {} settled {}",
            review.attempt_id,
            review.verdict.as_str()
        ),
        cause: None,
    };
    Some(match problem {
        Problem::FalseReject => Draft {
            detail: format!(
                "review attempt {} settled reject; the next run changed none of the cited paths and the next review accepted",
                review.attempt_id
            ),
            ..draft
        }
        .reviewer(Some(review)),
        Problem::IncompleteReview if review.infrastructure => draft.not_scored("infrastructure"),
        Problem::IncompleteReview => draft.reviewer(Some(review)),
        _ => draft.implementer(task.attempt_at(review.at)),
    })
}

/// The owner of a regression: its escape names the safeguard that failed.
fn regression_charge(task: &TaskFacts, link: &RegressionLink) -> Charge {
    let classification = link.classification;
    let draft = Draft {
        task,
        problem: Problem::Regression,
        at: link.at,
        detail: format!(
            "{} regressed it (nature {}, escape {})",
            link.regressing,
            label(&classification.nature),
            label(&classification.escape),
        ),
        cause: None,
    };
    match classification.nature {
        RegressionNature::MergeReintroduced => return draft.not_scored("merge_reintroduced"),
        RegressionNature::ContractDrift => return draft.not_scored("contract_drift"),
        RegressionNature::BrokeExisting
        | RegressionNature::DefectInNew
        | RegressionNature::Incomplete
        | RegressionNature::Unclassified => {}
    }
    match classification.escape {
        RegressionEscape::SpecAmbiguous => draft.orchestrator(),
        RegressionEscape::ReviewMissed => {
            let accepted = task
                .reviews
                .iter()
                .rev()
                .find(|review| review.verdict.passed());
            draft.reviewer(accepted)
        }
        RegressionEscape::NoTest => draft.implementer(task.final_attempt),
        RegressionEscape::TestNotSelected => draft.not_scored("test_not_selected"),
        RegressionEscape::PlatformOnly => draft.not_scored("platform_only"),
        RegressionEscape::Nondeterministic => draft.not_scored("nondeterministic"),
        RegressionEscape::Unclassified => draft.not_scored("unclassified"),
    }
}

/// An assessment's costly misses and the lock waits it caused, charged to
/// the pilot run behind it in the window of the assessment.
fn pilot_charges(graded: &Graded) -> impl Iterator<Item = Charge> + '_ {
    let charge = move |problem, at, detail| {
        let run = graded.run.as_ref();
        Charge {
            task_id: graded.task_id.clone(),
            problem,
            owner: Owner::Role(AgentRole::Pilots),
            crew: run.and_then(|run| run.crew.clone()),
            model: run.and_then(|run| run.model.clone()),
            run_id: graded.run_id(),
            at,
            anchor: graded.at,
            detail,
            cause: None,
        }
    };
    let misses = graded.costly.iter().map(move |(path, contest)| {
        charge(
            Problem::CostlyMiss,
            contest.at,
            costly_charge_detail(path, contest),
        )
    });
    let waits = graded
        .waits
        .iter()
        .map(move |wait| charge(Problem::LockWait, wait.started_at, wait_charge_detail(wait)));
    misses.chain(waits)
}

/// The window's problems by owner, with Not scored by reason and each
/// role's problems by row.
pub(super) fn strip(facts: &AgentFacts, since: DateTime<Utc>) -> Value {
    let mut owners: BTreeMap<AgentRole, u64> =
        AgentRole::ALL.iter().map(|role| (*role, 0)).collect();
    let mut rows: BTreeMap<AgentRole, BTreeMap<(String, String), u64>> = BTreeMap::new();
    let mut reasons: BTreeMap<&str, u64> = BTreeMap::new();
    let mut total = 0;
    for charge in facts.charges.iter().filter(|charge| charge.anchor >= since) {
        total += 1;
        match charge.owner {
            Owner::Role(role) => {
                *owners.entry(role).or_default() += 1;
                *rows
                    .entry(role)
                    .or_default()
                    .entry(row_key(charge.crew.as_deref(), charge.model.as_deref()))
                    .or_default() += 1;
            }
            Owner::NotScored(reason) => *reasons.entry(reason).or_default() += 1,
        }
    }
    let mut owner_counts: Map<String, Value> = owners
        .iter()
        .map(|(role, count)| (role.as_str().to_string(), json!(count)))
        .collect();
    owner_counts.insert(
        "not_scored".into(),
        json!({
            "count": reasons.values().sum::<u64>(),
            "reasons": reasons,
        }),
    );
    let charged: Map<String, Value> = AgentRole::ALL
        .iter()
        .map(|role| {
            let mut list: Vec<_> = rows.remove(role).unwrap_or_default().into_iter().collect();
            list.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));
            (
                role.as_str().to_string(),
                list.into_iter()
                    .map(|((crew, model), problems)| {
                        json!({"crew": crew, "model": model, "problems": problems})
                    })
                    .collect(),
            )
        })
        .collect();
    json!({
        "since": since.to_rfc3339_opts(SecondsFormat::Secs, true),
        "total": total,
        "owners": owner_counts,
        "charged": charged,
    })
}

/// The row a charge or credit counts in: its crew and model, `crew unknown`
/// and `model unknown` when the run (or, for an orchestrator, the task)
/// recorded none.
pub(super) fn row_key(crew: Option<&str>, model: Option<&str>) -> (String, String) {
    (
        crew.unwrap_or(super::CREW_UNKNOWN).to_string(),
        model.unwrap_or(MODEL_UNKNOWN).to_string(),
    )
}
