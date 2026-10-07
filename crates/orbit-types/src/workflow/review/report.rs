//! The reviewer report, its findings and normalization [ORB-11333].

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    NegativeControl, REVIEW_CONTRACT_VERSION, RetiredValidation, ReviewValidation, ReviewVerdict,
    ValidationOutcome, ValidationRole,
};

/// How a finding was closed, if at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum FindingDisposition {
    /// Still open; blocks a pass verdict.
    Open,
    /// Fixed by the reviewer in this attempt's reviewer commit.
    Repaired,
    /// Disposed by an authorized decision with a recorded reason.
    Disposed { reason: String },
}

/// One reviewer finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewFinding {
    pub id: String,
    pub severity: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    pub disposition: FindingDisposition,
    /// What the reviewer changed to fix this finding, in one line. Absent on
    /// open findings and in reports written before [ORB-13989].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change: Option<String>,
}

/// The structured report the reviewer persists as
/// [`REVIEW_REPORT_ARTIFACT`]. The gate validates it against the candidate
/// and the repository state; it is a claim, not a certificate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewReport {
    /// Named external checks still needed; only an otherwise complete review
    /// with no open defects may enter an evidence hold.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_evidence: Vec<super::super::ReviewEvidenceRequirement>,
    pub schema_version: u32,
    /// Must name the attempt the manifest was issued for.
    pub attempt_id: String,
    pub verdict: ReviewVerdict,
    pub summary: String,
    #[serde(default)]
    pub findings: Vec<ReviewFinding>,
    #[serde(default)]
    pub validation: Vec<ReviewValidation>,
    /// Earlier revisions' required-record ids this report deliberately no
    /// longer carries, each with its reason [ORB-14370].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retired_validation: Vec<RetiredValidation>,
    /// Why the review stopped when the verdict is not a pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation: Option<String>,
}

impl ReviewReport {
    /// Read a persisted report, tolerating benign shape drift that leaves its
    /// meaning unambiguous: a bare-string `disposition`, enum labels in other
    /// case or with `-`/space separators, `pass`/`fail`/`skipped` outcomes,
    /// a single path string, a numeric finding id or string `schema_version`,
    /// a missing `schema_version`, `summary` or finding `severity`, and
    /// `null` lists. Anything else that does not match the contract is
    /// refused with the offending field's path named.
    pub fn parse(content: &[u8]) -> Result<Self, String> {
        let mut value: Value = serde_json::from_slice(content)
            .map_err(|error| format!("review report is not JSON: {error}"))?;
        normalize_report(&mut value);
        serde_json::from_value(value.clone()).map_err(|error| locate_report_error(&value, error))
    }
}

/// Name the first field of a normalized report that fails its type, so a
/// reviewer can fix exactly that field; a missing field already names
/// itself.
fn locate_report_error(report: &Value, error: serde_json::Error) -> String {
    fn check<T: serde::de::DeserializeOwned>(path: &str, value: Option<&Value>) -> Option<String> {
        let value = value.cloned().unwrap_or(Value::Null);
        serde_json::from_value::<T>(value)
            .err()
            .map(|error| format!("{path}: {error}"))
    }
    if let Some(located) = check::<ReviewVerdict>("verdict", report.get("verdict")) {
        return located;
    }
    if let Some(Value::Array(findings)) = report.get("findings") {
        for (index, finding) in findings.iter().enumerate() {
            let path = format!("findings[{index}]");
            let located = check::<FindingDisposition>(
                &format!("{path}.disposition"),
                finding.get("disposition"),
            )
            .or_else(|| check::<ReviewFinding>(&path, Some(finding)));
            if let Some(located) = located {
                return located;
            }
        }
    }
    if let Some(Value::Array(records)) = report.get("validation") {
        for (index, record) in records.iter().enumerate() {
            let path = format!("validation[{index}]");
            let located =
                check::<ValidationOutcome>(&format!("{path}.outcome"), record.get("outcome"))
                    .or_else(|| {
                        record.get("role").and_then(|role| {
                            check::<ValidationRole>(&format!("{path}.role"), Some(role))
                        })
                    })
                    .or_else(|| {
                        record.get("control").and_then(|control| {
                            check::<NegativeControl>(&format!("{path}.control"), Some(control))
                        })
                    })
                    .or_else(|| check::<ReviewValidation>(&path, Some(record)));
            if let Some(located) = located {
                return located;
            }
        }
    }
    error.to_string()
}

