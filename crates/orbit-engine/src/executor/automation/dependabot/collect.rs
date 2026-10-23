use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_all;
use serde_json::{Map, Value, json};

use super::query::{AlertQuery, DependabotQueries};
use super::{OUTCOME_CAPABILITY_UNAVAILABLE, OUTCOME_NO_OPEN_ALERTS, OUTCOME_OPEN_ALERTS};
use crate::executor::automation::ci::AuthStatus;
use crate::executor::automation::ci::bounded_u64;
use crate::executor::automation::ci::optional_input_string;

const SCHEMA_VERSION: u64 = 2;
const DEFAULT_LIMIT: u64 = 100;
const MAX_LIMIT: u64 = 100;
const DEFAULT_LOCATION_LIMIT: u64 = 20;

struct FamilyData {
    collected: bool,
    outcome: &'static str,
    capability: Value,
    alerts: Vec<Value>,
}

impl FamilyData {
    fn collection_status(&self) -> &'static str {
        if self.collected {
            "fully_collected"
        } else {
            OUTCOME_CAPABILITY_UNAVAILABLE
        }
    }
}

pub(super) fn collect<Q: DependabotQueries + ?Sized>(
    queries: &Q,
    input: &Value,
) -> Result<Value, OrbitError> {
    let dependabot_limit = bounded_u64(input, "max_alerts", DEFAULT_LIMIT, MAX_LIMIT)?;
    let code_limit = bounded_u64(input, "max_code_scanning_alerts", DEFAULT_LIMIT, MAX_LIMIT)?;
    let secret_limit = bounded_u64(
        input,
        "max_secret_scanning_alerts",
        DEFAULT_LIMIT,
        MAX_LIMIT,
    )?;
    let location_limit = bounded_u64(
        input,
        "max_secret_locations",
        DEFAULT_LOCATION_LIMIT,
        MAX_LIMIT,
    )?;
    let pr_limit = bounded_u64(input, "max_pull_requests", DEFAULT_LIMIT, MAX_LIMIT)?;
    let repo = optional_input_string(input, "repo");
    let auth = queries.auth_status();
    if !auth.usable() {
        return unavailable_snapshot(&auth, &auth.detail);
    }

    let mut dependabot_errors = Vec::new();
    let repository = match queries.repo_view(repo.as_deref()) {
        Ok(repository) => repository,
        Err(error) => {
            dependabot_errors.push(json!({
                "query": "repo_view",
                "error": redact_all(&error.to_string()),
                "retryable": true,
            }));
            json!({})
        }
    };
    let dependabot = family_data(
        queries.open_alerts(repo.as_deref(), dependabot_limit),
        &auth,
    );
    let code_scanning = family_data(
        queries.open_code_scanning_alerts(repo.as_deref(), code_limit),
        &auth,
    );
    let mut secret_scanning = family_data(
        queries.open_secret_scanning_alerts(repo.as_deref(), secret_limit),
        &auth,
    );

    let pull_requests = if dependabot.collected {
        match queries.open_dependabot_pull_requests(repo.as_deref(), pr_limit) {
            Ok(pull_requests) => pull_requests,
            Err(error) => {
                dependabot_errors.push(json!({
                    "query": "dependabot_pull_requests",
                    "error": redact_all(&error.to_string()),
                }));
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    let mut secret_errors = Vec::new();
    let mut locations_at_cap = Vec::new();
    if secret_scanning.collected {
        for alert in &mut secret_scanning.alerts {
            let Some(number) = alert.get("number").and_then(Value::as_u64) else {
                secret_errors.push(json!({
                    "query": "secret_scanning_locations",
                    "error": "secret-scanning alert omitted its numeric identifier",
                }));
                continue;
            };
            match queries.secret_scanning_locations(repo.as_deref(), number, location_limit) {
                Ok(locations) => {
                    let at_cap = locations.len() as u64 == location_limit;
                    if at_cap {
                        locations_at_cap.push(number);
                    }
                    if let Some(object) = alert.as_object_mut() {
                        object.insert(
                            "locations".to_string(),
                            Value::Array(locations.into_iter().map(bound_alert).collect()),
                        );
                        object.insert("locations_at_cap".to_string(), Value::Bool(at_cap));
                    }
                }
                Err(error) => {
                    secret_errors.push(json!({
                        "query": "secret_scanning_locations",
                        "alert_number": number,
                        "error": redact_all(&error.to_string()),
                    }));
                    if let Some(object) = alert.as_object_mut() {
                        object.insert("locations".to_string(), json!([]));
                        object.insert("locations_at_cap".to_string(), Value::Bool(false));
                    }
                }
            }
        }
    }

    let dependabot_at_cap = dependabot.alerts.len() as u64 == dependabot_limit;
    let prs_at_cap = pull_requests.len() as u64 == pr_limit;
    let code_at_cap = code_scanning.alerts.len() as u64 == code_limit;
    let secret_at_cap = secret_scanning.alerts.len() as u64 == secret_limit;
    let dependabot_status = if dependabot.collected && dependabot_errors.is_empty() {
        "fully_collected"
    } else if dependabot.collected {
        "partially_collected"
    } else {
        OUTCOME_CAPABILITY_UNAVAILABLE
    };
    let secret_status = if secret_scanning.collected && secret_errors.is_empty() {
        "fully_collected"
    } else if secret_scanning.collected {
        "partially_collected"
    } else {
        OUTCOME_CAPABILITY_UNAVAILABLE
    };
    let code_status = code_scanning.collection_status();
    let collection_status =
        aggregate_collection_status(&[dependabot_status, code_status, secret_status]);

    let dependabot_alerts = dependabot
        .alerts
        .into_iter()
        .map(bound_alert)
        .collect::<Vec<_>>();
    let code_alerts = code_scanning
        .alerts
        .into_iter()
        .map(bound_alert)
        .collect::<Vec<_>>();
    let secret_alerts = secret_scanning
        .alerts
        .into_iter()
        .map(bound_alert)
        .collect::<Vec<_>>();
    let dependabot_notes = [
        dependabot_at_cap.then(|| {
            format!(
                "open alerts were listed at the {dependabot_limit}-alert cap; further alerts may exist"
            )
        }),
        prs_at_cap.then(|| {
            format!(
                "open Dependabot pull requests were listed at the {pr_limit}-PR cap; further PRs may exist"
            )
        }),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let code_notes = code_at_cap
        .then(|| {
            format!(
                "open Code scanning alerts were listed at the {code_limit}-alert cap; further alerts may exist"
            )
        })
        .into_iter()
        .collect::<Vec<_>>();
    let mut secret_notes = secret_at_cap
        .then(|| {
            format!(
                "open secret scanning alerts were listed at the {secret_limit}-alert cap; further alerts may exist"
            )
        })
        .into_iter()
        .collect::<Vec<_>>();
    if !locations_at_cap.is_empty() {
        secret_notes.push(format!(
            "secret-scanning locations reached the {location_limit}-location cap for alerts {:?}; further locations may exist",
            locations_at_cap
        ));
    }

    let snapshot = json!({
        "schema_version": SCHEMA_VERSION,
        "collected": dependabot.collected,
        "outcome_hint": dependabot.outcome,
        "capability": dependabot.capability,
        "collection_status": collection_status,
        "repository": repository,
        "open_alerts": dependabot_alerts,
        "open_dependabot_pull_requests": pull_requests.into_iter().map(bound_pull_request).collect::<Vec<_>>(),
        "query_errors": dependabot_errors,
        "truncation": {
            "alerts_limit": dependabot_limit,
            "alerts_at_cap": dependabot_at_cap,
            "pull_requests_limit": pr_limit,
            "pull_requests_at_cap": prs_at_cap,
            "notes": dependabot_notes,
        },
        "code_scanning": {
            "collected": code_scanning.collected,
            "collection_status": code_status,
            "outcome_hint": code_scanning.outcome,
            "capability": code_scanning.capability,
            "open_alerts": code_alerts,
            "query_errors": [],
            "truncation": {"alerts_limit": code_limit, "alerts_at_cap": code_at_cap, "notes": code_notes},
        },
        "secret_scanning": {
            "collected": secret_scanning.collected,
            "collection_status": secret_status,
            "outcome_hint": secret_scanning.outcome,
            "capability": secret_scanning.capability,
            "open_alerts": secret_alerts,
            "query_errors": secret_errors,
            "truncation": {
                "alerts_limit": secret_limit,
                "alerts_at_cap": secret_at_cap,
                "locations_limit_per_alert": location_limit,
                "locations_at_cap_alerts": locations_at_cap,
                "notes": secret_notes,
            },
        },
        "collected_at": chrono::Utc::now().to_rfc3339(),
    });
    redact_snapshot(snapshot)
}

fn family_data(result: Result<AlertQuery, OrbitError>, auth: &AuthStatus) -> FamilyData {
    match result {
        Ok(AlertQuery::Alerts(alerts)) => FamilyData {
            collected: true,
            outcome: if alerts.is_empty() {
                OUTCOME_NO_OPEN_ALERTS
            } else {
                OUTCOME_OPEN_ALERTS
            },
            capability: auth.to_json(),
            alerts,
        },
        Ok(AlertQuery::CapabilityUnavailable(detail)) => unavailable_family(auth, &detail),
        Err(error) => unavailable_family(auth, &redact_all(&error.to_string())),
    }
}

fn unavailable_family(auth: &AuthStatus, detail: &str) -> FamilyData {
    FamilyData {
        collected: false,
        outcome: OUTCOME_CAPABILITY_UNAVAILABLE,
        capability: json!({
            "available": false,
            "authenticated": auth.authenticated,
            "detail": redact_all(detail),
        }),
        alerts: Vec::new(),
    }
}

fn unavailable_snapshot(auth: &AuthStatus, detail: &str) -> Result<Value, OrbitError> {
    let family = || {
        json!({
            "collected": false,
            "collection_status": OUTCOME_CAPABILITY_UNAVAILABLE,
            "outcome_hint": OUTCOME_CAPABILITY_UNAVAILABLE,
            "capability": {
                "available": auth.available,
                "authenticated": auth.authenticated,
                "detail": redact_all(detail),
            },
            "open_alerts": [],
            "query_errors": [],
        })
    };
    redact_snapshot(json!({
        "schema_version": SCHEMA_VERSION,
        "collected": false,
        "collection_status": OUTCOME_CAPABILITY_UNAVAILABLE,
        "outcome_hint": OUTCOME_CAPABILITY_UNAVAILABLE,
        "capability": family()["capability"],
        "code_scanning": family(),
        "secret_scanning": family(),
        "collected_at": chrono::Utc::now().to_rfc3339(),
    }))
}

fn aggregate_collection_status(statuses: &[&str]) -> &'static str {
    if statuses.iter().all(|status| *status == "fully_collected") {
        "fully_collected"
    } else if statuses
        .iter()
        .all(|status| *status == OUTCOME_CAPABILITY_UNAVAILABLE)
    {
        OUTCOME_CAPABILITY_UNAVAILABLE
    } else {
        "partially_collected"
    }
}

fn bound_alert(mut alert: Value) -> Value {
    if let Some(object) = alert.as_object_mut() {
        for (key, value) in object.iter_mut() {
            if let Some(text) = value.as_str() {
                let max = if matches!(key.as_str(), "summary" | "message" | "rule_description") {
                    300
                } else {
                    500
                };
                *value = Value::String(one_line(text, max));
            }
        }
    }
    alert
}

fn bound_pull_request(mut pull_request: Value) -> Value {
    if let Some(object) = pull_request.as_object_mut() {
        bound_field(object, "title", 300);
        bound_field(object, "body", 1_000);
        bound_field(object, "url", 500);
        bound_field(object, "author", 100);
        bound_field(object, "head_branch", 200);
    }
    pull_request
}

fn bound_field(object: &mut Map<String, Value>, key: &str, max: usize) {
    if let Some(text) = object.get(key).and_then(Value::as_str) {
        object.insert(key.to_string(), Value::String(one_line(text, max)));
    }
}

fn one_line(text: &str, max: usize) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    compact.chars().take(max).collect()
}

fn redact_snapshot(snapshot: Value) -> Result<Value, OrbitError> {
    let encoded = serde_json::to_string(&snapshot).map_err(|error| {
        OrbitError::Execution(format!("failed to encode security-alert snapshot: {error}"))
    })?;
    serde_json::from_str(&redact_all(&encoded)).map_err(|error| {
        OrbitError::Execution(format!(
            "failed to decode redacted security-alert snapshot: {error}"
        ))
    })
}
