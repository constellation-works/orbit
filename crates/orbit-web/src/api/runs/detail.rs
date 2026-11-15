//! Run detail JSON and supplementary audit projections.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use orbit_core::application::job::{
    JobRunProjectionEvidence, JobRunProjectionProfile, RecordedJobRun,
    display_steps_with_agent_execution_to_json, project_job_run, run_display_steps,
};
use orbit_core::runtime::audit::run::{RunAuditView, RunProviderProcess};
use orbit_core::{JobRun, NotFoundKind, OrbitError, OrbitRuntime};
use serde_json::{Value, json};

/// One lazy scan of a run's audit trail, shared by the steps and
/// provider-process halves of a single run-detail response.
///
/// The view is computed at most once: whichever half asks first pays for
/// the scan, and a failure is cached too, so an unreadable trail is not
/// scanned twice. A run whose record steps are already activity-level
/// never triggers the steps half, so it pays only for the scan the
/// provider-process half needs. Nothing is cached across requests, so
/// provider-process liveness is still probed on every fetch.
struct LazyRunAuditView<'a> {
    runtime: &'a OrbitRuntime,
    run_id: &'a str,
    cached: RefCell<Option<Result<RunAuditView, String>>>,
}

impl LazyRunAuditView<'_> {
    fn load_run_audit_view(&self) -> Result<RunAuditView, String> {
        let mut cached = self.cached.borrow_mut();
        if let Some(result) = cached.as_ref() {
            return result.clone();
        }
        let computed = self
            .runtime
            .collect_run_audit_view(self.run_id)
            .map_err(|error| error.to_string());
        *cached = Some(computed.clone());
        computed
    }
}

pub(super) fn job_run_detail_to_json(runtime: &OrbitRuntime, run: &JobRun) -> Value {
    // [ORB-10971] Read the run's pipeline state. This projection used to drop
    // it entirely, so the dashboard could not see the waiting reasons or the
    // child-dispatch lineage the CLI already showed.
    let run_id = run.run_id.as_str();
    let state = or_warn(runtime.read_run_state(run_id), run_id, "pipeline state");
    let evidence = or_warn(
        runtime.activity_invocation_evidence(run_id),
        run_id,
        "invocation evidence",
    );
    let mut full = project_job_run(
        run,
        JobRunProjectionProfile::FullDetailSiblingSteps,
        JobRunProjectionEvidence {
            state: state.as_ref(),
            activity_evidence: Some(&evidence),
            ..JobRunProjectionEvidence::default()
        },
    );
    // Child dispatches persist lineage rather than task input. One
    // steps-free read covers every child. Only a non-terminal child is
    // reconciled, so a dead leaf does not stay running, and a row that
    // cannot be read still leaves that dispatch without state or labels.
    let child_ids = state
        .iter()
        .flat_map(|state| &state.child_dispatches)
        .map(|dispatch| dispatch.child_run_id.clone())
        .collect::<Vec<_>>();
    let children = load_child_runs(runtime, &child_ids);
    let titles = or_warn(
        super::tasks::task_titles(runtime, std::iter::once(run).chain(children.values())),
        run_id,
        "task titles",
    );
    super::tasks::add_tasks(&mut full, run, &titles);
    if let Some(dispatches) = full["child_dispatches"].as_array_mut() {
        for dispatch in dispatches {
            if let Some(child) = dispatch["child_run_id"]
                .as_str()
                .and_then(|id| children.get(id))
            {
                super::tasks::add_tasks(dispatch, child, &titles);
                // A fire-and-forget dispatch can stay submitted forever. Its
                // checkpoint is lineage, not the child's current outcome.
                dispatch["state"] = json!(child.state);
                dispatch["started_at"] = json!(child.started_at);
                dispatch["finished_at"] = json!(child.finished_at);
                dispatch["duration_ms"] = json!(child.duration_ms);
            } else {
                dispatch["task_ids"] = Value::Null;
                dispatch["tasks"] = Value::Null;
                dispatch["state"] = Value::Null;
                dispatch["started_at"] = Value::Null;
                dispatch["finished_at"] = Value::Null;
                dispatch["duration_ms"] = Value::Null;
            }
        }
    }
    // The failure chain and the cancel record are detail-only. One response
    // carries both, so the page does not fetch each child to find the cause.
    super::failure::attach_run_failure_context(runtime, run, state.as_ref(), &mut full);
    let audit_view = LazyRunAuditView {
        runtime,
        run_id,
        cached: RefCell::new(None),
    };
    let steps = run_display_steps(run, || match audit_view.load_run_audit_view() {
        Ok(view) => view.steps,
        Err(error) => {
            tracing::warn!(run_id, section = "audit steps", %error, "run detail omitted a section");
            Vec::new()
        }
    });

    // [ORB-10496] Provider subprocesses for this run's agent steps, with a
    // liveness verdict for any that have not reported an exit. Without this a
    // healthy long-running ship-pipeline implementation agent is
    // indistinguishable from a dead child without shell access to the host.
    let provider_processes = match audit_view.load_run_audit_view() {
        Ok(view) => view.provider_processes,
        Err(error) => {
            tracing::warn!(run_id, section = "provider processes", %error, "run detail omitted a section");
            Vec::new()
        }
    };

    // A claimed follower leaf names the owner task and claim it works for;
    // every other run has none. The dashboard's cancel confirmation reads this,
    // because cancelling a claimed leaf fails the claim on the owner. An
    // unreadable admission is logged and shown as no claim, like `orbit run show`.
    let pull_claim = runtime.pull_leaf_claim(run_id).unwrap_or_else(|error| {
        tracing::warn!(run_id, %error, "pull admission unreadable; run detail shown without its claim");
        None
    });

    // A pull drain's launched leaves that are still running: what a graceful
    // cancel waits for, and what `force` would stop.
    let claimed_leaves = or_warn(
        runtime.pull_drain_claimed_leaves(run_id),
        run_id,
        "claimed leaves",
    );
    // Unlike the running-leaf table, admission capacity includes launching
    // and unsettled claims and other workspace leaves. An unreadable reading
    // stays unknown rather than advertising an empty budget.
    let pull_occupancy = or_warn(
        runtime.pull_drain_occupancy(run_id),
        run_id,
        "pull occupancy",
    )
    .map(|occupancy| json!({ "occupied": occupancy.occupied, "reviewing": occupancy.reviewing }));
    let pull_breaker = state
        .as_ref()
        .and_then(|state| state.pull_drain.pull_breaker.as_ref());
    let refused_settlements = or_warn(
        runtime.pull_drain_refused_settlements(run_id),
        run_id,
        "refused settlements",
    );

    // A pull drain's crew window: the crews it runs and those it excluded for
    // its window, with why [ORB-13941]. Null for every other run.
    let crew_window = runtime
        .pull_drain_crew_window(run_id)
        .unwrap_or_else(|error| {
            tracing::warn!(run_id, %error, "pull drain crew window unreadable; run detail shown without it");
            None
        });

    json!({
        "run": full,
        "pull_claim": pull_claim,
        "claimed_leaves": claimed_leaves,
        "pull_occupancy": pull_occupancy,
        "pull_breaker": pull_breaker,
        "refused_settlements": refused_settlements,
        "crew_window": crew_window,
        "steps": display_steps_with_agent_execution_to_json(&steps, state.as_ref(), &evidence),
        "steps_source": steps.source.as_str(),
        "provider_processes": provider_processes
            .iter()
            .map(RunProviderProcess::to_json)
            .collect::<Vec<_>>(),
    })
}

