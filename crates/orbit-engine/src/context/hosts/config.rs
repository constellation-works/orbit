//! Runtime-resolved crew and pull-request configuration.

use orbit_types::workflow::activity_job::Provider;

/// Resolved crew assignment from `config.toml`. Each field
/// is independently optional — the resolver in
/// `crate::activity_job::crew` falls back to the inline activity value
/// for any field the config does not specify.
///
/// String fields from the on-disk crew assignment are parsed into the
/// strongly typed activity-job enums at the orbit-core boundary; an
/// unrecognized provider yields `None` for that field rather than
/// silently coercing dispatch to a wrong runtime.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CrewConfig {
    pub name: Option<String>,
    pub provider: Option<Provider>,
    pub model: Option<String>,
    pub reasoning_effort: Option<orbit_types::identity::ReasoningEffort>,
    /// The crew's `[crews.<name>] env` entries, names only. Dispatch overlays
    /// them on the agent environment through
    /// [`crate::RuntimeHost::crew_agent_subprocess_environment`].
    pub env: Vec<orbit_types::identity::CrewEnvVar>,
    /// The crew's name when it sets `env` and so is its own provider account
    /// ([`orbit_types::identity::Crew::account`]); `None` for a crew on its
    /// provider's default login.
    pub account: Option<String>,
    /// The crew sets `vision = false`: it may not be given images.
    pub vision_disabled: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrConfig {
    pub task_url_template: Option<String>,
    /// A run's `pr_open` also closes the earlier open delivery pull requests
    /// of the tasks it publishes, so each task keeps one
    /// ([`orbit_types::workflow::PR_CLOSE_ON_TERMINAL`] for a configured
    /// host). Off for hosts bound to no workspace configuration.
    pub close_superseded: bool,
    /// `pr.delivery_authors`: forge logins whose pull requests count as
    /// Orbit's. Empty means the login that opened this run's pull request.
    pub delivery_authors: Vec<String>,
    /// `workflow.ci_infrastructure_signatures`: log-line prefixes that mark a
    /// failed CI job as an infrastructure failure. Empty declares none.
    pub ci_infrastructure_signatures: Vec<String>,
    /// Owner policy for logged infrastructure failures; reservations survive runs.
    pub ci_infrastructure_retry: orbit_types::workflow::CiInfrastructureRetryPolicy,
    /// `workflow.ci_infrastructure_retry.handoff_steps`: failed-step name
    /// patterns of dependency handoffs. A failed set whose every failed job
    /// stopped in a matching step is transient. Empty declares none.
    pub ci_handoff_steps: Vec<String>,
    /// `pr.*`, `commit.*` and `branch.*` templates and switches; the default
    /// is Orbit's built-in titles, bodies, messages and branch names.
    pub conventions: orbit_types::workflow::DeliveryConventions,
}
