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
//! [`LeaseIntakeResponse`]. The socket accepts the lease operations and
//! nothing else.

mod client;
mod server;
mod socket;

#[cfg(test)]
mod tests;

use orbit_auth::scope::{LeaseTokenScope, LeaseTokenVerb};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use client::LeaseIntakeClient;
pub(crate) use server::IntakeLimits;
pub(crate) use socket::LeaseIntakeSocket;

/// The intake protocol version. A listener and an Orbit Web of different
/// versions refuse each other, since the listener's root-owned binary is
/// updated separately from the operator's.
pub const LEASE_INTAKE_PROTOCOL_VERSION: u32 = 1;

/// The socket's path under the operator's Orbit home.
pub const LEASE_INTAKE_SOCKET_PATH: &str = "run/lease-intake.sock";

/// Largest frame body either side accepts.
pub const LEASE_INTAKE_MAX_FRAME_BYTES: u32 = 64 * 1024;

/// The first frame each side sends.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaseIntakeHello {
    /// The sender's [`LEASE_INTAKE_PROTOCOL_VERSION`].
    pub protocol_version: u32,
    /// Set by the server when it refuses the connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<LeaseIntakeError>,
}

/// A refused connection or operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseIntakeError {
    /// Stable machine-readable reason, such as `protocol_version_mismatch`,
    /// `scope_refused` or `invalid_input`.
    pub code: String,
    /// Human-readable reason.
    pub message: String,
}

impl LeaseIntakeError {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

/// The lease operations the socket accepts (design §6). Notices arrive in a
/// later phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseIntakeOperation {
    /// `orbit.lease.request`.
    Request,
    /// `orbit.lease.renew`.
    Renew,
    /// `orbit.lease.expand`.
    Expand,
    /// `orbit.lease.release`.
    Release,
    /// `orbit.lease.show`.
    Show,
    /// `orbit.lease.list`.
    List,
    /// `orbit.lease.check`.
    Check,
}

impl LeaseIntakeOperation {
    /// The stored and displayed spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Renew => "renew",
            Self::Expand => "expand",
            Self::Release => "release",
            Self::Show => "show",
            Self::List => "list",
            Self::Check => "check",
        }
    }

    /// The token verb the operation needs.
    pub fn verb(self) -> LeaseTokenVerb {
        match self {
            Self::Request | Self::Renew | Self::Expand | Self::Release => LeaseTokenVerb::Lease,
            Self::Show | Self::List | Self::Check => LeaseTokenVerb::Read,
        }
    }
}

/// Who the listener authenticated: the token's id and scope, which names
/// its principal and class, and the client address it resolved.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaseIntakeAuthority {
    /// The token's public id.
    pub token_id: String,
    /// The token's principal, class, workspaces, verbs, priority ceiling and
    /// `hard` permission.
    pub scope: LeaseTokenScope,
    /// The client address the listener resolved.
    pub client_address: String,
}

/// One lease operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaseIntakeRequest {
    /// The operation.
    pub operation: LeaseIntakeOperation,
    /// The workspace, which the token's scope must name.
    pub workspace: String,
    /// The authenticated caller.
    pub authority: LeaseIntakeAuthority,
    /// The operation's JSON input, as the matching `orbit.lease.*` tool
    /// takes it. The principal comes from `authority`.
    #[serde(default)]
    pub input: Value,
}

/// The answer to one [`LeaseIntakeRequest`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LeaseIntakeResponse {
    /// The operation's own JSON contract, including structured refusals such
    /// as `granted: false`.
    Ok {
        /// The operation's output.
        result: Value,
    },
    /// The operation was refused or failed.
    Error(LeaseIntakeError),
}

/// The refusal both sides give for a protocol version other than their own.
fn version_mismatch_message(
    local: &str,
    local_version: u32,
    remote: &str,
    remote_version: u32,
) -> String {
    format!(
        "lease intake protocol mismatch: {local} speaks version {local_version} and {remote} \
         speaks version {remote_version}; install the same Orbit release for both"
    )
}
