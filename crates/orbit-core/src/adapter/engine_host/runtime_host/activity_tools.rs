use std::sync::Arc;

use orbit_engine::{DispatchError, ResolvedActivityTools};
use orbit_tools::{FsAuditLogger, ReservationOwnerContext, ToolContext};
use orbit_types::policy::UNRESTRICTED_FS_PROFILE;
use orbit_types::tool::{ToolSessionContext, is_exact_canonical_tool_name};

use crate::OrbitRuntime;
use crate::adapter::tool_host::build_orbit_tool_host;

pub(super) fn resolve_activity_tools(
    runtime: &OrbitRuntime,
    task_ids: &[String],
    baseline_tools: &[String],
) -> Result<ResolvedActivityTools, DispatchError> {
    if task_ids.is_empty() {
        return Ok(ResolvedActivityTools {
            requested_tools: Vec::new(),
            effective_tools: baseline_tools.to_vec(),
        });
    }
    let mut requested_tools = std::collections::BTreeSet::new();
    for task_id in task_ids {
        let task = runtime.get_task(task_id).map_err(|error| {
            DispatchError::CliInvocationFailed(format!(
                "load task `{task_id}` tool requirements: {error}"
            ))
        })?;
        for tool_name in task.required_tools {
            let reason = if tool_name.contains('*') {
                Some("wildcard and prefix requirements are not allowed".to_string())
            } else if !is_exact_canonical_tool_name(&tool_name) {
                Some("malformed canonical tool name".to_string())
            } else if !runtime.tool_registry().has(&tool_name) {
                Some("unknown registered tool".to_string())
            } else if !runtime.tool_registry().is_active(&tool_name) {
                Some("tool is not agent-facing".to_string())
            } else {
                runtime
                    .stores()
                    .tools()
                    .get_tool(&tool_name)
                    .map_err(|error| {
                        DispatchError::CliInvocationFailed(format!(
                            "read tool `{tool_name}` admission state for task `{task_id}`: {error}"
                        ))
                    })?
                    .filter(|tool| !tool.enabled)
                    .map(|_| "tool is inactive".to_string())
            };
            if let Some(reason) = reason {
                return Err(DispatchError::RequiredToolAdmission {
                    task_id: task_id.clone(),
                    tool_name,
                    reason,
                });
            }
            requested_tools.insert(tool_name);
        }
    }
    let requested_tools = requested_tools.into_iter().collect::<Vec<_>>();

    if requested_tools.is_empty() {
        return Ok(ResolvedActivityTools {
            requested_tools,
            effective_tools: baseline_tools.to_vec(),
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
    })
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
    // always activity-scoped: a missing `proc_allowed_programs` denies every
    // program instead of degrading to allow-all. Asset load already refuses
    // an activity that grants `proc.spawn` without the key ([ORB-10959]);
    // this keeps the enforcement point fail-closed on its own.
    let proc_spawn_activity_scoped = true;
    let proc_allowed_programs = proc_allowed_programs
        .map(|programs| programs.to_vec())
        .unwrap_or_default();
    let proc_spawn_environment = Some(runtime.execution_env_policy().agent_subprocess_env(&[]));

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
        reservation_owner: run_id.map(str::trim).filter(|value| !value.is_empty()).map(
            |owner_run_id| ReservationOwnerContext {
                owner_run_id: owner_run_id.to_string(),
                owner_metadata_json: Some(
                    serde_json::json!({
                        "source": "v2_activity",
                        "fs_profile": fs_profile.unwrap_or(UNRESTRICTED_FS_PROFILE),
                    })
                    .to_string(),
                ),
            },
        ),
        orbit_host: Some(build_orbit_tool_host(
            runtime,
            None,
            run_id
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned),
            ToolSessionContext::default(),
        )),
        ..Default::default()
    }
}
