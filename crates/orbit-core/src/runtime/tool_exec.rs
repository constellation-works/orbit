#[cfg(test)]
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use orbit_common::security::redaction::{redact_all_error, redact_sensitive_env_json};
use orbit_tools::ToolContext;
use orbit_types::record::OrbitEvent;
use orbit_types::workflow::tool_allowed;
use serde_json::Value;

use crate::{NotFoundKind, OrbitError, OrbitRuntime};

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

        self.check_tool_enabled(name)?;

        // ORB-10453: the capability chokepoint. Every tool caller in the
        // workspace reaches the registry through this function, so this is the
        // only place a governed tool operation is authorized — a per-command
        // guard would be reopened by the next entry point that skips it.
        self.authorize_tool_operation(name, &tool_context.session_context, capability_enforcement)?;

        if !tool_context.allowed_tools.is_empty()
            && !tool_allowed(name, &tool_context.allowed_tools)
        {
            self.with_mutation(|| {
                Ok((
                    (),
                    OrbitEvent::PolicyDenied {
                        tool: name.to_string(),
                    },
                ))
            })?;
            return Err(OrbitError::PolicyDenied(format!(
                "tool '{name}' is not in the activity allowlist"
            )));
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

        Ok(output)
    }

    pub fn run_tool_dry_run(&self, name: &str, input: &Value) -> Result<DryRunResult, OrbitError> {
        self.ensure_tool_agent_facing(name)?;
        self.check_tool_enabled(name)?;

        let schema = self
            .tool_registry()
            .get_schema(name)
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Tool, name.to_string()))?;

        let mut tool_context = ToolContext {
            cwd: std::env::current_dir()
                .ok()
                .map(|cwd| cwd.to_string_lossy().into_owned()),
            ..Default::default()
        };
        tool_context.workspace_root = resolve_workspace_root_from_context(self, &tool_context)?;

        // Validate required parameters are present
        let mut missing_params = Vec::new();
        if let Some(obj) = input.as_object() {
            for param in &schema.parameters {
                if param.required && !obj.contains_key(&param.name) {
                    missing_params.push(param.name.clone());
                }
            }
        } else if !schema.parameters.is_empty() {
            for param in &schema.parameters {
                if param.required {
                    missing_params.push(param.name.clone());
                }
            }
        }

        Ok(DryRunResult {
            tool_name: name.to_string(),
            policy_allowed: true,
            missing_params,
        })
    }

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

    // Stable checkout fast path: the runtime's own tree does not need git
    // probes. Linked worktrees and unrelated checkouts fall through.
    if canonical_cwd.starts_with(canonical_repo_root) {
        return Some(canonical_repo_root.to_path_buf());
    }

    let checkout_root = git_checkout_root(cwd)?;
    same_git_common_dir(&checkout_root, canonical_repo_root).then_some(checkout_root)
}

fn git_checkout_root(path: &Path) -> Option<PathBuf> {
    #[cfg(test)]
    GIT_CHECKOUT_PROBES.with(|count| count.set(count.get() + 1));

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

fn same_git_common_dir(left: &Path, right: &Path) -> bool {
    match (git_common_dir(left), git_common_dir(right)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

fn git_common_dir(path: &Path) -> Option<PathBuf> {
    #[cfg(test)]
    GIT_COMMON_DIR_PROBES.with(|count| count.set(count.get() + 1));

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

#[cfg(test)]
thread_local! {
    static GIT_CHECKOUT_PROBES: Cell<usize> = const { Cell::new(0) };
    static GIT_COMMON_DIR_PROBES: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
pub(crate) struct ContextResolutionProbes;

#[cfg(test)]
impl ContextResolutionProbes {
    pub(crate) fn capture() -> Self {
        GIT_CHECKOUT_PROBES.with(|count| count.set(0));
        GIT_COMMON_DIR_PROBES.with(|count| count.set(0));
        Self
    }

    pub(crate) fn git_checkout_probes(&self) -> usize {
        GIT_CHECKOUT_PROBES.with(Cell::get)
    }

    pub(crate) fn git_common_dir_probes(&self) -> usize {
        GIT_COMMON_DIR_PROBES.with(Cell::get)
    }
}

fn read_activity_fs_profile_from_env() -> Option<String> {
    let value = std::env::var("ORBIT_ACTIVITY_FS_PROFILE").ok()?;
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed.to_string())
}

#[derive(Debug, Clone)]
pub struct DryRunResult {
    pub tool_name: String,
    pub policy_allowed: bool,
    pub missing_params: Vec<String>,
}
