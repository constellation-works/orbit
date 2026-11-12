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
use orbit_types::task::{Task, TaskHistoryEntry, TaskStatus, complexity_bucket};
use orbit_types::workflow::{
    CapturedExperiments, ReviewVerdict, TagInstructionRole, V2AuditEvent, V2AuditEventKind,
};
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::agents::{self, ExecutionMode, task_execution_modes};

/// How long after landing a `regression_from` filing counts as an escape.
pub(super) const REGRESSION_WINDOW_DAYS: i64 = 14;
/// How far before the experiment's creation the accounting reads start, for
/// clock skew between the hosts that stamp records.
const READ_SLACK: Duration = Duration::days(1);
/// Invocation records read per implement activity.
const INVOCATION_LIMIT: usize = 100_000;
/// Stored names of an explorer delegation. Matched case-insensitively on
/// the full tool name and on the tail after the last `__`.
const EXPLORER_DELEGATION: &[&str] = &["orbit.agent.invoke", "orbit_agent_invoke"];
/// Audit rows read per page.
const AUDIT_PAGE: usize = 2_000;
/// The step recovery activity that resolves rebase conflicts.
const CONFLICT_RECOVERY_ACTIVITY: &str = "pr_conflict_recovery";

/// The `step_finished` outcome of a step that completed.
const STEP_SUCCESS: &str = "success";

/// Whether `name` is an explorer delegation, including the tool's own name
/// on the tail of an MCP identifier (`mcp__orbit__orbit_agent_invoke`).
fn is_explorer_delegation(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let tail = match lower.rsplit_once("__") {
        Some((_, tail)) => tail,
        None => lower.as_str(),
    };
    EXPLORER_DELEGATION
        .iter()
        .any(|known| *known == tail || *known == lower.as_str())
}

/// Which analysed tasks the implement invocations' tool calls show delegating
/// to an explorer.
struct ExplorerObservations {
    found: BTreeSet<String>,
    unobserved: BTreeSet<String>,
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
    estimated: bool,
    unpriced: bool,
    implement_ms: Option<u64>,
}

