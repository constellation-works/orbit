//! Observational runtime sources. Never use run listing/show or resume planners
//! here: those entry points may reconcile owners or recover coordination journals.
use super::sources::{
    AttentionCandidates, SharedOpenTasks, attention_reason, attention_task, workspace_degraded,
};
use crate::{
    OrbitRuntime,
    application::job::{self, RunOwnerLiveness},
};
use chrono::{DateTime, Duration, Utc};
use orbit_common::{
    OrbitError,
    governance::attention::{cursor, filter::AttentionFilter},
};
use orbit_store::contracts::{ClaimInspection, JobRunQuery, LocalPullPhase};
use orbit_types::{
    task::Task,
    tool::{
        AttentionHost, AttentionItem, AttentionKind, AttentionNext, AttentionSeverity,
        AttentionWorkspace,
    },
    workflow::{JobRun, JobRunState},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Instant,
};

const RUNTIME_KINDS: [AttentionKind; 3] = [
    AttentionKind::RunFailed,
    AttentionKind::RunStuck,
    AttentionKind::ClaimStale,
];

struct RuntimeAttention<'a> {
    runtime: &'a OrbitRuntime,
    workspace: &'a AttentionWorkspace,
    host: &'a AttentionHost,
    now: DateTime<Utc>,
    tasks: BTreeMap<String, Task>,
    /// Run-referenced tasks outside the shared open slice whose registry
    /// status is terminal, so a failed run for a settled task stays
    /// suppressed as it was when this map held every task.
    terminal_tasks: BTreeSet<String>,
    claims: Vec<ClaimInspection>,
}

