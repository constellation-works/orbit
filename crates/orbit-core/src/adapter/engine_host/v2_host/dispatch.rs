use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use orbit_common::OrbitError;
use orbit_common::protocol::tool_input::optional_string_list_alias;
use orbit_engine::DispatchError;
use orbit_tools::ToolContext;
use orbit_types::policy::Role;
use orbit_types::task::{
    TaskReferenceIndex, UnsatisfiableTaskDependency, unmet_task_dependencies_with_index,
    unsatisfiable_task_dependencies_with_index,
};
use orbit_types::tool::McpCapability;
use orbit_types::workflow::{CoreDeterministicAction, DeterministicAction};
use serde_json::Value;

use crate::OrbitRuntime;
use crate::runtime::task::locks::{
    EmptyTaskSurfacePolicy, TaskLockIndex, emit_expired_reservation_events,
    merge_task_lock_conflicts, parse_task_ids, requested_task_files_indexed, reserve_with_index,
    task_lock_conflicts_indexed, workspace_orbit_dir, workspace_task_reservation_id,
};

use super::{
    backlog_exclusion, ci_failure_tasks, dependabot_alert_tasks, pipeline_actions, scan_unresolved,
    task_pilot, triage, workspace_auto,
};

/// Whether `action` is dispatchable by this runtime — the capability probe
/// behind `RuntimeHost::has_deterministic_action` [ORB-10385].
pub(crate) fn is_deterministic_action_registered(action: &str) -> bool {
    DeterministicAction::parse(action).is_some()
}

