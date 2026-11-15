//! Lease token secrets: generation, format, hashing and constant-time
//! comparison.
//!
//! A secret is `orbl_` followed by 64 lowercase hex digits, 256 bits from the
//! OS random source. It is shown once, when it is minted; the store keeps only
//! its SHA-256. The prefix lets `orbit-common`'s redaction recognize a token
//! in captured output.

use std::fmt;

use orbit_common::OrbitError;
use orbit_common::security::token;
use sha2::{Digest, Sha256};

/// Prefix of every lease token secret.
pub const LEASE_TOKEN_PREFIX: &str = "orbl_";

/// Random bytes in a lease token secret (256 bits).
const LEASE_TOKEN_SECRET_BYTES: usize = 32;

/// Random bytes in a token's public identifier.
const LEASE_TOKEN_ID_BYTES: usize = 8;

/// Prefix of a token's public identifier, which names it in `list`, `revoke`
/// and audit rows. It is not a secret and does not authenticate.
pub const LEASE_TOKEN_ID_PREFIX: &str = "lt_";

/// A lease token secret. It has no `Display`, and its `Debug` hides the value,
/// so it cannot reach a log or error through formatting by accident.
pub struct LeaseTokenSecret(String);

impl LeaseTokenSecret {
    /// The secret itself, to show once at mint.
    pub fn expose_secret(&self) -> &str {
        &self.0
    }

    /// The SHA-256 the store keeps in place of the secret.
    pub fn digest(&self) -> LeaseTokenDigest {
        lease_token_digest(&self.0)
    }
}

impl fmt::Debug for LeaseTokenSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LeaseTokenSecret(<redacted>)")
    }
}

/// SHA-256 of a lease token secret.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct LeaseTokenDigest([u8; 32]);

impl LeaseTokenDigest {
    /// The digest bytes, as stored.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Compare against stored digest bytes in time independent of where they
    /// first differ.
    pub fn matches_stored(&self, stored: &[u8]) -> bool {
        token::constant_time_eq(&self.0, stored)
    }
}

impl fmt::Debug for LeaseTokenDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LeaseTokenDigest(..)")
    }
}

/// Mint a new secret from the OS random source.
pub fn generate_lease_token_secret() -> Result<LeaseTokenSecret, OrbitError> {
    let hex = token::random_hex(LEASE_TOKEN_SECRET_BYTES)?;
    Ok(LeaseTokenSecret(format!("{LEASE_TOKEN_PREFIX}{hex}")))
}

/// A new public identifier for a token row.
pub fn generate_lease_token_id() -> Result<String, OrbitError> {
    let hex = token::random_hex(LEASE_TOKEN_ID_BYTES)?;
    Ok(format!("{LEASE_TOKEN_ID_PREFIX}{hex}"))
}

/// Whether `raw` has the lease token secret shape. A malformed value is
/// refused as unknown without a store lookup.
pub fn is_lease_token_secret_shape(raw: &str) -> bool {
    raw.strip_prefix(LEASE_TOKEN_PREFIX).is_some_and(|hex| {
        hex.len() == LEASE_TOKEN_SECRET_BYTES * 2
            && hex
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// SHA-256 of a presented secret, used to look it up.
pub fn lease_token_digest(secret: &str) -> LeaseTokenDigest {
    LeaseTokenDigest(Sha256::digest(secret.as_bytes()).into())
}