impl RuntimeAttention<'_> {
    fn item(
        &self,
        kind: AttentionKind,
        refs: Value,
        since: DateTime<Utc>,
        reason: &str,
        detail: Value,
        next: AttentionNext,
    ) -> AttentionItem {
        let subject = if kind == AttentionKind::ClaimStale {
            json!({"claim_id": refs["claim_id"]})
        } else {
            refs.clone()
        };
        let task = refs["task_id"].as_str().and_then(|id| self.tasks.get(id));
        AttentionItem {
            id: cursor::attention_item_id(
                kind,
                &self.host.machine_id,
                &self.workspace.id,
                &subject,
            ),
            kind,
            severity: AttentionSeverity::Medium,
            workspace: self.workspace.clone(),
            host: self.host.clone(),
            refs,
            task: task.map(attention_task),
            since: Some(since),
            age_seconds: Some((self.now - since).num_seconds().max(0) as u64),
            expires_at: None,
            reason: attention_reason(reason),
            detail,
            next,
            fingerprint: String::new(),
        }
    }

    fn finish(&self, mut item: AttentionItem, state: Value, high: bool) -> AttentionItem {
        let aged = super::ATTENTION_AGE_BUMP_HOURS.iter().any(|hours| {
            item.age_seconds
                .is_some_and(|age| age >= u64::from(*hours) * 3600)
        });
        if high || aged {
            item.severity = AttentionSeverity::High;
        }
        item.fingerprint = cursor::attention_fingerprint(&item, state);
        item
    }

    fn run_next(&self, run: &JobRun, resume: bool) -> AttentionNext {
        AttentionNext {
            summary: if resume {
                "Resume from stored checkpoints"
            } else {
                "Inspect the run"
            }
            .into(),
            tool: Some("orbit.run".into()),
            input: Some(
                json!({"action": if resume { "resume" } else { "show" }, "id": run.run_id, "workspace": self.workspace.id}),
            ),
            command: format!(
                "orbit --workspace '{}' {} {}",
                self.workspace.id,
                if resume { "job resume" } else { "run show" },
                run.run_id
            ),
            needs: Vec::new(),
            also: Vec::new(),
        }
    }

    fn run_claim_state(&self, run: &JobRun) -> Result<(bool, bool), OrbitError> {
        let bindings = self
            .claims
            .iter()
            .filter(|claim| {
                claim.bound_run.as_ref().is_some_and(|bound| {
                    bound.run_id == run.run_id && bound.machine_id == self.host.machine_id
                })
            })
            .collect::<Vec<_>>();
        let local = self
            .runtime
            .stores()
            .jobs()
            .local_pull_for_run(&run.run_id)?;
        let bound = !bindings.is_empty() || local.is_some();
        let held = bindings
            .iter()
            .any(|claim| claim.claim.phase.is_unsettled())
            || local.is_some_and(|admission| {
                !matches!(
                    admission.phase,
                    LocalPullPhase::Settled | LocalPullPhase::Idle | LocalPullPhase::Refused
                )
            });
        Ok((bound, held))
    }

    // Advice is deliberately conservative. The ordinary resume endpoint still
    // performs all admission, checkpoint and lineage checks when an operator acts.
    fn run_resume_advice(&self, run: &JobRun, claimed: bool) -> Result<bool, OrbitError> {
        if claimed
            || run.input.as_ref().is_some_and(|input| {
                input
                    .get(orbit_types::workflow::SOURCE_REPAIR_KEY)
                    .is_some()
                    || orbit_types::workflow::run_input_declares_review_reconciliation(input)
                    || orbit_types::workflow::activity_job::run_input_declares_trusted_host(input)
            })
            || (run.state == JobRunState::Interrupted
                && job::run_owner_liveness(run) == RunOwnerLiveness::Alive)
        {
            return Ok(false);
        }
        Ok(self
            .runtime
            .read_run_definition_snapshot(&run.run_id)?
            .is_some()
            || self.runtime.load_v2_job_asset_by_name(&run.job_id).is_ok())
    }

    fn run_items(
        &self,
        kind: AttentionKind,
        runs: &[JobRun],
        folded: &BTreeSet<String>,
    ) -> Result<Vec<AttentionItem>, OrbitError> {
        let mut items = Vec::new();
        // Newest submission wins per (job, task), even if it is active or
        // successful. A newer attempt cannot resurrect an older failure.
        let mut newest = BTreeSet::new();
        for run in runs {
            let mut tasks = BTreeSet::new();
            if let Some(input) = &run.input {
                if let Some(id) = input["task_id"].as_str() {
                    tasks.insert(id.to_owned());
                }
                if let Some(ids) = input["task_ids"].as_array() {
                    tasks.extend(ids.iter().filter_map(Value::as_str).map(str::to_owned));
                }
            }
            let subjects = if tasks.is_empty() {
                vec![None]
            } else {
                tasks.into_iter().map(Some).collect()
            };
            for task_id in subjects {
                let is_newest = newest.insert((run.job_id.clone(), task_id.clone()));
                if folded.contains(&run.run_id)
                    || (kind == AttentionKind::RunFailed
                        && task_id.as_ref().is_some_and(|id| {
                            self.terminal_tasks.contains(id)
                                || self
                                    .tasks
                                    .get(id)
                                    .is_some_and(|task| task.status.is_terminal())
                        }))
                {
                    continue;
                }
                let (since, cause) =
                    match kind {
                        AttentionKind::RunFailed
                            if is_newest
                                && matches!(
                                    run.state,
                                    JobRunState::Failed
                                        | JobRunState::Timeout
                                        | JobRunState::Interrupted
                                ) =>
                        {
                            let Some(finished) = run.finished_at else {
                                continue;
                            };
                            if finished
                                < self.now
                                    - Duration::hours(i64::from(super::ATTENTION_RUN_WINDOW_HOURS))
                            {
                                continue;
                            }
                            // The failure window bounds candidates, not supersession:
                            // a newer long-running or completed submission can sit
                            // outside it. Consult the exact latest job/task row.
                            let latest = self.runtime.stores().jobs().list_job_runs_filtered(
                                &JobRunQuery {
                                    job_id: Some(run.job_id.clone()),
                                    task_id: task_id.clone(),
                                    limit: Some(1),
                                    include_steps: false,
                                    ..Default::default()
                                },
                            )?;
                            if latest
                                .first()
                                .is_some_and(|latest| latest.run_id != run.run_id)
                            {
                                continue;
                            }
                            (finished, None)
                        }
                        AttentionKind::RunStuck
                            if matches!(run.state, JobRunState::Running | JobRunState::Pending) =>
                        {
                            let since = run.started_at.unwrap_or(run.scheduled_at);
                            let cause = if (run.pid.is_some() || run.state == JobRunState::Running)
                                && job::run_owner_liveness(run) == RunOwnerLiveness::Stopped
                            {
                                "dead_owner"
                            } else if self.now - since
                                >= Duration::minutes(i64::from(
                                    super::ATTENTION_RUN_STUCK_AFTER_MINUTES,
                                ))
                            {
                                "wall_clock_bound"
                            } else {
                                continue;
                            };
                            (since, Some(cause))
                        }
                        _ => continue,
                    };
                let (claimed, held) = self.run_claim_state(run)?;
                let high = if kind == AttentionKind::RunFailed {
                    claimed
                } else {
                    held
                };
                let mut next = self.run_next(
                    run,
                    kind == AttentionKind::RunFailed && self.run_resume_advice(run, claimed)?,
                );
                if kind == AttentionKind::RunFailed {
                    next.also.push(AttentionNext {
                        summary: "Read the run logs".into(),
                        tool: None,
                        input: None,
                        command: format!(
                            "orbit --workspace '{}' run logs {}",
                            self.workspace.id, run.run_id
                        ),
                        needs: vec![],
                        also: vec![],
                    });
                } else {
                    next.also.push(AttentionNext {
                        summary: "Cancel the run after inspection".into(),
                        tool: None,
                        input: None,
                        command: format!(
                            "orbit --workspace '{}' run cancel {}",
                            self.workspace.id, run.run_id
                        ),
                        needs: vec![],
                        also: vec![],
                    });
                }
                let mut refs = json!({"run_id": run.run_id});
                if let Some(id) = task_id {
                    refs["task_id"] = json!(id);
                }
                let recorded = if kind == AttentionKind::RunFailed {
                    self.runtime.stores().jobs().get_job_run(&run.run_id)?
                } else {
                    None
                };
                let failure = recorded.as_ref().and_then(|run| {
                    run.steps
                        .iter()
                        .rev()
                        .find_map(|step| step.error_message.as_deref())
                });
                let reason = failure.map(str::to_owned).unwrap_or_else(|| match cause {
                    Some("dead_owner") => "Recorded run owner process is gone".into(),
                    Some(_) => "Run exceeds the configured wall-clock bound".into(),
                    None => format!("Run ended {}", run.state),
                });
                let item = self.item(kind, refs, since, &reason,
                    json!({"job_id": run.job_id, "state": run.state, "cause": cause, "held_claim": high}), next);
                let state = if let Some(cause) = cause {
                    json!({"run_id": run.run_id, "cause": cause})
                } else {
                    json!({"run_id": run.run_id, "state": run.state})
                };
                items.push(self.finish(item, state, high));
            }
        }
        Ok(items)
    }

    fn claim_items(&self) -> Result<Vec<AttentionItem>, OrbitError> {
        let mut items = Vec::new();
        for inspection in &self.claims {
            let claim = &inspection.claim;
            if !claim.phase.is_unsettled() {
                continue;
            }
            let mut diagnostics = BTreeSet::new();
            let expiry = parse_claim_time(&claim.reservation_expires_at)?;
            if expiry <= self.now {
                diagnostics.insert("reservation_expired");
            }
            if let Some(bound) = &inspection.bound_run {
                // Remote absence is not local absence. Never judge another
                // host's PID or missing run from the owner's local store.
                if bound.machine_id == self.host.machine_id {
                    match self.runtime.stores().jobs().get_job_run(&bound.run_id)? {
                        None => {
                            diagnostics.insert("bound_run_absent");
                        }
                        Some(run) if run.state.is_terminal() => {
                            diagnostics.insert("bound_run_terminal");
                        }
                        _ => {}
                    }
                }
            }
            if inspection.unresolved_merge_intent.is_some() {
                diagnostics.insert("unresolved_merge_intent");
            }
            if inspection.landing_invalidated {
                diagnostics.insert("landing_invalidated");
            }
            if diagnostics.is_empty() {
                continue;
            }
            let next = AttentionNext {
                summary: "Recover the possibly stale claim after inspection".into(),
                tool: Some("orbit.task.recover".into()),
                input: Some(
                    json!({"action": "recover_claim", "claim_id": claim.claim_id, "workspace": self.workspace.id}),
                ),
                command: format!(
                    "orbit --workspace '{}' task recover-claim {} --reason '<reason>' --request '<request_key>' --phase '<expected_phase>' --status '<status>'",
                    self.workspace.id, claim.claim_id
                ),
                needs: vec![
                    "reason".into(),
                    "request_key".into(),
                    "expected_phase".into(),
                    "status".into(),
                ],
                also: vec![AttentionNext {
                    summary: "Inspect current claims and their observed phases".into(),
                    tool: Some("orbit.drain.claims".into()),
                    input: Some(json!({"workspace": self.workspace.id})),
                    command: format!("orbit --workspace '{}' drain claims", self.workspace.id),
                    needs: vec![],
                    also: vec![],
                }],
            };
            let mut refs = json!({"claim_id": claim.claim_id, "task_id": claim.task_id});
            if let Some(bound) = &inspection.bound_run {
                refs["run_id"] = json!(bound.run_id);
            }
            let reason = format!(
                "Claim possibly stale: {}",
                diagnostics.iter().copied().collect::<Vec<_>>().join(", ")
            );
            let item = self.item(AttentionKind::ClaimStale, refs, parse_claim_time(&inspection.updated_at)?, &reason,
                json!({"phase": claim.phase, "diagnostics": diagnostics, "reservation_expires_at": claim.reservation_expires_at}), next);
            items.push(self.finish(item, json!({"claim_id": claim.claim_id, "phase": claim.phase, "diagnostics": diagnostics}), true));
        }
        Ok(items)
    }
}

