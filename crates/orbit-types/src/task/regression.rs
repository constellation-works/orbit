//! Typed classification carried by a `regression_from` relation: what kind
//! of regression the link records and why the delivery gates missed it.
//!
//! The payload is optional and additive. A link stored without it reads back
//! as absent, which is distinct from an explicit `unclassified`. A classified
//! link stored before severity existed reads as `medium` with source
//! `default`.

use serde::{Deserialize, Serialize};

use super::severity::{DefectSeverity, SeveritySource};

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

/// Most defect sites one `regression_from` link may name.
pub const MAX_REGRESSION_SITES: usize = 8;

/// The optional `regression` payload of a `regression_from` relation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(try_from = "RegressionClassificationWire")]
pub struct RegressionClassification {
    pub nature: RegressionNature,
    pub escape: RegressionEscape,
    /// How much the defect matters. A link stored without one reads as
    /// `medium` with [`Self::severity_source`] `default`.
    pub severity: DefectSeverity,
    /// Who set [`Self::severity`]; `filer` when a severity is given alone.
    pub severity_source: SeveritySource,
    /// Where the defect lives: canonical `symbol:` or `file:` selectors of
    /// the code that is wrong, as opposed to everything the culprit touched,
    /// or a `dir:` selector when no narrower site can be named. Absent on
    /// links filed without it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sites: Vec<String>,
}

/// The `regression` payload as written, before the severity defaults apply.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegressionClassificationWire {
    nature: RegressionNature,
    escape: RegressionEscape,
    #[serde(default)]
    severity: Option<DefectSeverity>,
    #[serde(default)]
    severity_source: Option<SeveritySource>,
    #[serde(default)]
    sites: Vec<String>,
}

impl TryFrom<RegressionClassificationWire> for RegressionClassification {
    type Error = String;

    fn try_from(wire: RegressionClassificationWire) -> Result<Self, Self::Error> {
        let (severity, severity_source) = match (wire.severity, wire.severity_source) {
            (None, None | Some(SeveritySource::Default)) => {
                (DefectSeverity::Medium, SeveritySource::Default)
            }
            (None, Some(source)) => {
                return Err(format!(
                    "`regression.severity_source` {} needs a `regression.severity`",
                    source.as_str()
                ));
            }
            (Some(severity), source) => (severity, source.unwrap_or(SeveritySource::Filer)),
        };
        Ok(Self {
            nature: wire.nature,
            escape: wire.escape,
            severity,
            severity_source,
            sites: wire.sites,
        })
    }
}

impl RegressionClassification {
    /// Whether nobody rated this link's severity: it reads as `medium` only
    /// by default.
    pub fn severity_unrated(&self) -> bool {
        self.severity_source == SeveritySource::Default
    }

    /// Check the shape of [`Self::sites`]: at most `MAX_REGRESSION_SITES`,
    /// no duplicates, each a `symbol:`, `file:` or `dir:` selector. Callers that can
    /// resolve selectors also canonicalize them against the workspace.
    pub fn validate_sites(&self) -> Result<(), String> {
        if self.sites.len() > MAX_REGRESSION_SITES {
            return Err(format!(
                "`regression.sites` names {} sites; at most {MAX_REGRESSION_SITES} are allowed",
                self.sites.len()
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for site in &self.sites {
            if !["symbol:", "file:", "dir:"]
                .iter()
                .any(|prefix| site.starts_with(prefix))
            {
                return Err(format!(
                    "`regression.sites` entry {site:?} must be a `symbol:`, `file:` or `dir:` selector"
                ));
            }
            if !seen.insert(site.as_str()) {
                return Err(format!("`regression.sites` repeats {site:?}"));
            }
        }
        Ok(())
    }
}

/// History event recording which context files an agent prompt marked as
/// regression-prone.
pub const REGRESSION_RISK_MARKED_EVENT: &str = "regression_risk_marked";

/// The decision of one [`REGRESSION_RISK_MARKED_EVENT`] history entry: the
/// paths one run's prompt for one activity marked, so analytics can compare
/// how marked paths fare once agents are told about them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegressionRiskMarks {
    /// The run whose prompt carried the marks.
    pub run_id: String,
    /// The activity the prompt was built for.
    pub activity: String,
    /// The marked repository-relative file paths, highest risk first.
    pub paths: Vec<String>,
    /// The trailing window, in days, the marks were computed over.
    pub window_days: u32,
}
