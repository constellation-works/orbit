//! `--until` ends a scorecard window in the past. A calendar window is then
//! `[until - window, until)`; a count window takes the newest N attempts
//! before `until`. Omitted, nothing is excluded for being at or after now,
//! and the payload still reports that instant as `until`.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write as _;
use std::path::Path;

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use orbit_core::TaskStatus;
use orbit_types::task::{
    TASK_ARTIFACT_SCHEMA_VERSION, TASK_COMMENTS_FILE_NAME, TASK_ENVELOPE_FILE_NAME,
    TaskCommentRowV2, TaskComplexity, TaskEnvelopeV2,
};
use serde_json::Value;

use super::agents::{Owned, WS, add, append, bundle_dir, implementation_run};
use super::codebase::{init_repo, land, pilot_receipt, seed_pilot_run};
use super::support::{Fixture, Server, isolated, json_ok};

const ROLES: [&str; 4] = ["orchestrators", "implementers", "reviewers", "pilots"];

/// Work stamped at `at` under crew `name`, for every role: the task's
/// orchestrator and `created_at`, one implementation attempt, one review
/// settlement and one pilot assessment.
fn seed(fixture: &Fixture, repo: &Path, name: &str, at: DateTime<Utc>) {
    let id = add(
        fixture,
        Owned {
            title: name,
            orchestrator: Some(name),
            origin: None,
            complexity: TaskComplexity::Medium,
            status: TaskStatus::Done,
            relations: vec![],
        },
    );
    let path = bundle_dir(fixture.path("").as_path(), &id)
        .expect("fixture task bundle")
        .join(TASK_ENVELOPE_FILE_NAME);
    let mut envelope: TaskEnvelopeV2 =
        serde_yaml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    envelope.created_at = at;
    fs::write(&path, serde_yaml::to_string(&envelope).unwrap()).unwrap();
    let index = rusqlite::Connection::open(fixture.global.join("tasks/index.sqlite")).unwrap();
    assert_eq!(
        index
            .execute(
                "UPDATE task_bundle_index SET created_at = ?1 WHERE task_id = ?2",
                (at.to_rfc3339(), &id),
            )
            .unwrap(),
        1,
        "the index row keeps the stamped created_at"
    );

    let file = format!("src/{name}.rs");
    implementation_run(
        fixture,
        &format!("jrun-impl-{name}"),
        &id,
        name,
        Some(&format!("{name}-model")),
        at - Duration::seconds(1),
        at + Duration::minutes(50),
    );
    append(
        fixture,
        &id,
        at,
        "started",
        None,
        Some(TaskStatus::InProgress),
    );
    settle(fixture, &id, name, at);
    let pilot_run = format!("jrun-pilot-{name}");
    seed_pilot_run(
        fixture,
        &pilot_run,
        name,
        &format!("{name}-model"),
        at - Duration::hours(1),
        at + Duration::hours(2),
    );
    let selector = format!("file:{file}");
    pilot_receipt(
        fixture,
        &id,
        at,
        Some(&pilot_run),
        &[],
        &[selector.as_str()],
    );
    land(fixture, repo, &id, &[&file], at + Duration::hours(1));
}

/// A review gate settlement at `at` by crew `reviewer`.
fn settle(fixture: &Fixture, task_id: &str, reviewer: &str, at: DateTime<Utc>) {
    let dir = bundle_dir(fixture.path("").as_path(), task_id).expect("fixture task bundle");
    let path = dir.join(TASK_COMMENTS_FILE_NAME);
    let rows = fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .count();
    let row = TaskCommentRowV2 {
        schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
        comment_id: format!("CM-{:04}", rows + 1),
        at,
        by: "system".into(),
        body: format!(
            "before-PR review settled attempt `rv-{reviewer}`: verdict **accept** (assurance: none).\n\
             - Reviewer: crew `{reviewer}` (codex / gpt-{reviewer})\n\
             - Implementation: `abc` on base `0000000` (1 commit(s), unchanged by review)\n\
             - Reviewer commit: none\n\
             - Final candidate: `abc`\n\
             - Reviewer runtime: 600s of 30 min\n\
             - Escalation: none"
        ),
        operation_id: None,
    };
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(file, "{}", serde_json::to_string(&row).unwrap()).unwrap();
}

