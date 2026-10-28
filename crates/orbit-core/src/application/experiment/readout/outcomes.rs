//! Each enrolled task's outcomes, read from records Orbit already keeps:
//! invocation accounting (tokens, cost, implement time), task history and
//! review gate comments, the run audit trail (recoveries, conflicts, applied
//! overlay digests), landed diff stats and `regression_from` links.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_runtime::cost::IMPLEMENT_INVOCATION_IDS;
use orbit_store::V2AuditEventFilter;
use orbit_store::contracts::{ExperimentAssignment, InvocationQuery, JobRunQuery};
use orbit_types::task::{Task, TaskHistoryEntry, TaskRelationType, TaskStatus, complexity_bucket};
use orbit_types::workflow::{
    PR_AUTO_FIX_ADMITTED_EVENT, PrAutoFixAttempt, PrAutoFixBlocker, ReviewVerdict, V2AuditEvent,
    V2AuditEventKind,
};
use serde::Deserialize;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::agents::review_gate_verdicts;

/// How long after landing a `regression_from` filing counts as an escape.
pub(super) const REGRESSION_WINDOW_DAYS: i64 = 14;
/// How far before the experiment's creation the accounting reads start, for
/// clock skew between the hosts that stamp records.
const READ_SLACK: Duration = Duration::days(1);
/// Invocation records read per implement activity.
const INVOCATION_LIMIT: usize = 100_000;
/// Audit rows read per page.
const AUDIT_PAGE: usize = 2_000;
/// The step recovery activity that resolves rebase conflicts.
const CONFLICT_RECOVERY_ACTIVITY: &str = "pr_conflict_recovery";
/// The task history event of an auto-fix started outside the run.
const AUTO_FIX_OUT_OF_RUN_EVENT: &str = "pr_auto_fix_out_of_run";

/// Whether an arm does better with a lower or a higher value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Better {
    Lower,
    Higher,
}

/// How a metric is summarised: a mean of per-task values, or a rate of
/// per-task 0/1 outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Mean,
    Rate,
}

pub(super) struct MetricSpec {
    pub(super) name: &'static str,
    pub(super) kind: Kind,
    pub(super) better: Better,
    pub(super) unit: &'static str,
    /// `false` for a metric whose source series does not exist yet.
    pub(super) measured: bool,
}

const fn spec(
    name: &'static str,
    kind: Kind,
    better: Better,
    unit: &'static str,
    measured: bool,
) -> MetricSpec {
    MetricSpec {
        name,
        kind,
        better,
        unit,
        measured,
    }
}

/// Every metric an experiment may name, in the definition's order.
pub(super) const METRIC_SPECS: &[MetricSpec] = &[
    spec(
        "implement_tokens",
        Kind::Mean,
        Better::Lower,
        "normalized_tokens",
        true,
    ),
    spec(
        "total_tokens",
        Kind::Mean,
        Better::Lower,
        "normalized_tokens",
        true,
    ),
    spec("cost_usd", Kind::Mean, Better::Lower, "usd", true),
    spec("lead_time", Kind::Mean, Better::Lower, "hours", true),
    spec(
        "implement_duration",
        Kind::Mean,
        Better::Lower,
        "minutes",
        true,
    ),
    spec(
        "turns_before_first_edit",
        Kind::Mean,
        Better::Lower,
        "turns",
        false,
    ),
    spec(
        "result_bytes_before_first_edit",
        Kind::Mean,
        Better::Lower,
        "bytes",
        false,
    ),
    spec(
        "first_pass_acceptance",
        Kind::Rate,
        Better::Higher,
        "rate",
        true,
    ),
    spec("review_repairs", Kind::Mean, Better::Lower, "count", true),
    spec("step_recoveries", Kind::Mean, Better::Lower, "count", true),
    spec("final_recoveries", Kind::Mean, Better::Lower, "count", true),
    spec("failed_checks", Kind::Mean, Better::Lower, "count", true),
    spec("merge_conflicts", Kind::Mean, Better::Lower, "count", true),
    spec(
        "delivery_done_rate",
        Kind::Rate,
        Better::Higher,
        "rate",
        true,
    ),
    spec(
        "escaped_regressions",
        Kind::Mean,
        Better::Lower,
        "count",
        true,
    ),
    spec("lines_changed", Kind::Mean, Better::Lower, "lines", true),
];

