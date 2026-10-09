//! Codex execution settings and agent subprocess environment projection.

use orbit_common::security::child_env::{
    allowlisted_child_env, inherited_child_env, unset_pass_names_from,
};

use crate::registry::ConfigSnapshot;

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
#[derive(Debug, Clone)]
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
}

impl Default for ExecutionEnvPolicy {
    fn default() -> Self {
        Self {
            inherit: false,
            pass: default_pass_list(),
        }
    }
}

impl ExecutionEnvPolicy {
    pub(super) fn from_snapshot(snapshot: &ConfigSnapshot) -> Self {
        Self {
            inherit: snapshot.execution_env_inherit,
            pass: snapshot.execution_env_pass.clone(),
        }
    }

    /// Whether the full process environment is inherited rather than
    /// allow-listed.
    pub fn inherit(&self) -> bool {
        self.inherit
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
            return inherited_child_env();
        }
        allowlisted_child_env(&self.pass, extras)
    }
}

impl ExecutionEnvPolicy {
    /// The operator-added `execution.env.pass` names the launching process
    /// holds no value for, so no agent it starts receives them [ORB-14777].
    ///
    /// The built-in defaults (`HOME`, `PATH`, `CODEX_HOME`, and the
    /// macOS-only `__CF_USER_TEXT_ENCODING`, …) are not reported: they can
    /// legitimately be absent on a host, and warning on each would fire on
    /// every drain start. A name the operator added is a statement that agents
    /// need it. Empty when the policy inherits the whole environment.
    pub fn unset_pass_names(&self) -> Vec<String> {
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
            .filter(|name| !defaults.contains(name))
            .cloned()
            .collect();
        unset_pass_names_from(parent, &added)
    }
}

fn default_pass_list() -> Vec<String> {
    ConfigSnapshot::default().execution_env_pass
}
