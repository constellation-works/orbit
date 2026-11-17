//! Keeps caller claims separate from trusted attribution to prevent impersonation.
//! Display them as unverified and leave absent claims anonymous; they grant no authority.

use std::fmt::{Display, Formatter};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Maximum UTF-8 byte length Orbit records for a self-reported actor.
///
/// Generous for a client name or model string (`claude-code`, `claude-opus-5`)
/// while bounding the size of the caller-controlled audit label.
pub const SELF_REPORTED_ACTOR_MAX_LEN: usize = 128;

/// The label an aggregate reports for traffic that claimed no identity at all.
pub const ANONYMOUS_ACTOR_LABEL: &str = "anonymous";

/// How an audit row's actor label was established.
///
/// This is the trust dimension, kept separate from *who* the actor is so a
/// caller can ask for authenticated-only, self-reported-only, or combined
/// counts without string-matching a label.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "snake_case")]
pub enum AuditAttribution {
    /// Orbit authenticated the caller: a managed run envelope, or a local CLI
    /// invocation whose identity Orbit itself supplied.
    Authenticated,
    /// The caller named itself and Orbit could not verify the claim. Counts
    /// belong in their own denominator, never folded into `Authenticated`.
    SelfReported,
    /// Neither a trusted identity nor a usable claim.
    Anonymous,
}

impl AuditAttribution {
    pub fn as_str(self) -> &'static str {
        match self {
            AuditAttribution::Authenticated => "authenticated",
            AuditAttribution::SelfReported => "self_reported",
            AuditAttribution::Anonymous => "anonymous",
        }
    }

    /// True when this row's actor may be used as an authenticated principal.
    /// Only [`AuditAttribution::Authenticated`] qualifies.
    pub fn is_authenticated(self) -> bool {
        matches!(self, AuditAttribution::Authenticated)
    }
}

impl Display for AuditAttribution {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AuditAttribution {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "authenticated" => Ok(AuditAttribution::Authenticated),
            "self_reported" => Ok(AuditAttribution::SelfReported),
            "anonymous" => Ok(AuditAttribution::Anonymous),
            other => Err(format!("unknown audit attribution: {other}")),
        }
    }
}

/// Reduce a caller-supplied identity claim to the bounded form Orbit records,
/// or `None` when there is nothing usable to record.
///
/// Rejects (as anonymous, never as an error the caller can act on):
/// - a claim that is empty or only whitespace,
/// - a claim containing a control character — newlines and tabs would let a
///   claim forge extra fields in any line-oriented rendering of the audit log,
/// - a claim longer than [`SELF_REPORTED_ACTOR_MAX_LEN`] after trimming.
///
/// Accepted claims are lowercased and have internal whitespace runs collapsed
/// to a single space, so `Claude  Code` and `claude code` aggregate as one
/// group. Case folding is safe here precisely because the value is never
/// compared against a trusted label: two claims colliding after normalization
/// are both still marked self-reported.
pub fn normalize_self_reported_actor(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > SELF_REPORTED_ACTOR_MAX_LEN {
        return None;
    }
    if trimmed.chars().any(char::is_control) {
        return None;
    }

    let normalized = trimmed
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    // `split_whitespace` on a non-empty, non-control string always yields at
    // least one token, but re-checking keeps the postcondition local: this
    // function never returns `Some("")`.
    (!normalized.is_empty()).then_some(normalized)
}
