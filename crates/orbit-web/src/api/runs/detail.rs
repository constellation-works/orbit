//! Run detail JSON and supplementary audit projections.

use orbit_core::application::job::{job_run_to_json_with_activity_provenance, run_display_steps};
use orbit_core::runtime::audit::run::RunProviderProcess;
use orbit_core::{JobRun, OrbitRuntime};
use serde_json::{Value, json};

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
    let mut full = job_run_to_json_with_activity_provenance(run, state.as_ref(), &evidence);
    // Child dispatches persist lineage rather than task input. Resolve their
    // runs first, then share one task-title lookup with the parent header.
    let children = state
        .iter()
        .flat_map(|state| &state.child_dispatches)
        .filter_map(|dispatch| {
            runtime.show_job_run(&dispatch.child_run_id).map_or_else(
                |error| {
                    tracing::warn!(child_run_id = %dispatch.child_run_id, %error, "child run state and task labels unavailable");
                    None
                },
                Some,
            )
        })
        .collect::<Vec<_>>();
    let titles = or_warn(
        super::super::run_tasks::task_titles(runtime, std::iter::once(run).chain(&children)),
        run_id,
        "task titles",
    );
    super::super::run_tasks::add_tasks(&mut full, run, &titles);
    if let Some(dispatches) = full["child_dispatches"].as_array_mut() {
        for dispatch in dispatches {
            if let Some(child) = children
                .iter()
                .find(|child| dispatch["child_run_id"] == child.run_id)
            {
                super::super::run_tasks::add_tasks(dispatch, child, &titles);
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
    // Keep pass health visible after terminalization, independently of the
    // live waiting-state fields the shared run projection omits then.
    full["drain_last_pass"] = serde_json::to_value(
        state
            .as_ref()
            .and_then(|state| state.pull_drain.drain_last_pass.as_ref()),
    )
    .unwrap_or(Value::Null);
    // The failure chain and the cancel record are detail-only. One response
    // carries both, so the page does not fetch each child to find the cause.
    crate::api::run_failure::attach_run_failure_context(runtime, run, state.as_ref(), &mut full);
    // Reshape into `{run, steps}` per the dashboard contract: the displayed
    // steps replace the record's own `steps` in the flat projection.
    if let Some(fields) = full.as_object_mut() {
        fields.remove("steps");
    }
    let steps = run_display_steps(run, || {
        or_warn(
            runtime.collect_run_audit_steps(run_id),
            run_id,
            "audit steps",
        )
    });

    // [ORB-10496] Provider subprocesses for this run's agent steps, with a
    // liveness verdict for any that have not reported an exit. Without this a
    // healthy long-running ship-pipeline implementation agent is
    // indistinguishable from a dead child without shell access to the host.
    let provider_processes = or_warn(
        runtime.collect_run_provider_processes(run_id),
        run_id,
        "provider processes",
    );

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
        "crew_window": crew_window,
        "steps": steps.to_json(),
        "steps_source": steps.source.as_str(),
        "provider_processes": provider_processes
            .iter()
            .map(run_provider_process_to_json)
            .collect::<Vec<_>>(),
    })
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

fn run_provider_process_to_json(record: &RunProviderProcess) -> Value {
    json!({
        "run_id": record.run_id,
        "event_id": record.event_id,
        "ts": record.ts.map(|ts| ts.to_rfc3339()),
        "step_id": record.step_id,
        "step_index": record.step_index,
        "provider": record.provider,
        "pid": record.pid,
        "pid_start_time": record.pid_start_time,
        "finished": record.finished,
        "liveness": record.liveness.as_str(),
        "exit_code": record.exit_code,
        "timed_out": record.timed_out,
        "duration_ms": record.duration_ms,
    })
}
