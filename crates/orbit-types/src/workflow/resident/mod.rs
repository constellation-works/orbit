//! The resident orchestrator's mandate and authority records.
//!
//! A mandate is an assignment task tagged [`RESIDENT_TAG`] that carries a
//! [`RESIDENT_MANDATE_ARTIFACT`]: what the resident works toward. Its
//! authority, what the resident may do, is a separate record kept in the
//! task's history as [`RESIDENT_AUTHORITY_CHANGED_EVENT`] entries: each entry
//! names one widening or narrowing and the authority it left, so the record
//! is versioned (its revision is the entry count) and its history is the
//! entries themselves. Only core writes those entries; widening is the
//! human's and narrowing is open to anyone who can write the assignment.

mod authority;
mod comments;
mod cycle;
mod mandate;

pub use authority::*;
pub use comments::*;
pub use cycle::*;
pub use mandate::*;

/// The tag that makes an assignment task a resident's mandate.
pub const RESIDENT_TAG: &str = "resident";
/// The task artifact that holds the mandate.
pub const RESIDENT_MANDATE_ARTIFACT: &str = "resident/mandate.json";
/// The mandate artifact's schema id.
pub const RESIDENT_MANDATE_SCHEMA: &str = "resident.mandate/v1";
/// The authority record's schema id.
pub const RESIDENT_AUTHORITY_SCHEMA: &str = "resident.authority/v1";
/// History event recording one change to a mandate's authority.
pub const RESIDENT_AUTHORITY_CHANGED_EVENT: &str = "resident_authority_changed";
/// The `via` of a grant an operator applied directly.
pub const RESIDENT_VIA_OPERATOR: &str = "operator";
/// The `via` of a narrowing.
pub const RESIDENT_VIA_NARROWING: &str = "narrowing";
/// Most exit criteria a mandate holds.
pub const RESIDENT_MAX_EXIT_CRITERIA: usize = 8;
/// Smallest cadence floor a mandate may declare, in minutes.
pub const RESIDENT_MIN_FLOOR_MINUTES: u32 = 5;
/// Longest relayed quote a grant request carries, in characters.
pub const RESIDENT_MAX_QUOTE_CHARS: usize = 2000;
