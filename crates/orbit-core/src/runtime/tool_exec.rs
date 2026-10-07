use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use orbit_common::security::redaction::{redact_all_error, redact_sensitive_env_json};
use orbit_tools::ToolContext;
use orbit_types::record::OrbitEvent;
use orbit_types::workflow::tool_allowed;
use serde_json::Value;

use crate::{OrbitError, OrbitRuntime};

/// Which trusted inputs Core may use when applying its capability registry.
///
/// Ordinary in-process and CLI calls may resolve capabilities from both their
/// session context and the process envelope. MCP calls must use only the grants
/// carried by their trusted session context: an empty or agent-only MCP session
/// cannot inherit operator authority from the server process that hosts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CapabilityEnforcement {
    Enforce,
    McpSessionOnly,
}

impl OrbitRuntime {
    pub(crate) fn execute_registered_tool(
        &self,
        name: &str,
        input: Value,
        mut tool_context: ToolContext,
        capability_enforcement: CapabilityEnforcement,
    ) -> Result<Value, OrbitError> {
        if tool_context.cwd.is_none() {
            tool_context.cwd = std::env::current_dir()
                .ok()
                .map(|cwd| cwd.to_string_lossy().into_owned());
        }

        populate_filesystem_policy_context(self, &mut tool_context)?;

        self.authorize_registered_tool(name, &input, &tool_context, capability_enforcement)?;
        check_activity_tool_policy(name, &tool_context).map_err(|error| match error {
            OrbitError::PolicyDenied(reason) => self.deny_activity_tool(name, reason),
            error => error,
        })?;

        if self.worker_invocation().is_some()
            && super::worker_coordination::is_coordination_tool(name)
        {
            let input = if name == "orbit.task.artifact.put" {
                orbit_tools::prepare_remote_task_artifact_put(
                    input,
                    tool_context.cwd.as_deref().map(Path::new),
                    tool_context.workspace_root.as_deref(),
                )?
            } else {
                input
            };
            return self.route_worker_tool(name, input, tool_context.session_context);
        }

        if ((name == "orbit.task.show" && input.get("_worker_read").is_some())
            || (name == "orbit.task.update" && input.get("_worker_update").is_some()))
            && let Some(output) =
                self.execute_worker_projection(name, &input, &tool_context.session_context)?
        {
            return Ok(output);
        }

        let output = match self
            .tool_registry()
            .execute(name, &tool_context, input)
            .map_err(redact_all_error)
        {
            Ok(output) => output,
            Err(OrbitError::PolicyDenied(reason)) => {
                self.with_mutation(|| {
                    Ok((
                        (),
                        OrbitEvent::PolicyDenied {
                            tool: name.to_string(),
                        },
                    ))
                })?;
                return Err(OrbitError::PolicyDenied(reason));
            }
            Err(error) => return Err(error),
        };
        let output = redact_sensitive_env_json(output);

        self.with_mutation(|| {
            Ok((
                (),
                OrbitEvent::ToolExecuted {
                    name: name.to_string(),
                },
            ))
        })?;

        if (name == "orbit.task.pull" || name.starts_with("orbit.drain."))
            && let Some(field) = corrupted_drain_identity(&output)
        {
            // A mutating call may already have committed. Its caller must
            // reconcile/replay the same request, never invent a replacement.
            let message = format!(
                "owner reply identity field `{field}` contains an environment redaction artefact; retry the same request"
            );
            return Err(match name {
                "orbit.drain.probe" | "orbit.drain.receipt.lookup" | "orbit.drain.claims" => {
                    OrbitError::OwnerNegotiation(message)
                }
                _ => OrbitError::OutcomeUnknown {
                    mcp_call_id: tool_context
                        .session_context
                        .mcp_call_id
                        .clone()
                        .unwrap_or_else(|| format!("redacted-reply:{name}")),
                    message,
                },
            });
        }

        Ok(output)
    }

    /// Record an activity tool-policy refusal and build its error. An audit
    /// write failure surfaces instead, so a refusal is never unrecorded.
    fn deny_activity_tool(&self, name: &str, reason: String) -> OrbitError {
        match self.with_mutation(|| {
            Ok((
                (),
                OrbitEvent::PolicyDenied {
                    tool: name.to_string(),
                },
            ))
        }) {
            Ok(()) => OrbitError::PolicyDenied(reason),
            Err(error) => error,
        }
    }

