//! Agent facade (`Agent`, `AgentConfig`, `ProviderOptions`) over provider runtimes;
//! imports `runtime`, `types`, `orbit-common`, and `orbit-types`.

#[allow(clippy::module_inception)]
mod agent;

pub use agent::{Agent, AgentConfig, ProviderOptions};
