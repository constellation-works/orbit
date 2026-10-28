//! The lease listener (`docs/design/path-leases/2_design.md` §7.1–§7.3).
//!
//! `orbit lease listen` runs this as the lease listener user (`orbit-lease`
//! by convention), never as the operator, so agents running as the operator
//! can neither mint its tokens nor run code as it. It serves the lease
//! routes under `/api` and `/healthz`, and nothing else: no dashboard,
//! cookie, browser session or CORS. Every lease request needs a bearer lease
//! token, verified through `orbit-auth` against the token store in the
//! running user's Orbit home, and is forwarded with the token's principal,
//! id and scope and the resolved client address to Orbit Web over the lease
//! intake socket. The listener never opens Orbit's main store.
//!
//! Refusals that never reach Orbit (an unresolvable client address, a rate
//! limit, a missing, unknown, revoked or out-of-scope token, a request
//! rejected after authentication) are audited in the listener user's token
//! store as bounded aggregates by canonical key, limit/reason and window,
//! without secrets, principals, routes or queries. Orbit audits every
//! operation it receives.

mod admission;
mod backend;
mod config;
mod limits;
mod router;
mod startup;
mod tls;
mod transport;

pub use config::LeaseListenerConfig;
pub use limits::LeaseListenerLimits;
pub use router::lease_listener_router;
pub use startup::{run_lease_listener, serve_lease_listener, serve_lease_listener_on};