    /// Admission shared by dispatch and dry-run, including input-dependent
    /// capability floors. This never invokes a tool implementation.
    pub(crate) fn authorize_registered_tool(
        &self,
        name: &str,
        input: &Value,
        tool_context: &ToolContext,
        capability_enforcement: CapabilityEnforcement,
    ) -> Result<(), OrbitError> {
        self.check_tool_enabled(name)?;
        check_tool_active(self.tool_registry(), name)?;

        // ORB-10453: the capability chokepoint. Every tool caller in the
        // workspace reaches the registry through this function, so this is the
        // only place a governed tool operation is authorized — a per-command
        // guard would be reopened by the next entry point that skips it.
        self.authorize_tool_operation(name, &tool_context.session_context, capability_enforcement)?;
        // Domain extensions preserve the authority of the operations they expose.
        // Discovery and a client-supplied mode never grant operator capabilities.
        if name == "orbit.pipeline.invoke"
            && !orbit_tools::has_pipeline_child_admission(tool_context)
        {
            self.authorize_tool_operation(
                "orbit.workflow.ship",
                &tool_context.session_context,
                capability_enforcement,
            )?;
        }
        if (name == "orbit.pipeline.invoke"
            && input.get("default_input") == Some(&Value::Bool(true)))
            || (name == "orbit.auto_task.update" && input.get("expected_enabled").is_some())
            || (name == "orbit.auto_task.mint" && input.get("acknowledge_unconditional").is_some())
        {
            self.authorize_tool_operation(
                "orbit.routine.control",
                &tool_context.session_context,
                capability_enforcement,
            )?;
        }
        if name == "orbit.auto_task.list"
            && input.get("view").and_then(Value::as_str) == Some("bounded")
        {
            self.authorize_tool_operation(
                "orbit.workflow.run.show",
                &tool_context.session_context,
                capability_enforcement,
            )?;
        }
        Ok(())
    }

    /// A registered-but-inactive entry is refused before it runs.
    ///
    /// The plugin host keeps a refused plugin's tool *names* on the registry
    /// so a caller that names one is told why rather than told the tool does
    /// not exist (design `docs/design/plugins/1_scope.md` §4.1, §4.8). That
    /// only holds if inactive also means uncallable, which is here: the
    /// registry is the one place every workspace caller passes through.
    fn check_tool_enabled(&self, name: &str) -> Result<(), OrbitError> {
        if let Some(stored) = self.stores().tools().get_tool(name)?
            && !stored.enabled
        {
            return Err(OrbitError::Execution(format!(
                "tool '{name}' is disabled; enable it with: orbit tool enable {name}"
            )));
        }
        Ok(())
    }
}

/// Reject scrubbed protocol identities without exempting arbitrary hash-shaped
/// secrets from redaction. Nested receipt, candidate and review identities are
/// included; prose may legitimately contain a redaction placeholder.
pub(crate) fn corrupted_drain_identity(value: &Value) -> Option<&str> {
    match value {
        Value::Object(fields) => fields.iter().find_map(|(key, value)| {
            let identity = matches!(
                key.as_str(),
                "commit" | "commits" | "tree" | "sha256" | "digest"
            ) || [
                "_fingerprint",
                "_commit",
                "_commits",
                "_tree",
                "_sha",
                "_sha256",
                "_hash",
                "_digest",
            ]
            .iter()
            .any(|suffix| key.ends_with(suffix));
            if identity && contains_env_redaction(value) {
                Some(key.as_str())
            } else {
                corrupted_drain_identity(value)
            }
        }),
        Value::Array(items) => items.iter().find_map(corrupted_drain_identity),
        _ => None,
    }
}

fn contains_env_redaction(value: &Value) -> bool {
    match value {
        Value::String(text) => text.contains("[REDACTED_ENV]"),
        Value::Array(items) => items.iter().any(contains_env_redaction),
        _ => false,
    }
}

/// Evaluate activity policy without emitting a mutation event. Real dispatch
/// records a refusal; dry-run only reports the decision.
pub(crate) fn check_activity_tool_policy(
    name: &str,
    tool_context: &ToolContext,
) -> Result<(), OrbitError> {
    if !tool_context.allowed_tools.is_empty() && !tool_allowed(name, &tool_context.allowed_tools) {
        return Err(OrbitError::PolicyDenied(format!(
            "tool '{name}' is not in the activity allowlist"
        )));
    }
    if let Some(policy) = tool_context
        .tool_deny_policy
        .as_ref()
        .filter(|policy| policy.denies(name))
    {
        return Err(OrbitError::PolicyDenied(policy.denial_message(name)));
    }
    Ok(())
}

/// Refuse a *plugin* entry the host registered inactive, reporting the
/// diagnostic the loader recorded (a missing grant, an unmet `requires`).
///
/// Scoped to plugin-backed entries on purpose. `Inactive` means two different
/// things in this registry: for a built-in it means "not on the agent tool
/// surface", which an operator may still call and which
/// `ensure_tool_agent_facing` decides; for a plugin tool it means the plugin
/// was refused at load, and nothing may call it until the operator fixes what
/// the diagnostic names (design `docs/design/plugins/1_scope.md` §4.1).
pub(crate) fn check_tool_active(
    registry: &orbit_tools::ToolRegistry,
    name: &str,
) -> Result<(), OrbitError> {
    if registry.is_active(name) {
        return Ok(());
    }
    match registry.plugin_binding(name) {
        Some(binding) => Err(OrbitError::PolicyDenied(
            binding.diagnostic.clone().unwrap_or_else(|| {
                format!("plugin tool '{name}' is registered but inactive on this host")
            }),
        )),
        None => Ok(()),
    }
}

