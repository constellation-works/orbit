//! Provider runtime trait (`AgentRuntime`), registry, and runtime factory; imports `agent`, `providers`, `types`, and `orbit-common`.

mod backend;
mod factory;
mod runtime_trait;

pub(crate) use backend::ProviderRegistry;
pub(crate) use factory::{AgentRuntimeFactory, resolve_runtime};
pub use runtime_trait::AgentRuntime;
