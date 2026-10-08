//! Minimal auth recovery calls through the provider's normal execution boundary.

use std::path::Path;
use std::time::Duration;

use orbit_agent::{Agent, AgentConfig, AgentOperation, AgentRequest};
use orbit_common::security::child_env::MCP_MANAGED_BINDING_ENV_VARS;
use orbit_types::workflow::{AuthProbe, AuthProbeSuccess};

use super::launcher::{orbit_tool_env, resolve_provider_launcher};
use super::orchestrator::provider_child_environment;
use super::spawn::{SpawnError, prepare_sandbox_for_dispatch};
use super::supervisor::{
    SpawnTraceContext, SpawnWithTimeoutRequest, spawn_for_supervision, spawn_with_timeout,
};
use crate::{DispatchError, RuntimeHost};

/// The recovery verdict, without provider output or credential values.
pub struct AuthProbeOutcome {
    pub passed: bool,
    pub timed_out: bool,
    pub credential_source: String,
}

/// Credential origin visible to a leaf after applying the host's env allowlist.
/// Cache/keychain labels describe the fallback; Orbit does not read credentials.
pub fn auth_credential_source(host: &dyn RuntimeHost, provider: &str) -> String {
    credential_source(provider, &host.agent_subprocess_environment(&[]))
}

fn credential_source(provider: &str, env: &[(String, String)]) -> String {
    if provider == "claude" {
        for name in ["CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_API_KEY"] {
            if env
                .iter()
                .any(|(key, value)| key == name && !value.is_empty())
            {
                return format!("{name} (passed environment)");
            }
        }
        if cfg!(target_os = "macos") {
            return "macOS keychain (no passed token)".into();
        }
    }
    "provider credential cache (no passed token identified)".into()
}

/// Run a declared probe with the leaf launcher, cleared allowlisted environment,
/// sandbox and process-tree timeout. No activity tools or broker are granted.
pub fn run_auth_probe(
    host: &dyn RuntimeHost,
    provider: &str,
    probe: &AuthProbe,
    run_id: &str,
    cwd: &Path,
) -> Result<AuthProbeOutcome, DispatchError> {
    if !(1..=120).contains(&probe.timeout_seconds)
        || probe.args.is_empty()
        || matches!(&probe.success, AuthProbeSuccess::StdoutContains { text } if text.is_empty())
    {
        return Err(DispatchError::CliInvocationPermanent(
            "auth_probe requires arguments, a 1..=120 second timeout and a non-empty success marker".into(),
        ));
    }
    let executor = host.resolve_cli_executor(provider)?;
    let program = resolve_provider_launcher(provider, &executor.command, Some(cwd))
        .map_err(|error| DispatchError::CliInvocationPermanent(error.message))?;
    let resolved = host.resolve_executor_sandbox(provider, None, Some(cwd))?;
    let prepared = prepare_sandbox_for_dispatch(resolved.as_ref())
        .map_err(|error| DispatchError::CliInvocationPermanent(error.message))?;
    // Rendering a normal invocation supplies exactly the provider's required
    // and fixed environment entries. Only the declared probe argv/stdin run.
    let config =
        AgentConfig::from_cli_config(executor.command, None, &host.provider_cli_config(provider))
            .map_err(|error| DispatchError::CliInvocationPermanent(error.to_string()))?;
    let agent = Agent::new(&config)
        .map_err(|error| DispatchError::CliInvocationPermanent(error.to_string()))?;
    let (invocation, _) = agent
        .invoke(AgentRequest {
            operation: AgentOperation::Activity {
                activity_id: "auth_probe".into(),
            },
            envelope_json: probe.stdin.as_bytes().to_vec(),
            verbose: false,
        })
        .map_err(|error| DispatchError::CliInvocationPermanent(error.to_string()))?;
    let mut env = provider_child_environment(
        host,
        provider,
        prepared.effective,
        invocation.required_env_vars,
    );
    let source = credential_source(provider, &env);
    // This is a credential check, not a nested activity. Do not forward the
    // parent's task identity, activity grants or broker binding.
    env.retain(|(key, _)| {
        !MCP_MANAGED_BINDING_ENV_VARS.contains(&key.as_str())
            && !key.starts_with("ORBIT_ACTIVITY_")
            && !["ORBIT_TASK_ID", "ORBIT_ACTIVE_TASK_ID"].contains(&key.as_str())
    });
    env.retain(|(key, _)| !invocation.fixed_env.iter().any(|(fixed, _)| key == fixed));
    env.extend(
        invocation
            .fixed_env
            .iter()
            .map(|(key, value)| ((*key).into(), (*value).into())),
    );
    env.extend(
        orbit_tool_env().map_err(|error| DispatchError::CliInvocationPermanent(error.message))?,
    );
    let mut spawned = spawn_for_supervision(
        &program,
        &probe.args,
        &env,
        Some(cwd),
        prepared.effective,
        provider,
    )
    .map_err(|error| DispatchError::CliInvocationPermanent(error.message))?;
    let guard = spawned.take_linux_post_run_guard();
    let registered = host
        .register_worker_process(spawned.child.id())
        .and_then(|()| {
            if prepared.effective.is_some_and(|sandbox| {
                sandbox.kind == orbit_types::workflow::ExecutorSandboxKind::LinuxBwrap
            }) {
                host.register_worker_pid_namespace(spawned.child.id())
            } else {
                Ok(())
            }
        });
    if let Err(error) = registered {
        let _ = spawned.child.kill();
        let _ = spawned.child.wait();
        return Err(DispatchError::CliInvocationPermanent(error.to_string()));
    }
    let cwd_text = cwd.display().to_string();
    let (stdout, _, exit, _, timed_out) = spawn_with_timeout(SpawnWithTimeoutRequest {
        program: &program,
        args: &probe.args,
        stdin_bytes: probe.stdin.as_bytes(),
        env: &env,
        cwd: Some(cwd),
        timeout: Duration::from_secs(probe.timeout_seconds),
        sandbox: prepared.effective,
        trace: SpawnTraceContext {
            provider,
            job_run_id: run_id,
            task_id: None,
            cwd: Some(&cwd_text),
        },
        output_capture_limit: Some(16 * 1024),
        on_spawn: None,
        on_progress: None,
        stopped_descendants: None,
        wait: None,
        live_readers: None,
        spawned_child: Some(spawned),
        #[cfg(unix)]
        cancel_pair: None,
    })
    .map_err(|SpawnError { message, .. }| DispatchError::CliInvocationPermanent(message))?;
    if let Some(guard) = guard {
        guard
            .verify()
            .map_err(|error| DispatchError::CliInvocationPermanent(error.to_string()))?;
    }
    host.refresh_persistence_after_cli_provider()
        .map_err(|error| DispatchError::CliInvocationPermanent(error.to_string()))?;
    let passed = !timed_out
        && exit == Some(0)
        && match &probe.success {
            AuthProbeSuccess::ExitZero => true,
            AuthProbeSuccess::StdoutContains { text } => {
                String::from_utf8_lossy(stdout.bytes()).contains(text)
            }
        };
    Ok(AuthProbeOutcome {
        passed,
        timed_out,
        credential_source: source,
    })
}
