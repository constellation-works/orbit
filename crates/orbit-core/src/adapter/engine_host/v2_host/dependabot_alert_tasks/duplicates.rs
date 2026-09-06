//! High-confidence coverage fingerprints for security-alert task candidates.

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_all;
use serde_json::{Value, json};

use crate::adapter::engine_host::v2_host::duplicate_tasks::{
    CoverageAnchor, CoverageFingerprint, DuplicateCandidate,
};

use super::{
    CODE_KEY_PREFIX, DEPENDABOT_KEY_PREFIX, SECRET_KEY_PREFIX, alert_number, field, line_suffix,
};

pub(super) fn dependabot_duplicate_candidate(
    key: &str,
    package: &str,
    manifest_path: &str,
) -> DuplicateCandidate {
    DuplicateCandidate::new(
        format!("{DEPENDABOT_KEY_PREFIX}{key}"),
        vec![
            CoverageFingerprint::new(
                "dependency_title",
                vec![CoverageAnchor::new(
                    "dependency_and_manifest",
                    format!("update {package} in {manifest_path}"),
                )],
            ),
            CoverageFingerprint::new(
                "dependency_fields",
                vec![
                    CoverageAnchor::new("package", format!("package {package}")),
                    CoverageAnchor::new("manifest_path", format!("manifest path {manifest_path}")),
                ],
            ),
        ],
    )
}

pub(super) fn code_duplicate_candidate(key: &str, alert: &Value) -> DuplicateCandidate {
    let number = alert_number(alert);
    let rule = field(alert, "rule_id");
    let path = field(alert, "path");
    let alert_anchor = format!("alert {number}");
    DuplicateCandidate::new(
        format!("{CODE_KEY_PREFIX}{key}"),
        vec![
            CoverageFingerprint::new(
                "code_scanning_title_and_alert",
                vec![
                    CoverageAnchor::new("rule_and_path", format!("fix {rule} in {path}")),
                    CoverageAnchor::new("alert_number", &alert_anchor),
                ],
            ),
            CoverageFingerprint::new(
                "code_scanning_fields",
                vec![
                    CoverageAnchor::new("alert_number", alert_anchor),
                    CoverageAnchor::new("rule", format!("rule {rule}")),
                    CoverageAnchor::new(
                        "location",
                        format!("location {path}{}", line_suffix(alert)),
                    ),
                ],
            ),
        ],
    )
}

pub(super) fn secret_duplicate_candidate(key: &str, alert: &Value) -> DuplicateCandidate {
    let number = alert_number(alert);
    let secret_type = field(alert, "secret_type");
    let alert_anchor = format!("alert {number}");
    let mut fingerprints = vec![CoverageFingerprint::new(
        "secret_scanning_title",
        vec![CoverageAnchor::new(
            "secret_type_and_alert",
            format!("rotate exposed {secret_type} from alert {number}"),
        )],
    )];
    if let Some(location) = alert
        .get("locations")
        .and_then(Value::as_array)
        .and_then(|locations| locations.first())
    {
        fingerprints.push(CoverageFingerprint::new(
            "secret_scanning_fields",
            vec![
                CoverageAnchor::new("alert_number", alert_anchor),
                CoverageAnchor::new("secret_type", format!("secret type {secret_type}")),
                CoverageAnchor::new(
                    "location",
                    format!("path {}{}", field(location, "path"), line_suffix(location)),
                ),
            ],
        ));
    }
    DuplicateCandidate::new(format!("{SECRET_KEY_PREFIX}{key}"), fingerprints)
}

pub(super) fn duplicate_lookup_error(family: &str, key: &str, error: &OrbitError) -> OrbitError {
    OrbitError::Execution(format!(
        "dependabot_alert_sweep retryable: {}",
        json!({
            "outcome": "retryable_error",
            "stage": "dedupe_lookup",
            "errors": [{
                "stage": "registration",
                "operation": "find_covering_task",
                "family": family,
                "key": key,
                "retryable": true,
                "message": bounded_error(&error.to_string()),
            }],
        })
    ))
}

fn bounded_error(message: &str) -> String {
    redact_all(message).chars().take(500).collect()
}
