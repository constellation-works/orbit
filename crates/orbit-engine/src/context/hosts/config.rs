//! Runtime-resolved crew and pull-request configuration.

use orbit_types::workflow::activity_job::Provider;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CrewConfig {
    pub provider: Option<Provider>,
    pub model: Option<String>,
    pub reasoning_effort: Option<orbit_types::identity::ReasoningEffort>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrConfig {
    pub task_url_template: Option<String>,
}
