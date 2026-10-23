//! The Reviewers scorecard (`/api/agents/reviewers/*`) through the runtime
//! API and the HTTP API, over fixture tasks whose review gate settlements cite
//! paths in a real repository: a reject the next run proved wrong, a reject
//! whose cited path the next run changed, reviewer fixes that held and that
//! failed, incomplete reviews, a review job's verified findings and a
//! `review_missed` regression filed long after the review that accepted.

use std::fs;
use std::io::Write as _;
use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use orbit_core::TaskStatus;
use orbit_core::application::agents::{
    AgentFacts, AgentQuery, AgentRole, Owner, Problem, agent_scorecard,
};
use orbit_core::application::task::TaskAddParams;
use orbit_types::task::{
    RegressionEscape, RegressionNature, TASK_ARTIFACT_SCHEMA_VERSION, TASK_COMMENTS_FILE_NAME,
    TaskCommentRowV2, TaskComplexity, TaskRelation, TaskRelationType, TaskType,
};
use serde_json::{Value, json};

use super::agents::{Owned, WS, add, append, as_f64, bundle_dir, implementation_run, regression};
use super::support::{Fixture, isolated, json_ok};

/// Run git in the fixture repository and return its trimmed stdout.
fn git(repo: &Path, args: &[&str]) -> String {
    let output = crate::git_test_support::command()
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// Write `files` and commit them on the current branch; the new commit.
fn commit(repo: &Path, files: &[(&str, &str)]) -> String {
    for (path, content) in files {
        fs::create_dir_all(repo.join(path).parent().unwrap()).unwrap();
        fs::write(repo.join(path), content).unwrap();
        git(repo, &["add", "--", path]);
    }
    git(repo, &["commit", "-qm", "fixture"]);
    git(repo, &["rev-parse", "HEAD"])
}

/// A comment on a fixture task at `at`, as the review gate posts its
/// settlement: the public comment write stamps the current time.
fn comment_at(fixture: &Fixture, task_id: &str, at: DateTime<Utc>, message: &str) {
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
        body: message.into(),
        operation_id: None,
    };
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(file, "{}", serde_json::to_string(&row).unwrap()).unwrap();
}

/// A review gate settlement comment, shaped as the gate writes it.
#[allow(clippy::too_many_arguments)]
fn settle(
    fixture: &Fixture,
    task_id: &str,
    attempt: &str,
    at: DateTime<Utc>,
    verdict: &str,
    reviewer: &str,
    findings: &[&str],
    reviewed: &str,
) {
    let findings = if findings.is_empty() {
        "Findings: none.".to_string()
    } else {
        format!("Findings:\n{}", findings.join("\n"))
    };
    comment_at(
        fixture,
        task_id,
        at,
        &format!(
            "before-PR review settled attempt `{attempt}`: verdict **{verdict}** (assurance: none).\n\n\
             {findings}\n\n\
             - Reviewer: crew `{reviewer}` (codex / gpt-{reviewer})\n\
             - Implementation: `{reviewed}` on base `0000000` (1 commit(s), unchanged by review)\n\
             - Reviewer commit: none\n\
             - Final candidate: `{reviewed}`\n\
             - Reviewer runtime: 600s of 30 min\n\
             - Escalation: none"
        ),
    );
}

fn task(fixture: &Fixture, title: &str) -> String {
    add(
        fixture,
        Owned {
            title,
            orchestrator: None,
            origin: None,
            complexity: TaskComplexity::Medium,
            status: TaskStatus::Backlog,
            relations: vec![],
        },
    )
}

/// One implementation attempt by crew `sol`, from `start` to a block an hour
/// later.
fn attempt(fixture: &Fixture, task_id: &str, run_id: &str, start: DateTime<Utc>) {
    implementation_run(
        fixture,
        run_id,
        task_id,
        "sol",
        Some("gpt-sol"),
        start - Duration::seconds(1),
        start + Duration::minutes(50),
    );
    append(
        fixture,
        task_id,
        start,
        "started",
        None,
        Some(TaskStatus::InProgress),
    );
}

