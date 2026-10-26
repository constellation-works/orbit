use std::collections::BTreeSet;
use std::sync::Arc;

use orbit_common::governance::authorization::{self, GrantedToolEffect, ToolGrantability};
use orbit_engine::{DispatchError, ResolvedActivityTools};
use orbit_tools::{ActivityBinding, FsAuditLogger, ReservationOwnerContext, ToolContext};
use orbit_types::policy::UNRESTRICTED_FS_PROFILE;
use orbit_types::task::Task;
use orbit_types::tool::ToolSessionContext;
use orbit_types::workflow::{ActivityToolDenyPolicy, tool_allowed, tools_allowed_by_disallow_list};

use crate::OrbitRuntime;
use crate::adapter::tool_host::build_orbit_tool_host;
use crate::application::job::{drops_disallowed_requirements, granted_tool_refusal};

/// The one activity a task's `granted_tools` reach. Every other activity,
/// including reviewers, recovery and pilot, never reads a grant.
const GRANT_LIFTING_ACTIVITY: &str = "agent_implement";

pub(super) fn resolve_activity_tools(
    runtime: &OrbitRuntime,
    task_ids: &[String],
    baseline_tools: &[String],
) -> Result<ResolvedActivityTools, DispatchError> {
    let tasks = load_tasks(runtime, task_ids)?;
    let (requested_tools, omitted_requirement_notes) = admit_required_tools(runtime, &tasks, None)?;
    if requested_tools.is_empty() {
        return Ok(ResolvedActivityTools {
            requested_tools,
            effective_tools: baseline_tools.to_vec(),
            omitted_requirement_notes,
            granted_tools: Vec::new(),
        });
    }

    let mut seen = std::collections::BTreeSet::new();
    let mut effective_tools = Vec::with_capacity(baseline_tools.len() + requested_tools.len());
    for tool in baseline_tools.iter().chain(requested_tools.iter()) {
        if seen.insert(tool.clone()) {
            effective_tools.push(tool.clone());
        }
    }
    Ok(ResolvedActivityTools {
        requested_tools,
        effective_tools,
        omitted_requirement_notes,
        granted_tools: Vec::new(),
    })
}

/// Deny mode: every registered agent-facing tool the disallow list does not
/// cover. A task requirement is already inside that set unless the list
/// covers it, and a requirement never overrides the activity's disallow list.
///
/// `required_tools` are the implementing activity's contract. `agent_implement`
/// and any activity outside the shipped non-implementer set still fail closed
/// when the list covers a requirement. A non-implementer drops that
/// requirement from `requested_tools` and records a note instead, so final
/// recovery, step recovery and reviewers can start for a task whose
/// implementer needs a tool they must not call. [ORB-15162]
///
/// For `agent_implement` only, an operator's `granted_tools` lift their exact
/// disallow entries first, so the callable set is the baseline plus required
/// plus granted names. A lift is list membership only: the tool still passes
/// its capability, process, coordination and claimed-mode checks at call time.
pub(super) fn resolve_activity_tool_denials(
    runtime: &OrbitRuntime,
    task_ids: &[String],
    activity: &str,
    disallow_list: &[String],
) -> Result<ResolvedActivityTools, DispatchError> {
    let tasks = load_tasks(runtime, task_ids)?;
    let grants = if activity == GRANT_LIFTING_ACTIVITY {
        resolve_granted_deny_lifts(runtime, &tasks, activity, disallow_list)?
    } else {
        GrantedDenyLifts::default()
    };
    let deny_policy = ActivityToolDenyPolicy {
        activity: activity.to_string(),
        disallow_list: disallow_list
            .iter()
            .filter(|entry| !grants.lifted.contains(*entry))
            .cloned()
            .collect(),
    };
    let (requested_tools, omitted_requirement_notes) =
        admit_required_tools(runtime, &tasks, Some(&deny_policy)).map_err(|error| match error {
            DispatchError::RequiredToolAdmission {
                task_id,
                tool_name,
                reason,
            } if grants.held_mutating.contains(&tool_name) => {
                DispatchError::RequiredToolAdmission {
                    reason: format!(
                        "{reason}; its grant is classified mutating, and a mutating granted tool is never called live"
                    ),
                    task_id,
                    tool_name,
                }
            }
            other => other,
        })?;
    let registered_tools = runtime.allowlist_known_tool_names();
    Ok(ResolvedActivityTools {
        requested_tools,
        effective_tools: tools_allowed_by_disallow_list(
            &deny_policy.disallow_list,
            registered_tools.iter().map(String::as_str),
        ),
        omitted_requirement_notes,
        granted_tools: grants.lifted.into_iter().collect(),
    })
}

