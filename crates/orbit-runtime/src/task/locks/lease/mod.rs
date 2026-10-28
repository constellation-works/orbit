//! Path-lease operations (`docs/design/path-leases`): request, renew,
//! expand, release, show, list, check and notices over the lease rows the
//! reservation store keeps.
//!
//! A lease is held by a principal (a person or an agent), never by a task.
//! Advisory leases are always granted and block nothing; a hard lease is
//! granted only when no selector of its hard surface overlaps a task context
//! lock, a task reservation or another hard lease. An overlapping hard
//! request waits in the lease queue (`queue`), holding nothing, unless it
//! asked `wait = false`, which refuses it with the holders named.
//!
//! Over SSH the principal class and its `lease.*` ceilings are attribution
//! plus a guard, not a security boundary. A lease token is bounded further:
//! its principal's active leases, each lease's selectors, and the size of
//! every answer it gets (see `TOKEN_MAX_ACTIVE_LEASES`).

mod admission;
mod holders;
mod input;
mod lifetime;
mod operations;
mod policy;
mod queue;
mod request;
mod response;

pub use admission::{LeaseTokenGrant, record_lease_intake_audit};
pub use lifetime::sweep_lease_lifetimes;
pub use operations::{check, list, notices, release, renew, renew_with_token, show};
pub use orbit_store::contracts::{LeasePrincipalClass, LeasePriority};
pub(crate) use policy::LeasePolicy;
pub use policy::lease_principal_class;
pub use queue::{expand, expand_with_token};
pub use request::{request, request_with_token};