/// A finding a review job filed from `filer`, with finding verification's
/// verdict on it.
fn filed_finding(fixture: &Fixture, filer: &str, verdict: &str, severity: Option<&str>) {
    let id = fixture
        .runtime
        .add_task(TaskAddParams {
            title: format!("review finding {verdict}"),
            description: "a defect the review job found".into(),
            task_type: Some(TaskType::Bug),
            tags: vec!["delivery-code-review".into()],
            complexity: TaskComplexity::Medium,
            relations: vec![TaskRelation {
                relation_type: TaskRelationType::SpawnedFrom,
                target: filer.parse().unwrap(),
                regression: None,
            }],
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string();
    let now = Utc::now();
    let mut record = json!({
        "schema_version": 1,
        "verdict": verdict,
        "confidence": "high",
        "verified_sha": "abc1234",
        "base_branch": "agent-main",
        "evidence": [],
        "reasoning": "fixture verdict",
        "crews": [],
        "escalated": false,
        "verified_at": now,
    });
    if let Some(severity) = severity {
        record["severity"] = json!(severity);
    } else {
        record["rejection"] = json!({"reason": "premise_wrong", "detail": "the code handles it"});
    }
    let dir = bundle_dir(fixture.path("").as_path(), &id).expect("fixture task bundle");
    let events = dir.join(orbit_types::task::TASK_EVENTS_FILE_NAME);
    let rows = fs::read_to_string(&events).unwrap().lines().count();
    let row = orbit_types::task::TaskEventRowV2 {
        schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
        event_id: format!("EV-{:04}", rows + 1),
        at: now,
        by: "finding-verification".into(),
        event_type: "finding_verified".into(),
        note: Some(record.to_string()),
        from_status: None,
        to_status: None,
        operation_id: None,
        decision: None,
    };
    let mut file = fs::OpenOptions::new().append(true).open(events).unwrap();
    writeln!(file, "{}", serde_json::to_string(&row).unwrap()).unwrap();
}

fn reviewers(facts: &AgentFacts, window: &str) -> Value {
    agent_scorecard(
        facts,
        AgentRole::Reviewers,
        &AgentQuery::parse(Some(window), Some(1)).unwrap(),
        std::time::Duration::ZERO,
    )
}

#[test]
fn reviewer_scorecard_does_not_charge_unresolvable_line_range_as_false_reject() {
    isolated(
        "agents_reviewers::reviewer_scorecard_does_not_charge_unresolvable_line_range_as_false_reject",
        || {
            let fixture = Fixture::new();
            let repo = fixture.work.parent().unwrap().to_path_buf();
            git(&repo, &["init", "-q"]);
            let rejected = commit(&repo, &[("src/a.rs", "before fix")]);
            let fixed = commit(&repo, &[("src/a.rs", "after fix")]);

            let now = Utc::now();
            let cited_range = task(&fixture, "line-range cited reject");
            settle(
                &fixture,
                &cited_range,
                "rv-range-reject",
                now - Duration::minutes(2),
                "reject",
                "vega",
                &["- `F1` [high, open] the parser drops a field\n  - Cited: src/a.rs:10-20"],
                &rejected,
            );
            settle(
                &fixture,
                &cited_range,
                "rv-range-accept",
                now - Duration::minutes(1),
                "accept",
                "lyra",
                &[],
                &fixed,
            );

            let server = fixture.counted_task_server();
            let scorecard = json_ok(server.get(&format!(
                "/api/agents/reviewers/scorecard?{WS}&window=30d&min_support=1"
            )));
            assert_eq!(scorecard["status"], "ok", "{scorecard}");
            assert_eq!(
                row(&scorecard, "vega")["counts"]["false_rejects"],
                0,
                "a line-range path absent from both trees leaves the reject unjudged: {scorecard}"
            );
        },
    );
}

fn row<'a>(payload: &'a Value, crew: &str) -> &'a Value {
    payload["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["crew"] == crew)
        .unwrap_or_else(|| panic!("a {crew} row: {payload}"))
}

fn stat<'a>(row: &'a Value, key: &str) -> &'a Value {
    row["stats"]
        .as_array()
        .unwrap()
        .iter()
        .find(|stat| stat["key"] == key)
        .map(|stat| &stat["value"])
        .unwrap_or_else(|| panic!("a {key} stat: {row}"))
}

