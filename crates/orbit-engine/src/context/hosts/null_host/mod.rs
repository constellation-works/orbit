//! Test-only null host: every [`RuntimeHost`](super::RuntimeHost) capability with the answer of a
//! host that lacks it.

mod defaults;
mod runtime_host;

pub use defaults::NullRuntimeHost;