fn crews(card: &Value) -> BTreeSet<String> {
    card["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["crew"].as_str().unwrap().to_string())
        .collect()
}

fn instant(value: &Value) -> DateTime<Utc> {
    let text = value
        .as_str()
        .unwrap_or_else(|| panic!("a timestamp: {value}"));
    assert!(
        text.ends_with('Z') && !text.contains('.'),
        "RFC 3339 UTC seconds: {text}"
    );
    DateTime::parse_from_rfc3339(text)
        .unwrap()
        .with_timezone(&Utc)
}

fn card(server: &Server, role: &str, query: &str) -> Value {
    json_ok(server.get(&format!(
        "/api/agents/{role}/scorecard?{WS}&min_support=1&{query}"
    )))
}

fn expect_crews(server: &Server, query: &str, expected: &BTreeSet<String>) {
    for role in ROLES {
        let body = card(server, role, query);
        assert_eq!(crews(&body), *expected, "{role} crews for {query}: {body}");
    }
}

/// A 7-day window ending 30 days ago counts only the work inside it, for
/// every role; omitting `until` keeps work at or after now, and a reviewer
/// window that ends 30 or more days back reports no provisional reviews.
#[test]
fn past_until_window_counts_work_before_until() {
    isolated(
        "agents_until::past_until_window_counts_work_before_until",
        || {
            let fixture = Fixture::new();
            let repo = init_repo(&fixture);
            let now = Utc::now();
            // Configured fixture crews, one per bucket. Inside a 7-day window
            // ending now, and after a window ending 30 days ago.
            seed(&fixture, &repo, "opus", now - Duration::days(2));
            // Inside [now-37d, now-30d), and older than the 30-day provisional lookback.
            seed(&fixture, &repo, "sonnet", now - Duration::days(33));
            // Older than both calendar windows, still inside a count window ending 30 days ago.
            seed(&fixture, &repo, "haiku", now - Duration::days(40));
            // After any `until` this test reports. Omitted, the window has no upper filter.
            seed(&fixture, &repo, "luna", now + Duration::hours(6));

            let server = fixture.server(false);
            let open: BTreeSet<String> = ["opus", "luna"].into_iter().map(str::to_string).collect();
            let settled: BTreeSet<String> = ["sonnet"].into_iter().map(str::to_string).collect();
            let before_until: BTreeSet<String> = ["sonnet", "haiku"]
                .into_iter()
                .map(str::to_string)
                .collect();

            expect_crews(&server, "window=7d", &open);
            expect_crews(&server, "window=7d&until=30d", &settled);
            let instant_end = (now - Duration::days(30)).to_rfc3339_opts(SecondsFormat::Secs, true);
            let encoded = instant_end.replace(':', "%3A");
            expect_crews(&server, &format!("window=7d&until={encoded}"), &settled);
            expect_crews(&server, "window=last50", &{
                let mut all = before_until.clone();
                all.extend(open.iter().cloned());
                all
            });
            expect_crews(&server, "window=last50&until=30d", &before_until);

            for role in ROLES {
                let open_card = card(&server, role, "window=7d");
                let (since, until) = (instant(&open_card["since"]), instant(&open_card["until"]));
                assert_eq!(
                    until - since,
                    Duration::hours(7 * 24),
                    "{role} omitted window is 7 days ending at the reported until: {open_card}"
                );
                assert!(
                    (until - now).num_seconds().abs() < 15 * 60,
                    "{role} omitted until is the scan's now, not a clamped end: {until}"
                );
                let closed = card(&server, role, "window=7d&until=30d");
                let (closed_since, closed_until) =
                    (instant(&closed["since"]), instant(&closed["until"]));
                assert_eq!(closed_until - closed_since, Duration::hours(7 * 24));
                assert!(
                    (closed_until - (now - Duration::days(30)))
                        .num_seconds()
                        .abs()
                        < 15 * 60,
                    "{role} until=30d ends about 30 days ago: {closed_until}"
                );
                let pinned = card(&server, role, &format!("window=7d&until={encoded}"));
                assert_eq!(
                    pinned["until"], instant_end,
                    "{role} serves the requested instant"
                );

                let detail = json_ok(server.get(&format!(
                    "/api/agents/{role}/detail?{WS}&window=7d&min_support=1&until=30d&crew=sonnet"
                )));
                assert_eq!(detail["row"]["crew"], "sonnet", "{role} detail: {detail}");
                assert_eq!(
                    instant(&detail["until"]) - instant(&detail["since"]),
                    Duration::hours(7 * 24)
                );

                let missing = server.get(&format!(
                    "/api/agents/{role}/detail?{WS}&window=7d&min_support=1&crew=sonnet"
                ));
                assert_eq!(
                    missing.status().as_u16(),
                    400,
                    "{role} sonnet is outside a window ending now"
                );
            }

            let reviewers = card(&server, "reviewers", "window=7d&until=30d");
            assert_eq!(
                reviewers["provisional"], 0,
                "a review 33 days ago is settled against now: {reviewers}"
            );
            let current = card(&server, "reviewers", "window=7d");
            assert!(
                current["provisional"].as_u64().unwrap() >= 1,
                "a review two days ago stays provisional: {current}"
            );

            for (query, needle) in [
                ("window=7d&until=yesterday", "RFC 3339"),
                ("window=7d&until=2026-11-01", "RFC 3339"),
                ("window=7d&until=2999-01-01T00%3A00%3A00Z", "future"),
                ("window=7d&until=", "RFC 3339"),
            ] {
                let response =
                    server.get(&format!("/api/agents/implementers/scorecard?{WS}&{query}"));
                assert_eq!(response.status().as_u16(), 400, "{query}");
                let body: Value = response.json().unwrap();
                let message = body["error"].as_str().unwrap();
                assert!(
                    message.contains(needle),
                    "{query} refusal ({needle}): {message}"
                );
            }
        },
    );
}
