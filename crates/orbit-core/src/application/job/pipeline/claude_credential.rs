//! The crew window of a drain on a host where some crews cannot run: a host
//! that needs a Claude worker credential and holds none [ORB-16663], or a
//! crew whose `[crews.<name>] env` source is unset.
//!
//! On macOS a `claude` leaf without `CLAUDE_CODE_OAUTH_TOKEN` or
//! `ANTHROPIC_API_KEY` is refused before the provider starts, and that refusal
//! is permanent, so each Claude leaf the drain claims fails. A drain started
//! from a shell that skips `~/.zprofile` would keep claiming Claude work into
//! those failures. A crew whose `env` names a source this environment does
//! not hold is refused the same way, rather than run on the provider's
//! default login. The drain's window therefore excludes those crews, and its
//! submission names the remediation once. Each crew is judged by its own
//! composed agent environment, so a Claude crew that maps its own credential
//! through `env` runs where the shared one is missing.

use orbit_common::OrbitError;
use orbit_engine::RuntimeHost;
use orbit_engine::activity_job::cli_runner::{
    CLAUDE_WORKER_CREDENTIAL_REMEDY, claude_worker_credential_held,
};
use orbit_types::identity::{Crew, resolve_crew};
use orbit_types::workflow::CrewEnvUnset;
use orbit_types::workflow::activity_job::Provider;

use crate::OrbitRuntime;

fn is_claude(crew: &Crew) -> bool {
    Provider::parse(&crew.assignment.provider).is_ok_and(|provider| provider == Provider::Claude)
}

/// Why a crew cannot run on this host.
enum Unrunnable {
    /// A Claude crew whose composed environment holds no worker credential.
    NoClaudeCredential,
    /// A crew whose `env` names a source this environment does not hold.
    UnsetEnv(CrewEnvUnset),
}

impl OrbitRuntime {
    /// Why `crew` cannot run here, judged on its own composed agent
    /// environment. `None` for a crew that can (or is disabled, which
    /// selection refuses on its own).
    fn crew_unrunnable(&self, crew: &Crew) -> Option<Unrunnable> {
        if !crew.enabled {
            return None;
        }
        let policy = self.execution_env_policy();
        let unset = policy.unset_crew_env(&crew.env);
        if !unset.is_empty() {
            return Some(Unrunnable::UnsetEnv(CrewEnvUnset {
                crew: crew.name.clone(),
                env: unset,
            }));
        }
        (is_claude(crew)
            && RuntimeHost::requires_claude_worker_credential(self)
            && !claude_worker_credential_held(&policy.crew_agent_subprocess_env(&crew.env, &[])))
        .then_some(Unrunnable::NoClaudeCredential)
    }

    /// Every enabled crew whose `[crews.<name>] env` names a source this
    /// runtime's environment (runtime defaults such as `clock.env` included)
    /// holds no value for. Names only, never values.
    pub fn unset_crew_env(&self) -> Vec<CrewEnvUnset> {
        self.context
            .settings()
            .crews()
            .values()
            .filter_map(|crew| match self.crew_unrunnable(crew) {
                Some(Unrunnable::UnsetEnv(unset)) => Some(unset),
                _ => None,
            })
            .collect()
    }

    /// The warning for [`Self::unset_crew_env`], for drain, ship and job
    /// starts and `orbit doctor`. `None` when every crew's sources are set.
    pub fn unset_crew_env_warning(&self) -> Option<String> {
        let unset = self.unset_crew_env();
        (!unset.is_empty()).then(|| {
            format!(
                "{}: unset or empty in this environment, so drains here exclude {} and \
                 {} activities are refused rather than run on another login. Export the \
                 source before starting (clock-started runs read ~/.orbit/clock.env)",
                unset
                    .iter()
                    .map(CrewEnvUnset::describe)
                    .collect::<Vec<_>>()
                    .join("; "),
                if unset.len() == 1 {
                    "that crew"
                } else {
                    "those crews"
                },
                if unset.len() == 1 { "its" } else { "their" },
            )
        })
    }

    /// The crews a drain submitted here may run, from the window it asked for
    /// (`requested`, canonical names, empty when it named none).
    ///
    /// With every crew runnable the window is returned unchanged. Otherwise
    /// the unrunnable crews are dropped: an explicit window that names only
    /// them is refused, and an unnamed window becomes every enabled crew that
    /// can run here. An empty list would mean "unrestricted", so a host with
    /// no such crew refuses rather than admit work it cannot run.
    pub(crate) fn drain_crews_runnable_here(
        &self,
        requested: Vec<String>,
    ) -> Result<Vec<String>, OrbitError> {
        let crews = self.context.settings().crews();
        let blocked: Vec<(&Crew, Unrunnable)> = crews
            .values()
            .filter_map(|crew| self.crew_unrunnable(crew).map(|reason| (crew, reason)))
            .collect();
        if blocked.is_empty() {
            return Ok(requested);
        }
        let runnable = |name: &str| {
            resolve_crew(name, crews)
                .is_ok_and(|crew| !blocked.iter().any(|(blocked, _)| blocked.name == crew.name))
        };
        if !requested.is_empty() {
            let kept: Vec<String> = requested
                .into_iter()
                .filter(|name| runnable(name))
                .collect();
            if kept.is_empty() {
                return Err(OrbitError::InvalidInput(format!(
                    "--allow-crew names only crews this host cannot run: {}",
                    blocked_reasons(&blocked)
                )));
            }
            return Ok(kept);
        }
        let names: Vec<String> = crews
            .values()
            .filter(|crew| crew.enabled && runnable(&crew.name))
            .map(|crew| crew.name.clone())
            .collect();
        if names.is_empty() {
            return Err(OrbitError::InvalidInput(format!(
                "this host has no enabled crew it can run, so a drain here would claim \
                 nothing: {}",
                blocked_reasons(&blocked)
            )));
        }
        Ok(names)
    }

    /// The submission warning for a drain whose window excludes Claude crews
    /// for want of the credential [ORB-16663]. `None` when none is excluded.
    pub(crate) fn claude_worker_credential_warning(&self) -> Option<String> {
        self.context
            .settings()
            .crews()
            .values()
            .any(|crew| {
                matches!(
                    self.crew_unrunnable(crew),
                    Some(Unrunnable::NoClaudeCredential)
                )
            })
            .then(|| {
                format!(
                    "this host has no Claude worker credential, so this drain runs no Claude \
                     crew without its own: {CLAUDE_WORKER_CREDENTIAL_REMEDY}"
                )
            })
    }
}

/// One clause per blocked crew; the Claude remediation is named once.
fn blocked_reasons(blocked: &[(&Crew, Unrunnable)]) -> String {
    let mut reasons: Vec<String> = blocked
        .iter()
        .filter_map(|(_, reason)| match reason {
            Unrunnable::UnsetEnv(unset) => Some(format!("{} is unset", unset.describe())),
            Unrunnable::NoClaudeCredential => None,
        })
        .collect();
    let claude: Vec<&str> = blocked
        .iter()
        .filter(|(_, reason)| matches!(reason, Unrunnable::NoClaudeCredential))
        .map(|(crew, _)| crew.name.as_str())
        .collect();
    if !claude.is_empty() {
        reasons.push(format!(
            "Claude crews {} have no worker credential: {CLAUDE_WORKER_CREDENTIAL_REMEDY}",
            claude.join(", ")
        ));
    }
    reasons.join("; ")
}