pub(crate) fn run_deterministic(
    runtime: &OrbitRuntime,
    action: &str,
    config: &Value,
    input: &Value,
    mut tool_context: ToolContext,
) -> Result<Value, DispatchError> {
    let Some(DeterministicAction::Core(deterministic_action)) = DeterministicAction::parse(action)
    else {
        return Err(DispatchError::DeterministicActionNotRegistered(
            action.to_string(),
        ));
    };
    // ORB-10453: this is the run's own machinery, not the agent it hosts, so
    // it carries `Runner` — the grant that lets a run perform the destruction
    // it exists to perform (`release_locks` frees another run's reservation).
    // The sanction travels with the dispatcher's tool context rather than with
    // ambient process state, so an agent inside the same run does not inherit
    // it: `run_agent_loop_activity` builds its own context and never lands here.
    tool_context
        .session_context
        .effective_capabilities
        .insert(McpCapability::Runner);
    if matches!(
        deterministic_action,
        CoreDeterministicAction::PrepareTaskPilot
            | CoreDeterministicAction::ApplyTaskPilotResults
            | CoreDeterministicAction::ListTriageCandidates
            | CoreDeterministicAction::ApplyTriageDispositions
    ) {
        let claim_input = input.get("prepared").unwrap_or(input);
        if let Some(claim) = crate::application::automation::members::claim(runtime, claim_input)
            .map_err(|error| DispatchError::DeterministicActionFailed {
                action: action.into(),
                message: error.to_string(),
            })?
        {
            let owner = tool_context
                .reservation_owner
                .as_ref()
                .map(|owner| owner.owner_run_id.as_str());
            if owner.is_none() || owner != claim.action_id.as_deref() {
                return Err(DispatchError::DeterministicActionFailed {
                    action: action.into(),
                    message: "state claim belongs to another run".into(),
                });
            }
        }
    }
    match deterministic_action {
        CoreDeterministicAction::OrbitToolCall => {
            // The `config` block shape (see deterministic_reference.yaml):
            //   config: { tool_name: <name>, args: <object> }
            // Input overrides config when both are present.
            let tool_name = input
                .get("tool_name")
                .or_else(|| config.get("tool_name"))
                .and_then(Value::as_str)
                .ok_or_else(|| DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: "missing `tool_name` in config or input".to_string(),
                })?;
            let args = input
                .get("args")
                .or_else(|| config.get("args"))
                .cloned()
                .unwrap_or(Value::Null);

            runtime
                .run_tool_with_context_and_role(tool_name, args, Role::Admin, tool_context)
                .map_err(|err| DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: format!("{err}"),
                })
        }
        // Retired Phase 4 stubs. These used to return structured skipped
        // success, which made unavailable git/API behavior look like a
        // completed deterministic action. Keep the action names registered
        // so legacy assets fail with an actionable message instead of an
        // "unknown action" error.
        CoreDeterministicAction::PromoteAgentMain => {
            let target = input
                .get("target_branch")
                .and_then(Value::as_str)
                .unwrap_or("main");
            let source = input
                .get("source_branch")
                .and_then(Value::as_str)
                .unwrap_or("agent-main");
            Err(DispatchError::DeterministicActionFailed {
                action: action.to_string(),
                message: format!(
                    "deterministic action `promote_agent_main` is a retired stub; refusing to report promotion from `{source}` to `{target}` as skipped success. Use shipped `git_merge` plus `git_push`, or the `pr_open` workflow, for supported v2 git flow."
                ),
            })
        }
        CoreDeterministicAction::RevertOnRed => {
            let sha = input
                .get("commit_sha")
                .and_then(Value::as_str)
                .unwrap_or("");
            Err(DispatchError::DeterministicActionFailed {
                action: action.to_string(),
                message: format!(
                    "deterministic action `revert_on_red` is a retired stub; no automatic revert implementation ships today, so commit `{sha}` was not reverted. Use an explicit git revert/manual incident task or add a real deterministic action before wiring this workflow."
                ),
            })
        }
        CoreDeterministicAction::ContextConflictCheck => {
            let task_ids = parse_task_ids(input).map_err(|error| {
                DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: error.to_string(),
                }
            })?;
            let index = TaskLockIndex::load(runtime).map_err(|error| {
                DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: error.to_string(),
                }
            })?;
            let repo_root = runtime.paths().repo_root.as_path();
            let requested_files = requested_task_files_indexed(&index, &task_ids, repo_root)
                .map_err(|error| DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: error.to_string(),
                })?;
            let task_conflicts =
                task_lock_conflicts_indexed(&index, &task_ids, &requested_files, repo_root);
            runtime
                .reconcile_stale_owned_reservations_for_files(&requested_files, 32)
                .map_err(|error| DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: error.to_string(),
                })?;
            let reservation_check = runtime
                .stores()
                .task_reservations()
                .check_task_reservation_conflicts(
                    orbit_store::contracts::TaskReservationCheckParams {
                        workspace_orbit_dir: workspace_orbit_dir(runtime),
                        workspace_id: workspace_task_reservation_id(runtime).map_err(|error| {
                            DispatchError::DeterministicActionFailed {
                                action: action.to_string(),
                                message: error.to_string(),
                            }
                        })?,
                        requested_files,
                    },
                )
                .map_err(|error| DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: error.to_string(),
                })?;
            emit_expired_reservation_events(runtime, &reservation_check.expired_reservations)
                .map_err(|error| DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: error.to_string(),
                })?;
            let conflicts = merge_task_lock_conflicts(task_conflicts, reservation_check.conflicts);
            Ok(serde_json::json!({
                "clear": conflicts.is_empty(),
                "conflicts": conflicts,
            }))
        }
        CoreDeterministicAction::Sleep => {
            let seconds = input
                .get("seconds")
                .and_then(Value::as_f64)
                .ok_or_else(|| DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: "missing `seconds`".to_string(),
                })?;
            if !(0.0..=3600.0).contains(&seconds) {
                return Err(DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: "`seconds` must be between 0 and 3600".to_string(),
                });
            }
            let started_at = Instant::now();
            std::thread::sleep(Duration::from_secs_f64(seconds));
            Ok(serde_json::json!({
                "slept_seconds": started_at.elapsed().as_secs_f64(),
            }))
        }
        // Turn one host-collected CI evidence snapshot into ordinary backlog
        // bug tasks [ORB-11107]. All GitHub access already happened in the
        // engine-private `collect_ci_evidence` step; this action only reads
        // that JSON and writes tasks.
        CoreDeterministicAction::FileCiFailureTasks => {
            ci_failure_tasks::file_ci_failure_tasks(runtime, input).map_err(|error| {
                DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: error.to_string(),
                }
            })
        }
        CoreDeterministicAction::ConsolidateCodeScanningTasks => {
            dependabot_alert_tasks::consolidate::consolidate_code_scanning_tasks(runtime, input)
                .map_err(|error| DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: error.to_string(),
                })
        }
        CoreDeterministicAction::FileDependabotAlertTasks => {
            dependabot_alert_tasks::file_dependabot_alert_tasks(runtime, input).map_err(|error| {
                DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: error.to_string(),
                }
            })
        }
        // Fire every due, enabled auto-task definition and mint a task from
        // its template [ORB-10149]. Reads definitions from this workspace's
        // `.orbit/auto_tasks/`; catch-up collapses and `skip_if_open` dedupe
        // are enforced in the scheduler core.
        CoreDeterministicAction::RunAutoTaskScheduler => {
            crate::application::auto_tasks::run_scheduler_action_json(runtime, input).map_err(
                |error| DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: error.to_string(),
                },
            )
        }
        // The admissible work for one drain iteration [ORB-10819]: the
        // conflict-free backlog leaves, plus one backlog epic root when no
        // `epic_pipeline` run is live. Leaves and epic are independent — an
        // active epic excludes overlapping leaves through its lock
        // reservation, not through a blanket hold.
        CoreDeterministicAction::ClassifyWorkspaceAutoTasks => {
            workspace_auto::classify_workspace_auto_tasks(runtime, action, input)
        }
        // Stamp a drain deadline, or answer whether a stamped one has passed
        // [ORB-10819]. Gates the start of the next iteration only; nothing
        // here cancels or shortens an in-flight child run.
        CoreDeterministicAction::DrainWindow => {
            workspace_auto::drain_window(runtime, action, input)
        }
        // ADR-0223: scheduled shipment resolves only the active runtime's
        // canonical ship input; cross-workspace enumeration stays in the
        // legacy CLI sweep and `workflow.auto_ship` is deliberately ignored.
        CoreDeterministicAction::ResolveWorkspaceShipInput => {
            resolve_workspace_ship_input(runtime, action)
        }
        // Materialize the workspace backlog for auto-dispatch.
        // Filters by `status: backlog`. In automatic mode, drops any backlog
        // task group whose context overlaps files
        // already held by `in-progress`/`review` tasks. Sorts critical →
        // high → medium → low then by `created_at` ascending so older
        // high-priority work ships first. Caps at `max_tasks` (default 50).
        CoreDeterministicAction::ListBacklogTasks => {
            backlog_exclusion::list_backlog_tasks(runtime, action, input)
        }
        // Resolve one epic's unfinished descendants once, in a deterministic
        // dependency-first order, so the enclosing workflow can drain them
        // with an ordinary sequential loop.
        CoreDeterministicAction::ListEpicDescendants => {
            workspace_auto::list_epic_descendants(runtime, action, input)
        }
        // Materialize blocked tasks attributable to a terminally-failed job
        // run for the triage pipeline [ORB-10129]. Human-blocked tasks (no
        // `job_run_id`, or a non-failed run) never appear; tasks whose
        // re-backlog budget is exhausted take the gave-up path here.
        CoreDeterministicAction::ListTriageCandidates => {
            triage::list_triage_candidates(runtime, action, input)
        }
        // Workspace drain scan [ORB-10779]: proposed/backlog/blocked
        // tasks, failed/timeout job-runs, and unresolved check_later notes.
        // Read-only; empty is success. Optional `fail_if_nonempty` fails
        // closed for a workspace-wide leftover set; `epic_pipeline` now
        // gates on `list_epic_descendants` instead [ORB-10818].
        CoreDeterministicAction::ScanUnresolvedWork => {
            scan_unresolved::scan_unresolved_work(runtime, action, input)
        }
        // Apply the triage agent's per-task verdicts under deterministic
        // bounds: candidates-only, `environmental`-only re-backlog, durable
        // re-backlog budget, idempotent under overlap [ORB-10129].
        CoreDeterministicAction::ApplyTriageDispositions => {
            triage::apply_triage_dispositions(runtime, action, input)
        }
        // Materialize a workspace-scoped task-pilot working set and partition
        // it into bounded groups without promoting or dispatching any task.
        CoreDeterministicAction::PrepareTaskPilot => task_pilot::prepare(runtime, action, input),
        // Validate all agent proposals before writing, then replace only the
        // exact prepared tasks' context_files fields.
        CoreDeterministicAction::ApplyTaskPilotResults => task_pilot::apply(runtime, action, input),
        // [ORB-11333] Reserve a fresh reviewer start for the committed,
        // base-synchronized candidate and hand it a pinned manifest; then
        // settle the reviewer's report into an honest verdict, reviewer-
        // attributed repair commits, and a certificate the PR steps recheck.
        CoreDeterministicAction::ReviewGateAdmit => {
            crate::application::review::review_gate_admit(runtime, action, input)
        }
        CoreDeterministicAction::ReviewGateSettle => {
            crate::application::review::review_gate_settle(runtime, action, input)
        }
        // Guard the auto-dispatch bundle output before fan_out.
        // Rejects duplicated task_ids, unknown ids, and oversize
        // bundles with a structured error so a misgrouped backlog
        // never silently dispatches.
        CoreDeterministicAction::ValidateBundles => {
            pipeline_actions::validate_bundles(action, input)
        }
        // Thin passthrough over `orbit.task.locks.reserve`. Exists as a
        // dedicated action (rather than a `orbit_tool_call` config) so a
        // workflow inside a `loop:` with `break_when:` can reference
        // `steps.<id>.output.reserved` directly without leaking the
        // generic `{tool_name, args}` envelope into the activity's
        // input_schema.
        CoreDeterministicAction::ReserveLocks => {
            let admission = dependency_admission_for_input(runtime, input).map_err(|err| {
                DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: format!("{err}"),
                }
            })?;
            // Fail before the first poll: an archived/rejected/dangling
            // `blocked_by` target cannot be cleared by waiting, so returning
            // `reserved: false` here would burn the gate's whole iteration
            // budget and then report a file conflict that does not exist.
            if !admission.unsatisfiable.is_empty() {
                update_run_waiting_reasons(
                    runtime,
                    input,
                    non_empty(
                        admission
                            .unsatisfiable
                            .iter()
                            .map(|dependency| dependency.dependency_id.clone())
                            .collect(),
                    ),
                    None,
                    action,
                )?;
                return Err(DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: admission.unsatisfiable_message(),
                });
            }
            if !admission.waiting_on.is_empty() {
                update_run_waiting_reasons(
                    runtime,
                    input,
                    Some(admission.waiting_on.clone()),
                    None,
                    action,
                )?;
                return Ok(serde_json::json!({
                    "reserved": false,
                    "waiting_on_deps": admission.waiting_on,
                    "conflicts": [],
                }));
            }

            let lock_index = TaskLockIndex::load(runtime).map_err(|err| {
                DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: format!("{err}"),
                }
            })?;
            let mut output = reserve_with_index(
                runtime,
                input.clone(),
                tool_context.agent_name.clone(),
                tool_context.model_name.clone(),
                tool_context.reservation_owner.clone(),
                &lock_index,
                EmptyTaskSurfacePolicy::Admit,
            )
            .map_err(|err| DispatchError::DeterministicActionFailed {
                action: action.to_string(),
                message: format!("{err}"),
            })?;
            // Always publish `waiting_on_deps` (empty here) so the gate
            // pipeline can reference `steps.reserve.output.waiting_on_deps`
            // unconditionally, whichever branch produced the output.
            if let Some(object) = output.as_object_mut() {
                object
                    .entry("waiting_on_deps")
                    .or_insert_with(|| Value::Array(Vec::new()));
            }
            let waiting_on_locks = waiting_locks_from_reserve_output(&output);
            update_run_waiting_reasons(runtime, input, None, non_empty(waiting_on_locks), action)?;
            Ok(output)
        }
        // Thin passthrough over `orbit.task.locks.release` so workflows
        // can free admission-window reservations after child runs finish.
        CoreDeterministicAction::ReleaseLocks => runtime
            .run_tool_with_context_and_role(
                "orbit.task.locks.release",
                input.clone(),
                Role::Admin,
                tool_context,
            )
            .map_err(|err| DispatchError::DeterministicActionFailed {
                action: action.to_string(),
                message: format!("{err}"),
            }),
        // Submit a child v2 Job and block on its terminal state.
        // Chains `orbit.pipeline.invoke` + `orbit.pipeline.wait` so
        // workflows can model "dispatch and join" as a single step
        // with `{status, run_id, pipeline?, error?}` output.
        CoreDeterministicAction::InvokeAndWait => {
            pipeline_actions::invoke_and_wait(runtime, action, input, tool_context)
        }
        // Submit a child v2 Job and return as soon as its Run is durable, so
        // the parent keeps working while it executes [ORB-10819]. The caller
        // re-observes the child; this returns no status.
        CoreDeterministicAction::InvokeDetached => {
            pipeline_actions::invoke_detached(runtime, action, input, tool_context)
        }
        // Fail a workflow if child wait results did not reach `succeeded`, or
        // apply the explicit workspace-sequencer recording policy.
        CoreDeterministicAction::PipelineSuccessGuard => {
            let output = pipeline_actions::pipeline_success_guard(action, input)?;
            pipeline_actions::record_pipeline_results_audit(runtime, action, input, &output)?;
            Ok(output)
        }
        // Post-loop gate signal: the admission window never opened in
        // time. Emits a `gate.starvation` audit event with task_ids and
        // conflicting_files so an epic-orchestrator parent can decide
        // to replan, then fails the Run with a structured error.
        CoreDeterministicAction::GateStarvationFail => {
            pipeline_actions::gate_starvation_fail(runtime, action, input)
        }
    }
}