fn normalize_report(report: &mut Value) {
    let Some(report) = report.as_object_mut() else {
        return;
    };
    match report.get("schema_version") {
        None | Some(Value::Null) => {
            report.insert(
                "schema_version".to_string(),
                Value::from(REVIEW_CONTRACT_VERSION),
            );
        }
        Some(Value::String(raw)) => {
            if let Ok(version) = raw.trim().parse::<u32>() {
                report.insert("schema_version".to_string(), Value::from(version));
            }
        }
        Some(_) => {}
    }
    normalize_label_field(report, "verdict", &[]);
    if matches!(report.get("summary"), None | Some(Value::Null)) {
        report.insert("summary".to_string(), Value::String(String::new()));
    }
    for list in ["findings", "validation", "retired_validation"] {
        if report.get(list).is_some_and(Value::is_null) {
            report.remove(list);
        }
    }
    if let Some(Value::Array(findings)) = report.get_mut("findings") {
        findings
            .iter_mut()
            .filter_map(Value::as_object_mut)
            .for_each(normalize_finding);
    }
    if let Some(Value::Array(records)) = report.get_mut("validation") {
        for record in records.iter_mut().filter_map(Value::as_object_mut) {
            normalize_label_field(
                record,
                "outcome",
                &[
                    ("pass", "passed"),
                    ("success", "passed"),
                    ("fail", "failed"),
                    ("failure", "failed"),
                    ("skipped", "not_run"),
                ],
            );
            for optional in ["id", "role", "control", "sources"] {
                if record.get(optional).is_some_and(Value::is_null) {
                    record.remove(optional);
                }
            }
            if let Some(Value::Number(id)) = record.get("id") {
                let id = id.to_string();
                record.insert("id".to_string(), Value::String(id));
            }
            normalize_label_field(record, "role", &[]);
            normalize_label_field(record, "control", &[]);
            if let Some(Value::String(source)) = record.get("sources") {
                let sources = Value::Array(vec![Value::String(source.clone())]);
                record.insert("sources".to_string(), sources);
            }
        }
    }
}

fn normalize_finding(finding: &mut serde_json::Map<String, Value>) {
    if let Some(Value::Number(id)) = finding.get("id") {
        let id = id.to_string();
        finding.insert("id".to_string(), Value::String(id));
    }
    if matches!(finding.get("severity"), None | Some(Value::Null)) {
        finding.insert(
            "severity".to_string(),
            Value::String("unspecified".to_string()),
        );
    }
    if matches!(finding.get("summary"), None | Some(Value::Null))
        && let Some(title) = finding.get("title").cloned()
    {
        finding.insert("summary".to_string(), title);
    }
    match finding.get("paths") {
        Some(Value::Null) => {
            finding.remove("paths");
        }
        Some(Value::String(path)) => {
            let paths = Value::Array(vec![Value::String(path.clone())]);
            finding.insert("paths".to_string(), paths);
        }
        _ => {}
    }
    let disposition = match finding.remove("disposition") {
        Some(Value::String(kind)) => {
            let mut object = serde_json::Map::new();
            object.insert("kind".to_string(), Value::String(kind));
            Some(object)
        }
        Some(Value::Object(mut object)) => {
            if !object.contains_key("kind")
                && let Some(status) = object.remove("status")
            {
                object.insert("kind".to_string(), status);
            }
            Some(object)
        }
        Some(other) => {
            finding.insert("disposition".to_string(), other);
            None
        }
        None => None,
    };
    if let Some(mut disposition) = disposition {
        normalize_label_field(&mut disposition, "kind", &[("fixed", "repaired")]);
        finding.insert("disposition".to_string(), Value::Object(disposition));
    }
}

/// Canonicalize an enum label: trimmed, lower case, `-` and spaces as `_`,
/// then mapped through `aliases`. Non-string values are left for serde to
/// refuse.
fn normalize_label_field(
    object: &mut serde_json::Map<String, Value>,
    key: &str,
    aliases: &[(&str, &str)],
) {
    let Some(Value::String(raw)) = object.get(key) else {
        return;
    };
    let label = raw.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    let label = aliases
        .iter()
        .find(|(alias, _)| *alias == label)
        .map_or(label.clone(), |(_, canonical)| (*canonical).to_string());
    object.insert(key.to_string(), Value::String(label));
}
