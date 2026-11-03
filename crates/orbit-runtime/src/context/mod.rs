//! Actor identity, shared runtime state and admitted settings.

mod actor;
mod settings;
mod state;
mod stores;

pub use actor::{ActorIdentity, ActorKind, resolve_write_actor_label, trusted_write_identity};
pub use settings::OrbitRuntimeSettings;
pub use state::OrbitContext;
pub use stores::OrbitStores;
pub(crate) use stores::{OrbitExecutionAssets, OrbitHostStore, OrbitPolicyContext};
