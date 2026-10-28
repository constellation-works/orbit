//! Lease intake request and response wire types.

use orbit_auth::scope::{LeaseTokenScope, LeaseTokenVerb};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The first frame each side sends.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaseIntakeHello {
    /// The sender's [`super::LEASE_INTAKE_PROTOCOL_VERSION`].
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
    pub(super) fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

/// The lease operations the socket accepts (design §6).
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
    /// One page of `orbit.lease.notices`.
    Notices,
    /// `orbit.lease.notices` as a stream of pages, each read from the
    /// previous page's cursor; audited once, when it opens.
    Events,
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
            Self::Notices => "notices",
            Self::Events => "events",
        }
    }

    /// The token verb the operation needs.
    pub fn verb(self) -> LeaseTokenVerb {
        match self {
            Self::Request | Self::Renew | Self::Expand | Self::Release => LeaseTokenVerb::Lease,
            Self::Show | Self::List | Self::Check | Self::Notices | Self::Events => {
                LeaseTokenVerb::Read
            }
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
