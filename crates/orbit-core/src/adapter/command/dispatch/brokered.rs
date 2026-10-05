//! The host side of a run's plugin broker: execute one authenticated request
//! — a plugin tool, one of the read-only `github.*` built-ins that need the
//! host's `gh` credentials, or a claimed reviewer's manifest read or report
//! write that needs the claim's owner route — through the audited dispatch,
//! under the run's authority (`docs/design/plugins/2_agent_call_broker.md` §3, §4.3–§4.4, §5).
//!
//! Everything that decides authority — task, job run, activity policy, agent
//! identity, filesystem profile and program policy — comes from the
//! [`PluginBrokerRun`] the step runner built when it dispatched the agent.
//! The request contributes only the tool, its input and a `cwd` that must lie
//! within the run's worktree; this process's environment describes the host,
//! not the agent, and is never read for the call.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_engine::PluginBrokerRun;
use orbit_tools::{ActivityBinding, ToolContext};
use orbit_types::policy::Role;
use orbit_types::tool::{McpCapability, ToolSessionContext};
use orbit_types::workflow::tool_allowed;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::runtime::plugin::broker::{
    BrokerDispatch, BrokerRequest, EntryPoint, is_host_credentialed_read,
};
use crate::runtime::tool_exec::CapabilityEnforcement;

use super::audit::{AuditContext, brokered_agent_identity, brokered_role_label};
use super::claimed_review::is_claimed_review_artifact;
use super::execute::{BrokeredAudit, ToolEntryPoint};

/// Executes a broker's requests for the one run it serves.
pub(crate) struct RunDispatch {
    runtime: OrbitRuntime,
    run: PluginBrokerRun,
    /// The run's worktree, resolved once so a request `cwd` is compared
    /// against the real directory rather than a spelling of it.
    worktree: PathBuf,
    sessions: std::sync::Arc<orbit_tools::plugin::BrokerSessions>,
}

impl RunDispatch {
    pub(crate) fn new(runtime: OrbitRuntime, run: PluginBrokerRun) -> Self {
        let worktree = run
            .caller
            .worktree
            .canonicalize()
            .unwrap_or_else(|_| run.caller.worktree.clone());
        Self {
            runtime,
            run,
            worktree,
            sessions: Default::default(),
        }
    }

    /// The directory the call runs in, or why the request itself cannot run,
    /// whatever the tool: a `cwd` outside the run's worktree, another
    /// workspace, or a dry run, which the nested `orbit` answers without the
    /// broker.
    ///
    /// The result is the resolved directory that was checked, never the
    /// caller's spelling of it. The worktree is the agent's to write, so a
    /// link that resolved inside it here could be repointed before the backend
    /// starts; the call runs, and is audited, at the resolved path instead.
    /// The path is not held open, so a directory swapped along it before the
    /// spawn still moves the backend; this narrows the window, not closes it.
    fn checked_cwd(
        &self,
        cwd: &Path,
        workspace: Option<&str>,
        dry_run: bool,
    ) -> Result<PathBuf, OrbitError> {
        if dry_run {
            return Err(OrbitError::PolicyDenied(
                "the plugin broker runs calls; a dry run is answered by the nested orbit"
                    .to_string(),
            ));
        }
        let resolved = cwd
            .canonicalize()
            .ok()
            .filter(|resolved| resolved.starts_with(&self.worktree))
            .ok_or_else(|| {
                OrbitError::PolicyDenied(format!(
                    "the plugin broker runs calls only from within this run's worktree; `{}` is \
                     not",
                    cwd.display()
                ))
            })?;
        if let Some(workspace) = workspace
            && self.run.workspace.as_deref() != Some(workspace)
        {
            return Err(OrbitError::PolicyDenied(format!(
                "the plugin broker serves only this run's workspace, not `{workspace}`"
            )));
        }
        Ok(resolved)
    }

    /// The run's own activity policy, applied fail-closed: an allowlist
    /// that names nothing admits nothing here, where an in-process caller
    /// with no activity would be unrestricted. A non-empty list and a deny
    /// policy are enforced again, and recorded, by the tool chokepoint.
    fn refuse_outside_activity_policy(&self, tool: &str) -> Result<(), OrbitError> {
        if self.run.tool_deny_policy.is_none() && !tool_allowed(tool, &self.run.allowed_tools) {
            return Err(OrbitError::PolicyDenied(format!(
                "tool '{tool}' is not in the activity allowlist"
            )));
        }
        Ok(())
    }