pub(super) fn metric_spec(name: &str) -> Option<&'static MetricSpec> {
    METRIC_SPECS.iter().find(|spec| spec.name == name)
}

/// One task's value for one metric.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Outcome {
    Value(f64),
    /// Not settled yet: the task is in flight, or its regression window is
    /// still open.
    Open,
    /// Settled, but the records hold no value (no covered invocation, no
    /// review, nothing landed).
    Unmeasured,
}

/// One enrolled, unforced task with its outcomes.
pub(super) struct TaskOutcome {
    pub(super) task_id: String,
    pub(super) arm: String,
    pub(super) segment: u32,
    pub(super) complexity: String,
    /// The crew pool recorded at admission for the task's latest
    /// delivery, when that evidence exists [ORB-19487]. `None` means the
    /// historical pool is unavailable, not that the crew mismatched.
    pub(super) admitted_pool: Option<BTreeSet<String>>,
    /// What `admitted_pool` is, for a mismatch message.
    pub(super) admitted_pool_label: Option<String>,
    /// `task.crew`, the crew of the task's latest delivery [ORB-19416].
    pub(super) crew: String,
    pub(super) terminal: bool,
    pub(super) assigned_at: DateTime<Utc>,
    pub(super) assignment_digest: Option<String>,
    pub(super) values: BTreeMap<&'static str, Outcome>,
}

/// What the records say about the enrolled tasks.
pub(super) struct Observed {
    pub(super) tasks: Vec<TaskOutcome>,
    /// Each arm's overlay digests as its runs received them.
    pub(super) run_digests: BTreeMap<String, BTreeSet<String>>,
    /// Landed tasks whose regression window is still open.
    pub(super) open_regression_windows: usize,
}

#[derive(Default)]
struct RunEvents {
    step_recoveries: u64,
    final_recoveries: u64,
    merge_conflicts: u64,
}

#[derive(Default)]
struct Spend {
    cost: Option<f64>,
    implement_ms: Option<u64>,
}

/// The head and blocker an out-of-run auto-fix start names.
#[derive(Deserialize)]
struct OutOfRunStart {
    pr_number: String,
    head_sha: String,
    blocker: PrAutoFixBlocker,
}

fn crew_names_from_entries(entries: Option<&Value>) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    if let Some(Value::Array(items)) = entries {
        for item in items {
            if let Some(name) = item.as_str() {
                names.insert(name.to_string());
            } else if let Some(name) = item.get("name").and_then(Value::as_str) {
                names.insert(name.to_string());
            }
        }
    }
    names
}

fn captured_pool_for_tier(pools: Option<&Value>, tier: &str) -> BTreeSet<String> {
    let Some(pool) = pools.and_then(|pools| pools.get(tier)) else {
        return BTreeSet::new();
    };
    crew_names_from_entries(pool.get("crews").or_else(|| pool.as_array().map(|_| pool)))
}

