//! Codex execution settings and agent subprocess environment projection.

use orbit_common::security::child_env::{
    allowlisted_child_env_from, apply_crew_env_from, inherited_child_env_from,
    login_auth_credential_names, unset_crew_env_from, unset_pass_names_from,
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
    env: orbit_common::Env,
    /// Whether a child inherits the parent environment wholesale.
    ///
    /// `ConfigSnapshot::execution_env_inherit` is a derived invariant pinned to
    /// `false` — `execution.env.inherit` stopped being settable in ORB-00365,
    /// because a workspace `config.toml` could flip it and replace the global
    /// value. The flag survives as the single seam that decides between full
    /// inheritance and the allowlist, so the two behaviors stay one branch in
    /// one place rather than two spawn paths. Inheritance still omits
    /// privilege-bearing `ORBIT_*` names.
    pub(crate) inherit: bool,
    /// `execution.env.pass`: the names an operator admits by name.
    pub(crate) pass: Vec<String>,
    /// `execution.env.login_auth`: providers authenticated by the CLI's own
    /// login. Their credential variables are not reported when unset.
    pub(crate) login_auth: Vec<String>,
    /// Immutable parent snapshot for a runtime supplied with credential defaults.
    parent: Arc<Vec<(String, String)>>,
    /// The source variable names the configured crews' `[crews.<name>] env`
    /// entries read (a `file:` source is read from disk, not the environment),
    /// so runtime defaults (`clock.env`) and a detached worker's launch
    /// environment can supply them as well as `pass` names.
    crew_sources: Vec<String>,
}

impl std::fmt::Debug for ExecutionEnvPolicy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExecutionEnvPolicy")
            .field("inherit", &self.inherit)
            .field("pass", &self.pass)
            .field("login_auth", &self.login_auth)
            .field("crew_sources", &self.crew_sources)
            .finish()
    }
}

impl ExecutionEnvPolicy {
    /// The default allowlist applied to the supplied parent environment.
    pub fn new(env: &orbit_common::Env) -> Self {
        Self {
            inherit: false,
            pass: default_pass_list(),
            login_auth: Vec::new(),
            parent: Arc::new(parent_vars(env)),
            env: env.clone(),
            crew_sources: Vec::new(),
        }
    }
}

impl ExecutionEnvPolicy {
    pub(super) fn from_snapshot(env: &orbit_common::Env, snapshot: &ConfigSnapshot) -> Self {
        Self {
            inherit: snapshot.execution_env_inherit,
            pass: snapshot.execution_env_pass.clone(),
            login_auth: snapshot.execution_env_login_auth.clone(),
            parent: Arc::new(parent_vars(env)),
            env: env.clone(),
            crew_sources: Vec::new(),
        }
    }

    /// Record the sources the configured crews' `env` entries read.
    #[must_use]
    pub(super) fn with_crew_env_sources<'a>(
        mut self,
        entries: impl IntoIterator<Item = &'a CrewEnvVar>,
    ) -> Self {
        for source in entries
            .into_iter()
            .filter_map(|entry| entry.source.variable())
        {
            if !self.crew_sources.iter().any(|held| held == source) {
                self.crew_sources.push(source.to_string());
            }
        }
        self
    }

    /// The parent process snapshot before runtime-supplied child defaults.
    pub fn process_env(&self) -> &orbit_common::Env {
        &self.env
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

    /// Use the environment captured by the composition root.
    #[must_use]
    pub fn with_parent_env(mut self, env: &orbit_common::Env) -> Self {
        self.env = env.clone();
        self.parent = Arc::new(parent_vars(env));
        self
    }

    /// Use the parent environment with defaults for this policy's pass
    /// names and crew `env` sources. Non-empty ambient values take precedence.
    /// Other defaults are ignored, including when a provider later requests
    /// those names as extras. The process environment is never changed.
    #[must_use]
    pub fn with_defaults(mut self, defaults: &[(String, String)]) -> Self {
        let mut parent = parent_vars(&self.env);
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
        self.parent = Arc::new(parent);
        self
    }

    /// The complete environment an agent subprocess is launched with.
    ///
    /// This is the only place the policy becomes a concrete child environment,
    /// and every subprocess launcher starts from a cleared environment and
    /// applies exactly this — so `inherit = false` really is allowlist-based
    /// rather than a filter over ambient variables. `extras` carries the names
    /// a provider declares it requires. `inherit = true` forwards the parent
    /// environment except privilege-bearing `ORBIT_*` names, the same names
    /// `allowlisted_child_env_from` refuses.
    pub fn agent_subprocess_env(&self, extras: &[&str]) -> Vec<(String, String)> {
        if self.inherit {
            return inherited_child_env_from(&self.parent);
        }
        allowlisted_child_env_from(&self.env, &self.parent, &self.pass, extras)
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
            apply_crew_env_from(&self.env, &mut env, &self.parent, crew_env);
        }
        env
    }

    /// The crew `env` entries whose source variable the effective parent
    /// environment, runtime defaults included, holds no non-empty value for,
    /// or whose source file is missing, empty or refused. Names and paths only.
    pub fn unset_crew_env(&self, crew_env: &[CrewEnvVar]) -> Vec<CrewEnvVar> {
        if crew_env.is_empty() {
            return Vec::new();
        }
        unset_crew_env_from(&self.env, &self.parent, crew_env)
    }

    /// The values the effective parent environment holds for the crews' `env`
    /// sources, for a detached worker's launch environment: the worker reads
    /// them from its own environment when it composes a crew's agent env.
    pub fn crew_source_env(&self) -> Vec<(String, String)> {
        if self.crew_sources.is_empty() {
            return Vec::new();
        }
        self.parent
            .iter()
            .filter(|(name, value)| self.crew_sources.contains(name) && !value.is_empty())
            .cloned()
            .collect()
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
    /// need it. Credential variables of a provider named in
    /// `execution.env.login_auth` are not reported either. Empty when the
    /// policy inherits the whole environment.
    pub fn unset_pass_names(&self) -> Vec<String> {
        self.unset_pass_names_in(&self.parent)
    }

    /// [`Self::unset_pass_names`] over an explicit parent environment.
    pub(crate) fn unset_pass_names_in(&self, parent: &[(String, String)]) -> Vec<String> {
        if self.inherit {
            return Vec::new();
        }
        let defaults = default_pass_list();
        let added: Vec<String> = self
            .pass
            .iter()
            .filter(|name| {
                !defaults.contains(name)
                    && !self.login_auth_covers(name)
                    && !PLATFORM_SPECIFIC_TEMPLATE_PASS_NAMES.contains(&name.as_str())
            })
            .cloned()
            .collect();
        unset_pass_names_from(parent, &added)
    }
}

impl ExecutionEnvPolicy {
    /// Whether `name` is a credential variable of a provider the operator
    /// declared login-authenticated in `execution.env.login_auth`. Orbit
    /// reads no credential: the declaration is the only signal.
    fn login_auth_covers(&self, name: &str) -> bool {
        self.login_auth
            .iter()
            .any(|provider| login_auth_credential_names(provider).contains(&name))
    }
}

fn parent_vars(env: &orbit_common::Env) -> Vec<(String, String)> {
    env.utf8_vars()
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect()
}

fn default_pass_list() -> Vec<String> {
    ConfigSnapshot::default().execution_env_pass
}