impl Spend {
    fn complete_cost(&self) -> Option<f64> {
        self.cost.filter(|cost| !self.unpriced && cost.is_finite())
    }
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

/// What a readout reads from its tasks' run audit trails.
struct RunAudit {
    /// Recoveries and conflicts per task.
    events: BTreeMap<String, RunEvents>,
    /// Each arm's overlay digests as its dispatches received them.
    digests: BTreeMap<String, BTreeSet<String>>,
    /// Roles in which each task's arm overlay reached a step that succeeded.
    delivered: BTreeMap<String, BTreeSet<TagInstructionRole>>,
    /// The crew memory each task's first memory-bearing dispatch received.
    memory: BTreeMap<String, TaskMemoryDelivery>,
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
            // An unreadable checkpoint leaves admission evidence
            // unavailable for this run rather than failing the readout.
            if (selection.is_none() || pools.is_none())
                && let Some(state) = self.read_run_state(&run.run_id.to_string()).ok().flatten()
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
            // The latest run with admission evidence for this task is the
            // latest relevant delivery [ORB-19520]: resolve it first, then
            // verify its crew. A crew redrawn by a later complexity rerate
            // (or changed after admission) has no such delivery, so its
            // historical membership is unavailable rather than a mismatch;
            // an older superseded delivery that happens to name the current
            // crew must not invent one.
            if selection.get("crew").and_then(Value::as_str) != Some(crew) {
                return Ok(None);
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
        shadow: bool,
    ) -> Result<Observed, OrbitError> {
        let ids: BTreeSet<String> = enrolled.iter().map(|row| row.task_id.clone()).collect();
        if ids.is_empty() {
            return Ok(Observed {
                tasks: Vec::new(),
                first_edit_incomplete: false,
                run_digests: BTreeMap::new(),
                open_regression_windows: 0,
                interactive_excluded: 0,
            });
        }
        let since = created_at - READ_SLACK;
        let assigned_at: BTreeMap<String, DateTime<Utc>> = enrolled
            .iter()
            .map(|row| (row.task_id.clone(), row.assigned_at))
            .collect();
        let explorer = self.explorer_observations(&ids, &assigned_at, since)?;
        let all_tasks = self.list_task_metadata()?;
        let tasks: BTreeMap<&str, &Task> = all_tasks
            .iter()
            .filter(|task| ids.contains(task.id.as_str()))
            .map(|task| (task.id.as_str(), task))
            .collect();
        let tokens = self.task_token_totals(Some(since))?;
        let spend = self.task_spend(&ids, since)?;
        let lines = self.landed_task_lines_changed(&ids, since)?;
        let reviewable_rows = self.landed_task_reviewable_lines(&ids, since)?;
        let RunAudit {
            events: run_events,
            digests: run_digests,
            delivered: delivered_roles,
            memory: task_memory,
        } = self.run_audit(experiment_id, &ids, since)?;
        let first_edits = self.first_implement_edits(Some(since))?;

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
        // Only a job applies an arm's overlay, so a task worked in an
        // interactive session was assigned but never treated.
        let executed: Vec<(&Task, &[TaskHistoryEntry])> = tasks
            .values()
            .map(|task| {
                let history = histories
                    .get(task.id.as_str())
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                (*task, history)
            })
            .collect();
        let modes = task_execution_modes(self, since, &executed, now);
        let mut interactive_excluded = 0;

        let claimed_treatments = self.latest_claim_experiment_treatments(experiment_id, &ids)?;
        // Captures are only reported for shadow arms, so a live readout
        // never scans (or fails on) historical pipeline checkpoints.
        let mut captured_memory = if shadow {
            self.captured_memory_from_runs(experiment_id, &ids)?
        } else {
            BTreeMap::new()
        };
        for (id, claim) in &claimed_treatments {
            if let Some(memory) = &claim.captured_memory {
                captured_memory
                    .entry(id.clone())
                    .or_insert_with(|| memory.clone());
            }
        }
        let mut outcomes = Vec::with_capacity(enrolled.len());
        for row in enrolled {
            let (Some(arm), Some(task)) = (row.arm.as_ref(), tasks.get(row.task_id.as_str()))
            else {
                continue;
            };
            let id = row.task_id.as_str();
            if modes.get(id) == Some(&ExecutionMode::Interactive) {
                interactive_excluded += 1;
                continue;
            }
            let terminal = task.status.is_terminal();
            let done = task.status == TaskStatus::Done;
            let settled = |value: Option<f64>| match (terminal, value) {
                (false, _) => Outcome::Open,
                (true, Some(value)) => Outcome::Value(value),
                (true, None) => Outcome::Unmeasured,
            };
            let history = histories.get(id).map(Vec::as_slice).unwrap_or_default();
            let comments = self.get_task_comments(id)?;
            let verdicts = agents::review_gate_verdicts(history, &comments);
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
            let cost = task_spend.and_then(Spend::complete_cost);
            values.insert("cost_usd", settled(cost));
            let cost_source = task_spend
                .filter(|_| terminal && cost.is_some())
                .map(|spend| {
                    if spend.estimated {
                        CostSource::Estimated
                    } else {
                        CostSource::Reported
                    }
                });
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
            // A newest-first scan may retain a later retry when truncated.
            // Completeness is scan-wide: no task's earliest invocation is
            // certified, even when the scan holds a score for that task.
            let first_edit = if first_edits.truncated {
                None
            } else {
                first_edits.by_task.get(id).and_then(|edit| edit.score)
            };
            values.insert(
                "turns_before_first_edit",
                settled(first_edit.map(|(turns, _)| turns as f64)),
            );
            values.insert(
                "result_bytes_before_first_edit",
                settled(first_edit.map(|(_, bytes)| bytes as f64)),
            );
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
            // No review happened without a settlement, so that is
            // unmeasured, not a measured zero repairs.
            values.insert(
                "review_repairs",
                settled((!verdicts.is_empty()).then_some(repairs as f64)),
            );
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
            let reviewable = if !done {
                ReviewableDenominator::NotDone
            } else {
                match reviewable_rows.get(id) {
                    None => ReviewableDenominator::Missing,
                    Some(None) => ReviewableDenominator::Unavailable,
                    Some(Some(lines)) => ReviewableDenominator::Measured(*lines),
                }
            };
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
                has_review_settlement: !verdicts.is_empty(),
                admitted_treatment: self.admitted_experiment_treatment(
                    id,
                    experiment_id,
                    claimed_treatments.get(id),
                )?,
                explorer_call: explorer.found.contains(id),
                explorer_unobserved: explorer.unobserved.contains(id),
                first_edit_trace_incomplete: first_edits
                    .by_task
                    .get(id)
                    .is_some_and(|edit| edit.incomplete_trace),
                assigned_at: row.assigned_at,
                assignment_digest: row.digest.clone(),
                delivered_roles: delivered_roles.get(id).cloned().unwrap_or_default(),
                memory: task_memory.get(id).cloned(),
                captured_memory: captured_memory.get(id).cloned(),
                cost_source,
                reviewable,
                values,
            });
        }
        Ok(Observed {
            tasks: outcomes,
            first_edit_incomplete: first_edits.truncated,
            run_digests,
            open_regression_windows,
            interactive_excluded,
        })
    }

    /// Captured crew memory per task from its runs' `experiment_capture`
    /// (or pipeline `initial_input`), earliest run first. Shadow arms are
    /// captured but never delivered, so this is their only memory evidence;
    /// live arms keep using the overlay-applied audit rows for delivery.
    fn captured_memory_from_runs(
        &self,
        experiment_id: &str,
        ids: &BTreeSet<String>,
    ) -> Result<BTreeMap<String, TaskMemoryDelivery>, OrbitError> {
        let mut captured: BTreeMap<String, TaskMemoryDelivery> = BTreeMap::new();
        for id in ids {
            let mut runs = self.stores().jobs().list_job_runs_filtered(&JobRunQuery {
                task_id: Some(id.clone()),
                include_steps: false,
                ..Default::default()
            })?;
            runs.sort_by(|left, right| {
                left.created_at
                    .cmp(&right.created_at)
                    .then_with(|| left.run_id.cmp(&right.run_id))
            });
            for run in runs {
                // The run input already answers when it carries a capture;
                // pipeline state is only a fallback for runs without one.
                // An unreadable fallback checkpoint skips this run instead
                // of failing the whole readout, matching the soft skip for
                // a capture that does not deserialize below.
                let raw = match run
                    .input
                    .as_ref()
                    .and_then(|input| input.get("experiment_capture"))
                    .cloned()
                {
                    Some(raw) => Some(raw),
                    None => match self.read_run_state(&run.run_id.to_string()) {
                        Ok(state) => state
                            .as_ref()
                            .and_then(|state| state.initial_input.get("experiment_capture"))
                            .cloned(),
                        Err(_) => continue,
                    },
                };
                let Some(raw) = raw else { continue };
                let Ok(capture) = serde_json::from_value::<CapturedExperiments>(raw) else {
                    continue;
                };
                let Some(entry) = capture
                    .assignments
                    .iter()
                    .find(|entry| entry.experiment == experiment_id)
                else {
                    continue;
                };
                let Some(memory) = &entry.memory else {
                    continue;
                };
                if !memory.enabled {
                    continue;
                }
                let Some(delivery) = memory.deliveries.first() else {
                    continue;
                };
                captured.insert(
                    id.clone(),
                    TaskMemoryDelivery {
                        week: delivery
                            .revised_at
                            .as_deref()
                            .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
                            .map(|at| at.format("%G-W%V").to_string()),
                        digest: delivery.digest.clone(),
                        model: delivery.model.clone(),
                        model_revision: delivery.block.revision.model,
                        global_revision: delivery.block.revision.global,
                        renderer_version: delivery.renderer_version,
                    },
                );
                break;
            }
        }
        Ok(captured)
    }

    /// The summed complete cost of `ids` (crew memory dreaming tasks) and
    /// how many of them have an unpriced invocation, which leave the sum.
    pub(super) fn dreaming_spend(
        &self,
        ids: &BTreeSet<String>,
        since: DateTime<Utc>,
    ) -> Result<(f64, usize), OrbitError> {
        if ids.is_empty() {
            return Ok((0.0, 0));
        }
        let spend = self.task_spend(ids, since - READ_SLACK)?;
        let mut total = 0.0;
        let mut unpriced = 0;
        for task in spend.values() {
            match task.complete_cost() {
                Some(cost) => total += cost,
                None if task.unpriced => unpriced += 1,
                None => {}
            }
        }
        Ok((total, unpriced))
    }

    /// Cost over every linked invocation (provider-reported first, then the
    /// shared table-derived estimate), and implement wall time, per task.
    /// Any unpriced invocation makes the task's cost unmeasured. An invocation
    /// linked to several tasks counts in full toward each, as tokens do.
    fn task_spend(
        &self,
        ids: &BTreeSet<String>,
        since: DateTime<Utc>,
    ) -> Result<BTreeMap<String, Spend>, OrbitError> {
        let mut spend: BTreeMap<String, Spend> = BTreeMap::new();
        for invocation in self.invocation_spend(Some(since))? {
            for id in invocation.task_ids.iter().filter(|id| ids.contains(*id)) {
                let task = spend.entry(id.clone()).or_default();
                if let Some(cost) = invocation.provider_cost_usd.or(invocation.derived_cost_usd) {
                    *task.cost.get_or_insert(0.0) += cost;
                    task.estimated |= invocation.provider_cost_usd.is_none();
                } else {
                    task.unpriced = true;
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

    /// Analysed-task ids with at least one explorer-delegation tool call on
    /// an implement invocation at or after that task's assignment, and those
    /// with none whose absence is unproven because an invocation at or after
    /// assignment lost early tool calls to the bounded output capture.
    ///
    /// Each task is read on its own: the store applies the `task_id` filter
    /// before its newest-first limit, so unrelated workspace invocations,
    /// however many and however new, cannot evict an enrolled task's
    /// recorded call the way one workspace-wide scan could [ORB-20573].
    fn explorer_observations(
        &self,
        ids: &BTreeSet<String>,
        assigned_at: &BTreeMap<String, DateTime<Utc>>,
        since: DateTime<Utc>,
    ) -> Result<ExplorerObservations, OrbitError> {
        let mut found = BTreeSet::new();
        let mut incomplete = BTreeSet::new();
        for id in ids {
            let Some(assigned) = assigned_at.get(id) else {
                continue;
            };
            for activity in IMPLEMENT_INVOCATION_IDS {
                if found.contains(id) {
                    break;
                }
                let records = self.workspace_invocation_records(InvocationQuery {
                    since: Some(since),
                    activity_id: Some((*activity).to_string()),
                    task_id: Some(id.clone()),
                    limit: INVOCATION_LIMIT,
                    ..Default::default()
                })?;
                for record in records {
                    if record.ts < *assigned {
                        continue;
                    }
                    if record
                        .tool_calls
                        .iter()
                        .any(|call| is_explorer_delegation(&call.tool_name))
                    {
                        found.insert(id.clone());
                        break;
                    }
                    if record.tool_calls_incomplete {
                        incomplete.insert(id.clone());
                    }
                }
            }
        }
        let unobserved = incomplete.difference(&found).cloned().collect();
        Ok(ExplorerObservations { found, unobserved })
    }

    /// Reviewable added plus deleted lines per task. `None` means at least
    /// one row in the window has an unavailable denominator, so the task is
    /// left out of per-line metrics rather than filled in from a shared range.
    /// A task with no row is absent. Several measured rows add.
    pub(crate) fn landed_task_reviewable_lines(
        &self,
        ids: &BTreeSet<String>,
        since: DateTime<Utc>,
    ) -> Result<BTreeMap<String, Option<u64>>, OrbitError> {
        let mut lines: BTreeMap<String, Option<u64>> = BTreeMap::new();
        for row in self.landed_diff_stats(Some(since))? {
            if !ids.contains(&row.task_id) {
                continue;
            }
            let slot = lines.entry(row.task_id).or_insert(Some(0));
            match row.reviewable {
                None => *slot = None,
                Some(reviewable) => {
                    if let Some(total) = slot.as_mut() {
                        *total = total.saturating_add(reviewable.total());
                    }
                }
            }
        }
        Ok(lines)
    }

    /// Recoveries and conflicts per task from its runs' audit trails, the
    /// overlay digests each arm's dispatches received, the roles in which
    /// each task's arm overlay reached a step that then succeeded, and the
    /// crew memory each task first received.
    fn run_audit(
        &self,
        experiment_id: &str,
        ids: &BTreeSet<String>,
        since: DateTime<Utc>,
    ) -> Result<RunAudit, OrbitError> {
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
        let mut memory: BTreeMap<String, TaskMemoryDelivery> = BTreeMap::new();
        // (run, step) -> (task, role) of each dispatch that received an arm
        // overlay of this experiment.
        let mut dispatches: BTreeMap<(String, String), BTreeSet<(String, TagInstructionRole)>> =
            BTreeMap::new();
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
                            step_id,
                            task_id,
                            overlays,
                            ..
                        } = event.kind
                            && ids.contains(&task_id)
                        {
                            for overlay in overlays
                                .into_iter()
                                .filter(|overlay| overlay.experiment == experiment_id)
                            {
                                dispatches
                                    .entry((row.run_id.clone(), step_id.clone()))
                                    .or_default()
                                    .insert((task_id.clone(), overlay.role));
                                // Oldest first: the first memory delivery
                                // names the task's revision stratum.
                                if let Some(received) = &overlay.memory {
                                    memory.entry(task_id.clone()).or_insert_with(|| {
                                        TaskMemoryDelivery {
                                            week: received
                                                .revised_at
                                                .as_deref()
                                                .and_then(|at| {
                                                    DateTime::parse_from_rfc3339(at).ok()
                                                })
                                                .map(|at| at.format("%G-W%V").to_string()),
                                            digest: received.digest.clone(),
                                            model: received.model.clone(),
                                            model_revision: received.model_revision,
                                            global_revision: received.global_revision,
                                            renderer_version: received.renderer_version,
                                        }
                                    });
                                }
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
        let mut delivered: BTreeMap<String, BTreeSet<TagInstructionRole>> = BTreeMap::new();
        let runs: BTreeSet<&str> = dispatches.keys().map(|(run, _)| run.as_str()).collect();
        for run in runs {
            let finished = self.step_outcomes(run)?;
            for ((_, step), received) in dispatches
                .iter()
                .filter(|((dispatch_run, _), _)| dispatch_run == run)
            {
                if finished.get(step).map(String::as_str) != Some(STEP_SUCCESS) {
                    continue;
                }
                for (task, role) in received {
                    delivered.entry(task.clone()).or_default().insert(*role);
                }
            }
        }
        Ok(RunAudit {
            events,
            digests,
            delivered,
            memory,
        })
    }

    /// Each step's last `step_finished` outcome in `run`'s audit trail.
    fn step_outcomes(&self, run: &str) -> Result<BTreeMap<String, String>, OrbitError> {
        let mut outcomes = BTreeMap::new();
        let mut offset = 0;
        loop {
            let page = self.list_v2_audit_events(V2AuditEventFilter {
                run_id: Some(run.to_string()),
                source: Some("v2_envelope".to_string()),
                body_kinds: vec!["step_finished".to_string()],
                limit: Some(AUDIT_PAGE),
                offset: Some(offset),
                oldest_first: true,
                ..Default::default()
            })?;
            let fetched = page.len();
            offset += fetched;
            for row in page {
                let Ok(event) = serde_json::from_str::<V2AuditEvent>(&row.payload_json) else {
                    continue;
                };
                if let V2AuditEventKind::StepFinished {
                    step_id, outcome, ..
                } = event.kind
                {
                    outcomes.insert(step_id, outcome);
                }
            }
            if fetched < AUDIT_PAGE {
                break;
            }
        }
        Ok(outcomes)
    }
}

use super::metrics::{failed_check_heads, landing, regressions};
pub(super) use super::model::{
    CostSource, Kind, METRIC_SPECS, MetricSpec, Observed, Outcome, ReviewableDenominator,
    TaskMemoryDelivery, TaskOutcome, metric_spec,
};
