/// Environment variable that grants [`orbit_types::tool::McpCapability::Operator`] to a caller the
/// envelope would otherwise leave unprivileged.
///
/// This is the escape hatch, and it is deliberately trivial to set: it is not
/// defending against a determined caller, it is making a deliberate act look
/// deliberate. Every use is logged and audited with
/// [`super::CallerProvenance::OperatorOverride`] so an override is visible in the
/// trail rather than indistinguishable from an ordinary operator session.
pub use crate::env_vars::ORBIT_OPERATOR as OPERATOR_OVERRIDE_ENV;

/// Whether [`OPERATOR_OVERRIDE_ENV`] is set to a truthy value in this
/// process's environment.
///
/// Exposed so a caller outside this module — actor-identity resolution, in
/// particular — can tell "an operator deliberately raised this" apart from
/// "nothing identified the caller" without re-deriving the truthy spellings
/// this module already owns.
pub fn operator_override_active(env: &crate::Env) -> bool {
    env_truthy(env.orbit_operator())
}

/// Whether this process's environment declares it to be an agent or a step of
/// a managed run.
///
/// Exposed so a caller outside this module can refuse to *propagate* operator
/// authority it may itself hold — the federated MCP client asks this before
/// composing a destination's argv, so a server an agent launched cannot hand
/// operator authority onward to another machine [ORB-12564]. It is the same
/// observation [`super::CallerEnvelope::from_process_env`] makes, named once so the
/// two cannot disagree about what an agent looks like.
pub fn agent_context_declared(env: &crate::Env) -> bool {
    agent_declared_in_env(env)
}

/// Whether an environment variable is set to a truthy value.
///
/// Accepts the same spellings as the engine's managed-run flag so a caller does
/// not have to remember which Orbit variable takes which vocabulary.
fn env_truthy(value: Option<&std::ffi::OsStr>) -> bool {
    value
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
}

fn env_present(value: Option<&std::ffi::OsStr>) -> bool {
    value
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|value| !value.trim().is_empty())
}

pub(super) fn agent_declared_in_env(env: &crate::Env) -> bool {
    if [env.orbit_agent_name(), env.orbit_agent_model()]
        .into_iter()
        .any(env_present)
    {
        return true;
    }
    if env_truthy(env.orbit_managed_run_context()) {
        return true;
    }
    env.orbit_task_actor_kind()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("agent"))
}

/// Whether a person is plausibly present at this process.
///
/// Both streams are required. `stdin` alone is true for a piped-output shell
/// pipeline, and `stderr` alone is true for a subprocess whose stderr was
/// inherited from a terminal — neither on its own distinguishes a person from
/// an automated caller that happened to inherit one handle.
pub(super) fn interactive_terminal() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}