impl OrbitRuntime {
    /// The crew pool a task's latest delivery was admitted against
    /// [ORB-19487]: its run's frozen `crew_selection.eligible_pool` for an
    /// ordinary route (complexity, purpose, type, default, hold or a
    /// run-level pool override captured in `auto_crew_pools`), or, for an
    /// explicit run crew, the captured complexity pool at the admission
    /// tier in that run's `auto_crew_pools`. The selection may be frozen
    /// in the run input or in its pipeline state (a claimed leaf).
    /// `None` when no run records that evidence for the task.
    pub(super) fn admitted_crew_pool(
        &self,
        task_id: &str,
        crew: &str,
    ) -> Result<Option<(BTreeSet<String>, String)>, OrbitError> {
        let mut runs = self.stores().jobs().list_job_runs_filtered(&JobRunQuery {
            task_id: Some(task_id.to_string()),
            include_steps: false,
            ..Default::default()
        })?;
        runs.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| right.run_id.cmp(&left.run_id))
        });
        for run in runs {
            let input = run.input.as_ref();
            let mut selection = input
                .and_then(|value| value.get("crew_selection"))
                .filter(|selection| {
                    selection.get("task_id").and_then(Value::as_str) == Some(task_id)
                })
                .cloned();
            let mut pools = input
                .and_then(|value| value.get("auto_crew_pools"))
                .cloned();
            if (selection.is_none() || pools.is_none())
                && let Some(state) = self.read_run_state(&run.run_id.to_string())?
            {
                if selection.is_none() {
                    selection = state.crew_selection.filter(|selection| {
                        selection.get("task_id").and_then(Value::as_str) == Some(task_id)
                    });
                }
                if pools.is_none() {
                    pools = state
                        .initial_input
                        .get("auto_crew_pools")
                        .cloned()
                        .or_else(|| {
                            input
                                .and_then(|value| value.get("auto_crew_pools"))
                                .cloned()
                        });
                }
            }
            let Some(selection) = selection else {
                continue;
            };
            // Only the delivery that ran the crew the task now names is
            // relevant: a crew redrawn by a later complexity rerate (or
            // changed after admission) has no such delivery, so its
            // historical membership is unavailable rather than a mismatch.
            if selection.get("crew").and_then(Value::as_str) != Some(crew) {
                continue;
            }
            let source = selection
                .get("source")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let tier = selection
                .get("complexity")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if source == "explicit" {
                let pool = captured_pool_for_tier(pools.as_ref(), tier);
                if pool.is_empty() {
                    return Ok(None);
                }
                return Ok(Some((pool, format!("admission {tier} complexity pool"))));
            }
            let eligible = crew_names_from_entries(selection.get("eligible_pool"));
            if !eligible.is_empty() {
                return Ok(Some((eligible, "recorded admission pool".to_string())));
            }
            let pool = captured_pool_for_tier(pools.as_ref(), tier);
            if !pool.is_empty() {
                return Ok(Some((pool, format!("admission {tier} complexity pool"))));
            }
            return Ok(None);
        }
        Ok(None)
    }

    /// Read every outcome of `enrolled` (unforced, assigned rows) of the
    /// experiment `experiment_id`, created at `created_at`.
    pub(super) fn experiment_outcomes(
        &self,
        experiment_id: &str,
        created_at: DateTime<Utc>,
        enrolled: &[&ExperimentAssignment],
        now: DateTime<Utc>,
    ) -> Result<Observed, OrbitError> {
        let ids: BTreeSet<String> = enrolled.iter().map(|row| row.task_id.clone()).collect();
        if ids.is_empty() {
            return Ok(Observed {
                tasks: Vec::new(),
                run_digests: BTreeMap::new(),
                open_regression_windows: 0,
            });
        }
        let since = created_at - READ_SLACK;
        let all_tasks = self.list_task_metadata()?;
        let tasks: BTreeMap<&str, &Task> = all_tasks
            .iter()
            .filter(|task| ids.contains(task.id.as_str()))
            .map(|task| (task.id.as_str(), task))
            .collect();
        let tokens = self.task_token_totals(Some(since))?;
        let spend = self.task_spend(&ids, since)?;
        let lines = self.landed_lines(&ids, since)?;
        let (run_events, run_digests) = self.run_audit(experiment_id, &ids, since)?;

        let mut landed_at = BTreeMap::new();
        let mut histories = BTreeMap::new();
        for id in &ids {
            let history = self.get_task_history(id)?;
            if let Some(at) = landing(&history) {
                landed_at.insert(id.clone(), at);
            }
            histories.insert(id.clone(), history);
        }
        let regressions = regressions(&all_tasks, &landed_at);
        let window = Duration::days(REGRESSION_WINDOW_DAYS);
        let mut open_regression_windows = 0;

        let mut outcomes = Vec::with_capacity(enrolled.len());
        for row in enrolled {
            let (Some(arm), Some(task)) = (row.arm.as_ref(), tasks.get(row.task_id.as_str()))
            else {
                continue;
            };
            let id = row.task_id.as_str();
            let terminal = task.status.is_terminal();
            let done = task.status == TaskStatus::Done;
            let settled = |value: Option<f64>| match (terminal, value) {
                (false, _) => Outcome::Open,
                (true, Some(value)) => Outcome::Value(value),
                (true, None) => Outcome::Unmeasured,
            };
            let history = histories.get(id).map(Vec::as_slice).unwrap_or_default();
            let comments = self.get_task_comments(id)?;
            let verdicts = review_gate_verdicts(&comments);
            let first_judged = verdicts
                .iter()
                .find(|verdict| **verdict != ReviewVerdict::Incomplete);
            let task_tokens = tokens.get(id);
            let covered = |spend: Option<&orbit_runtime::cost::TaskTokenSpend>| {
                spend
                    .filter(|spend| spend.invocation_count > 0)
                    .filter(|spend| spend.uncovered_invocation_count == 0)
                    .map(|spend| spend.normalized_token_total as f64)
            };
            let task_spend = spend.get(id);
            let events = run_events.get(id);
            let landed = landed_at.get(id).filter(|_| done);
            let escaped = match landed {
                None if terminal => Outcome::Unmeasured,
                None => Outcome::Open,
                Some(at) if now < *at + window => {
                    open_regression_windows += 1;
                    Outcome::Open
                }
                Some(_) => Outcome::Value(regressions.get(id).copied().unwrap_or(0) as f64),
            };
            let mut values = BTreeMap::new();
            values.insert(
                "implement_tokens",
                settled(covered(task_tokens.map(|totals| &totals.implement))),
            );
            values.insert(
                "total_tokens",
                settled(covered(task_tokens.map(|totals| &totals.all))),
            );
            values.insert("cost_usd", settled(task_spend.and_then(|spend| spend.cost)));
            values.insert(
                "lead_time",
                settled(landed.map(|at| (*at - task.created_at).num_seconds() as f64 / 3_600.0)),
            );
            values.insert(
                "implement_duration",
                settled(
                    task_spend
                        .and_then(|spend| spend.implement_ms)
                        .map(|ms| ms as f64 / 60_000.0),
                ),
            );
            values.insert("turns_before_first_edit", Outcome::Unmeasured);
            values.insert("result_bytes_before_first_edit", Outcome::Unmeasured);
            values.insert(
                "first_pass_acceptance",
                match first_judged {
                    Some(verdict) => Outcome::Value(if *verdict == ReviewVerdict::Accept {
                        1.0
                    } else {
                        0.0
                    }),
                    None if terminal => Outcome::Unmeasured,
                    None => Outcome::Open,
                },
            );
            let repairs = verdicts
                .iter()
                .filter(|verdict| **verdict == ReviewVerdict::AcceptWithFixes)
                .count();
            values.insert("review_repairs", settled(Some(repairs as f64)));
            let count = |pick: fn(&RunEvents) -> u64| Some(events.map_or(0, pick) as f64);
            values.insert("step_recoveries", settled(count(|e| e.step_recoveries)));
            values.insert("final_recoveries", settled(count(|e| e.final_recoveries)));
            values.insert("merge_conflicts", settled(count(|e| e.merge_conflicts)));
            values.insert(
                "failed_checks",
                settled(Some(failed_check_heads(history) as f64)),
            );
            values.insert(
                "delivery_done_rate",
                settled(Some(if done { 1.0 } else { 0.0 })),
            );
            values.insert("escaped_regressions", escaped);
            values.insert(
                "lines_changed",
                settled(lines.get(id).filter(|_| done).map(|lines| *lines as f64)),
            );
            let crew_name = task.crew.clone().unwrap_or_else(|| "unset".to_string());
            let (admitted_pool, admitted_pool_label) =
                match self.admitted_crew_pool(id, &crew_name)? {
                    Some((pool, label)) => (Some(pool), Some(label)),
                    None => (None, None),
                };
            outcomes.push(TaskOutcome {
                task_id: id.to_string(),
                arm: arm.clone(),
                segment: row.segment.unwrap_or(0),
                complexity: complexity_bucket(task.complexity.as_ref().map(|c| c.as_str()))
                    .to_string(),
                admitted_pool,
                admitted_pool_label,
                crew: crew_name,
                terminal,
                assigned_at: row.assigned_at,
                assignment_digest: row.digest.clone(),
                values,
            });
        }
        Ok(Observed {
            tasks: outcomes,
            run_digests,
            open_regression_windows,
        })
    }

    /// Provider cost over every linked invocation, and implement wall time,
    /// per task. An invocation linked to several tasks counts in full toward
    /// each, as the token series counts it.
    fn task_spend(
        &self,
        ids: &BTreeSet<String>,
        since: DateTime<Utc>,
    ) -> Result<BTreeMap<String, Spend>, OrbitError> {
        let mut spend: BTreeMap<String, Spend> = BTreeMap::new();
        for invocation in self.invocation_spend(Some(since))? {
            for id in invocation.task_ids.iter().filter(|id| ids.contains(*id)) {
                if let Some(cost) = invocation.provider_cost_usd {
                    *spend.entry(id.clone()).or_default().cost.get_or_insert(0.0) += cost;
                }
            }
        }
        for activity in IMPLEMENT_INVOCATION_IDS {
            let records = self.workspace_invocation_records(InvocationQuery {
                since: Some(since),
                activity_id: Some((*activity).to_string()),
                limit: INVOCATION_LIMIT,
                ..Default::default()
            })?;
            for record in records {
                for id in record.task_ids.iter().filter(|id| ids.contains(*id)) {
                    *spend
                        .entry(id.clone())
                        .or_default()
                        .implement_ms
                        .get_or_insert(0) += record.duration_ms;
                }
            }
        }
        Ok(spend)
    }

    /// Lines added plus deleted over every landed delivery, per task.
    fn landed_lines(
        &self,
        ids: &BTreeSet<String>,
        since: DateTime<Utc>,
    ) -> Result<BTreeMap<String, u64>, OrbitError> {
        let mut lines: BTreeMap<String, u64> = BTreeMap::new();
        for row in self.landed_diff_stats(Some(since))? {
            if !ids.contains(&row.task_id) {
                continue;
            }
            let changed: u64 = [row.code, row.tests, row.other]
                .iter()
                .map(|class| class.added + class.deleted)
                .sum();
            *lines.entry(row.task_id).or_default() += changed;
        }
        Ok(lines)
    }

    /// Recoveries and conflicts per task from its runs' audit trails, and
    /// the overlay digests each arm's dispatches received.
    #[allow(clippy::type_complexity)]
    fn run_audit(
        &self,
        experiment_id: &str,
        ids: &BTreeSet<String>,
        since: DateTime<Utc>,
    ) -> Result<
        (
            BTreeMap<String, RunEvents>,
            BTreeMap<String, BTreeSet<String>>,
        ),
        OrbitError,
    > {
        let mut run_tasks: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for id in ids {
            let runs = self.stores().jobs().list_job_runs_filtered(&JobRunQuery {
                task_id: Some(id.clone()),
                include_steps: false,
                ..Default::default()
            })?;
            for run in runs {
                run_tasks
                    .entry(run.run_id.to_string())
                    .or_default()
                    .push(id.clone());
            }
        }
        let mut events: BTreeMap<String, RunEvents> = BTreeMap::new();
        let mut digests: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut seen = BTreeSet::new();
        let mut offset = 0;
        loop {
            let page = self.list_v2_audit_events(V2AuditEventFilter {
                since: Some(since),
                source: Some("v2_envelope".to_string()),
                body_kinds: vec![
                    "step_recovery_attempted".to_string(),
                    "final_recovery_attempted".to_string(),
                    "experiment_overlay_applied".to_string(),
                ],
                limit: Some(AUDIT_PAGE),
                offset: Some(offset),
                oldest_first: true,
                ..Default::default()
            })?;
            let fetched = page.len();
            offset += fetched;
            for row in page {
                let Ok(payload) = serde_json::from_str::<Value>(&row.payload_json) else {
                    continue;
                };
                let field = |name: &str| payload.get(name).and_then(Value::as_str);
                let Some(event_id) = field("event_id") else {
                    continue;
                };
                if !seen.insert(event_id.to_string()) {
                    continue;
                }
                let tasks = run_tasks
                    .get(&row.run_id)
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                let mut bump = |pick: fn(&mut RunEvents) -> &mut u64| {
                    for task in tasks {
                        *pick(events.entry(task.clone()).or_default()) += 1;
                    }
                };
                match field("body_kind") {
                    Some("step_recovery_attempted") => {
                        if field("recovery_activity") == Some(CONFLICT_RECOVERY_ACTIVITY) {
                            bump(|e| &mut e.merge_conflicts);
                        } else {
                            bump(|e| &mut e.step_recoveries);
                        }
                    }
                    Some("final_recovery_attempted") if field("outcome") != Some("skipped") => {
                        bump(|e| &mut e.final_recoveries);
                    }
                    Some("experiment_overlay_applied") => {
                        let Ok(event) = serde_json::from_value::<V2AuditEvent>(payload) else {
                            continue;
                        };
                        if let V2AuditEventKind::ExperimentOverlayApplied {
                            task_id, overlays, ..
                        } = event.kind
                            && ids.contains(&task_id)
                        {
                            for overlay in overlays
                                .into_iter()
                                .filter(|overlay| overlay.experiment == experiment_id)
                            {
                                digests
                                    .entry(overlay.arm)
                                    .or_default()
                                    .insert(overlay.digest);
                            }
                        }
                    }
                    _ => {}
                }
            }
            if fetched < AUDIT_PAGE {
                break;
            }
        }
        Ok((events, digests))
    }
}