/// Run detail for a run this host's store records under a workspace no
/// registered runtime serves (a removed follower workspace). Same `{run,
/// steps}` contract as [`job_run_detail_to_json`], from the stored record
/// and pipeline state alone: the live sections (provider processes, pull
/// claim and drain occupancy) need the workspace's runtime and are empty.
/// `recorded_workspace` names the workspace the records are under.
pub(super) fn recorded_run_detail_to_json(
    reader: &OrbitRuntime,
    recorded: &RecordedJobRun,
) -> Value {
    let run = &recorded.run;
    let run_id = run.run_id.as_str();
    let state = recorded.state.as_ref();
    let mut full = project_job_run(
        run,
        JobRunProjectionProfile::FullDetailSiblingSteps,
        JobRunProjectionEvidence {
            state,
            activity_evidence: Some(&[]),
            ..JobRunProjectionEvidence::default()
        },
    );
    let titles = or_warn(
        super::tasks::task_titles(reader, std::iter::once(run)),
        run_id,
        "task titles",
    );
    super::tasks::add_tasks(&mut full, run, &titles);
    let steps = run_display_steps(run, Vec::new);
    json!({
        "run": full,
        "recorded_workspace": {
            "workspace_id": recorded.workspace_id,
            "registered": false,
        },
        "pull_claim": Value::Null,
        "claimed_leaves": [],
        "pull_occupancy": Value::Null,
        "pull_breaker": state.and_then(|state| state.pull_drain.pull_breaker.as_ref()),
        "refused_settlements": [],
        "crew_window": Value::Null,
        "steps": display_steps_with_agent_execution_to_json(&steps, state, &[]),
        "steps_source": steps.source.as_str(),
        "provider_processes": [],
    })
}

/// Steps-free rows for `child_ids`, indexed by run id.
///
/// A non-terminal row is replaced by [`OrbitRuntime::show_job_run`] so a
/// dead leaf does not stay running. A row that cannot be read is left out,
/// and the dispatch renders with null state and task labels.
fn load_child_runs(runtime: &OrbitRuntime, child_ids: &[String]) -> HashMap<String, JobRun> {
    if child_ids.is_empty() {
        return HashMap::new();
    }
    let loaded = match runtime.get_job_runs_by_id(child_ids) {
        Ok(runs) => runs,
        Err(error) => {
            for id in child_ids {
                warn_child_unavailable(id, &error);
            }
            return HashMap::new();
        }
    };
    let mut runs: HashMap<String, JobRun> = loaded
        .into_iter()
        .map(|run| (run.run_id.clone(), run))
        .collect();
    let loaded_ids: HashSet<String> = runs.keys().cloned().collect();
    let live: Vec<String> = runs
        .iter()
        .filter(|(_, run)| !run.state.is_terminal())
        .map(|(id, _)| id.clone())
        .collect();
    for id in live {
        match runtime.show_job_run(&id) {
            Ok(run) => {
                runs.insert(run.run_id.clone(), run);
            }
            Err(error) => {
                warn_child_unavailable(&id, &error);
                runs.remove(&id);
            }
        }
    }
    let mut missing = HashSet::new();
    for id in child_ids {
        if loaded_ids.contains(id) || !missing.insert(id.clone()) {
            continue;
        }
        let error = OrbitError::not_found(NotFoundKind::JobRun, id.clone());
        warn_child_unavailable(id, &error);
    }
    runs
}

fn warn_child_unavailable(child_run_id: &str, error: &OrbitError) {
    tracing::warn!(child_run_id, %error, "child run state and task labels unavailable");
}

/// The run detail still renders when one supplementary read fails, but the
/// failure is logged so it is not mistaken for "nothing recorded".
fn or_warn<T: Default>(
    result: Result<T, orbit_core::OrbitError>,
    run_id: &str,
    section: &str,
) -> T {
    result.unwrap_or_else(|error| {
        tracing::warn!(run_id, section, %error, "run detail omitted a section");
        T::default()
    })
}
