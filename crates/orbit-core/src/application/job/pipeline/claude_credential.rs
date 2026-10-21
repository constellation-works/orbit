//! The crew window of a drain on a host that needs a Claude worker credential
//! and holds none [ORB-16663].
//!
//! On macOS a `claude` leaf without `CLAUDE_CODE_OAUTH_TOKEN` or
//! `ANTHROPIC_API_KEY` is refused before the provider starts, and that refusal
//! is permanent, so each Claude leaf the drain claims fails. A drain started
//! from a shell that skips `~/.zprofile` would keep claiming Claude work into
//! those failures. Its window therefore excludes the Claude crews, and its
//! submission names the remediation once.
//!
//! A drain queued behind a live one is read back the same way: when its
//! submission recorded every listed Claude credential name as unset, readiness
//! names the Claude crews in its window so the dashboard can flag it before it
//! starts.

use orbit_common::OrbitError;
use orbit_engine::RuntimeHost;
use orbit_engine::activity_job::cli_runner::{
    CLAUDE_WORKER_CREDENTIAL_ENV, CLAUDE_WORKER_CREDENTIAL_REMEDY, claude_worker_credential_held,
};
use orbit_types::identity::{Crew, resolve_crew};
use orbit_types::workflow::activity_job::Provider;

use crate::OrbitRuntime;

fn is_claude(crew: &Crew) -> bool {
    Provider::parse(&crew.assignment.provider).is_ok_and(|provider| provider == Provider::Claude)
}

impl OrbitRuntime {
    /// Whether a Claude crew this host configures cannot run here: the host
    /// requires the credential and the policy environment, which every leaf
    /// receives, holds neither name.
    fn claude_crews_blocked(&self) -> bool {
        RuntimeHost::requires_claude_worker_credential(self)
            && !claude_worker_credential_held(
                &self.execution_env_policy().agent_subprocess_env(&[]),
            )
            && self
                .context
                .settings()
                .crews()
                .values()
                .any(|crew| crew.enabled && is_claude(crew))
    }

    /// The crews a drain submitted here may run, from the window it asked for
    /// (`requested`, canonical names, empty when it named none).
    ///
    /// With no Claude crew blocked the window is returned unchanged. Otherwise
    /// the Claude crews are dropped: an explicit window that names only them
    /// is refused, and an unnamed window becomes every enabled crew that runs
    /// another provider. An empty list would mean "unrestricted", so a host
    /// with no such crew refuses rather than admit Claude work.
    pub(crate) fn drain_crews_without_worker_credential(
        &self,
        requested: Vec<String>,
    ) -> Result<Vec<String>, OrbitError> {
        if !self.claude_crews_blocked() {
            return Ok(requested);
        }
        let crews = self.context.settings().crews();
        let runnable = |name: &str| resolve_crew(name, crews).is_ok_and(|crew| !is_claude(&crew));
        if !requested.is_empty() {
            let kept: Vec<String> = requested
                .into_iter()
                .filter(|name| runnable(name))
                .collect();
            if kept.is_empty() {
                return Err(OrbitError::InvalidInput(format!(
                    "--allow-crew names only Claude crews, and this host cannot run them: \
                     {CLAUDE_WORKER_CREDENTIAL_REMEDY}"
                )));
            }
            return Ok(kept);
        }
        let names: Vec<String> = crews
            .values()
            .filter(|crew| crew.enabled && !is_claude(crew))
            .map(|crew| crew.name.clone())
            .collect();
        if names.is_empty() {
            return Err(OrbitError::InvalidInput(format!(
                "this host has no enabled crew on another provider, and its Claude crews cannot \
                 run without a worker credential, so a drain here would claim nothing: \
                 {CLAUDE_WORKER_CREDENTIAL_REMEDY}"
            )));
        }
        Ok(names)
    }

    /// The Claude crews in a submitted drain's window (`window`, canonical
    /// names, empty for every enabled crew) that it will start with no worker
    /// credential, judged from the names its submission recorded as unset
    /// (`env_pass_unset`). Empty unless a Claude credential name is among them
    /// and no other one is still listed in `execution.env.pass`. Only names are
    /// compared, never values.
    pub(crate) fn claude_crews_without_recorded_credential(
        &self,
        window: &[String],
        env_pass_unset: &[String],
    ) -> Vec<String> {
        let unset = |name: &str| env_pass_unset.iter().any(|unset| unset == name);
        let pass = self.execution_env_policy().pass_names();
        let still_passed = CLAUDE_WORKER_CREDENTIAL_ENV
            .iter()
            .any(|name| !unset(name) && pass.iter().any(|passed| passed == name));
        if still_passed || !CLAUDE_WORKER_CREDENTIAL_ENV.iter().any(|name| unset(name)) {
            return Vec::new();
        }
        let crews = self.context.settings().crews();
        if window.is_empty() {
            return crews
                .values()
                .filter(|crew| crew.enabled && is_claude(crew))
                .map(|crew| crew.name.clone())
                .collect();
        }
        window
            .iter()
            .filter(|name| resolve_crew(name, crews).is_ok_and(|crew| is_claude(&crew)))
            .cloned()
            .collect()
    }

    /// The submission warning for a drain whose window excludes Claude crews
    /// for want of the credential [ORB-16663]. `None` when none is excluded.
    pub(crate) fn claude_worker_credential_warning(&self) -> Option<String> {
        self.claude_crews_blocked().then(|| {
            format!(
                "this host has no Claude worker credential, so this drain runs no Claude crew: \
                 {CLAUDE_WORKER_CREDENTIAL_REMEDY}"
            )
        })
    }
}
