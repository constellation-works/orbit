//! Public precedent retrieval preserves citations for requests written before risk metadata.

use chrono::{Duration, Utc};
use orbit_core::application::task::precedent_provider::{
    PrecedentProviderRequest, decision_precedent_category, decision_precedents_from_history,
};
use orbit_types::task::{
    DecisionAnswer, DecisionBinding, DecisionGate, DecisionRequest, DecisionResolution,
    TaskHistoryEntry,
};
use serde_json::{Value, json};

#[test]
fn legacy_request_keeps_its_precedent_citation() {
    let raised_at = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let answered_at = raised_at + Duration::days(1);
    let cutoff = answered_at + Duration::hours(1);
    let request = DecisionRequest::new(
        "T-1".into(),
        DecisionGate::AgentRequest,
        vec![DecisionAnswer::plain("a", "Choice", "Consequence")],
        DecisionBinding::BlockEpisode {
            episode: "fixture".into(),
        },
        "fixture".into(),
        raised_at,
    );
    let mut entry = request.history_entry("fixture");
    let mut legacy: Value = serde_json::from_str(entry.note.as_deref().unwrap()).unwrap();
    legacy.as_object_mut().unwrap().remove("risk_tier");
    legacy.as_object_mut().unwrap().remove("category");
    legacy["answers"][0]
        .as_object_mut()
        .unwrap()
        .remove("risk_tier");
    entry.note = Some(legacy.to_string());
    let resolution: DecisionResolution = serde_json::from_value(json!({
        "request_id": request.id, "answer_id": "a", "answered_by": "human:fixture", "answered_at": answered_at
    })).unwrap();
    let answer = TaskHistoryEntry {
        at: answered_at,
        by: "human:fixture".into(),
        event: "decision_answered".into(),
        note: Some(serde_json::to_string(&resolution).unwrap()),
        from_status: None,
        to_status: None,
        operation_id: None,
        decision: None,
    };
    let query: PrecedentProviderRequest = serde_json::from_value(json!({
        "contract": "orbit.precedent-provider/1", "workspace_id": "ws_fixture",
        "request": {"id":"dr-query", "gate":"agent_request", "category":"ws_fixture/agent_request/untopiced",
            "question":"Choose?", "options":[{"id":"a", "label":"Choice"}], "raised_at":cutoff},
        "as_of":cutoff, "k":8
    })).unwrap();
    let category = decision_precedent_category("ws_fixture", &request);
    let expected = orbit_types::assistance::assistance_digest(
        &serde_json::to_vec(&(legacy, category, resolution, "human:fixture", answered_at)).unwrap(),
    );
    let records = decision_precedents_from_history(&query, &[entry, answer]);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].citation.digest, expected);
}
