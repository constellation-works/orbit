//! `MetricsEntry` is persisted as day-partitioned JSONL, so its actor
//! attribution has to survive a write/read cycle unchanged and keep reading
//! the lines earlier writers produced.

use chrono::{TimeZone, Utc};

use crate::identity::ActorIdentity;
use crate::telemetry::MetricsEntry;

fn entry(actor: ActorIdentity) -> MetricsEntry {
    MetricsEntry {
        ts: Utc
            .with_ymd_and_hms(2026, 9, 7, 12, 0, 0)
            .single()
            .expect("unambiguous fixture timestamp"),
        job_run: "jrun-20260907-0001".to_string(),
        step: "implement".to_string(),
        task_id: Some("ORB-11731".to_string()),
        actor_identity: actor,
        tool_invocations: 3,
        token_usage: Some(1_024),
        step_duration_ms: Some(4_200),
        retry_count: 1,
    }
}

#[test]
fn human_attribution_survives_a_jsonl_round_trip() {
    let written = entry(ActorIdentity::human("daniel"));

    let line = serde_json::to_string(&written).expect("serialize metrics entry");
    let read_back: MetricsEntry = serde_json::from_str(&line).expect("deserialize metrics entry");

    assert_eq!(read_back, written);
    assert_eq!(
        read_back.actor_identity,
        ActorIdentity::human("daniel"),
        "a human step must not be re-read as an agent model"
    );
}

#[test]
fn agent_attribution_survives_a_jsonl_round_trip() {
    for model in ["gpt-5", "claude / opus"] {
        let written = entry(ActorIdentity::agent(model));

        let line = serde_json::to_string(&written).expect("serialize metrics entry");
        let read_back: MetricsEntry =
            serde_json::from_str(&line).expect("deserialize metrics entry");

        assert_eq!(read_back, written, "round trip changed the {model} entry");
    }
}

#[test]
fn legacy_jsonl_lines_with_bare_string_actors_still_parse() {
    let cases = [
        (r#""gpt-5.5""#, ActorIdentity::agent("gpt-5.5")),
        (r#""codex / gpt-5.5""#, ActorIdentity::agent("gpt-5.5")),
        (r#""system""#, ActorIdentity::System),
        (
            r#"{"agent":{"name":"claude","model":"claude-opus-5"}}"#,
            ActorIdentity::agent("claude-opus-5"),
        ),
    ];

    for (actor_json, expected) in cases {
        let line = format!(
            r#"{{"ts":"2026-09-07T12:00:00Z","job_run":"jrun-legacy","step":"implement","actor_identity":{actor_json},"tool_invocations":1,"token_usage":10,"step_duration_ms":50,"retry_count":0}}"#
        );

        let parsed: MetricsEntry = serde_json::from_str(&line).expect("parse legacy metrics line");

        assert_eq!(parsed.actor_identity, expected, "parsing {actor_json}");
        assert_eq!(parsed.job_run, "jrun-legacy");
    }
}
