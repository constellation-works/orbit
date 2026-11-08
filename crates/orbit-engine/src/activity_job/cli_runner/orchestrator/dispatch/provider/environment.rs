//! Child environment, provenance and per-run advisory database preparation.

use super::super::super::environment::{
    activity_deadline_env, add_browser_kit_env, require_claude_worker_credential, require_crew_env,
};
use super::super::super::policy::{activity_policy_env, drop_inherited_policy_env};
use super::super::CliDispatchContext;
use crate::activity_job::cli_runner::inspection::SourceInspection;
use crate::activity_job::dispatcher::{DispatchError, ResolvedSandbox};
use crate::context::{ProvenanceEnv, ResolvedActivityTools, provenance_env};
use orbit_common::process::build_budget::WAIT_DIRECTORY_ENV;
use orbit_common::security::child_env::{
    ACTIVITY_MCP_TOOLSETS_ENV, MCP_MANAGED_REGISTRY_ROOT_ENV, MCP_MANAGED_WORKSPACE_ENV,
};
use orbit_tools::ToolContext;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Environment variable `scripts/cargo-deny.sh` reads for a writable
/// advisory database root [ORB-11983].
const CARGO_DENY_DB_PATH_ENV: &str = "CARGO_DENY_DB_PATH";

/// Scratch subdirectory holding this run's cargo-deny advisory database.
const CARGO_DENY_ADVISORY_DB_SUBDIR: &str = "cargo-deny-advisory-db";

/// Prepare the per-run cargo-deny advisory database directory [ORB-21352].
///
/// The sandbox binds only `$CARGO_HOME/registry` and `$CARGO_HOME/git`
/// writable, so cargo-deny cannot lock the default
/// `$CARGO_HOME/advisory-dbs`. When the effective sandbox profile already
/// grants this run write access to its scratch directory, create the
/// advisory directory there and return it for `CARGO_DENY_DB_PATH`
/// injection. Every other dispatch — no effective sandbox, or a profile
/// such as the read-only reviewer that denies scratch writes — returns
/// `None` and gains no writable advisory path, so cargo-deny in that
/// profile still fails closed. A directory that cannot be created fails
/// the dispatch rather than skipping the advisory check.
fn prepare_cargo_deny_advisory_db_dir(
    sandbox: Option<&ResolvedSandbox>,
    scratch_dir: Option<&Path>,
) -> Result<Option<PathBuf>, DispatchError> {
    let (Some(sandbox), Some(scratch_dir)) = (sandbox, scratch_dir) else {
        return Ok(None);
    };
    let advisory_db_dir = scratch_dir.join(CARGO_DENY_ADVISORY_DB_SUBDIR);
    if !sandbox_profile_allows_modify(&sandbox.fs_profile, &advisory_db_dir)? {
        return Ok(None);
    }
    orbit_common::fs::io::create_private_dir_all(&advisory_db_dir).map_err(|error| {
        DispatchError::CliInvocationPermanent(format!(
            "create cargo-deny advisory database dir `{}`: {error}",
            advisory_db_dir.display()
        ))
    })?;
    Ok(Some(advisory_db_dir))
}

/// Whether the resolved sandbox profile allows modifying `path`, under the
/// same last-match-wins precedence the sandbox compilers apply.
///
/// The resolved profile holds workspace-absolute rules, while
/// [`CompiledFsRules`](orbit_types::policy::CompiledFsRules) only decides
/// workspace-relative paths, so the decision is replayed here with
/// [`match_glob`](orbit_types::policy::match_glob) over the absolute
/// candidate. No matching rule means denied.
fn sandbox_profile_allows_modify(
    profile: &orbit_types::policy::ResolvedFsProfile,
    path: &Path,
) -> Result<bool, DispatchError> {
    let candidate = path.display().to_string().replace('\\', "/");
    let mut allowed = false;
    for rule in &profile.modify {
        let (negated, body) = rule
            .strip_prefix('!')
            .map(|rest| (true, rest))
            .unwrap_or((false, rule.as_str()));
        let mut pattern = body.replace('\\', "/");
        while let Some(stripped) = pattern.strip_prefix("./") {
            pattern = stripped.to_string();
        }
        if pattern.is_empty() {
            pattern = ".".to_string();
        }
        let matched = orbit_types::policy::match_glob(&pattern, &candidate).map_err(|error| {
            DispatchError::CliInvocationPermanent(format!(
                "evaluate sandbox modify rules for the cargo-deny advisory database: {error}"
            ))
        })?;
        if matched {
            allowed = !negated;
        }
    }
    Ok(allowed)
}