/// When the task last moved to done.
fn landing(history: &[TaskHistoryEntry]) -> Option<DateTime<Utc>> {
    history
        .iter()
        .rev()
        .find(|entry| entry.to_status == Some(TaskStatus::Done))
        .map(|entry| entry.at)
}

/// `regression_from` filings per landed culprit, filed within the window
/// after it landed, whatever their escape class.
fn regressions(
    tasks: &[Task],
    landed_at: &BTreeMap<String, DateTime<Utc>>,
) -> BTreeMap<String, u64> {
    let window = Duration::days(REGRESSION_WINDOW_DAYS);
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    for task in tasks {
        let culprits: BTreeSet<String> = task
            .relations
            .iter()
            .filter(|relation| relation.relation_type == TaskRelationType::RegressionFrom)
            .map(|relation| relation.target.to_string())
            .collect();
        for culprit in culprits {
            if let Some(at) = landed_at.get(&culprit)
                && task.created_at >= *at
                && task.created_at <= *at + window
            {
                *counts.entry(culprit).or_default() += 1;
            }
        }
    }
    counts
}

/// Distinct pull-request heads the task's auto-fix history saw fail a
/// required check.
fn failed_check_heads(history: &[TaskHistoryEntry]) -> usize {
    let mut heads = BTreeSet::new();
    for entry in history {
        let Some(note) = entry.note.as_deref() else {
            continue;
        };
        let (pr, head, blocker) = if entry.event == PR_AUTO_FIX_ADMITTED_EVENT {
            let Ok(attempt) = serde_json::from_str::<PrAutoFixAttempt>(note) else {
                continue;
            };
            (attempt.pr_number, attempt.head_sha, attempt.blocker)
        } else if entry.event == AUTO_FIX_OUT_OF_RUN_EVENT {
            let Ok(start) = serde_json::from_str::<OutOfRunStart>(note) else {
                continue;
            };
            (start.pr_number, start.head_sha, start.blocker)
        } else {
            continue;
        };
        if blocker == PrAutoFixBlocker::FailedChecks {
            heads.insert((pr, head));
        }
    }
    heads.len()
}
