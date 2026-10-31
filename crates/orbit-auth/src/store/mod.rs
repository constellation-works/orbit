//! The lease token store: `lease-tokens.db` in the listener user's Orbit home.
//!
//! The store holds each token's SHA-256, never its secret, with its scope,
//! timestamps and an audit trail of mints, revocations and refused
//! verifications. It is opened through `orbit-common`'s SQLite support and is
//! never created here: root provisions it, `0600` in a `0700` Orbit home,
//! when it creates the listener user. A user without one (the operator, say)
//! finds no store and is told to run the command as the listener user, so no
//! store ever appears in the operator's home. Opening refuses a store or home
//! that is group- or world-accessible or owned by another user.

mod audit;
mod connection;
mod private_access;
mod rows;
mod schema;
mod tokens;

pub use audit::lease_token_actor;

use rusqlite::Connection;

use crate::scope::{LeaseTokenRecord, LeaseTokenRefusal};
use crate::token::LeaseTokenSecret;

/// File name of the token store in the running user's Orbit home.
pub const LEASE_TOKEN_STORE_FILE: &str = "lease-tokens.db";

const TOKEN_COLUMNS: &str = "token_id, principal, principal_class, workspaces, verbs, \
     max_priority, hard, created_at, expires_at, last_used_at, revoked_at";

/// A newly minted token. The secret is here once, to show to the operator; the
/// store has only its hash.
#[derive(Debug)]
pub struct MintedLeaseToken {
    /// The stored row.
    pub record: LeaseTokenRecord,
    /// The secret, shown once.
    pub secret: LeaseTokenSecret,
}

/// What revoking a token did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseTokenRevocation {
    /// The token was active and is now revoked.
    Revoked(LeaseTokenRecord),
    /// The token had already been revoked; nothing changed.
    AlreadyRevoked(LeaseTokenRecord),
}

/// The decision for a presented token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseTokenDecision {
    /// The token is valid for the request; here are its principal and scope.
    Allow(LeaseTokenRecord),
    /// The token is refused, and the refusal has been audited.
    Deny(LeaseTokenRefusal),
}

/// An open lease token store.
pub struct LeaseTokenStore {
    connection: Connection,
}