/// [ORB-11187] This activity resolves the *workspace's* shipping defaults —
/// mode and base branch. Completion authority is deliberately not among them:
/// it is granted per invocation by an operator and reaches the pipelines through
/// run input, so no workspace configuration can silently turn it on.
const COMPLETION_IS_NEVER_WORKSPACE_RESOLVED: crate::application::workflow::CompletionPolicy =
    crate::application::workflow::CompletionPolicy::Review;

fn resolve_workspace_ship_input(
    runtime: &OrbitRuntime,
    action: &str,
) -> Result<Value, DispatchError> {
    if let Some(binding) = runtime.workspace_runtime_binding() {
        return crate::application::workflow::build_ship_input(
            binding.ship_mode,
            runtime.workflow_base_branch(),
            &[],
            COMPLETION_IS_NEVER_WORKSPACE_RESOLVED,
            &[],
        )
        .map_err(|error| DispatchError::DeterministicActionFailed {
            action: action.to_string(),
            message: format!("resolve workspace ship input: {error}"),
        });
    }

    crate::application::workflow::build_ship_input(
        crate::application::workflow::ShipMode::Local,
        runtime.workflow_base_branch(),
        &[],
        COMPLETION_IS_NEVER_WORKSPACE_RESOLVED,
        &[],
    )
    .map_err(|error| DispatchError::DeterministicActionFailed {
        action: action.to_string(),
        message: format!("resolve workspace ship input: {error}"),
    })
}

