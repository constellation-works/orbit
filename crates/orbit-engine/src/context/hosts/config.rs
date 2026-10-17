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
    pub provider: Option<Provider>,
    pub model: Option<String>,
    pub reasoning_effort: Option<orbit_types::identity::ReasoningEffort>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrConfig {
    pub task_url_template: Option<String>,
    /// `pr.close_on_terminal`: a run's `pr_open` also closes the earlier open
    /// delivery pull requests of the tasks it publishes, so each task keeps
    /// one. Off for hosts bound to no workspace configuration.
    pub close_superseded: bool,
    /// `pr.delivery_authors`: forge logins whose pull requests count as
    /// Orbit's. Empty means the login that opened this run's pull request.
    pub delivery_authors: Vec<String>,
}
