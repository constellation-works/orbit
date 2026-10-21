//! Codex execution settings and agent subprocess environment projection.

use orbit_common::security::child_env::{
    allowlisted_child_env, allowlisted_child_env_from, apply_crew_env_from, inherited_child_env,
    unset_crew_env_from, unset_pass_names_from,
};
use orbit_types::identity::CrewEnvVar;
use std::sync::Arc;

use crate::registry::ConfigSnapshot;

// The shipped config template includes this for macOS, where CoreFoundation
// needs it. Keep treating it as a default on other platforms too, where it is
// normally absent from the launching environment.
const PLATFORM_SPECIFIC_TEMPLATE_PASS_NAMES: &[&str] = &["__CF_USER_TEXT_ENCODING"];

/// Codex sandbox and approval policy resolved from `[execution.codex]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexExecutionPolicy {
    sandbox: String,
    approval_policy: Option<String>,
}

impl Default for CodexExecutionPolicy {
    fn default() -> Self {
        Self {
            sandbox: "workspace-write".to_string(),
            approval_policy: None,
        }
    }
}

impl CodexExecutionPolicy {
    pub(super) fn from_snapshot(snapshot: &ConfigSnapshot) -> Self {
        Self {
            sandbox: snapshot.codex_sandbox.clone(),
            approval_policy: snapshot.codex_approval_policy.clone(),
        }
    }

    /// Configured sandbox mode.
    pub fn sandbox(&self) -> &str {
        &self.sandbox
    }

    /// Configured approval policy, when one is set.
    pub fn approval_policy(&self) -> Option<&str> {
        self.approval_policy.as_deref()
    }
}

/// Environment passthrough policy for agent subprocesses, resolved from
/// `[execution.env]`.
#[derive(Clone)]
pub struct ExecutionEnvPolicy {
    /// Whether a child inherits the parent environment wholesale.
    ///
    /// `ConfigSnapshot::execution_env_inherit` is a derived invariant pinned to
    /// `false` — `execution.env.inherit` stopped being settable in ORB-00365,
    /// because a workspace `config.toml` could flip it and replace the global
    /// value. The flag survives as the single seam that decides between full
    /// inheritance and the allowlist, so the two behaviors stay one branch in
    /// one place rather than two spawn paths.
    pub(crate) inherit: bool,
    /// `execution.env.pass`: the names an operator admits by name.
    pub(crate) pass: Vec<String>,
    /// Immutable parent snapshot for a runtime supplied with credential defaults.
    parent: Option<Arc<Vec<(String, String)>>>,
    /// The source names the configured crews' `[crews.<name>] env` entries
    /// read, so runtime defaults (`clock.env`) and a detached worker's launch
    /// environment can supply them as well as `pass` names.
    crew_sources: Vec<String>,
}

impl std::fmt::Debug for ExecutionEnvPolicy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExecutionEnvPolicy")
            .field("inherit", &self.inherit)
            .field("pass", &self.pass)
            .field("has_parent_snapshot", &self.parent.is_some())
            .field("crew_sources", &self.crew_sources)
            .finish()
    }
}

impl Default for ExecutionEnvPolicy {
    fn default() -> Self {
        Self {
            inherit: false,
            pass: default_pass_list(),
            parent: None,
            crew_sources: Vec::new(),
        }
    }
}

impl ExecutionEnvPolicy {
    pub(super) fn from_snapshot(snapshot: &ConfigSnapshot) -> Self {
        Self {
            inherit: snapshot.execution_env_inherit,
            pass: snapshot.execution_env_pass.clone(),
            parent: None,
            crew_sources: Vec::new(),
        }
    }