/// The dependency picture for a bundle, split by what the caller should do
/// about it.
///
/// `waiting_on` is the poll-and-retry set; `unsatisfiable` is the fail-now set.
/// An edge appears in exactly one of them — every unsatisfiable edge is also
/// unmet, but it is reported only as unsatisfiable so the two causes never
/// blur together in a diagnostic.
#[derive(Debug, Default)]
pub(super) struct BundleDependencyAdmission {
    pub(super) waiting_on: Vec<String>,
    pub(super) unsatisfiable: Vec<UnsatisfiableTaskDependency>,
}

impl BundleDependencyAdmission {
    /// Failure message for the unsatisfiable set. Prefixed with a stable
    /// `task.dependencies.unsatisfiable:` marker so an operator (or an
    /// epic-level orchestrator parsing run errors) can tell this apart from
    /// `gate.starvation`, which means the opposite thing: waiting was
    /// legitimate but ran out of budget.
    pub(super) fn unsatisfiable_message(&self) -> String {
        let labels = self
            .unsatisfiable
            .iter()
            .map(UnsatisfiableTaskDependency::label)
            .collect::<Vec<_>>()
            .join("; ");
        format!(
            "task.dependencies.unsatisfiable: {labels}. \
             These dependency edges can never reach 'done', so waiting cannot clear them \
             (no file-lock conflict is involved). Fix the task graph and re-dispatch."
        )
    }
}

