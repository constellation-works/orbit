//! The lease intake socket (`docs/design/path-leases/2_design.md` §7.2).
//!
//! The lease listener runs as its own OS user and cannot open the operator's
//! Orbit store, so it hands authenticated lease operations to Orbit Web over
//! a Unix socket. Orbit Web serves the socket only when the operator's global
//! `lease.intake.peer_user` names that user, and accepts a connection only
//! when the kernel reports that user as its peer. Every agent runs as the
//! operator, so the operator's own user is refused like any other.
//!
//! The listener user is trusted for identity: it sends the principal, class,
//! token id, token scope and client address it authenticated, and Orbit
//! grants within the stricter of that scope and the workspace's class policy.
//!
//! Wire format: every frame is a 4-byte big-endian body length followed by
//! that many bytes of JSON. A connection opens with a [`LeaseIntakeHello`]
//! each way; either side refuses a protocol version other than its own,
//! naming both. Then each [`LeaseIntakeRequest`] gets one
//! [`LeaseIntakeResponse`], except `events`, which streams: it gets one
//! response per notice page, about one a second while no notices arrive,
//! until either side closes the connection or an error response ends it.
//! The socket accepts the lease operations and nothing else.

mod client;
mod protocol;
mod server;
mod socket;
mod wire;

pub use client::LeaseIntakeClient;
pub use protocol::{
    LEASE_INTAKE_MAX_FRAME_BYTES, LEASE_INTAKE_PROTOCOL_VERSION, LEASE_INTAKE_SOCKET_PATH,
};
pub(crate) use server::IntakeLimits;
pub(crate) use socket::LeaseIntakeSocket;
pub use wire::{
    LeaseIntakeAuthority, LeaseIntakeError, LeaseIntakeHello, LeaseIntakeOperation,
    LeaseIntakeRequest, LeaseIntakeResponse,
};