/// Fill the trusted runtime-owned pieces of a registered tool's filesystem
/// policy context without replacing an explicitly supplied activity context.
///
/// CLI-backed activities call this before registry dispatch so `proc.spawn`
/// receives the same canonical checkout and policy engine as in-process
/// activities. The registry chokepoint calls it again for ordinary filesystem
/// tools, making the operation idempotent while preserving fail-closed profile
/// handling when the managed envelope omitted `ORBIT_ACTIVITY_FS_PROFILE`.
pub(crate) fn populate_filesystem_policy_context(
    runtime: &OrbitRuntime,
    tool_context: &mut ToolContext,
) -> Result<(), OrbitError> {
    if tool_context.workspace_root.is_none() {
        tool_context.workspace_root = resolve_workspace_root_from_context(runtime, tool_context)?;
    }
    if tool_context.policy_engine.is_none() {
        tool_context.policy_engine = Some(Arc::new(runtime.policy_engine().clone()));
    }
    if tool_context.fs_profile.is_none() {
        tool_context.fs_profile = read_activity_fs_profile_from_env();
    }
    Ok(())
}

/// Task scope is supplied by the caller (host or trusted run envelope).
///
/// Cwd-inside-repo is not a task selector: scanning the table and returning
/// the first row was both arbitrary and unused by root resolution.
pub(crate) fn resolve_task_id_from_context(
    _runtime: &OrbitRuntime,
    _tool_context: &ToolContext,
) -> Result<Option<String>, OrbitError> {
    Ok(None)
}

fn resolve_workspace_root_from_context(
    runtime: &OrbitRuntime,
    tool_context: &ToolContext,
) -> Result<Option<PathBuf>, OrbitError> {
    let canonical_repo_root = canonical_repo_root(runtime);
    if let Some(workspace_root) = active_git_checkout_root(&canonical_repo_root, tool_context) {
        return Ok(Some(workspace_root));
    }
    Ok(Some(canonical_repo_root))
}

fn canonical_repo_root(runtime: &OrbitRuntime) -> PathBuf {
    runtime
        .context
        .paths()
        .repo_root
        .canonicalize()
        .unwrap_or_else(|_| runtime.context.paths().repo_root.clone())
}

fn active_git_checkout_root(
    canonical_repo_root: &Path,
    tool_context: &ToolContext,
) -> Option<PathBuf> {
    let cwd = tool_context.cwd.as_deref()?;
    let cwd = Path::new(cwd);
    let canonical_cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());

    // An ordinary path in the runtime checkout needs no git probes. A nested
    // checkout has its own .git marker, even when it is inside this path.
    if canonical_cwd.starts_with(canonical_repo_root) {
        let nested_checkout = canonical_cwd
            .ancestors()
            .take_while(|ancestor| *ancestor != canonical_repo_root)
            .any(|ancestor| ancestor.join(".git").exists());
        if !nested_checkout {
            return Some(canonical_repo_root.to_path_buf());
        }
    }

    let checkout_root = git_checkout_root(cwd)?;
    let repo_common_dir = git_common_dir(canonical_repo_root)?;
    // A linked worktree shares the runtime repository's common directory. A
    // source-inspection slot is a standalone repository the CLI runner
    // materialized for this repository, so it is recognized by its owned
    // layout instead; without it, a pilot's subprocesses would run in the
    // primary rather than at the pinned revision [ORB-13800].
    let owned_checkout = git_common_dir(&checkout_root)
        .is_some_and(|checkout_common_dir| checkout_common_dir == repo_common_dir)
        || orbit_engine::activity_job::cli_runner::is_source_inspection_checkout(
            &repo_common_dir,
            &checkout_root,
        );
    owned_checkout.then_some(checkout_root)
}

fn git_checkout_root(path: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .current_dir(path)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    let raw_path = stdout.lines().next()?.trim();
    if raw_path.is_empty() {
        return None;
    }
    let path = PathBuf::from(raw_path);
    Some(path.canonicalize().unwrap_or(path))
}

fn git_common_dir(path: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .current_dir(path)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    let raw_path = stdout.lines().next()?.trim();
    if raw_path.is_empty() {
        return None;
    }
    let path = PathBuf::from(raw_path);
    Some(path.canonicalize().unwrap_or(path))
}

fn read_activity_fs_profile_from_env() -> Option<String> {
    let value = std::env::var("ORBIT_ACTIVITY_FS_PROFILE").ok()?;
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed.to_string())
}