fn parse_claim_time(time: &str) -> Result<DateTime<Utc>, OrbitError> {
    DateTime::parse_from_rfc3339(time)
        .map(|time| time.with_timezone(&Utc))
        .map_err(|error| OrbitError::Store(format!("invalid claim timestamp: {error}")))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn read_runtime_attention(
    runtime: &OrbitRuntime,
    workspace: &AttentionWorkspace,
    host: &AttentionHost,
    filter: &AttentionFilter,
    now: DateTime<Utc>,
    operator: bool,
    candidates: &mut AttentionCandidates,
    open_tasks: &SharedOpenTasks,
) {
    let kinds = RUNTIME_KINDS
        .into_iter()
        .filter(|kind| filter.selected(*kind))
        .collect::<Vec<_>>();
    if kinds.is_empty() {
        return;
    }
    if !operator {
        candidates.degraded.extend(kinds.into_iter().map(|kind| {
            workspace_degraded(
                workspace,
                kind.as_str(),
                "capability",
                "This runtime source requires the Operator capability",
            )
        }));
        return;
    }
    let owner = runtime.coordination_task_reads_visible();
    let started = Instant::now();
    let sources = (|| {
        let tasks = if owner {
            open_tasks
                .get(runtime)?
                .iter()
                .map(|task| (task.id.clone(), task.clone()))
                .collect()
        } else {
            BTreeMap::new()
        };
        let claims = if owner {
            runtime.inspect_execution_claims()?
        } else {
            Vec::new()
        };
        Ok::<_, OrbitError>(RuntimeAttention {
            runtime,
            workspace,
            host,
            now,
            tasks,
            terminal_tasks: BTreeSet::new(),
            claims,
        })
    })();
    let mut sources = match sources {
        Ok(sources) => sources,
        Err(error) => {
            candidates.degraded.extend(kinds.into_iter().map(|kind| {
                workspace_degraded(
                    workspace,
                    kind.as_str(),
                    "source_unavailable",
                    error.to_string(),
                )
            }));
            return;
        }
    };
    let run_read = Instant::now();
    let runs = if kinds.iter().any(|kind| *kind != AttentionKind::ClaimStale) {
        // Raw rows, not list_job_runs: observation must not mark an orphan interrupted.
        (|| {
            let mut runs = if filter.selected(AttentionKind::RunFailed) {
                runtime
                    .stores()
                    .jobs()
                    .list_job_runs_filtered(&JobRunQuery {
                        recency_since: Some(
                            now - Duration::hours(i64::from(super::ATTENTION_RUN_WINDOW_HOURS)),
                        ),
                        include_steps: false,
                        ..Default::default()
                    })?
            } else {
                Vec::new()
            };
            // Active rows are not bounded by the failed-run window.
            if filter.selected(AttentionKind::RunStuck) {
                let seen = runs
                    .iter()
                    .map(|run| run.run_id.clone())
                    .collect::<BTreeSet<_>>();
                runs.extend(
                    runtime
                        .stores()
                        .jobs()
                        .list_job_runs_filtered(&JobRunQuery {
                            active_only: true,
                            include_steps: false,
                            ..Default::default()
                        })?
                        .into_iter()
                        .filter(|run| !seen.contains(&run.run_id)),
                );
            }
            Ok::<_, OrbitError>(runs)
        })()
    } else {
        Ok(Vec::new())
    };
    let run_read_cost = run_read.elapsed();
    let common_cost = started.elapsed() - run_read_cost;
    let mut runs = runs;
    if let Ok(runs) = &mut runs {
        runs.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| a.run_id.cmp(&b.run_id))
        });
        runs.retain(|run| {
            run.executed_on
                .as_ref()
                .is_none_or(|origin| origin.machine_id == host.machine_id)
        });
    }
    // The shared slice holds only open tasks; a run naming a settled task
    // needs its terminal status from the registry projection (no envelope
    // read) so `run_failed` suppression keeps its former meaning.
    if owner && let Ok(runs) = &runs {
        let missing = runs
            .iter()
            .flat_map(|run| {
                let mut ids = Vec::new();
                if let Some(input) = &run.input {
                    if let Some(id) = input["task_id"].as_str() {
                        ids.push(id.to_owned());
                    }
                    if let Some(list) = input["task_ids"].as_array() {
                        ids.extend(list.iter().filter_map(Value::as_str).map(str::to_owned));
                    }
                }
                ids
            })
            .filter(|id| !sources.tasks.contains_key(id))
            .collect::<BTreeSet<_>>();
        if !missing.is_empty()
            && let Ok(index) = runtime.task_status_index_for(&missing)
        {
            sources.terminal_tasks = index
                .into_iter()
                .filter(|(_, status)| status.is_terminal())
                .map(|(id, _)| id)
                .collect();
        }
    }
    let folded = candidates
        .items
        .iter()
        .filter(|item| item.kind == AttentionKind::Blocked)
        .filter_map(|item| item.refs["run_id"].as_str().map(str::to_owned))
        .collect();
    for kind in kinds {
        let started = Instant::now();
        let result = if kind == AttentionKind::ClaimStale {
            if owner {
                sources.claim_items()
            } else {
                Err(OrbitError::Execution(
                    "Claim inspection is authoritative only on the workspace owner".into(),
                ))
            }
        } else {
            match &runs {
                Ok(runs) => sources.run_items(kind, runs, &folded),
                Err(error) => Err(OrbitError::Store(error.to_string())),
            }
        };
        let elapsed = common_cost
            + started.elapsed()
            + if kind == AttentionKind::ClaimStale {
                std::time::Duration::ZERO
            } else {
                run_read_cost
            };
        candidates
            .timings
            .insert(kind.as_str().into(), elapsed.as_micros());
        if elapsed.as_secs() >= super::ATTENTION_SOURCE_TIMEOUT_SECONDS {
            candidates.degraded.push(workspace_degraded(
                workspace,
                kind.as_str(),
                "source_timeout",
                "Runtime source read budget exhausted",
            ));
        } else {
            match result {
                Ok(items) => candidates.items.extend(items),
                Err(error) => candidates.degraded.push(workspace_degraded(
                    workspace,
                    kind.as_str(),
                    "source_unavailable",
                    error.to_string(),
                )),
            }
        }
    }
}
