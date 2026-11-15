use orbit_common::OrbitError;
use orbit_types::identity::require_canonical_agent_family;

const ORBIT_AGENT_NAME: &str = "ORBIT_AGENT_NAME";
const ORBIT_AGENT_MODEL: &str = "ORBIT_AGENT_MODEL";
const ORBIT_ACTOR: &str = "ORBIT_ACTOR";
const OS_USER_ENV: &[&str] = &["USER", "USERNAME", "LOGNAME"];

/// Actor label recorded when [`OPERATOR_OVERRIDE_ENV`](orbit_common::governance::authorization::OPERATOR_OVERRIDE_ENV)
/// is the only signal identifying the caller.
const OPERATOR_ACTOR_LABEL: &str = "operator";
const UNKNOWN_ACTOR_LABEL: &str = "unknown";
const HUMAN_AUDIT_ROLE: &str = "human";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorKind {
    Unknown,
    Human,
    Agent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorIdentity {
    pub kind: ActorKind,
    pub label: String,
}

impl ActorIdentity {
    pub fn unknown() -> Self {
        Self {
            kind: ActorKind::Unknown,
            label: UNKNOWN_ACTOR_LABEL.to_string(),
        }
    }

    pub fn human(label: impl Into<String>) -> Self {
        Self {
            kind: ActorKind::Human,
            label: normalize_actor_label(label.into(), "human"),
        }
    }

    pub fn agent(label: impl Into<String>) -> Self {
        Self {
            kind: ActorKind::Agent,
            label: normalize_actor_label(label.into(), "agent"),
        }
    }

    /// Resolve the self-reported process identity used by CLI and dashboard
    /// entry points.
    ///
    /// The environment is not an authentication boundary. Agent values are
    /// therefore reduced to the same canonical family used by tool dispatch.
    /// An agent envelope without a canonical identity records `unknown` with
    /// a warning, before considering any human identity signals.
    /// Absent an agent envelope, an explicit `ORBIT_ACTOR` or operator
    /// override is recorded as a named human actor rather than `unknown` —
    /// those overrides are themselves a deliberate, audited act. Bare CLI
    /// otherwise records the OS user (`human:<username>`). A caller with
    /// no remaining signal is also recorded as `unknown`, and then only with a
    /// warning: write paths must not construct that literal themselves.
    pub fn from_env() -> Self {
        let agent = std::env::var(ORBIT_AGENT_NAME)
            .ok()
            .filter(|value| !value.trim().is_empty());
        let model = std::env::var(ORBIT_AGENT_MODEL)
            .ok()
            .filter(|value| !value.trim().is_empty());

        match require_canonical_agent_family(agent.as_deref(), model.as_deref()) {
            Ok(Some(family)) => return Self::agent(family),
            Err(error) => {
                tracing::warn!(
                    target: "orbit.core.actor",
                    actor = UNKNOWN_ACTOR_LABEL,
                    %error,
                    "agent identity did not canonicalize; recording unknown"
                );
                return Self::unknown();
            }
            Ok(None) => {}
        }

        if orbit_common::governance::authorization::agent_context_declared() {
            tracing::warn!(
                target: "orbit.core.actor",
                actor = UNKNOWN_ACTOR_LABEL,
                "agent envelope has no canonical identity; recording unknown"
            );
            return Self::unknown();
        }

        if let Some(label) = std::env::var(ORBIT_ACTOR)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
        {
            return Self::human(label);
        }

        if orbit_common::governance::authorization::operator_override_active() {
            return Self::human(OPERATOR_ACTOR_LABEL);
        }

        if let Some(user) = os_user_name() {
            return Self::human(format!("human:{user}"));
        }

        tracing::warn!(
            target: "orbit.core.actor",
            actor = UNKNOWN_ACTOR_LABEL,
            "no actor identity resolved; recording last-resort unknown"
        );
        Self::unknown()
    }

    /// Audit `role` for this process actor.
    ///
    /// Bare CLI records `human` (or `operator` when the operator override is
    /// the identity). Agent envelopes keep the canonical family. The
    /// last-resort unattributed label stays `unknown`.
    pub fn audit_role(&self) -> &str {
        match self.kind {
            ActorKind::Agent => self.label.as_str(),
            ActorKind::Human if self.label == OPERATOR_ACTOR_LABEL => OPERATOR_ACTOR_LABEL,
            ActorKind::Human => HUMAN_AUDIT_ROLE,
            ActorKind::Unknown => UNKNOWN_ACTOR_LABEL,
        }
    }

    /// Resolve the actor label recorded on a write.
    ///
    /// Explicit `model`/`agent` wins and must be a canonical family (or a
    /// full model string that infers one). Otherwise the process actor from
    /// [`Self::from_env`] is used.
    pub fn resolve_write_label(
        &self,
        agent: Option<&str>,
        model: Option<&str>,
    ) -> Result<String, OrbitError> {
        resolve_write_actor_label(&self.label, agent, model)
    }
}

/// Shared write-path actor resolution: explicit model/agent to a canonical
/// family, otherwise the process actor already resolved by [`ActorIdentity::from_env`].
pub fn resolve_write_actor_label(
    process_label: &str,
    agent: Option<&str>,
    model: Option<&str>,
) -> Result<String, OrbitError> {
    match require_canonical_agent_family(agent, model)? {
        Some(family) => Ok(family),
        None => Ok(process_label.to_string()),
    }
}

/// Preserve attribution supplied by a trusted in-process workflow boundary.
/// Public agent surfaces must use [`resolve_write_actor_label`] instead.
pub fn trusted_write_identity(
    agent: Option<&str>,
    model: Option<&str>,
) -> (Option<String>, Option<String>) {
    let trim = |value: Option<&str>| {
        value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    };
    (trim(agent), trim(model))
}

fn os_user_name() -> Option<String> {
    OS_USER_ENV.iter().find_map(|key| {
        std::env::var(key).ok().and_then(|value| {
            let trimmed = value.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        })
    })
}

impl Default for ActorIdentity {
    fn default() -> Self {
        Self::unknown()
    }
}

fn normalize_actor_label(label: String, default_label: &str) -> String {
    let label = label.trim();
    if label.is_empty() {
        default_label.to_string()
    } else {
        label.to_string()
    }
}
