use orbit_engine::{DispatchError, ResolvedCliExecutor, ResolvedShellExecutor};
use orbit_types::workflow::ExecutorType;
use orbit_types::workflow::activity_job::{Provider, ProviderEntryPoint};

use crate::OrbitRuntime;

/// Map a v2 provider name to the CLI executor that dispatches it. Env-var
/// overrides (`ORBIT_V2_CLI_<PROVIDER>`) let smokes substitute a fixture
/// binary for the real provider CLI; production normally comes from the
/// registered executor def, falling back to the provider name itself
/// (`claude`, `codex`, `gemini`, `grok`, `ollama`) when no executor is registered.
pub(crate) fn resolve_cli_executor(
    runtime: &OrbitRuntime,
    provider: &str,
) -> Result<ResolvedCliExecutor, DispatchError> {
    let env_key = format!("ORBIT_V2_CLI_{}", provider.to_ascii_uppercase());
    let env_command = std::env::var(&env_key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    if let Some(def) = runtime.get_executor_def(provider).map_err(|err| {
        DispatchError::CliInvocationFailed(format!("load executor `{provider}`: {err}"))
    })? {
        if !matches!(
            def.executor_type,
            ExecutorType::DirectAgent | ExecutorType::AgentCli
        ) {
            return Err(DispatchError::CliInvocationFailed(format!(
                "executor `{provider}` has type `{}`; backend: cli requires a direct_agent or agent_cli executor",
                def.executor_type
            )));
        }

        let command = env_command
            .or_else(|| {
                def.command
                    .as_ref()
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
            })
            .ok_or_else(|| {
                DispatchError::CliInvocationFailed(format!(
                    "executor `{provider}` is missing a command"
                ))
            })?;

        return Ok(ResolvedCliExecutor {
            command,
            args: def.args,
        });
    }

    if let Some(command) = env_command {
        return Ok(ResolvedCliExecutor {
            command,
            args: Vec::new(),
        });
    }

    // Canonical provider identity + capability live on the orbit-common
    // `Provider` surface (ORB-10091). Selecting a provider that parses but has
    // no CLI runtime (`openai_compat`, HTTP-only) or does not parse at all
    // fails with a stable diagnostic and never silently falls back to a
    // different runtime.
    match Provider::parse(provider) {
        Ok(parsed)
            if Provider::capabilities(ProviderEntryPoint::Orbit).contains(&parsed)
                && parsed.has_cli_runtime() =>
        {
            Ok(ResolvedCliExecutor {
                command: parsed.as_str().to_string(),
                args: Vec::new(),
            })
        }
        Ok(Provider::OpenaiCompat) => Err(DispatchError::CliInvocationFailed(
            "provider openai_compat is unsupported by the Orbit CLI entry point (HTTP-only)"
                .to_string(),
        )),
        Ok(parsed) => Err(DispatchError::CliInvocationFailed(format!(
            "provider {parsed} is unsupported by the Orbit CLI entry point"
        ))),
        Err(err) => Err(DispatchError::CliInvocationFailed(format!(
            "{err} — no CLI runtime registered"
        ))),
    }
}

/// Resolve the executor definition backing a `local_shell` deterministic step.
///
/// Deliberately separate from [`resolve_cli_executor`]: that function resolves
/// an *agent* provider and rejects everything that is not `direct_agent` /
/// `agent_cli`, while this one accepts only `local_shell`. A shell step can
/// therefore never borrow an agent executor's identity, and an agent activity
/// can never be pointed at a shell definition.
///
/// A definition written before [ORB-11294] spells this family `cli_command`;
/// `ExecutorType` accepts that spelling as an alias, so a legacy bundled or
/// user-authored `local-shell` definition loads unchanged and its `command`,
/// `args`, `env`, and `timeout_seconds` keep working as the step's defaults.
pub(crate) fn resolve_local_shell_executor(
    runtime: &OrbitRuntime,
    executor: &str,
) -> Result<ResolvedShellExecutor, DispatchError> {
    let Some(def) = runtime.get_executor_def(executor).map_err(|err| {
        DispatchError::DeterministicActionFailed {
            action: "local_shell".to_string(),
            message: format!("load executor `{executor}`: {err}"),
        }
    })?
    else {
        return Err(DispatchError::DeterministicActionFailed {
            action: "local_shell".to_string(),
            message: format!(
                "executor `{executor}` is not registered; run `orbit init` to seed the shipped \
                 definitions or register your own"
            ),
        });
    };
    if def.executor_type != ExecutorType::LocalShell {
        return Err(DispatchError::DeterministicActionFailed {
            action: "local_shell".to_string(),
            message: format!(
                "executor `{executor}` has type `{}`; a local_shell step requires a local_shell executor",
                def.executor_type
            ),
        });
    }
    Ok(ResolvedShellExecutor {
        command: def
            .command
            .as_ref()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        args: def.args,
        env: def.env.into_iter().collect(),
        timeout_seconds: def.timeout_seconds,
    })
}
