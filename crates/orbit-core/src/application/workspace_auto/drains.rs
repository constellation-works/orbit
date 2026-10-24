use chrono::SecondsFormat;
use orbit_common::OrbitError;
use orbit_store::contracts::{DrainLeafOccupancy, JobRunQuery};
use orbit_types::workflow::{DrainAdmissionsStop, DrainWorkerLimit, JobRun, PipelineState};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::admission::pass_error::PassError;
use crate::application::contracts::PULL_DRAIN_JOB;
use crate::runtime::engine::crew::crew_allowlist_names_from_input;

/// The job that ships loose leaves. Its live runs are read for two things at
/// once: how many slots are occupied, and which backlog tasks are already
/// spoken for. The second matters because a leaf handed to a detached child
/// stays `backlog` until that child moves it to `in-progress` — the child's
/// own run input is the only record of the claim in between, and without it
/// the next iteration would hand the same task to a second child.
pub(super) const LEAF_JOB_NAME: &str = "task_auto_pipeline";

/// The drain job itself. Readiness reads its live run to report the ceiling a
/// running drain is actually admitting under [ORB-11253].
pub(super) const DRAIN_JOB_NAME: &str = "workspace_auto_pipeline";

/// Default ceiling on concurrently live leaf runs. Matches the `max_workers`
/// the fan-out used while the drain waited on its leaves, so steady-state
/// parallelism is unchanged; what changed is that a slot reopens the moment
/// its own child finishes rather than when the slowest child in the batch does.
pub(super) const DEFAULT_MAX_ACTIVE_LEAF_RUNS: u64 = 5;