fn load_tasks(runtime: &OrbitRuntime, task_ids: &[String]) -> Result<Vec<Task>, DispatchError> {
    task_ids
        .iter()
        .map(|task_id| {
            runtime.get_task(task_id).map_err(|error| {
                DispatchError::CliInvocationFailed(format!(
                    "load task `{task_id}` tool requirements: {error}"
                ))
            })
        })
        .collect()
}

/// Granted names whose disallow entries the implementer lifts, and covered
/// grants it keeps denied because they are classified mutating.
#[derive(Default)]
struct GrantedDenyLifts {
    lifted: BTreeSet<String>,
    held_mutating: BTreeSet<String>,
}

/// Classify every selected task's grant against the implementer's disallow
/// list. A grant the list does not cover changes nothing. A covered
/// read/repeat-safe grant lifts its exact entry; a covered mutating grant
/// stays denied, because a mutation never runs live before review. A grant
/// that is unavailable, now always operator-only, or still covered by a
/// wildcard entry refuses admission rather than starting a run without it.
fn resolve_granted_deny_lifts(
    runtime: &OrbitRuntime,
    tasks: &[Task],
    activity: &str,
    disallow_list: &[String],
) -> Result<GrantedDenyLifts, DispatchError> {
    let mut grants = GrantedDenyLifts::default();
    for task in tasks {
        for tool_name in &task.granted_tools {
            let refuse = |reason: String| DispatchError::RequiredToolAdmission {
                task_id: task.id.clone(),
                tool_name: tool_name.clone(),
                reason: format!("granted tool: {reason}"),
            };
            let unavailable = granted_tool_refusal(runtime, tool_name).map_err(|error| {
                DispatchError::CliInvocationFailed(format!(
                    "read granted tool `{tool_name}` admission state for task `{}`: {error}",
                    task.id
                ))
            })?;
            if let Some(reason) = unavailable {
                return Err(refuse(reason));
            }
            if !tool_allowed(tool_name, disallow_list) {
                continue;
            }
            let classification = authorization::classify_agent_tool_grant(tool_name);
            if classification.grantability == ToolGrantability::AlwaysOperatorOnly {
                return Err(refuse(
                    "the tool is always operator-only, so a grant cannot lift its denial".into(),
                ));
            }
            if let Some(entry) = disallow_list.iter().find(|entry| {
                *entry != tool_name && tool_allowed(tool_name, std::slice::from_ref(*entry))
            }) {
                return Err(refuse(format!(
                    "a grant lifts only an exact disallow entry, and `{entry}` still covers it for {activity}"
                )));
            }
            if classification.effect == GrantedToolEffect::Mutating {
                grants.held_mutating.insert(tool_name.clone());
                continue;
            }
            grants.lifted.insert(tool_name.clone());
        }
    }
    Ok(grants)
}

