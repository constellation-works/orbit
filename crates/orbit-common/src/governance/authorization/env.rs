/// Environment variable that grants [`orbit_types::tool::McpCapability::Operator`] to a caller the
/// envelope would otherwise leave unprivileged.
///
/// This is the escape hatch, and it is deliberately trivial to set: it is not
/// defending against a determined caller, it is making a deliberate act look
/// deliberate. Every use is logged and audited with
/// [`super::CallerProvenance::OperatorOverride`] so an override is visible in the
/// trail rather than indistinguishable from an ordinary operator session.
pub const OPERATOR_OVERRIDE_ENV: &str = "ORBIT_OPERATOR";

/// Whether [`OPERATOR_OVERRIDE_ENV`] is set to a truthy value in this
/// process's environment.
///
/// Exposed so a caller outside this module — actor-identity resolution, in
/// particular — can tell "an operator deliberately raised this" apart from
/// "nothing identified the caller" without re-deriving the truthy spellings
/// this module already owns.
pub fn operator_override_active() -> bool {
    env_truthy(OPERATOR_OVERRIDE_ENV)
}

/// Process-envelope variables that declare the caller to be an agent.
const AGENT_ENVELOPE_ENV: &[&str] = &["ORBIT_AGENT_NAME", "ORBIT_AGENT_MODEL"];

/// Whether this process's environment declares it to be an agent or a step of
/// a managed run.
///
/// Exposed so a caller outside this module can refuse to *propagate* operator
/// authority it may itself hold — the federated MCP client asks this before
/// composing a destination's argv, so a server an agent launched cannot hand
/// operator authority onward to another machine [ORB-12564]. It is the same
/// observation [`super::CallerEnvelope::from_process_env`] makes, named once so the
/// two cannot disagree about what an agent looks like.
pub fn agent_context_declared() -> bool {
    agent_declared_in_env()
}

/// Set to `agent` by the activity runner for agent-backed steps.
const ACTOR_KIND_ENV: &str = "ORBIT_TASK_ACTOR_KIND";

/// Set by the engine for any process it spawns as part of a managed run.
const MANAGED_RUN_ENV: &str = "ORBIT_MANAGED_RUN_CONTEXT";

/// Whether an environment variable is set to a truthy value.
///
/// Accepts the same spellings as the engine's managed-run flag so a caller does
/// not have to remember which Orbit variable takes which vocabulary.
pub(super) fn env_truthy(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn env_present(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| !value.trim().is_empty())
}

pub(super) fn agent_declared_in_env() -> bool {
    if AGENT_ENVELOPE_ENV.iter().copied().any(env_present) {
        return true;
    }
    if env_truthy(MANAGED_RUN_ENV) {
        return true;
    }
    std::env::var(ACTOR_KIND_ENV).is_ok_and(|value| value.trim().eq_ignore_ascii_case("agent"))
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
