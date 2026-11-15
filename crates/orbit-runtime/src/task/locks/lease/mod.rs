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
//! Over SSH the principal class and its policy ceilings are attribution
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
#[cfg(feature = "test-support")]
pub use policy::install_test_lease_policy;
pub use policy::{
    LEASE_AGING_MINUTES, LEASE_DEFAULT_MODE, LEASE_HARD_ALLOWED_AGENT, LEASE_HARD_ALLOWED_HUMAN,
    LEASE_MAX_PRIORITY_AGENT, LEASE_MAX_PRIORITY_HUMAN, LeasePolicy, PATH_LEASES_ENABLED,
    lease_principal_class,
};
pub use queue::{expand, expand_with_token};
pub use request::{request, request_with_token};