fn dependency_admission_for_input(
    runtime: &OrbitRuntime,
    input: &Value,
) -> Result<BundleDependencyAdmission, OrbitError> {
    let Some(raw_task_ids) =
        optional_string_list_alias(input, &["task_ids", "taskIds", "task-ids"])?
    else {
        return Ok(BundleDependencyAdmission::default());
    };
    let task_ids = parse_task_ids(&serde_json::json!({ "task_ids": raw_task_ids }))?;
    let status_by_id = runtime.task_status_index()?;
    let reference_index = TaskReferenceIndex::from_status_index(&status_by_id);
    let mut waiting_on = BTreeSet::new();
    let mut unsatisfiable = Vec::new();
    for task_id in task_ids {
        let task = runtime.get_task(&task_id)?;
        let dead_ends =
            unsatisfiable_task_dependencies_with_index(&task, &status_by_id, &reference_index);
        let dead_end_ids = dead_ends
            .iter()
            .map(|dependency| dependency.dependency_id.clone())
            .collect::<BTreeSet<_>>();
        for dependency in unmet_task_dependencies_with_index(&task, &status_by_id, &reference_index)
        {
            if !dead_end_ids.contains(&dependency.id) {
                waiting_on.insert(dependency.id);
            }
        }
        unsatisfiable.extend(dead_ends);
    }
    Ok(BundleDependencyAdmission {
        waiting_on: waiting_on.into_iter().collect(),
        unsatisfiable,
    })
}

