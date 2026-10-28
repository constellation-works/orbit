//! The one severity scale review findings and `regression_from` links share,
//! and who set a recorded severity.

use serde::{Deserialize, Serialize};

/// How much a defect matters, from `critical` down to `nit`. Review findings
/// and `regression_from` links rate on this one scale, and the agent
/// scorecards weigh both by [`Self::weight`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "lowercase")]
pub enum DefectSeverity {
    /// Data loss or corruption, a security hole, every drain halted, or
    /// silently wrong results.
    Critical,
    /// A core workflow broken with no workaround.
    High,
    /// A feature broken or wrong in a real path, with a workaround.
    Medium,
    /// An edge case, a misleading message or a minor UX defect.
    Low,
    /// Wording or doc drift, cosmetic, no behavior impact.
    Nit,
}

impl DefectSeverity {
    /// Every level, most severe first.
    pub const ALL: [DefectSeverity; 5] = [
        DefectSeverity::Critical,
        DefectSeverity::High,
        DefectSeverity::Medium,
        DefectSeverity::Low,
        DefectSeverity::Nit,
    ];

    /// The wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            DefectSeverity::Critical => "critical",
            DefectSeverity::High => "high",
            DefectSeverity::Medium => "medium",
            DefectSeverity::Low => "low",
            DefectSeverity::Nit => "nit",
        }
    }

    /// The level a wire name names, case-insensitively.
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        Self::ALL
            .into_iter()
            .find(|level| level.as_str().eq_ignore_ascii_case(value))
    }

    /// The scorecard weight: critical 8, high 4, medium 2, low 1, nit 0.25.
    pub fn weight(self) -> f64 {
        match self {
            DefectSeverity::Critical => 8.0,
            DefectSeverity::High => 4.0,
            DefectSeverity::Medium => 2.0,
            DefectSeverity::Low => 1.0,
            DefectSeverity::Nit => 0.25,
        }
    }
}

/// Who set a recorded severity.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "lowercase")]
pub enum SeveritySource {
    /// The agent or operator that filed the link rated it.
    Filer,
    /// Finding verification, run by a different crew, confirmed or adjusted it.
    Verified,
    /// An operator overrode it.
    Operator,
    /// A backfill of links stored before severity existed rated it.
    Backfill,
    /// Nobody rated it: a link stored without a severity reads as `medium`.
    Default,
}

impl SeveritySource {
    /// The wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            SeveritySource::Filer => "filer",
            SeveritySource::Verified => "verified",
            SeveritySource::Operator => "operator",
            SeveritySource::Backfill => "backfill",
            SeveritySource::Default => "default",
        }
    }
}
