//! Typed classification carried by a `regression_from` relation: what kind
//! of regression the link records and why the delivery gates missed it.
//!
//! The payload is optional and additive. A link stored without it reads back
//! as absent, which is distinct from an explicit `unclassified`.

use serde::{Deserialize, Serialize};

/// What kind of regression a `regression_from` link records.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RegressionNature {
    /// The culprit broke behavior that worked before it landed.
    BrokeExisting,
    /// The defect is in code the culprit itself introduced.
    DefectInNew,
    /// The culprit shipped without part of what its task required.
    Incomplete,
    /// A merge restored code or assets an earlier change had removed.
    MergeReintroduced,
    /// Docs, schemas or a contract drifted from the behavior the culprit shipped.
    ContractDrift,
    /// The filer could not tell which nature applies.
    Unclassified,
}

/// Why the delivery gates did not catch a `regression_from` defect.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RegressionEscape {
    /// No test covered the broken behavior.
    NoTest,
    /// A covering test existed but the delivery's gates did not run it.
    TestNotSelected,
    /// The defect shows only on a platform or host the gates did not exercise.
    PlatformOnly,
    /// The defect was visible in the diff and review did not flag it.
    ReviewMissed,
    /// The task or spec left the expected behavior open to the shipped reading.
    SpecAmbiguous,
    /// The failure depends on timing, ordering or environment and passed when gated.
    Nondeterministic,
    /// The filer could not tell how the defect escaped.
    Unclassified,
}

/// The optional `regression` payload of a `regression_from` relation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RegressionClassification {
    pub nature: RegressionNature,
    pub escape: RegressionEscape,
}