// `pub(super)` (not private): the sibling `tests/dispatch.rs` unit-tests this
// pure parsing helper directly rather than through a full lock-conflict
// integration setup — see docs/design-patterns/test_layout.md migration
// recipe step 6.
pub(super) fn waiting_locks_from_reserve_output(output: &Value) -> Vec<String> {
    output
        .get("conflicts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|conflict| conflict.get("file").and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

// `pub(super)` (not private): the sibling `tests/dispatch.rs` unit-tests this
// writer directly for no-op, error, lock, and lost-update coverage rather
// than only through a full `reserve_locks` setup — see
// docs/design-patterns/test_layout.md migration recipe step 6.
pub(super) fn update_run_waiting_reasons(
    runtime: &OrbitRuntime,
    input: &Value,
    waiting_on_deps: Option<Vec<String>>,
    waiting_on_locks: Option<Vec<String>>,
    action: &str,
) -> Result<(), DispatchError> {
    let Some(run_id) = input.get("run_id").and_then(Value::as_str) else {
        return Ok(());
    };
    // Same transactional primitive as checkpoints and child dispatch: a
    // separate read then whole-document write would discard a concurrent
    // admissions-stop or worker-limit that landed in between.
    let mut waiting_on_deps = waiting_on_deps;
    let mut waiting_on_locks = waiting_on_locks;
    runtime
        .stores()
        .jobs()
        .update_run_state(run_id, &mut |_, state| {
            state.set_waiting_reasons(waiting_on_deps.take(), waiting_on_locks.take());
            Ok(())
        })
        .map(|_| ())
        .map_err(|err| DispatchError::DeterministicActionFailed {
            action: action.to_string(),
            message: format!("{err}"),
        })
}

fn non_empty(values: Vec<String>) -> Option<Vec<String>> {
    (!values.is_empty()).then_some(values)
}