/// Reads the engine-injected drain `run_id` from an activity input.
pub(super) fn drain_run_id(input: &Value) -> Option<&str> {
    input
        .get("run_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// The live worker ceiling an operator has set on *this* drain [ORB-11253].
///
/// The engine injects the executing run's id into every activity input, which
/// is the only handle the admission path has on the coordinator whose control
/// it must read. Absent or unreadable state degrades to the submitted ceiling:
/// a drain that cannot read its own control must keep admitting at the value it
/// was started with rather than stalling.
pub(super) fn live_worker_limit(runtime: &OrbitRuntime, input: &Value) -> Option<DrainWorkerLimit> {
    let run_id = drain_run_id(input)?;
    read_drain_worker_limit(runtime, run_id)
}

pub(super) fn live_admissions_stop(
    runtime: &OrbitRuntime,
    input: &Value,
) -> Option<DrainAdmissionsStop> {
    live_drain_state(runtime, input).and_then(|state| state.pull_drain.drain_admissions_stop)
}

/// The executing drain's own run state, read through the engine-injected run
/// id. Absent or unreadable state reads as no operator control at all.
pub(super) fn live_drain_state(runtime: &OrbitRuntime, input: &Value) -> Option<PipelineState> {
    let run_id = drain_run_id(input)?;
    runtime.read_run_state(run_id).ok().flatten()
}

/// The live drain run, with both the ceiling it was submitted with and the one
/// an operator has since set.
pub(super) struct ActiveDrain {
    pub(super) run_id: String,
    pub(super) input: Value,
    submitted: u32,
    pub(super) limit: Option<DrainWorkerLimit>,
    pub(super) stop: Option<DrainAdmissionsStop>,
}

impl ActiveDrain {
    pub(super) fn admissions_stopped(&self) -> bool {
        self.stop.is_some()
    }

    pub(super) fn effective_max_active_leaf_runs(&self) -> u32 {
        self.limit
            .as_ref()
            .map_or(self.submitted, |limit| limit.max_active_leaf_runs)
    }
}

/// Report the running coordinator separately from runs waiting for its slot,
/// oldest first. The job's active-run limit permits one running drain and
/// pending successors.
pub(super) fn workspace_drains(
    runtime: &OrbitRuntime,
) -> Result<(Option<ActiveDrain>, Vec<Value>), OrbitError> {
    let mut runs = runtime
        .stores()
        .jobs()
        .list_pending_or_running_job_runs(DRAIN_JOB_NAME)?;
    runs.extend(
        runtime
            .stores()
            .jobs()
            .list_job_runs_filtered(&JobRunQuery {
                job_id: Some(DRAIN_JOB_NAME.to_string()),
                state: Some(orbit_types::workflow::JobRunState::Retrying),
                include_steps: false,
                ..JobRunQuery::default()
            })?,
    );
    let mut queued = Vec::new();
    let mut active = None;
    for run in runs {
        if run.state == orbit_types::workflow::JobRunState::Pending {
            queued.push((
                (run.scheduled_at, run.created_at, run.run_id.clone()),
                queued_drain(runtime, &run),
            ));
            continue;
        }
        if !matches!(
            run.state,
            orbit_types::workflow::JobRunState::Running
                | orbit_types::workflow::JobRunState::Retrying
        ) {
            continue;
        }
        if active.is_some() {
            continue;
        }
        let input = run.input.unwrap_or_else(|| json!({}));
        let submitted = input
            .get("max_active_leaf_runs")
            .and_then(json_u32)
            .unwrap_or(DEFAULT_MAX_ACTIVE_LEAF_RUNS as u32);
        let state = runtime
            .stores()
            .jobs()
            .read_run_state(&run.run_id)
            .ok()
            .flatten();
        let limit = state
            .as_ref()
            .and_then(|state| state.pull_drain.drain_worker_limit.clone());
        let stop = state.and_then(|state| state.pull_drain.drain_admissions_stop);
        active = Some(ActiveDrain {
            run_id: run.run_id,
            input,
            submitted,
            limit,
            stop,
        });
    }
    // In the order the job's queue starts them.
    queued.sort_by(|(left, _), (right, _)| left.cmp(right));
    Ok((active, queued.into_iter().map(|(_, drain)| drain).collect()))
}

/// One drain waiting for the live one's slot, with what its window will do
/// once it starts: the ceiling, completion policy, crews and window length it
/// was submitted with, the `execution.env.pass` names its submission did not
/// hold (names only), and the Claude crews in its window that will start
/// without a worker credential.
fn queued_drain(runtime: &OrbitRuntime, run: &JobRun) -> Value {
    let input = run.input.as_ref();
    let field = |key: &str| input.and_then(|input| input.get(key));
    // An unreadable allowlist is reported as unrestricted; the drain itself
    // refuses it when it starts.
    let allowed_crews = input
        .and_then(|input| crew_allowlist_names_from_input(input).ok())
        .unwrap_or_default();
    let env_pass_unset = runtime
        .stores()
        .jobs()
        .read_run_state(&run.run_id)
        .ok()
        .flatten()
        .map(|state| state.env_pass_unset)
        .unwrap_or_default();
    json!({
        "run_id": run.run_id,
        "completion": field("completion").and_then(Value::as_str).unwrap_or("review"),
        "max_active_leaf_runs": field("max_active_leaf_runs")
            .and_then(json_u32)
            .unwrap_or(DEFAULT_MAX_ACTIVE_LEAF_RUNS as u32),
        // `null` when the window admits every crew.
        "allowed_crews": (!allowed_crews.is_empty()).then_some(&allowed_crews),
        // `0` is a single pass.
        "for_seconds": field("for_seconds").and_then(json_u32).unwrap_or(0),
        "submitted_at": run.created_at.to_rfc3339_opts(SecondsFormat::Secs, true),
        "uncredentialed_crews": runtime
            .claude_crews_without_recorded_credential(&allowed_crews, &env_pass_unset),
        "env_pass_unset": env_pass_unset,
    })
}

/// The live pull-drain run on a replica checkout, with its admissions stop
/// and the ceiling it is admitting under [ORB-16337].
pub(super) struct LivePullDrain {
    pub(super) run_id: String,
    pub(super) input: Value,
    pub(super) stop: Option<DrainAdmissionsStop>,
    /// Why its last pass requested no claim although its window was open
    /// [ORB-15637].
    pub(super) refusal: Option<String>,
    submitted: u32,
    /// An operator adjustment, when one has been accepted. It wins over
    /// `submitted`, the same way it does for a local drain.
    pub(super) limit: Option<DrainWorkerLimit>,
}

impl LivePullDrain {
    /// The ceiling this pull drain is admitting under: the operator
    /// adjustment when one is recorded, otherwise the value it was submitted
    /// with (the job default of 5 when the input omits it).
    pub(super) fn effective_max_active_leaf_runs(&self) -> u32 {
        self.limit
            .as_ref()
            .map_or(self.submitted, |limit| limit.max_active_leaf_runs)
    }
}

/// The running `workspace_pull_pipeline` run, if any. `orbit run auto --stop`
/// stops its admissions along with the auto coordinator's, so readiness must
/// show it or a replica's operator cannot tell a live drain from an idle one.
pub(super) fn live_pull_drain(runtime: &OrbitRuntime) -> Result<Option<LivePullDrain>, OrbitError> {
    let runs = runtime
        .stores()
        .jobs()
        .list_pending_or_running_job_runs(PULL_DRAIN_JOB)?;
    Ok(runs
        .into_iter()
        .find(|run| run.state == orbit_types::workflow::JobRunState::Running)
        .map(|run| {
            let input = run.input.unwrap_or_else(|| json!({}));
            let submitted = input
                .get("max_active_leaf_runs")
                .and_then(json_u32)
                .unwrap_or(DEFAULT_MAX_ACTIVE_LEAF_RUNS as u32);
            let state = runtime
                .stores()
                .jobs()
                .read_run_state(&run.run_id)
                .ok()
                .flatten();
            let refusal = state
                .as_ref()
                .and_then(|state| state.pull_drain.drain_last_pass.as_ref())
                .and_then(|pass| pass.admission_refusal.clone());
            let limit = state
                .as_ref()
                .and_then(|state| state.pull_drain.drain_worker_limit.clone());
            LivePullDrain {
                run_id: run.run_id,
                input,
                stop: state.and_then(|state| state.pull_drain.drain_admissions_stop),
                refusal,
                submitted,
                limit,
            }
        }))
}

fn read_drain_worker_limit(runtime: &OrbitRuntime, run_id: &str) -> Option<DrainWorkerLimit> {
    runtime
        .stores()
        .jobs()
        .read_run_state(run_id)
        .ok()
        .flatten()
        .and_then(|state| state.pull_drain.drain_worker_limit)
}

/// A durable run-input ceiling. The generic job surface persists every
/// `--input key=value` as a JSON string, so `"7"` must parse the same as `7`
/// ([ORB-11273]). Matches `job_input_u32` on the worker-limit write path.
fn json_u32(value: &Value) -> Option<u32> {
    match value {
        Value::Number(number) => number.as_u64().and_then(|value| u32::try_from(value).ok()),
        Value::String(text) => text.trim().parse::<u32>().ok(),
        _ => None,
    }
}

/// A live `task_auto_pipeline` run and the tasks it is carrying.
pub(crate) struct LiveLeafRun {
    pub(crate) run_id: String,
    pub(crate) task_ids: Vec<String>,
}

pub(super) fn live_leaf_runs(
    runtime: &OrbitRuntime,
    action: &str,
) -> Result<Vec<LiveLeafRun>, PassError> {
    // Reconcile first, unscoped: occupancy now counts every live leaf
    // definition (`task_pr_pipeline`, `task_local_pipeline`, and the claimed
    // pair), not just wrappers. One orphaned `running` row — a worker killed
    // by a reboot or an OOM — would occupy a slot forever, and the drain
    // would keep shipping at a quietly lower parallelism, which is exactly
    // the kind of thing nobody notices [ORB-12649].
    runtime
        .reconcile_stale_job_runs(None)
        .map_err(|error| PassError::store(action, "reconcile stale job runs", error))?;
    read_live_leaf_runs(runtime)
        .map_err(|error| PassError::store(action, format!("list live {LEAF_JOB_NAME} runs"), error))
}

/// The one capacity reading legacy and pull admission share [ORB-12617].
///
/// Wrapper runs alone are no longer the whole story: a pulled claim binds a
/// `task_claimed_*_pipeline` run with no wrapper above it, and an admission
/// that has been requested but has no run yet still holds a slot. Counting
/// those here — from the same store transaction the pull allocator checks — is
/// what stops the two paths from each admitting a full ceiling.
pub(super) fn shared_leaf_occupancy(
    runtime: &OrbitRuntime,
) -> Result<DrainLeafOccupancy, OrbitError> {
    runtime.stores().jobs().drain_leaf_occupancy()
}

pub(crate) fn read_live_leaf_runs(runtime: &OrbitRuntime) -> Result<Vec<LiveLeafRun>, OrbitError> {
    let runs = runtime
        .stores()
        .jobs()
        .list_pending_or_running_job_runs(LEAF_JOB_NAME)?;
    Ok(runs
        .into_iter()
        .map(|run| LiveLeafRun {
            run_id: run.run_id,
            task_ids: run
                .input
                .as_ref()
                .and_then(|input| input.get("task_ids"))
                .and_then(Value::as_array)
                .map(|ids| {
                    ids.iter()
                        .filter_map(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(ToOwned::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect())
}