    fn tool_context(&self, cwd: &Path, session_context: ToolSessionContext) -> ToolContext {
        let run = &self.run;
        let agent = brokered_agent_identity(run.agent_name.as_deref(), run.model_name.as_deref());
        let mut tool_context = ToolContext {
            cwd: Some(cwd.to_string_lossy().into_owned()),
            session_context,
            allowed_tools: run.allowed_tools.clone(),
            tool_deny_policy: run.tool_deny_policy.clone(),
            agent_name: agent.clone(),
            model_name: agent,
            proc_spawn_environment: Some(
                self.runtime
                    .execution_env_policy()
                    .agent_subprocess_env(&[]),
            ),
            fs_profile: Some(run.caller.fs_profile.name.clone()),
            activity_binding: run.job_run_id.clone().map(|job_run_id| ActivityBinding {
                job_run_id,
                task_id: run.task_id.clone(),
            }),
            ..Default::default()
        };
        run.caller.restrict(&mut tool_context);
        tool_context.brokered_caller = Some(run.caller.clone());
        tool_context
    }
}

impl BrokerDispatch for RunDispatch {
    fn call(
        &self,
        request: BrokerRequest,
        peer_pid: u32,
        cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<Value, OrbitError> {
        let BrokerRequest {
            tool,
            input,
            cwd,
            workspace,
            entry_point,
            dry_run,
        } = request;
        let checked = self.checked_cwd(&cwd, workspace.as_deref(), dry_run);
        let cwd = checked.as_ref().map_or(cwd, Clone::clone);
        let run = &self.run;
        // The agent's sandbox is what the broker authenticated, so the call
        // runs with an agent's authority — never the host's.
        let session_context = ToolSessionContext {
            workspace_id: run.workspace.clone(),
            effective_capabilities: BTreeSet::from([McpCapability::Agent]),
            ..Default::default()
        };
        let audit = BrokeredAudit {
            peer_pid,
            role: brokered_role_label(run.agent_name.as_deref(), run.model_name.as_deref()),
            context: AuditContext {
                session_id: None,
                task_id: run.task_id.clone(),
                job_run_id: run.job_run_id.clone(),
                activity_id: Some(run.activity_name.clone()),
                step_index: None,
            },
            working_directory: cwd.to_string_lossy().into_owned(),
        };
        let entry_point = match entry_point {
            EntryPoint::Cli => ToolEntryPoint::Cli,
            EntryPoint::Mcp => ToolEntryPoint::Mcp,
        };
        self.runtime
            .execute_brokered_dispatch(
                &tool,
                input,
                entry_point,
                session_context.clone(),
                audit,
                |input| {
                    checked?;
                    // The claimed reviewer's manifest read and report write
                    // reach the owner over the claim's route, which the
                    // sandbox cannot open; the broker carries exactly those.
                    if is_claimed_review_artifact(&tool) {
                        self.runtime.ensure_tool_agent_facing(&tool)?;
                        self.refuse_outside_activity_policy(&tool)?;
                        if let Some(policy) = &run.tool_deny_policy
                            && policy.denies(&tool)
                        {
                            return Err(OrbitError::PolicyDenied(policy.denial_message(&tool)));
                        }
                        return super::claimed_review::execute_brokered(
                            &self.runtime,
                            run,
                            &tool,
                            input,
                            session_context,
                        );
                    }
                    // A built-in tool is answered by the nested `orbit`
                    // itself; the broker exists to run plugin backends and
                    // the closed set of reads that need the host's `gh`
                    // credentials.
                    if self.runtime.tool_registry().plugin_binding(&tool).is_none()
                        && !is_host_credentialed_read(&tool)
                    {
                        return Err(OrbitError::PolicyDenied(format!(
                            "the plugin broker runs plugin tools, the read-only github.* tools \
                             and a claimed reviewer's artifact calls only; '{tool}' is none of \
                             these"
                        )));
                    }
                    self.runtime.ensure_tool_agent_facing(&tool)?;
                    self.refuse_outside_activity_policy(&tool)?;
                    self.runtime.run_tool_with_context_and_role_and_capability(
                        &tool,
                        input,
                        Role::Admin,
                        {
                            let mut ctx = self.tool_context(&cwd, session_context);
                            ctx.broker_call = Some(orbit_tools::plugin::BrokerCall {
                                sessions: std::sync::Arc::clone(&self.sessions),
                                cancelled,
                            });
                            ctx
                        },
                        CapabilityEnforcement::McpSessionOnly,
                    )
                },
            )
            .map(|outcome| outcome.value)
    }
}
