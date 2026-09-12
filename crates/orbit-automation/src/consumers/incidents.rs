//! Bounded authoritative run lineage and current task-intent evidence.

use crate::host::{AutomationHost, RunOwnerLiveness};
use crate::{
    AutomationError,
    members::incidents::{IncidentFacts, incident_key},
};
use chrono::{DateTime, Utc};
use orbit_store::contracts::TaskCandidates;
use orbit_types::{
    task::{Task, TaskEnvelopeV2, TaskStatus},
    workflow::{JobRun, JobRunState, PipelineState},
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// Blocked task ids grouped by the settled incident they share.
pub type IncidentMembers = BTreeMap<String, Vec<String>>;

/// Whether `task` is blocked by `run_id`'s failure right now, as its latest
/// status transition records it.
pub fn failure_coupled<H: AutomationHost>(
    host: &H,
    task: &Task,
    run_id: &str,
) -> Result<bool, AutomationError> {
    if task.status != TaskStatus::Blocked || task.job_run_id.as_deref() != Some(run_id) {
        return Ok(false);
    }

    // Only the latest status transition counts: an older failure the task has
    // since moved past is not what is blocking it now.
    let history = host.get_task_history(&task.id)?;
    let last = history.iter().rev().find(|entry| entry.to_status.is_some());

    Ok(last.is_some_and(|entry| {
        entry.event == "workflow_run_failed"
            && entry.to_status == Some(TaskStatus::Blocked)
            && entry
                .note
                .as_deref()
                .is_some_and(|note| note.contains(&format!("run_id={run_id},")))
    }))
}

/// A run created by triage; diagnosing one again would recurse on itself.
fn is_diagnostic_origin(run: &JobRun) -> bool {
    run.job_id == "task_triage_pipeline"
        || run
            .input
            .as_ref()
            .and_then(|input| input.get("automation_origin"))
            .and_then(Value::as_str)
            == Some("triage")
}

/// Walk retry links to the run that started the episode, under a fixed budget.
fn root<H: AutomationHost>(
    host: &H,
    run: &JobRun,
    session: &mut IncidentSession,
) -> Result<String, AutomationError> {
    let mut current = run.clone();
    let mut seen = BTreeSet::new();

    for _ in 0..50 {
        if !seen.insert(current.run_id.clone()) {
            return Err(AutomationError::Deferred("incident_lineage_cycle".into()));
        }
        let Some(parent) = &current.retry_source_run_id else {
            return Ok(current.run_id);
        };
        current = session
            .backend_run(host, parent)?
            .ok_or_else(|| AutomationError::Deferred("incident_lineage_missing".into()))?;
    }

    Err(AutomationError::Deferred("incident_lineage_budget".into()))
}

/// The settled incident key and evidence for one blocked task. An episode
/// that has not stopped, or whose cause is ambiguous, defers.
pub fn observe<H: AutomationHost>(
    host: &H,
    task: &Task,
) -> Result<(String, Value), AutomationError> {
    observe_with_settlement(host, task, true)
}

fn observe_with_settlement<H: AutomationHost>(
    host: &H,
    task: &Task,
    require_settled: bool,
) -> Result<(String, Value), AutomationError> {
    let mut session = IncidentSession::new();
    let diagnosis = diagnose(host, task, &mut session)?;
    Ok((
        incident_key(&diagnosis.facts(require_settled))?,
        diagnosis.evidence,
    ))
}

struct Diagnosis {
    workspace: String,
    episode: String,
    cause: Option<String>,
    failure: bool,
    recovery_settled: bool,
    coupled: bool,
    diagnostic_origin: bool,
    cancelled: bool,
    evidence: Value,
}

impl Diagnosis {
    fn facts(&self, require_settled: bool) -> IncidentFacts {
        IncidentFacts {
            workspace: self.workspace.clone(),
            episode: Some(self.episode.clone()),
            cause: self.cause.clone(),
            failure: self.failure,
            recovery_settled: !require_settled || self.recovery_settled,
            current_failure_coupling: self.coupled,
            diagnostic_origin: self.diagnostic_origin,
            cancellation: self.cancelled,
        }
    }
}

fn diagnose<H: AutomationHost>(
    host: &H,
    task: &Task,
    session: &mut IncidentSession,
) -> Result<Diagnosis, AutomationError> {
    session.stats.observes += 1;

    let run_id = task
        .job_run_id
        .as_deref()
        .ok_or_else(|| AutomationError::Deferred("human_block".into()))?;

    let mut run = session.show_run(host, run_id)?;
    let coupled = failure_coupled(host, task, run_id)?;
    let cancelled = run.state == JobRunState::Cancelled;
    let failed = matches!(run.state, JobRunState::Failed | JobRunState::Timeout);

    let mut path_settled = true;
    let mut diagnostic_origin = is_diagnostic_origin(&run);
    let mut episode = root(host, &run, session)?;

    // A blocking dispatch with a typed terminal child result is explicit
    // causality. Multiple failing children need separate obligations; ambiguous
    // wrapper evidence is withheld instead of guessing from error prose.
    let mut seen = BTreeSet::new();
    for _ in 0..50 {
        if !seen.insert(run.run_id.clone()) {
            return Err(AutomationError::Deferred("incident_lineage_cycle".into()));
        }

        let liveness = session.probe_liveness(host, &run);
        path_settled &= run.state.is_terminal() && liveness == RunOwnerLiveness::Stopped;
        diagnostic_origin |= is_diagnostic_origin(&run);

        let state = session.run_state(host, &run.run_id)?;
        session.note_run(&run, liveness, &state);
        path_settled &= state
            .as_ref()
            .is_none_or(|state| !state.child_dispatches.iter().any(|d| d.phase.is_open()));

        let children: Vec<_> = state
            .as_ref()
            .map(|state| {
                state
                    .child_dispatches
                    .iter()
                    .filter(|dispatch| {
                        dispatch.blocking
                            && matches!(
                                dispatch.child_status.as_deref(),
                                Some("failed" | "timeout")
                            )
                    })
                    .collect()
            })
            .unwrap_or_default();

        if children.len() > 1 {
            return Err(AutomationError::Deferred("incident_unresolved".into()));
        }

        let Some(child) = children.first() else {
            break;
        };

        run = session.show_run(host, &child.child_run_id)?;
        episode = root(host, &run, session)?;
    }

    if seen.len() == 50 {
        return Err(AutomationError::Deferred("incident_lineage_budget".into()));
    }

    // Collect the whole retry tree under the episode root: an incident is only
    // settled once every run in it has stopped.
    let root_run = session
        .backend_run(host, &episode)?
        .ok_or_else(|| AutomationError::Deferred("incident_lineage_missing".into()))?;

    let mut related = vec![root_run];
    let mut lineage = BTreeSet::from([episode.clone()]);
    let mut index = 0;

    while index < related.len() {
        let children = session.retries(host, &related[index].run_id)?;

        if children.len() > 50 || related.len() + children.len() > 1000 {
            return Err(AutomationError::Deferred("incident_scan_budget".into()));
        }

        for child in children {
            if lineage.insert(child.run_id.clone()) {
                related.push(child);
            }
        }

        index += 1;
    }

    let mut settled = true;
    for related_run in &related {
        if !lineage.contains(&related_run.run_id) {
            continue;
        }
        let liveness = session.probe_liveness(host, related_run);
        let state = session.run_state(host, &related_run.run_id)?;
        session.note_run(related_run, liveness, &state);
        settled &= related_run.state.is_terminal() && liveness == RunOwnerLiveness::Stopped;
    }

    let cause_liveness = session.probe_liveness(host, &run);
    let state = session.run_state(host, &run.run_id)?;
    session.note_run(&run, cause_liveness, &state);
    settled &= cause_liveness == RunOwnerLiveness::Stopped;
    let descendants_settled = state
        .as_ref()
        .is_none_or(|state| !state.child_dispatches.iter().any(|d| d.phase.is_open()));

    // The failing step names the cause; without one the incident stays unresolved.
    let cause = run
        .steps
        .iter()
        .find(|step| matches!(step.state, JobRunState::Failed | JobRunState::Timeout))
        .map(|step| format!("{}:{}:{}", episode, step.step_index, step.target_id));

    Ok(Diagnosis {
        workspace: host.workspace_id()?,
        episode: episode.clone(),
        cause,
        failure: failed && matches!(run.state, JobRunState::Failed | JobRunState::Timeout),
        recovery_settled: path_settled && settled && descendants_settled,
        coupled,
        diagnostic_origin,
        cancelled: cancelled || run.state == JobRunState::Cancelled,
        evidence: json!({"episode":episode,"cause_run_id":run.run_id,"coupled_run_id":run_id,
        "task_revision":task.updated_at, "task_id":task.id}),
    })
}

/// Hydrate at most 1,000 blocked task envelopes to find the complete current
/// cohort, including tasks coupled to different wrappers of the same cause.
/// An incomplete inventory never certifies partial incident coverage.
pub fn members<H: AutomationHost>(host: &H) -> Result<IncidentMembers, AutomationError> {
    IncidentSession::new().inventory(host)
}

/// Bounded reuse of incident inventory work for one `Host` evaluation.
///
/// Lifetime: one `members::evaluate` call. `Host::observe` may populate the
/// session; `Host::admission` reuses that snapshot only after a freshness
/// check against current blocked-task identities, related-run owner/state,
/// retry children, child-dispatch tokens, and live liveness probes. Liveness
/// is never cached by run ID across a changed owner/state observation. A
/// failed freshness check rebuilds. Candidate fingerprint `observe` stays a
/// fresh read so `material_changed` does not depend on the session.
#[derive(Clone, Debug, Default)]
pub struct IncidentSession {
    cached: Option<IncidentMembers>,
    freshness: Option<FreshnessSnapshot>,
    stats: IncidentWorkStats,
    building: FreshnessSnapshot,
    recording: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IncidentWorkStats {
    pub inventory_builds: usize,
    pub inventory_reuses: usize,
    pub task_reads: usize,
    pub run_reads: usize,
    pub liveness_probes: usize,
    pub observes: usize,
}

#[derive(Clone, Debug, Default)]
struct FreshnessSnapshot {
    tasks: Vec<TaskIdentity>,
    runs: BTreeMap<String, RunIdentity>,
    retries: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TaskIdentity {
    id: String,
    status: TaskStatus,
    job_run_id: Option<String>,
    updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RunIdentity {
    state: JobRunState,
    pid: Option<u32>,
    pid_start_time: Option<String>,
    liveness: RunOwnerLiveness,
    child_dispatches: Vec<(String, String, bool, Option<String>, bool)>,
}

impl IncidentSession {
    /// A session with no cached inventory.
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn stats(&self) -> IncidentWorkStats {
        self.stats.clone()
    }

    /// The current blocked-task cohort per incident, rebuilt unless this
    /// session's snapshot is still fresh.
    pub fn inventory<H: AutomationHost>(
        &mut self,
        host: &H,
    ) -> Result<IncidentMembers, AutomationError> {
        if let Some(cached) = self.cached.clone()
            && self.fresh(host)?
        {
            self.stats.inventory_reuses += 1;
            return Ok(cached);
        }

        let built = self.rebuild(host)?;
        self.cached = Some(built.clone());
        Ok(built)
    }

    fn rebuild<H: AutomationHost>(&mut self, host: &H) -> Result<IncidentMembers, AutomationError> {
        self.stats.inventory_builds += 1;
        self.recording = true;
        self.building = FreshnessSnapshot::default();

        let listed = blocked_candidates(host)?;
        self.building.tasks = listed.items.iter().map(envelope_identity).collect();

        let mut members: IncidentMembers = BTreeMap::new();
        let mut unsettled = BTreeSet::new();

        for candidate in &listed.items {
            let task = self.read_task(host, &candidate.id)?;
            if let Ok(diagnosis) = diagnose(host, &task, self) {
                if let Ok(key) = incident_key(&diagnosis.facts(true)) {
                    members.entry(key).or_default().push(task.id);
                } else if let Ok(key) = incident_key(&diagnosis.facts(false)) {
                    unsettled.insert(key);
                }
            }
        }

        // An incident with any still-unsettled member is not yet diagnosable at all.
        members.retain(|key, _| !unsettled.contains(key));

        for ids in members.values_mut() {
            ids.sort();
        }

        self.recording = false;
        self.freshness = Some(std::mem::take(&mut self.building));
        Ok(members)
    }

    fn fresh<H: AutomationHost>(&mut self, host: &H) -> Result<bool, AutomationError> {
        let Some(snapshot) = self.freshness.clone() else {
            return Ok(false);
        };

        let listed = blocked_candidates(host)?;
        let tasks: Vec<_> = listed.items.iter().map(envelope_identity).collect();
        if tasks != snapshot.tasks {
            return Ok(false);
        }

        for (run_id, expected) in &snapshot.runs {
            let Some(run) = self.backend_run(host, run_id)? else {
                return Ok(false);
            };
            let liveness = self.probe_liveness(host, &run);
            if run.state != expected.state
                || run.pid != expected.pid
                || run.pid_start_time != expected.pid_start_time
                || liveness != expected.liveness
            {
                return Ok(false);
            }

            let state = self.run_state(host, run_id)?;
            if child_dispatch_token(state.as_ref()) != expected.child_dispatches {
                return Ok(false);
            }
        }

        for (run_id, expected) in &snapshot.retries {
            let children = self.retries(host, run_id)?;
            let mut child_ids: Vec<String> =
                children.iter().map(|child| child.run_id.clone()).collect();
            child_ids.sort();
            if &child_ids != expected {
                return Ok(false);
            }
        }

        Ok(true)
    }

    fn read_task<H: AutomationHost>(
        &mut self,
        host: &H,
        id: &str,
    ) -> Result<Task, AutomationError> {
        self.stats.task_reads += 1;
        Ok(host.get_task(id)?)
    }

    fn show_run<H: AutomationHost>(
        &mut self,
        host: &H,
        run_id: &str,
    ) -> Result<JobRun, AutomationError> {
        self.stats.run_reads += 1;
        Ok(host.show_job_run(run_id)?)
    }

    fn backend_run<H: AutomationHost>(
        &mut self,
        host: &H,
        run_id: &str,
    ) -> Result<Option<JobRun>, AutomationError> {
        self.stats.run_reads += 1;
        Ok(host.job_run(run_id)?)
    }

    fn run_state<H: AutomationHost>(
        &mut self,
        host: &H,
        run_id: &str,
    ) -> Result<Option<PipelineState>, AutomationError> {
        self.stats.run_reads += 1;
        Ok(host.read_run_state(run_id)?)
    }

    fn retries<H: AutomationHost>(
        &mut self,
        host: &H,
        run_id: &str,
    ) -> Result<Vec<JobRun>, AutomationError> {
        self.stats.run_reads += 1;
        let children = host.job_run_retries(run_id, 51)?;
        if self.recording {
            let mut ids: Vec<String> = children.iter().map(|child| child.run_id.clone()).collect();
            ids.sort();
            self.building.retries.insert(run_id.to_string(), ids);
        }
        Ok(children)
    }

    fn probe_liveness<H: AutomationHost>(&mut self, host: &H, run: &JobRun) -> RunOwnerLiveness {
        self.stats.liveness_probes += 1;
        host.run_owner_liveness(run)
    }

    fn note_run(
        &mut self,
        run: &JobRun,
        liveness: RunOwnerLiveness,
        state: &Option<PipelineState>,
    ) {
        if !self.recording {
            return;
        }
        self.building
            .runs
            .entry(run.run_id.clone())
            .and_modify(|identity| {
                identity.state = run.state;
                identity.pid = run.pid;
                identity.pid_start_time = run.pid_start_time.clone();
                identity.liveness = liveness;
                identity.child_dispatches = child_dispatch_token(state.as_ref());
            })
            .or_insert(RunIdentity {
                state: run.state,
                pid: run.pid,
                pid_start_time: run.pid_start_time.clone(),
                liveness,
                child_dispatches: child_dispatch_token(state.as_ref()),
            });
    }
}

fn envelope_identity(task: &TaskEnvelopeV2) -> TaskIdentity {
    TaskIdentity {
        id: task.id.clone(),
        status: task.status,
        job_run_id: task.job_run_id.clone(),
        updated_at: task.updated_at,
    }
}

fn child_dispatch_token(
    state: Option<&PipelineState>,
) -> Vec<(String, String, bool, Option<String>, bool)> {
    let Some(state) = state else {
        return Vec::new();
    };
    let mut rows: Vec<_> = state
        .child_dispatches
        .iter()
        .map(|dispatch| {
            (
                dispatch.child_run_id.clone(),
                dispatch.phase.as_str().to_string(),
                dispatch.blocking,
                dispatch.child_status.clone(),
                dispatch.phase.is_open(),
            )
        })
        .collect();
    rows.sort();
    rows
}

fn blocked_candidates<H: AutomationHost>(host: &H) -> Result<TaskCandidates, AutomationError> {
    let candidates = host.task_candidates(
        &orbit_store::contracts::TaskListFilter {
            statuses: Some(vec![TaskStatus::Blocked]),
            ..Default::default()
        },
        1001,
    )?;

    if candidates.total > 1000 {
        return Err(AutomationError::Deferred(
            "incident_inventory_budget".into(),
        ));
    }

    Ok(candidates)
}
