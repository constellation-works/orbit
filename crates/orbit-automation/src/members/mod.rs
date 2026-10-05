//! State consumers share the sweep, generation-fenced Store and receipt path.

use crate::AutomationError;
use chrono::{DateTime, Utc};
use orbit_types::workflow::automation::{members::*, *};
use std::collections::{BTreeMap, BTreeSet};

mod admission;
mod evaluate;
pub mod incidents;
mod observe;
pub mod preparation;
mod reconcile;
#[cfg(test)]
mod tests;

pub use evaluate::evaluate;

pub struct MemberPage {
    pub candidates: Vec<StateMember>,
    pub withheld: BTreeMap<String, String>,
    pub next: Option<String>,
}

pub enum MemberOutcome {
    Pending,
    /// The run's deterministic apply settled every member of the attempt:
    /// only apply output with host-verified origin is admissible, and a member
    /// it did not apply is failed at its fingerprint without retry.
    Settled(MemberBatchEvidence),
    /// The run stopped before any apply output existed; the attempt retries
    /// as a whole while its budget lasts. Requires proof the owner and all
    /// applicable recoveries stopped.
    Failed(String),
}

pub enum MemberAdmission {
    Admit,
    /// The member is still authoritative, but cannot be admitted yet.
    Withhold(String),
    /// The member no longer describes authoritative source state.
    Retire(String),
}

pub trait MemberHost {
    fn head(&self, branch: &str) -> Result<(String, SourceRevision), AutomationError>;

    fn observe(
        &self,
        after: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<MemberPage, AutomationError>;

    fn admission(&self, member: &StateMember) -> Result<MemberAdmission, AutomationError>;

    /// The subset of `keys` — member keys, or task ids a page withheld —
    /// that the source query still observes, answered by identity rather
    /// than by paging.
    fn observable(&self, keys: &BTreeSet<String>) -> Result<BTreeSet<String>, AutomationError>;

    fn lookup(&self, attempt: &MemberAttempt) -> Result<Option<String>, AutomationError>;

    fn admit(&self, attempt: &MemberAttempt) -> Result<String, AutomationError>;

    fn outcome(&self, attempt: &MemberAttempt) -> Result<MemberOutcome, AutomationError>;

    /// Whether `assessment`, certified under an earlier fingerprint contract,
    /// still describes `member` [ORB-13638]. A host that cannot tell answers
    /// `false`, so the member is assessed again rather than trusted.
    fn carries_forward(&self, _member: &StateMember, _assessment: &MemberAssessment) -> bool {
        false
    }
}

pub struct MemberEvaluation<'a> {
    pub consumer: &'a str,
    pub epoch: &'a str,
    pub trigger: &'a StateTrigger,
    pub enabled: bool,
    pub dry_run: bool,
    pub now: DateTime<Utc>,
}