/// Reviewer rows per crew and model over one fixture workspace: a reject
/// whose next run changed none of the cited paths and whose next review
/// accepted is the reviewer's false reject, not the implementer's reject; a
/// reject whose next run changed a cited path holds its finding; reviewer
/// fixes hold or fail; incomplete reviews count unless infrastructure caused
/// them; a review job earns its CONFIRMED findings and is charged its
/// REJECTED ones; rejecting more without being right never raises a score; a
/// `review_missed` regression filed weeks after the accepting review still
/// lowers that row; and reviews younger than the 30-day lookback are
/// provisional per row and in total.
#[test]
fn reviewer_scorecard_scores_findings_that_held_false_rejects_and_late_misses() {
    isolated(
        "agents_reviewers::reviewer_scorecard_scores_findings_that_held_false_rejects_and_late_misses",
        || {
            let fixture = Fixture::new();
            let now = Utc::now();
            let hours = Duration::hours;

            // The rejected candidate changed two files; the next run after one
            // reject touched only an unrelated file, after the other it
            // changed a cited one.
            let repo = fixture.work.parent().unwrap().to_path_buf();
            git(&repo, &["init", "-q"]);
            commit(
                &repo,
                &[
                    ("src/a.rs", "a0"),
                    ("src/b.rs", "b0"),
                    ("src/other.rs", "o0"),
                ],
            );
            let rejected = commit(&repo, &[("src/a.rs", "a1"), ("src/b.rs", "b1")]);
            let unrelated = commit(&repo, &[("src/other.rs", "o1")]);
            git(&repo, &["checkout", "-q", &rejected]);
            let addressed = commit(&repo, &[("src/b.rs", "b2")]);

            // vega rejects; the next run changes only an unrelated file and
            // lyra accepts: a false reject.
            let false_reject = task(&fixture, "false reject");
            attempt(&fixture, &false_reject, "jrun-false-1", now - hours(8));
            settle(
                &fixture,
                &false_reject,
                "rv-f1",
                now - hours(7) - Duration::minutes(30),
                "reject",
                "vega",
                &["- `F1` [high, open] the parser drops a field\n  - Cited: src/a.rs"],
                &rejected,
            );
            append(
                &fixture,
                &false_reject,
                now - hours(7) - Duration::minutes(29),
                "review_gate",
                None,
                Some(TaskStatus::Blocked),
            );
            attempt(&fixture, &false_reject, "jrun-false-2", now - hours(6));
            settle(
                &fixture,
                &false_reject,
                "rv-f2",
                now - hours(5) - Duration::minutes(30),
                "accept",
                "lyra",
                &[],
                &unrelated,
            );

            // vega rejects with two findings; the next run changes the file
            // the critical one cites: it held, the low one did not.
            let held = task(&fixture, "held reject");
            attempt(&fixture, &held, "jrun-held-1", now - hours(8));
            settle(
                &fixture,
                &held,
                "rv-h1",
                now - hours(7) - Duration::minutes(30),
                "reject",
                "vega",
                &[
                    "- `F1` [critical, open] the lock is never released\n  - Cited: src/b.rs:12",
                    "- `F2` [low, open] a name is unclear\n  - Cited: src/a.rs",
                ],
                &rejected,
            );
            append(
                &fixture,
                &held,
                now - hours(7) - Duration::minutes(29),
                "review_gate",
                None,
                Some(TaskStatus::Blocked),
            );
            attempt(&fixture, &held, "jrun-held-2", now - hours(6));
            settle(
                &fixture,
                &held,
                "rv-h2",
                now - hours(5) - Duration::minutes(30),
                "accept",
                "lyra",
                &[],
                &addressed,
            );

            // lyra fixes a medium finding and owner revalidation passes;
            // another fix fails revalidation and is escalated.
            let fixed = task(&fixture, "fixes held");
            settle(
                &fixture,
                &fixed,
                "rv-x1",
                now - hours(4),
                "accept_with_fixes",
                "lyra",
                &[
                    "- `F1` [medium, fixed] an error was swallowed\n  - Changed: propagate the error (src/c.rs)",
                ],
                &addressed,
            );
            let failed = task(&fixture, "fixes failed");
            settle(
                &fixture,
                &failed,
                "rv-x2",
                now - hours(4),
                "accept_with_fixes",
                "lyra",
                &[
                    "- `F1` [high, fixed] a bound is off by one\n  - Changed: widen the bound (src/d.rs)",
                ],
                &addressed,
            );
            comment_at(
                &fixture,
                &failed,
                now - hours(3),
                "## Review gate escalation\n\nOrbit held PR publication. Verdict: `reject`. The reviewer's fixes did not pass owner revalidation.",
            );

            // vega settles two incomplete reviews: the host interrupted one,
            // the other was not infrastructure.
            let interrupted = task(&fixture, "incomplete: interrupted");
            settle(
                &fixture,
                &interrupted,
                "rv-i1",
                now - hours(2),
                "incomplete",
                "vega",
                &[],
                &addressed,
            );
            append(
                &fixture,
                &interrupted,
                now - hours(2) + Duration::minutes(1),
                "workflow_run_interrupted",
                None,
                Some(TaskStatus::Blocked),
            );
            let incomplete = task(&fixture, "incomplete: reviewer");
            settle(
                &fixture,
                &incomplete,
                "rv-i2",
                now - hours(2),
                "incomplete",
                "vega",
                &[],
                &addressed,
            );
            append(
                &fixture,
                &incomplete,
                now - hours(2) + Duration::minutes(1),
                "workflow_run_failed",
                Some("validation failed: the review produced no verdict"),
                Some(TaskStatus::Blocked),
            );

            // lyra accepted work 20 days ago, and 40 days ago.
            let old_accept = task(&fixture, "accepted 20 days ago");
            settle(
                &fixture,
                &old_accept,
                "rv-o1",
                now - Duration::days(20),
                "accept",
                "lyra",
                &[],
                &addressed,
            );
            let older_accept = task(&fixture, "accepted 40 days ago");
            settle(
                &fixture,
                &older_accept,
                "rv-o2",
                now - Duration::days(40),
                "accept",
                "lyra",
                &[],
                &addressed,
            );

            // orion's review job filed two findings: one CONFIRMED, one REJECTED.
            let review_job = task(&fixture, "delivery code review");
            implementation_run(
                &fixture,
                "jrun-orion",
                &review_job,
                "orion",
                Some("gpt-orion"),
                now - hours(3),
                now - hours(1),
            );
            append(
                &fixture,
                &review_job,
                now - hours(3) + Duration::seconds(1),
                "started",
                None,
                Some(TaskStatus::InProgress),
            );
            filed_finding(&fixture, &review_job, "CONFIRMED", Some("high"));
            filed_finding(&fixture, &review_job, "REJECTED", None);

            // Who each new problem counts against.
            let facts = fixture.runtime.agent_scan().unwrap();
            let owners = |task: &str, problem: Problem| -> Vec<(Owner, Option<String>)> {
                facts
                    .charges()
                    .iter()
                    .filter(|charge| charge.task_id == task && charge.problem == problem)
                    .map(|charge| (charge.owner, charge.crew.clone()))
                    .collect()
            };
            let reviewer = |crew: &str| (Owner::Role(AgentRole::Reviewers), Some(crew.to_string()));
            assert_eq!(
                owners(&false_reject, Problem::FalseReject),
                vec![reviewer("vega")]
            );
            assert_eq!(
                owners(&false_reject, Problem::ReviewReject),
                vec![],
                "a false reject is the reviewer's, not the implementer's"
            );
            assert_eq!(owners(&held, Problem::FalseReject), vec![]);
            assert_eq!(
                owners(&held, Problem::ReviewReject),
                vec![(Owner::Role(AgentRole::Implementers), Some("sol".into()))],
                "a reject that held stays the implementer's"
            );
            assert_eq!(
                owners(&incomplete, Problem::IncompleteReview),
                vec![reviewer("vega")]
            );
            assert_eq!(
                owners(&interrupted, Problem::IncompleteReview),
                vec![(Owner::NotScored("infrastructure"), None)]
            );
            assert_eq!(owners(&failed, Problem::FailedFix), vec![reviewer("lyra")]);
            let rejected_findings: Vec<_> = facts
                .charges()
                .iter()
                .filter(|charge| charge.problem == Problem::RejectedFinding)
                .map(|charge| (charge.owner, charge.crew.clone(), charge.run_id.clone()))
                .collect();
            assert_eq!(
                rejected_findings,
                vec![(
                    Owner::Role(AgentRole::Reviewers),
                    Some("orion".into()),
                    Some("jrun-orion".into())
                )],
                "a REJECTED finding counts against the review job run that filed it"
            );

            // The ranking: one row per crew and model with the §5.3 terms.
            let scorecard = reviewers(&facts, "90d");
            assert_eq!(scorecard["status"], "ok", "{scorecard}");
            assert_eq!(scorecard["unit"], "settled reviews");
            let (vega, lyra, orion) = (
                row(&scorecard, "vega"),
                row(&scorecard, "lyra"),
                row(&scorecard, "orion"),
            );
            assert_eq!(vega["model"], "gpt-vega");
            assert_eq!(
                (
                    &vega["units"],
                    &vega["counts"]["findings_held"],
                    &vega["counts"]["false_rejects"],
                    &vega["counts"]["incomplete_reviews"]
                ),
                (&json!(4), &json!(8), &json!(1), &json!(1)),
                "only the critical finding whose cited path the next run changed held: {vega}"
            );
            assert_eq!(
                (
                    &lyra["units"],
                    &lyra["counts"]["findings_held"],
                    &lyra["counts"]["fixes_held"],
                    &lyra["counts"]["failed_fixes"]
                ),
                (&json!(6), &json!(2), &json!(1), &json!(1)),
                "the medium fix held; the failed one earns nothing: {lyra}"
            );
            assert_eq!(
                (
                    &orion["units"],
                    &orion["counts"]["findings_held"],
                    &orion["counts"]["rejected_findings"]
                ),
                (&json!(1), &json!(4), &json!(1)),
                "a review job counts through its filed findings: {orion}"
            );
            assert_eq!(lyra["counts"]["missed_regressions"], 0);

            // Shown, not scored: verdict outcomes and the accept rate against
            // peers on the same tiers.
            assert_eq!(
                [
                    "outcome_held_up",
                    "outcome_caught",
                    "outcome_false_alarm",
                    "outcome_missed",
                    "outcome_unresolved"
                ]
                .map(|key| stat(vega, key).clone()),
                [json!(0), json!(1), json!(1), json!(0), json!(2)],
                "{vega}"
            );
            assert_eq!(stat(lyra, "accept_rate"), &json!(100));
            assert_eq!(stat(lyra, "peer_accept_rate"), &json!(0));
            assert_eq!(stat(lyra, "accepts_often"), &json!(true), "{lyra}");
            assert_eq!(stat(vega, "accepts_often"), &json!(false), "{vega}");
            assert_eq!(stat(vega, "median_review_minutes"), &json!(10));

            // Provisional: reviews younger than 30 days, per row and in total.
            assert_eq!(
                lyra["provisional"], 5,
                "the review 40 days ago is past the lookback: {lyra}"
            );
            assert_eq!(vega["provisional"], 4);
            assert_eq!(orion["provisional"], 1);
            assert_eq!(scorecard["provisional"], 10, "{scorecard}");
            let recent = reviewers(&facts, "24h");
            assert_eq!(
                (
                    &row(&recent, "lyra")["units"],
                    &row(&recent, "lyra")["provisional"]
                ),
                (&json!(4), &json!(4))
            );
            assert_eq!(recent["provisional"], 9);
            let quiet = reviewers(&facts, "1h");
            assert_eq!(
                (
                    quiet["rows"].as_array().map(Vec::len),
                    &quiet["provisional"]
                ),
                (Some(0), &json!(0)),
                "a window with no review has none provisional"
            );

            // Rejecting more without being right never raises a score: a
            // reject the next run proves wrong lowers vega, and so does one
            // not judged yet.
            let raw = |payload: &Value, crew: &str| as_f64(&row(payload, crew)["raw"]);
            let before = raw(&scorecard, "vega");
            let another = task(&fixture, "another false reject");
            settle(
                &fixture,
                &another,
                "rv-a1",
                now - hours(1),
                "reject",
                "vega",
                &["- `F1` [high, open] the parser drops a field\n  - Cited: src/a.rs"],
                &rejected,
            );
            settle(
                &fixture,
                &another,
                "rv-a2",
                now - Duration::minutes(30),
                "accept",
                "lyra",
                &[],
                &unrelated,
            );
            let after_false = reviewers(&fixture.runtime.agent_scan().unwrap(), "90d");
            assert_eq!(row(&after_false, "vega")["counts"]["false_rejects"], 2);
            assert!(raw(&after_false, "vega") < before, "{after_false}");
            let pending = task(&fixture, "reject not judged yet");
            settle(
                &fixture,
                &pending,
                "rv-p1",
                now - Duration::minutes(20),
                "reject",
                "vega",
                &["- `F1` [critical, open] a race\n  - Cited: src/b.rs"],
                &rejected,
            );
            let after_pending = reviewers(&fixture.runtime.agent_scan().unwrap(), "90d");
            assert_eq!(
                row(&after_pending, "vega")["counts"]["findings_held"],
                8,
                "a reject earns nothing until its next run"
            );
            assert!(
                raw(&after_pending, "vega") < raw(&after_false, "vega"),
                "{after_pending}"
            );

            // A `review_missed` regression filed 20 days after lyra accepted
            // still lowers lyra's row for that review.
            let score_before = as_f64(&row(&after_pending, "lyra")["score"]);
            regression(
                &fixture,
                &old_accept,
                RegressionNature::DefectInNew,
                RegressionEscape::ReviewMissed,
            );
            let facts = fixture.runtime.agent_scan().unwrap();
            let missed = facts
                .charges()
                .iter()
                .find(|charge| {
                    charge.task_id == old_accept && charge.problem == Problem::Regression
                })
                .expect("the regression is charged");
            assert_eq!(missed.crew.as_deref(), Some("lyra"));
            assert_eq!(
                missed.anchor,
                now - Duration::days(20),
                "it falls in the window of the review"
            );
            assert!(
                missed.at - missed.anchor >= Duration::days(19),
                "filed long after the review"
            );
            let late = reviewers(&facts, "30d");
            assert_eq!(
                row(&late, "lyra")["counts"]["missed_regressions"],
                1,
                "{late}"
            );
            assert!(as_f64(&row(&reviewers(&facts, "90d"), "lyra")["score"]) < score_before);
            assert_eq!(stat(row(&late, "lyra"), "outcome_missed"), &json!(1));

            // The routes serve the same rows, and the detail names the tasks.
            let server = fixture.counted_task_server();
            let payload = json_ok(server.get(&format!(
                "/api/agents/reviewers/scorecard?{WS}&window=30d&min_support=1"
            )));
            assert_eq!(payload["status"], "ok");
            assert_eq!(
                payload["provisional"],
                row(&payload, "lyra")["provisional"].as_u64().unwrap()
                    + row(&payload, "vega")["provisional"].as_u64().unwrap()
                    + row(&payload, "orion")["provisional"].as_u64().unwrap()
            );
            let detail = json_ok(server.get(&format!(
                "/api/agents/reviewers/detail?{WS}&window=30d&min_support=1&crew=vega"
            )));
            let term = |key: &str| {
                detail["terms"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|term| term["key"] == key)
                    .unwrap()
                    .clone()
            };
            let tasks = |key: &str| -> Vec<String> {
                term(key)["tasks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|task| task["task_id"].as_str().unwrap().to_string())
                    .collect()
            };
            let mut false_rejects = tasks("false_rejects");
            false_rejects.sort();
            let mut expected = vec![false_reject.clone(), another.clone()];
            expected.sort();
            assert_eq!(false_rejects, expected);
            assert_eq!(tasks("findings_held"), vec![held.clone()]);
            assert_eq!(detail["provisional"], payload["provisional"]);
        },
    );
}