pub(super) struct ProviderEnvironmentInput<'a> {
    pub(super) context: CliDispatchContext<'a>,
    pub(super) provider: &'a str,
    pub(super) model: &'a Option<String>,
    pub(super) inspection: &'a Option<SourceInspection>,
    pub(super) tool_ctx: &'a ToolContext,
    pub(super) task_id: Option<&'a str>,
    pub(super) wall_clock_timeout: Duration,
    pub(super) timeout_seconds: u64,
    pub(super) scratch: &'a Path,
    pub(super) tool_disallow_list: &'a Option<Vec<String>>,
    pub(super) activity_tools: &'a ResolvedActivityTools,
    pub(super) orbit_env: Vec<(String, String)>,
    pub(super) subprocess_cwd: &'a Option<PathBuf>,
    pub(super) sandbox: Option<&'a ResolvedSandbox>,
    pub(super) child_env: &'a mut Vec<(String, String)>,
    pub(super) fixed_env: &'a [(&'static str, &'static str)],
}

pub(super) struct PreparedProviderEnvironment {
    pub(super) scratch_dir: Option<PathBuf>,
    pub(super) build_wait_directory: tempfile::TempDir,
}

pub(super) fn prepare_provider_environment(
    input: ProviderEnvironmentInput<'_>,
) -> Result<PreparedProviderEnvironment, DispatchError> {
    let ProviderEnvironmentInput {
        context,
        provider,
        model,
        inspection,
        tool_ctx,
        task_id,
        wall_clock_timeout,
        timeout_seconds,
        scratch,
        tool_disallow_list,
        activity_tools,
        orbit_env,
        subprocess_cwd,
        sandbox,
        child_env,
        fixed_env,
    } = input;
    let CliDispatchContext {
        host,
        spec,
        run_id,
        activity_name,
        fs_profile,
        ..
    } = context;
    // ADR-0182: external CLI agents get the same active-task hook binding as
    // direct-agent executions. The AGENT_* fields preserve ORB-10342's
    // commit-telemetry contract and omit unknown model/task values.
    let mut dispatch_env = provenance_env(ProvenanceEnv {
        orbit_run_id: inspection.is_none().then_some(run_id),
        orbit_managed_run_context: true,
        orbit_agent_name: tool_ctx.agent_name.as_deref(),
        orbit_agent_model: tool_ctx.model_name.as_deref(),
        // A source inspection is an Orbit-dispatched invocation without a
        // job run. Give its nested MCP child a separate managed identity.
        orbit_session_id: inspection.as_ref().map(|_| run_id),
        orbit_task_id: task_id,
        orbit_active_task: true,
        agent_run_id: Some(run_id),
        agent_model: model.as_deref(),
        agent_task_id: task_id,
    });
    dispatch_env.push(("ORBIT_TASK_ACTOR_KIND".to_string(), "agent".to_string()));
    // A nested `proc.spawn` may run as long as this invocation has left. The
    // supervisor's clock starts at spawn, a moment after this, so the stamped
    // deadline never outlasts the provider.
    dispatch_env.extend(activity_deadline_env(wall_clock_timeout));
    dispatch_env.push((
        orbit_common::security::child_env::ACTIVITY_TIMEOUT_ENV.to_string(),
        timeout_seconds.saturating_mul(1000).to_string(),
    ));
    let build_wait_directory = tempfile::Builder::new()
        .prefix("build-waits-")
        .tempdir_in(scratch)
        .map_err(|error| {
            DispatchError::CliInvocationPermanent(format!("create build wait channel: {error}"))
        })?;
    dispatch_env.push((
        WAIT_DIRECTORY_ENV.to_string(),
        build_wait_directory.path().display().to_string(),
    ));
    dispatch_env.extend(activity_policy_env(
        spec,
        activity_name,
        tool_disallow_list.as_deref(),
        &activity_tools.effective_tools,
        fs_profile,
    ));
    // Start managed servers with the core surface and add only explicitly
    // granted tools. The activity policy still enforces the callable set.
    let mut stamped_tools = vec!["core".to_string()];
    stamped_tools.extend(
        activity_tools
            .granted_tools
            .iter()
            .chain(activity_tools.requested_tools.iter())
            .map(|tool| format!("tool:{tool}")),
    );
    // An allowlist activity explicitly grants its baseline tools. Deny-mode
    // activities instead have a broad effective set, so retain only their
    // explicit lifts and the notice/memory tools admitted by the host.
    stamped_tools.extend(
        activity_tools
            .effective_tools
            .iter()
            .filter(|tool| {
                tool_disallow_list.is_none()
                    || tool.starts_with("orbit.notice.")
                    || tool.starts_with("orbit.memory.")
            })
            .map(|tool| format!("tool:{tool}")),
    );
    dispatch_env.push((
        ACTIVITY_MCP_TOOLSETS_ENV.to_string(),
        stamped_tools.join(","),
    ));
    dispatch_env.extend(orbit_env);
    // Spawned CLI agents resolve the Orbit registry from $HOME unless a
    // managed registry locator is set. A dispatching run already knows its
    // registry; inject it so a provider whose HOME is a tool-specific
    // directory (e.g. ~/.codex) can still reach `orbit tool run`. [ORB-10909]
    //
    // The injected value is the authoritative shared registry root, never the
    // dispatching checkout's workspace `.orbit`. A managed run's workspace
    // state root is mounted read-only inside the sandbox and does not own the
    // task store. `ORBIT_ROOT` cannot carry this contract because it is the
    // operator's explicit data-root override and deliberately pins global,
    // shared, and local roots together. The managed-only locator changes only
    // the global registry. Workspace selection is the separate logical
    // `ORBIT_WORKSPACE` selector below, not the linked-worktree cwd.
    // [ORB-10980] [ORB-11066] [ORB-11117]
    let registry_locator_injected = if let Some(registry_root) = host.orbit_registry_root() {
        dispatch_env.push((MCP_MANAGED_REGISTRY_ROOT_ENV.to_string(), registry_root));
        true
    } else {
        false
    };
    // Carry the trusted logical `ws_*` identity so nested `orbit tool run`
    // and `orbit mcp serve` do not rediscover ownership from a linked-worktree
    // cwd. The child honors this only with managed provenance plus a run or
    // source-inspection invocation identity;
    // an explicit `--workspace` or tool-payload selector still wins and still
    // fails closed. [ORB-11117]
    if let Some(workspace) = host.orbit_workspace_selector() {
        dispatch_env.push((MCP_MANAGED_WORKSPACE_ENV.to_string(), workspace));
    }
    // Canonical before the provider starts, so the Linux post-run guard's
    // scratch exemption cannot be redirected by the child.
    let scratch_dir = match subprocess_cwd.as_ref() {
        Some(cwd) => {
            let scratch =
                orbit_common::fs::path::ensure_orbit_scratch_dir(cwd).map_err(|error| {
                    DispatchError::CliInvocationPermanent(format!(
                        "failed to create worker scratch dir: {error}"
                    ))
                })?;
            dispatch_env.push((
                orbit_common::fs::path::ORBIT_SCRATCH_DIR_ENV.to_string(),
                scratch.display().to_string(),
            ));
            Some(scratch)
        }
        None => None,
    };
    // [ORB-21352] A write-granting sandboxed agent gets a per-run writable
    // cargo-deny advisory database under its scratch dir; `dispatch_env` is
    // appended to the child environment last, so this value overrides any
    // same-named value the allowlist forwarded. No disable-fetch variable
    // is set: the child still fetches a fresh advisory database.
    if let Some(advisory_db_dir) =
        prepare_cargo_deny_advisory_db_dir(sandbox, scratch_dir.as_deref())?
    {
        dispatch_env.push((
            CARGO_DENY_DB_PATH_ENV.to_string(),
            advisory_db_dir.display().to_string(),
        ));
    }
    require_crew_env(&spec.crew_env, child_env)?;
    require_claude_worker_credential(host, provider, child_env)?;
    if inspection.is_some() {
        // This invocation has no job-run authority. An outer managed process
        // may have passed its own run id through the allowlist.
        child_env.retain(|(key, _)| key != "ORBIT_RUN_ID");
    }
    if registry_locator_injected {
        // A host process may itself have been launched with an operator
        // `ORBIT_ROOT`. Do not reinterpret that pinned-data-root input as the
        // managed child's workspace root; the authoritative registry locator
        // above supersedes it for this execution envelope. [ORB-11066]
        child_env.retain(|(key, _)| key != "ORBIT_ROOT");
    }
    // The envelope allowlist forwards an outer run's activity and process
    // policy names; drop them, then stamp this activity's envelope.
    drop_inherited_policy_env(child_env);
    // Provider-pinned entries replace any same-named value the allowlist
    // forwarded, so an outer process cannot re-enable what the provider
    // disables for headless runs. [ORB-13664]
    child_env.retain(|(key, _)| !fixed_env.iter().any(|(fixed, _)| *fixed == key.as_str()));
    child_env.extend(
        fixed_env
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string())),
    );
    child_env.extend(dispatch_env);
    add_browser_kit_env(child_env);
    if host.worker_invocation().is_some() {
        child_env.push(("ORBIT_WORKER_CONTEXT_REQUIRED".into(), "1".into()));
    }
    Ok(PreparedProviderEnvironment {
        scratch_dir,
        build_wait_directory,
    })
}