/// Admit every selected task's `required_tools`: exact canonical names of
/// registered, agent-facing, enabled tools. A deny-mode activity that is not
/// the implementer omits a tool its disallow list covers and returns a note
/// for each omission. Returns the admitted names sorted and deduped, plus
/// those notes in task order.
fn admit_required_tools(
    runtime: &OrbitRuntime,
    tasks: &[Task],
    deny_policy: Option<&ActivityToolDenyPolicy>,
) -> Result<(Vec<String>, Vec<String>), DispatchError> {
    let mut requested_tools = BTreeSet::new();
    let mut omitted_requirement_notes = Vec::new();
    let drop_denied =
        deny_policy.is_some_and(|policy| drops_disallowed_requirements(&policy.activity));
    for task in tasks {
        let task_id = &task.id;
        for tool_name in task.required_tools.iter().cloned() {
            let reason = granted_tool_refusal(runtime, &tool_name).map_err(|error| {
                DispatchError::CliInvocationFailed(format!(
                    "read tool `{tool_name}` admission state for task `{task_id}`: {error}"
                ))
            })?;
            if let Some(reason) = reason {
                return Err(DispatchError::RequiredToolAdmission {
                    task_id: task_id.clone(),
                    tool_name,
                    reason,
                });
            }
            if let Some(policy) = deny_policy.filter(|policy| policy.denies(&tool_name)) {
                if drop_denied {
                    let note = format!(
                        "task `{task_id}` required tool `{tool_name}` dropped from requested_tools: {}; required_tools bind the implementing activity and never override a disallow list",
                        policy.denial_message(&tool_name)
                    );
                    tracing::warn!("{note}");
                    omitted_requirement_notes.push(note);
                    continue;
                }
                let reason = policy.denial_message(&tool_name);
                return Err(DispatchError::RequiredToolAdmission {
                    task_id: task_id.clone(),
                    tool_name,
                    reason,
                });
            }
            requested_tools.insert(tool_name);
        }
    }
    Ok((
        requested_tools.into_iter().collect(),
        omitted_requirement_notes,
    ))
}

pub(super) fn tool_context_for_activity(
    runtime: &OrbitRuntime,
    run_id: Option<&str>,
    fs_profile: Option<&str>,
    fs_audit: Option<Arc<dyn FsAuditLogger>>,
    proc_allowed_programs: Option<&[String]>,
) -> ToolContext {
    let workspace_root = runtime
        .paths()
        .repo_root
        .canonicalize()
        .unwrap_or_else(|_| runtime.paths().repo_root.clone());

    // Every context built here belongs to a v2 activity, so `proc.spawn` is
    // always activity-scoped: a missing program policy denies every program
    // instead of degrading to allow-all. The caller sets deny mode on this
    // context when the activity declares `proc_disallowed_programs`; asset
    // load refuses a `proc.spawn` grant without either policy ([ORB-10959]).
    let proc_spawn_activity_scoped = true;
    let proc_allowed_programs = proc_allowed_programs
        .map(|programs| programs.to_vec())
        .unwrap_or_default();
    let proc_spawn_environment = Some(runtime.agent_environment(&[]));
    let run_id = run_id.map(str::trim).filter(|value| !value.is_empty());

    ToolContext {
        cwd: std::env::current_dir()
            .ok()
            .map(|cwd| cwd.to_string_lossy().into_owned()),
        workspace_root: Some(workspace_root),
        policy_engine: Some(Arc::new(runtime.policy_engine().clone())),
        fs_profile: Some(fs_profile.unwrap_or(UNRESTRICTED_FS_PROFILE).to_string()),
        fs_audit,
        proc_allowed_programs,
        proc_spawn_environment,
        proc_spawn_activity_scoped,
        reservation_owner: run_id.map(|owner_run_id| ReservationOwnerContext {
            owner_run_id: owner_run_id.to_string(),
            owner_metadata_json: Some(
                serde_json::json!({
                    "source": "v2_activity",
                    "fs_profile": fs_profile.unwrap_or(UNRESTRICTED_FS_PROFILE),
                })
                .to_string(),
            ),
        }),
        // The dispatcher adds the task from the activity's run input; the
        // run is this context's own, never a tool's.
        activity_binding: run_id.map(|job_run_id| ActivityBinding {
            job_run_id: job_run_id.to_string(),
            task_id: None,
        }),
        orbit_host: Some(build_orbit_tool_host(
            runtime,
            None,
            run_id.map(ToOwned::to_owned),
            ToolSessionContext::default(),
        )),
        ..Default::default()
    }
}