    /// Record the sources the configured crews' `env` entries read.
    #[must_use]
    pub(super) fn with_crew_env_sources<'a>(
        mut self,
        entries: impl IntoIterator<Item = &'a CrewEnvVar>,
    ) -> Self {
        for entry in entries {
            if !self.crew_sources.contains(&entry.source) {
                self.crew_sources.push(entry.source.clone());
            }
        }
        self
    }

    /// Whether the full process environment is inherited rather than
    /// allow-listed.
    pub fn inherit(&self) -> bool {
        self.inherit
    }

    /// The `execution.env.pass` names the operator admits.
    pub fn pass_names(&self) -> &[String] {
        &self.pass
    }

    /// Every name a runtime default may supply: the `pass` names and the
    /// crews' `env` sources.
    pub fn default_names(&self) -> Vec<String> {
        let mut names = self.pass.clone();
        for source in &self.crew_sources {
            if !names.contains(source) {
                names.push(source.clone());
            }
        }
        names
    }

    /// Capture the ambient environment with defaults for this policy's pass
    /// names and crew `env` sources. Non-empty ambient values take precedence.
    /// Other defaults are ignored, including when a provider later requests
    /// those names as extras. The process environment is never changed.
    #[must_use]
    pub fn with_defaults(mut self, defaults: &[(String, String)]) -> Self {
        let mut parent = std::env::vars().collect::<Vec<_>>();
        for (name, value) in defaults {
            if !(self.pass.contains(name) || self.crew_sources.contains(name))
                || parent
                    .iter()
                    .any(|(key, held)| key == name && !held.is_empty())
            {
                continue;
            }
            parent.retain(|(key, _)| key != name);
            parent.push((name.clone(), value.clone()));
        }
        self.parent = Some(Arc::new(parent));
        self
    }

    /// The complete environment an agent subprocess is launched with.
    ///
    /// This is the only place the policy becomes a concrete child environment,
    /// and every subprocess launcher starts from a cleared environment and
    /// applies exactly this — so `inherit = false` really is allowlist-based
    /// rather than a filter over ambient variables. `extras` carries the names
    /// a provider declares it requires.
    pub fn agent_subprocess_env(&self, extras: &[&str]) -> Vec<(String, String)> {
        if self.inherit {
            return self
                .parent
                .as_deref()
                .cloned()
                .unwrap_or_else(inherited_child_env);
        }
        if let Some(parent) = &self.parent {
            return allowlisted_child_env_from(parent, &self.pass, extras);
        }
        allowlisted_child_env(&self.pass, extras)
    }

    /// [`Self::agent_subprocess_env`] for an agent of a crew with
    /// `[crews.<name>] env` entries, overlaid with
    /// `orbit_common::security::child_env::apply_crew_env_from`: crew entries
    /// win over `pass`, and a crew that sets one Claude credential has the
    /// other removed.
    pub fn crew_agent_subprocess_env(
        &self,
        crew_env: &[CrewEnvVar],
        extras: &[&str],
    ) -> Vec<(String, String)> {
        let mut env = self.agent_subprocess_env(extras);
        if !crew_env.is_empty() {
            apply_crew_env_from(&mut env, &self.parent_env(), crew_env);
        }
        env
    }

    /// The crew `env` entries whose source the effective parent environment,
    /// runtime defaults included, holds no non-empty value for. Names only.
    pub fn unset_crew_env(&self, crew_env: &[CrewEnvVar]) -> Vec<CrewEnvVar> {
        if crew_env.is_empty() {
            return Vec::new();
        }
        unset_crew_env_from(&self.parent_env(), crew_env)
    }

    /// The values the effective parent environment holds for the crews' `env`
    /// sources, for a detached worker's launch environment: the worker reads
    /// them from its own environment when it composes a crew's agent env.
    pub fn crew_source_env(&self) -> Vec<(String, String)> {
        if self.crew_sources.is_empty() {
            return Vec::new();
        }
        self.parent_env()
            .into_iter()
            .filter(|(name, value)| self.crew_sources.contains(name) && !value.is_empty())
            .collect()
    }

    fn parent_env(&self) -> Vec<(String, String)> {
        match &self.parent {
            Some(parent) => parent.as_ref().clone(),
            None => std::env::vars().collect(),
        }
    }
}

impl ExecutionEnvPolicy {
    /// The operator-added `execution.env.pass` names the effective parent
    /// environment holds no value for, including runtime-supplied defaults,
    /// so no agent it starts receives them [ORB-14777].
    ///
    /// The built-in defaults (`HOME`, `PATH`, `CODEX_HOME`, and the
    /// platform-specific `__CF_USER_TEXT_ENCODING`, …) are not reported: they
    /// can legitimately be absent on a host, and warning on each would fire on
    /// every drain start. A name the operator added is a statement that agents
    /// need it. Empty when the policy inherits the whole environment.
    pub fn unset_pass_names(&self) -> Vec<String> {
        if let Some(parent) = &self.parent {
            return self.unset_pass_names_in(parent);
        }
        self.unset_pass_names_in(&std::env::vars().collect::<Vec<_>>())
    }

    /// [`Self::unset_pass_names`] over an explicit parent environment.
    pub fn unset_pass_names_in(&self, parent: &[(String, String)]) -> Vec<String> {
        if self.inherit {
            return Vec::new();
        }
        let defaults = default_pass_list();
        let added: Vec<String> = self
            .pass
            .iter()
            .filter(|name| {
                !defaults.contains(name)
                    && !PLATFORM_SPECIFIC_TEMPLATE_PASS_NAMES.contains(&name.as_str())
            })
            .cloned()
            .collect();
        unset_pass_names_from(parent, &added)
    }
}

fn default_pass_list() -> Vec<String> {
    ConfigSnapshot::default().execution_env_pass
}
