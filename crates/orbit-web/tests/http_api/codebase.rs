//! Codebase analytics (`/api/codebase/analytics`) through the HTTP API, over
//! fixture tasks, relations, history, reservations, lock denials, landed
//! commits and task-pilot receipts.

use std::fs;

use chrono::{DateTime, Duration, Utc};
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams, TaskWriteScreen};
use orbit_core::{OrbitError, TaskStatus};
use orbit_engine::RuntimeHost;
use orbit_types::task::{
    DefectSeverity, RegressionClassification, RegressionEscape, RegressionNature, SeveritySource,
    TaskComplexity, TaskOrigin,
};
use orbit_types::workflow::handoff::EXTERNAL_MERGE_SETTLED_EVENT;
use serde_json::{Value, json};

use super::support::{Fixture, isolated, json_ok};

#[path = "codebase/fixtures.rs"]
mod fixtures;
use fixtures::*;
pub(super) use fixtures::{
    add, add_with_status, append_event, append_row, bundle_dir, init_repo, land, pilot_receipt,
    pilot_receipt_with, pilot_run, reserve, reserve_request, seed_pilot_run,
};

const WS: &str = "workspace=ws_http_fixture";

// File sets shared by the nearby-size riskiness scenarios: ten tasks of ten
// files and ten of eleven, so the estimator's two-size fits stay comparable.
const NEAR_TEN: [&str; 10] = [
    "src/near00.rs",
    "src/near01.rs",
    "src/near02.rs",
    "src/near03.rs",
    "src/near04.rs",
    "src/near05.rs",
    "src/near06.rs",
    "src/near07.rs",
    "src/near08.rs",
    "src/near09.rs",
];
const NEAR_ELEVEN: [&str; 11] = [
    "src/near00.rs",
    "src/near01.rs",
    "src/near02.rs",
    "src/near03.rs",
    "src/near04.rs",
    "src/near05.rs",
    "src/near06.rs",
    "src/near07.rs",
    "src/near08.rs",
    "src/near09.rs",
    "src/near10.rs",
];

/// Record a landed-delivery row per task id, keeping each row's commit
/// distinct from `base_seq`; the line count is the diff-lines basis input.
fn land_rows(fixture: &Fixture, ids: &[String], base_seq: usize, lines: u64) {
    for (index, id) in ids.iter().enumerate() {
        landed_row(fixture, id, base_seq + index, lines);
    }
}

/// Read one task list row's `riskiness` object through the public API.
/// Callers keep their own assertions, so each case name stays on failures.
fn task_riskiness(server: &super::support::Server, id: &str) -> Value {
    let rows = json_ok(server.get(&format!("/api/tasks?{WS}&limit=100")));
    rows["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == id)
        .map(|row| row["riskiness"].clone())
        .unwrap()
}

/// Assert a fitted probability within 1e-6; the `{value}` context names the case.
fn assert_risk_probability(value: &Value, expected: f64) {
    let actual = value["probability"].as_f64().unwrap();
    assert!((actual - expected).abs() < 1e-6, "{expected}: {value}");
}

/// Block a task on a failed workflow run, the shape the `blocked` ranking
/// classifies by its error code.
fn block_on_run_failure(fixture: &Fixture, task: &str, run: &str, code: &str, error: &str) {
    transition_and_return(
        fixture,
        task,
        "workflow_run_failed",
        Some(format!(
            "workflow run failed: job=task_pr_pipeline, run_id={run}, error_code={code}, error={error}"
        )),
        TaskStatus::Blocked,
    );
}

/// Note a PR-merge completion on a done task: the merge note is the landing
/// evidence the related view reads.
fn note_pr_completion(
    fixture: &Fixture,
    task: &str,
    now: DateTime<Utc>,
    event: &str,
    note: String,
) {
    append_event(
        fixture,
        task,
        now,
        event,
        Some(note),
        Some(TaskStatus::Done),
    );
}

/// Record a run followed by a review rejection, the `rejected` ranking's
/// exercised shape.
fn reject_after_run(fixture: &Fixture, task: &str, now: DateTime<Utc>) {
    for (event, status) in [
        ("started", TaskStatus::InProgress),
        ("review_rejected", TaskStatus::Rejected),
    ] {
        append_event(fixture, task, now, event, None, Some(status));
    }
}

// Every analytics scenario gets its own authority-scrubbed child and fixture.
// Derive the exact libtest name here so selectors cannot drift from isolation.
macro_rules! analytics_test {
    ($name:ident, |$fixture:ident| $body:block) => {
        #[test]
        fn $name() {
            isolated(concat!("codebase::", stringify!($name)), || {
                let $fixture = Fixture::new();
                $body
            });
        }
    };
}

analytics_test!(
    codebase_excludes_foreign_and_unscoped_denials_without_local_activity,
    |fixture| {
        for (index, workspace, file) in [
            (0, Some("ws_foreign"), "src/foreign-only.rs"),
            (1, None, "src/legacy-only.rs"),
        ] {
            lock_denial_in_workspace(&fixture, index, None, None, file, workspace);
        }
        let server = fixture.server(false);
        let body = analytics(&server, "window=all&min_support=1");
        assert_eq!(body["tasks_scanned"], 0);
        assert_eq!(body["lock_contention"]["rows"], json!([]));
        assert_eq!(body["lock_contention"]["below_support"], 0);
    }
);

analytics_test!(
    codebase_scopes_production_lock_denials_and_support_but_audit_remains_global,
    |fixture| {
        fs::create_dir_all(fixture.runtime.paths().repo_root.join("src")).unwrap();
        fs::write(fixture.runtime.paths().repo_root.join("src/shared.rs"), "").unwrap();
        let holder = add(&fixture, "holder", &["src/shared.rs"], vec![]);
        let waiter = add(&fixture, "waiter", &["src/shared.rs"], vec![]);
        let request = |task: &str| request_lock(&fixture, task);
        assert_eq!(request(&holder)["reserved"], true);
        for _ in 0..2 {
            assert_eq!(request(&waiter)["reserved"], false);
        }
        let local = denied_events(&fixture);
        assert_eq!(local.len(), 2);
        assert!(
            local
                .iter()
                .all(|event| { event.workspace_id.as_deref() == Some("ws_http_fixture") })
        );

        for (index, workspace, file) in [
            (100, Some("ws_foreign"), "src/shared.rs"),
            (101, Some("ws_foreign"), "src/foreign-only.rs"),
            (102, None, "src/shared.rs"),
            (103, None, "src/legacy-only.rs"),
        ] {
            // A local task ID or relative path cannot attribute an old
            // unscoped row, or override an explicit foreign workspace.
            let task = if index == 100 { "HF-999" } else { &waiter };
            lock_denial_in_workspace(&fixture, index, Some(task), None, file, workspace);
        }

        let server = fixture.server(false);
        let body = analytics(&server, "window=all&min_support=1");
        let section = &body["lock_contention"];
        assert_eq!(section["rows"].as_array().unwrap().len(), 1, "{body}");
        let lock = row(section, "src/shared.rs").unwrap();
        assert_eq!(lock["numerator"], 1, "local denial retries count once");
        assert_eq!(lock["denominator"], 2);
        assert_eq!(lock["rate"], 0.5);
        assert_eq!(lock["reservations"], 1);
        assert_eq!(lock["denials"], 2, "only local attempts count");
        let strict = analytics(&server, "window=all&min_support=3");
        assert_eq!(strict["lock_contention"]["rows"], json!([]));
        assert_eq!(strict["lock_contention"]["below_support"], 1);

        let global = denied_events(&fixture);
        assert_eq!(global.len(), 6, "the shared audit reader stays global");
        let audit = json_ok(server.get(&format!(
            "/api/audit?{WS}&since=24h&tool=orbit.task.locks.reserve&status=denied&limit=100"
        )));
        assert_eq!(audit.as_array().unwrap().len(), 6);
        for index in 100..104 {
            assert!(
                audit
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|event| { event["execution_id"] == format!("lock-denial-{index}") })
            );
        }
    }
);

analytics_test!(
    codebase_lock_wait_spans_first_denial_to_admission_once_per_task,
    |fixture| {
        fs::create_dir_all(fixture.runtime.paths().repo_root.join("src")).unwrap();
        fs::write(fixture.runtime.paths().repo_root.join("src/shared.rs"), "").unwrap();
        let holder = add(&fixture, "holder", &["src/shared.rs"], vec![]);
        let waiter = add(&fixture, "waiter", &["src/shared.rs"], vec![]);
        let leaver = add(&fixture, "leaver", &["src/shared.rs"], vec![]);
        let request = |task: &str| request_lock(&fixture, task);
        let held = request(&holder);
        assert_eq!(held["reserved"], true);
        // The waiter retries for several passes; the leaver is refused
        // once, then archived without ever being admitted.
        for _ in 0..3 {
            assert_eq!(request(&waiter)["reserved"], false);
        }
        assert_eq!(request(&leaver)["reserved"], false);
        fixture
            .runtime
            .run_tool(
                "orbit.task.update",
                json!({"id": leaver, "status": "archived"}),
            )
            .unwrap();
        fixture
            .runtime
            .run_deterministic(
                "release_locks",
                &json!({}),
                &json!({"reservation_id": held["reservation_id"]}),
                Default::default(),
            )
            .unwrap();
        let admitted = request(&waiter);
        assert_eq!(admitted["reserved"], true, "{admitted}");

        let denied_at = |task: &str| -> Vec<DateTime<Utc>> {
            let mut times: Vec<_> = denied_events(&fixture)
                .into_iter()
                .filter(|event| event.task_id.as_deref() == Some(task))
                .map(|event| event.timestamp)
                .collect();
            times.sort();
            times
        };
        let waiter_denials = denied_at(&waiter);
        assert_eq!(waiter_denials.len(), 3);
        let locks = fixture
            .runtime
            .run_tool("orbit.task.locks", json!({}))
            .unwrap();
        let granted_at = locks["by_reservation"]
            .as_array()
            .unwrap()
            .iter()
            .find(|reservation| reservation["reservation_id"] == admitted["reservation_id"])
            .and_then(|reservation| reservation["created_at"].as_str())
            .map(|at| {
                DateTime::parse_from_rfc3339(at)
                    .unwrap()
                    .with_timezone(&Utc)
            })
            .unwrap_or_else(|| panic!("the admission's reservation is listed: {locks}"));

        let server = fixture.server(false);
        let body = analytics(&server, "window=all&min_support=1");
        let section = &body["lock_contention"];
        assert_eq!(section["wait_time_recorded"], true);
        let lock = row(section, "src/shared.rs").unwrap_or_else(|| panic!("{section}"));
        assert_eq!(lock["denials"], 4);
        assert_eq!(
            (&lock["wait"]["episodes"], &lock["wait"]["waiting"]),
            (&json!(2), &json!(0)),
            "three retries by the waiter are one episode beside the leaver's: {lock}"
        );
        assert_eq!(section["rows_by_wait"][0]["path"], "src/shared.rs");

        let detail = fixture
            .runtime
            .run_tool(
                "orbit.codebase.show",
                json!({ "path": "src/shared.rs", "window": "all", "min_support": 1 }),
            )
            .unwrap();
        let waits = detail["rankings"]["lock_contention"]["waits"]
            .as_array()
            .unwrap()
            .clone();
        let episode = |task: &str| {
            let found: Vec<&Value> = waits
                .iter()
                .filter(|wait| wait["task_id"] == task)
                .collect();
            assert_eq!(found.len(), 1, "one episode for {task}: {waits:?}");
            found[0].clone()
        };
        let millis = |at: DateTime<Utc>| at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let waited = episode(&waiter);
        assert_eq!(waited["end"], "admitted");
        assert_eq!(waited["denials"], 3);
        assert_eq!(waited["started_at"], millis(waiter_denials[0]), "{waited}");
        assert_eq!(waited["ended_at"], millis(granted_at), "{waited}");
        let waiter_ms = (granted_at - waiter_denials[0]).num_milliseconds();
        assert_eq!(waited["wait_ms"], waiter_ms);
        let left = episode(&leaver);
        assert_eq!(
            left["end"], "left_backlog",
            "archiving ends the wait: {left}"
        );
        assert_eq!(left["denials"], 1);
        assert_eq!(left["started_at"], millis(denied_at(&leaver)[0]));
        let leaver_ms = left["wait_ms"].as_i64().unwrap();
        assert!(waits.iter().all(|wait| wait["task_id"] != holder.as_str()));
        assert_eq!(
            lock["wait"]["total_ms"],
            waiter_ms + leaver_ms,
            "the row sums its episodes: {lock}"
        );
        assert_eq!(lock["wait"]["max_ms"], waiter_ms.max(leaver_ms));
        assert_eq!(lock["wait"]["median_ms"], (waiter_ms + leaver_ms) / 2);
    }
);

analytics_test!(
    codebase_sections_count_distinct_tasks_and_split_causes,
    |fixture| {
        let now = Utc::now();

        // Regressions: three edges on one done task, each naming its site,
        // and a second done task.
        let regressed = done(&fixture, "regressed", &["src/flaky.rs"]);
        done(&fixture, "clean", &["src/flaky.rs"]);
        for index in 0..3 {
            add(
                &fixture,
                &format!("regression {index}"),
                &["src/fix.rs"],
                sited_link(&regressed, &["file:src/flaky.rs"]),
            );
        }

        // Footprint misses: one task widened twice onto the same path,
        // once typed and once in the note-only shape of older history.
        let widened = add(&fixture, "widened", &["src/widened.rs"], vec![]);
        widen(&fixture, &widened, "src/widened.rs", true);
        widen(&fixture, &widened, "src/widened.rs", false);
        add(&fixture, "planned", &["src/widened.rs"], vec![]);

        // Blocked: auth infrastructure, unclassified, and a run failure.
        let auth = add(&fixture, "auth block", &["src/blocked.rs"], vec![]);
        block_on_run_failure(
            &fixture,
            &auth,
            "jrun-auth",
            "provider_unavailable",
            "OAuth token expired",
        );
        let unknown = add(&fixture, "unrecorded block", &["src/blocked.rs"], vec![]);
        transition_and_return(
            &fixture,
            &unknown,
            "claim_failed",
            None,
            TaskStatus::Blocked,
        );
        let failed = add(&fixture, "run block", &["src/blocked.rs"], vec![]);
        block_on_run_failure(&fixture, &failed, "jrun-fail", "-", "tests failed");

        // Rejected: one after a run, one at proposal, one live task with a run.
        let ran = add_with_status(
            &fixture,
            "rejected after run",
            &["src/rejected.rs"],
            vec![],
            TaskStatus::Rejected,
        );
        reject_after_run(&fixture, &ran, now);
        add_with_status(
            &fixture,
            "rejected at proposal",
            &["src/rejected.rs"],
            vec![],
            TaskStatus::Rejected,
        );
        let live = add(&fixture, "live with run", &["src/rejected.rs"], vec![]);
        transition_and_return(&fixture, &live, "started", None, TaskStatus::InProgress);

        // Lock contention: one run retrying twenty times on one path.
        for index in 0..20 {
            lock_denial(
                &fixture,
                index,
                Some("HF-777"),
                Some("jrun-retry"),
                "src/lock.rs",
            );
        }
        reserve(&fixture, "res-lock", "HF-778", &["src/lock.rs"], now);

        // Change coupling: a and b together three times, a alone once, and
        // one sweeping reservation above the large-footprint cap.
        for index in 0..3 {
            reserve(
                &fixture,
                &format!("res-ab-{index}"),
                "HF-800",
                &["src/a.rs", "src/b.rs"],
                now,
            );
        }
        reserve(&fixture, "res-a", "HF-801", &["src/a.rs"], now);
        let sweeping: Vec<String> = (0..13)
            .map(|index| format!("src/sweep{index}.rs"))
            .collect();
        let mut large: Vec<&str> = sweeping.iter().map(String::as_str).collect();
        large.extend(["src/a.rs", "src/b.rs"]);
        reserve(&fixture, "res-large", "HF-802", &large, now);

        let server = fixture.server(false);
        let low = analytics(&server, "window=all&min_support=1");

        assert_eq!(
            rate_of(&low["regressions"], "src/flaky.rs"),
            (1, 2),
            "three edges on one task count once: {}",
            low["regressions"]
        );
        let flaky = row(&low["regressions"], "src/flaky.rs").unwrap();
        assert_eq!(flaky["rate"], 0.5);
        assert_eq!(flaky["regressed_tasks"][0]["task_id"], regressed);
        assert_eq!(
            flaky["regressed_tasks"][0]["regression_tasks"]
                .as_array()
                .unwrap()
                .len(),
            3
        );

        let lock = row(&low["lock_contention"], "src/lock.rs").unwrap();
        assert_eq!(
            lock["contending_tasks"], 1,
            "a retry loop is one contender: {lock}"
        );
        assert_eq!(lock["denials"], 20);
        assert_eq!(lock["reservations"], 1, "reservations are a separate field");
        assert_eq!(rate_of(&low["lock_contention"], "src/lock.rs"), (1, 2));
        assert_eq!(
            (
                low["lock_contention"]["reservations"].clone(),
                low["lock_contention"]["denials"].clone()
            ),
            (json!(6), json!(20)),
            "the window totals count every reservation, large ones included, and every denial: {}",
            low["lock_contention"]
        );
        assert_eq!(low["row_limit"], 25, "the view names the ranking cut-off");

        let miss = row(&low["footprint_misses"], "src/widened.rs").unwrap();
        assert_eq!(rate_of(&low["footprint_misses"], "src/widened.rs"), (1, 2));
        assert_eq!(
            miss["widenings"], 2,
            "both events read, from the typed decision and from the legacy note"
        );

        let pair = low["change_coupling"]["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["paths"] == json!(["src/a.rs", "src/b.rs"]))
            .unwrap_or_else(|| panic!("a/b pair: {}", low["change_coupling"]));
        assert_eq!(
            (pair["numerator"].clone(), pair["denominator"].clone()),
            (json!(3), json!(4))
        );
        assert_eq!(pair["rate"], 0.75);
        assert_eq!(low["change_coupling"]["excluded_large"], 1);
        assert!(
            low["change_coupling"]["rows"]
                .as_array()
                .unwrap()
                .iter()
                .all(|row| !row["paths"][0].as_str().unwrap().contains("sweep")),
            "the large reservation pairs nothing"
        );

        let blocked = row(&low["blocked"], "src/blocked.rs")
            .unwrap_or_else(|| panic!("blocked row: {}", low["blocked"]));
        assert_eq!(
            rate_of(&low["blocked"], "src/blocked.rs"),
            (1, 3),
            "only the run failure counts against the file: {blocked}"
        );
        assert_eq!(blocked["unclassified"], 1);
        let cause = |name: &str| {
            low["blocked"]["causes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["cause"] == name)
                .map(|row| row["tasks"].as_u64().unwrap())
        };
        assert_eq!(cause("auth"), Some(1));
        assert_eq!(cause("unclassified"), Some(1));
        assert_eq!(cause("run_failure"), Some(1));

        assert_eq!(rate_of(&low["rejected"], "src/rejected.rs"), (1, 2));
        assert_eq!(low["rejected"]["excluded_without_run"], 1);

        // The default support threshold of 15 omits every fixture row.
        let default = analytics(&server, "window=all");
        assert_eq!(default["min_support"], 15);
        assert!(row(&default["regressions"], "src/flaky.rs").is_none());
        assert!(default["regressions"]["below_support"].as_u64().unwrap() >= 1);
        let strict = analytics(&server, "window=all&min_support=4");
        assert!(
            strict["change_coupling"]["rows"]
                .as_array()
                .unwrap()
                .is_empty(),
            "pairs reserved together fewer than min_support times are omitted"
        );
    }
);

analytics_test!(codebase_block_causes_require_failure_evidence, |fixture| {
    let work_failures = WORK_FAILURES;
    for (index, error) in work_failures.iter().enumerate() {
        let path = format!("src/work_{index}.rs");
        let task = add(&fixture, error, &[&path], vec![]);
        block_on_run_failure(
            &fixture,
            &task,
            &format!("jrun-work-{index}"),
            "execution_failed",
            error,
        );
    }
    let infra_failures = INFRA_FAILURES;
    let mut expected_causes = std::collections::BTreeMap::new();
    for (index, (cause, code, error)) in infra_failures.iter().enumerate() {
        let path = format!("src/infra_{index}.rs");
        let task = add(&fixture, error, &[&path], vec![]);
        block_on_run_failure(&fixture, &task, &format!("jrun-infra-{index}"), code, error);
        *expected_causes.entry(*cause).or_insert(0_u64) += 1;
    }
    for (title, path, event) in [
        ("manual block", "src/manual.rs", "status_changed"),
        ("unknown block", "src/unknown.rs", "dependency_blocked"),
    ] {
        let task = add(&fixture, title, &[path], vec![]);
        transition_and_return(
            &fixture,
            &task,
            event,
            Some("needs investigation of sandbox authentication module".into()),
            TaskStatus::Blocked,
        );
    }
    let server = fixture.server(false);
    let response = analytics(&server, "window=all&min_support=1");
    let blocked = &response["blocked"];
    for index in 0..work_failures.len() {
        assert_eq!(rate_of(blocked, &format!("src/work_{index}.rs")), (1, 1));
    }
    for index in 0..infra_failures.len() {
        assert!(row(blocked, &format!("src/infra_{index}.rs")).is_none());
    }
    assert_eq!(rate_of(blocked, "src/unknown.rs"), (0, 1));
    assert_eq!(row(blocked, "src/unknown.rs").unwrap()["unclassified"], 1);
    assert!(
        row(blocked, "src/manual.rs").is_none(),
        "a manual block counts against no path: {blocked}"
    );
    expected_causes.insert("run_failure", work_failures.len() as u64);
    expected_causes.insert("unclassified", 1);
    expected_causes.insert("manual", 1);
    let actual_causes = blocked["causes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row["cause"].as_str().unwrap(),
                row["tasks"].as_u64().unwrap(),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(actual_causes, expected_causes);
});

// [ORB-16793] Every recorded cause of a block is classified from its event
// and typed fields: a claimed leaf's settlement classes, run failure
// handoffs naming `run=`, base conflicts (counted only on the paths they
// recorded), review and validation-environment stops, and manual blocks by
// actor kind. Only an entry that records no cause stays unclassified.
analytics_test!(
    codebase_blocked_ranking_classifies_every_recorded_cause,
    |fixture| {
        let block = |path: &str, by: &str, event: &str, note: Option<String>| {
            let task = add(&fixture, path, &[path], vec![]);
            transition_by_and_return(&fixture, &task, by, event, note, TaskStatus::Blocked);
        };
        let claim = |path: &str, evidence: &str, class: &str, reason: &str| {
            block(
                path,
                "hm_follower",
                "claim_failed",
                Some(claim_failed_note(evidence, class, reason)),
            );
        };
        for (path, evidence, class, reason) in [
            (
                "src/claim_candidate.rs",
                "failure",
                "candidate",
                "cargo test failed",
            ),
            (
                "src/claim_recovery.rs",
                "final_recovery",
                "candidate",
                "escalated",
            ),
            (
                "src/claim_auth.rs",
                "provider_unavailable",
                "provider",
                "OAuth token expired",
            ),
            (
                "src/claim_provider.rs",
                "failure",
                "provider",
                "CLI unusable",
            ),
            (
                "src/claim_forge.rs",
                "forge_unavailable",
                "transient",
                "push refused",
            ),
            (
                "src/claim_red.rs",
                "baseline_red",
                "baseline_red",
                "make test red on base",
            ),
            (
                "src/claim_hold.rs",
                "evidence_hold",
                "-",
                "awaiting evidence",
            ),
            (
                "src/claim_cancel.rs",
                "failure",
                "operator_cancel",
                "operator cancelled",
            ),
        ] {
            claim(path, evidence, class, reason);
        }
        for (path, by, event, note) in [
            (
                "src/handoff_failed.rs",
                "system",
                "pr_failure_handoff",
                Some(
                    "failure handoff published PR #7: run=jrun-h, failed_step=validate, original_base=b0, target_base=b0, conflicts=none reported",
                ),
            ),
            (
                "src/review_gate.rs",
                "system",
                "review_gate_escalation",
                Some("review gate escalated: run=jrun-r, decision=reject"),
            ),
            (
                "src/run_named.rs",
                "system",
                "legacy_failure",
                Some("legacy failure: run=jrun-l, failed_step=implement"),
            ),
            (
                "src/validation_env.rs",
                "system",
                "validation_environment_blocked",
                Some("required validation lacked a tool in its environment: run=jrun-v"),
            ),
            (
                "src/sync_base.rs",
                "system",
                "pr_failure_handoff",
                Some(
                    "failure handoff published PR #9: run=jrun-s, failed_step=sync_base, original_base=b0, target_base=b1, conflicts=none reported",
                ),
            ),
            ("src/by_operator.rs", "human:dan", "status_changed", None),
            ("src/by_agent.rs", "claude", "status_changed", None),
            (
                "src/revoked.rs",
                "hm_owner",
                "claim_revoked",
                Some("recover"),
            ),
            ("src/forced.rs", "system", "forced", None),
            ("src/claim_unknown.rs", "hm_follower", "claim_failed", None),
            (
                "src/source_run.rs",
                "system",
                "candidate_resumed",
                Some("resumed: source_run=jrun-old"),
            ),
        ] {
            block(path, by, event, note.map(str::to_string));
        }
        // Base conflicts: one names its conflicting path, one names none.
        let conflicted = add(&fixture, "conflicted", &["src/declared.rs"], vec![]);
        transition_and_return(
            &fixture,
            &conflicted,
            "pr_conflict_blocked",
            Some(
                "failure handoff published PR #8: run=jrun-c, failed_step=sync_base, \
              original_base=b0, target_base=b1, conflicts=src/conflict.rs"
                    .into(),
            ),
            TaskStatus::Blocked,
        );
        let server = fixture.server(false);
        let response = analytics(&server, "window=all&min_support=1");
        let blocked = &response["blocked"];
        for path in [
            "src/claim_candidate.rs",
            "src/claim_recovery.rs",
            "src/handoff_failed.rs",
            "src/review_gate.rs",
            "src/run_named.rs",
            "src/conflict.rs",
        ] {
            assert_eq!(
                rate_of(blocked, path),
                (1, 1),
                "{path} is attributed: {blocked}"
            );
        }
        assert!(
            row(blocked, "src/declared.rs").is_none(),
            "a base conflict counts only on the paths it recorded: {blocked}"
        );
        for path in ["src/claim_unknown.rs", "src/source_run.rs"] {
            assert_eq!(row(blocked, path).unwrap()["unclassified"], 1, "{path}");
        }
        for path in [
            "src/claim_auth.rs",
            "src/claim_provider.rs",
            "src/claim_forge.rs",
            "src/claim_red.rs",
            "src/validation_env.rs",
            "src/claim_hold.rs",
            "src/claim_cancel.rs",
            "src/sync_base.rs",
            "src/by_operator.rs",
            "src/by_agent.rs",
            "src/revoked.rs",
            "src/forced.rs",
        ] {
            assert!(
                row(blocked, path).is_none(),
                "{path} counts against no path: {blocked}"
            );
        }
        let causes = blocked["causes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                let name = match row["actor"].as_str() {
                    Some(actor) => format!("{}:{actor}", row["cause"].as_str().unwrap()),
                    None => row["cause"].as_str().unwrap().to_string(),
                };
                (name, row["tasks"].as_u64().unwrap())
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        let expected = [
            ("auth", 1),
            ("base_conflict", 2),
            ("baseline_red", 1),
            ("environment", 1),
            ("forge_unavailable", 1),
            ("manual:agent", 1),
            ("manual:operator", 2),
            ("manual:system", 1),
            ("provider_unavailable", 1),
            ("run_failure", 5),
            ("unclassified", 2),
        ]
        .into_iter()
        .map(|(name, tasks)| (name.to_string(), tasks))
        .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(causes, expected, "{blocked}");
        assert_eq!(blocked["not_failures"], 2, "{blocked}");
        assert_eq!(
            blocked["unclassified_events"],
            json!([
                { "event": "candidate_resumed", "tasks": 1 },
                { "event": "claim_failed", "tasks": 1 },
            ]),
            "the remaining unclassified blocks name their events"
        );
    }
);

analytics_test!(
    codebase_contention_counts_file_only_runs_and_deduplicates_retries,
    |fixture| {
        let now = Utc::now();
        reserve_request(
            &fixture,
            "res-file",
            &[],
            Some("jrun-holder"),
            &["src/run-lock.rs"],
            now,
        );
        lock_denial(&fixture, 0, None, Some("jrun-waiter"), "src/run-lock.rs");

        for index in 0..3 {
            for run in ["jrun-holder", "jrun-waiter"] {
                reserve_request(
                    &fixture,
                    &format!("res-retry-{run}-{index}"),
                    &[],
                    Some(run),
                    &["src/retry-lock.rs"],
                    now,
                );
            }
            lock_denial(
                &fixture,
                index + 1,
                None,
                Some("jrun-waiter"),
                "src/retry-lock.rs",
            );
            // Task retries across different runs still count by task,
            // even when separate tasks also share a run.
            let run = format!("jrun-task-{index}");
            reserve_request(
                &fixture,
                &format!("res-task-{index}"),
                &["HF-901"],
                Some(&run),
                &["src/task-lock.rs"],
                now,
            );
            lock_denial(
                &fixture,
                index + 4,
                Some("HF-902"),
                Some(&run),
                "src/task-lock.rs",
            );
        }

        // Without tasks or a run, each recorded request counts separately.
        for index in 0..2 {
            reserve_request(
                &fixture,
                &format!("res-unowned-{index}"),
                &[],
                None,
                &["src/unowned-lock.rs"],
                now,
            );
            lock_denial(&fixture, index + 7, None, None, "src/unowned-lock.rs");
        }

        let server = fixture.server(false);
        for support in [1, 2] {
            let body = analytics(&server, &format!("window=all&min_support={support}"));
            for (path, reservations, denials, contenders, requesters) in [
                ("src/run-lock.rs", 1, 1, 1, 2),
                ("src/retry-lock.rs", 6, 3, 1, 2),
                ("src/task-lock.rs", 3, 3, 1, 2),
                ("src/unowned-lock.rs", 2, 2, 2, 4),
            ] {
                let lock = row(&body["lock_contention"], path)
                    .unwrap_or_else(|| panic!("{path} missing at support {support}: {body}"));
                assert_eq!(lock["numerator"], contenders, "{lock}");
                assert_eq!(lock["denominator"], requesters, "{lock}");
                assert_eq!(lock["rate"], 0.5, "{lock}");
                assert_eq!(lock["contending_tasks"], contenders, "{lock}");
                assert_eq!(lock["reservations"], reservations, "{lock}");
                assert_eq!(lock["denials"], denials, "{lock}");
            }
        }
        let strict = analytics(&server, "window=all&min_support=3");
        assert_eq!(strict["lock_contention"]["below_support"], 3);
        assert_eq!(
            strict["lock_contention"]["rows"].as_array().unwrap().len(),
            1
        );
        assert!(row(&strict["lock_contention"], "src/unowned-lock.rs").is_some());
    }
);

analytics_test!(codebase_window_rollup_and_cache_reuse_one_scan, |fixture| {
    let old = done(&fixture, "old", &["crates/alpha/src/old.rs"]);
    append_event(
        &fixture,
        &old,
        Utc::now() - Duration::days(60),
        "status_changed",
        None,
        Some(TaskStatus::Done),
    );
    let new = done(&fixture, "new", &["crates/alpha/tests/new.rs"]);
    for (target, site) in [
        (&old, "file:crates/alpha/src/old.rs"),
        (&new, "file:crates/alpha/tests/new.rs"),
    ] {
        add(
            &fixture,
            "regression",
            &["README.md"],
            sited_link(target, &[site]),
        );
    }

    let server = fixture.counted_task_server();
    let query = |params: &str| analytics(&server, &format!("min_support=1&{params}"));
    let before = server.task_query_trace().len();
    let recent = query("window=30d");
    let cold = server.task_query_trace()[before..].to_vec();
    assert_eq!(
        operations(&cold, "task_history_read"),
        4,
        "the first request reads each task's history once: {cold:?}"
    );
    assert!(row(&recent["regressions"], "crates/alpha/tests/new.rs").is_some());
    assert!(
        row(&recent["regressions"], "crates/alpha/src/old.rs").is_none(),
        "a task done 60 days ago is outside 30d"
    );

    let before = server.task_query_trace().len();
    let quarter = query("window=90d");
    let by_dir = query("window=90d&rollup=dir");
    let by_crate = query("window=all&rollup=crate");
    let warm = server.task_query_trace()[before..].to_vec();
    assert_eq!(
        operations(&warm, "task_history_read"),
        0,
        "requests inside the cache period reuse the scan: {warm:?}"
    );
    assert_eq!(quarter["computed_at"], recent["computed_at"]);
    assert!(row(&quarter["regressions"], "crates/alpha/src/old.rs").is_some());
    assert_eq!(rate_of(&by_dir["regressions"], "crates/alpha/src/"), (1, 1));
    assert_eq!(
        rate_of(&by_dir["regressions"], "crates/alpha/tests/"),
        (1, 1)
    );
    assert_eq!(rate_of(&by_crate["regressions"], "crates/alpha/"), (2, 2));
    assert_eq!(by_crate["rollup"], "crate");

    for query in ["window=1y", "rollup=module"] {
        assert_eq!(
            codebase_get(&server, "analytics", query).status().as_u16(),
            400,
            "{query}"
        );
    }
});

// A scan keeps each task's history-derived facts in the store, so a later
// scan by any server process reads no bundle and diffs no commit for an
// unchanged task; a new history event or a reopen recomputes that task
// alone, and only it; and a dropped table or rows in another fact format rebuild the
// same analytics.
analytics_test!(
    codebase_scan_reuses_stored_task_facts_until_a_task_changes,
    |fixture| {
        let repo = init_repo(&fixture);
        let now = Utc::now();
        seed_pilot_run(
            &fixture,
            "jrun-pilot",
            "opus",
            "claude-opus",
            now - Duration::hours(3),
            now - Duration::hours(2),
        );
        let landed = done(&fixture, "landed", &["src/landed.rs"]);
        pilot_receipt(
            &fixture,
            &landed,
            now - Duration::minutes(150),
            Some("jrun-pilot"),
            &["file:src/landed.rs"],
            &["file:src/landed.rs"],
        );
        land(
            &fixture,
            &repo,
            &landed,
            &["src/landed.rs", "src/partner.rs"],
            now - Duration::hours(1),
        );
        let blocked = add(&fixture, "blocked", &["src/blocked.rs"], vec![]);
        transition_and_return(
            &fixture,
            &blocked,
            "workflow_run_failed",
            Some("run_id=jrun-1 error_code=agent_failed".into()),
            TaskStatus::Blocked,
        );
        let quiet = add(&fixture, "quiet", &["src/quiet.rs"], vec![]);
        let rejected = add_with_status(
            &fixture,
            "rejected",
            &["src/rejected.rs"],
            vec![],
            TaskStatus::Rejected,
        );
        reject_after_run(&fixture, &rejected, now);
        let mut all = vec![
            landed.clone(),
            blocked.clone(),
            quiet.clone(),
            rejected.clone(),
        ];
        all.sort();

        // Each scan runs in a new server, so no in-process cache serves it.
        let scan = |computed: &[String]| {
            let server = fixture.counted_task_server();
            let response = codebase_get(&server, "analytics", "window=all&min_support=1");
            assert_eq!(
                response.headers()["age"],
                "0",
                "a fresh scan reports its age"
            );
            let body = json_ok(response);
            let trace = server.task_query_trace();
            assert_eq!(computed_tasks(&trace), computed, "{trace:?}");
            assert_eq!(
                operations(&trace, "task_history_read"),
                computed.len(),
                "only a recomputed task's history is read: {trace:?}"
            );
            (body, trace)
        };

        let (cold, trace) = scan(&all);
        assert_eq!(operations(&trace, "codebase_landed_diff"), 1, "{trace:?}");
        assert!(row(&cold["blocked"], "src/blocked.rs").is_some(), "{cold}");
        assert!(row(&cold["blocked"], "src/quiet.rs").is_none(), "{cold}");
        assert!(
            row(&cold["rejected"], "src/rejected.rs").is_some(),
            "{cold}"
        );

        let (warm, trace) = scan(&[]);
        for read in ["task_comments_read", "codebase_landed_diff"] {
            assert_eq!(operations(&trace, read), 0, "{read}: {trace:?}");
        }
        assert_eq!(without_computed_at(warm), without_computed_at(cold.clone()));

        transition_and_return(
            &fixture,
            &quiet,
            "workflow_run_failed",
            Some("run_id=jrun-2 error_code=agent_failed".into()),
            TaskStatus::Blocked,
        );
        fixture
            .runtime
            .update_task_as_human(
                &rejected,
                TaskUpdateParams {
                    status: Some(TaskStatus::Backlog),
                    ..Default::default()
                },
                "fixture".into(),
            )
            .unwrap();
        let mut changed_tasks = vec![quiet.clone(), rejected.clone()];
        changed_tasks.sort();
        let (changed, _) = scan(&changed_tasks);
        assert!(
            row(&changed["blocked"], "src/quiet.rs").is_some(),
            "{changed}"
        );
        assert!(
            row(&changed["rejected"], "src/rejected.rs").is_none(),
            "{changed}"
        );

        let store = fixture.runtime.sqlite_store().unwrap();
        let connection = store.connection();
        connection
            .lock()
            .unwrap()
            .execute_batch("DROP TABLE codebase_task_facts")
            .unwrap();
        let (rebuilt, _) = scan(&all);
        assert_eq!(
            without_computed_at(rebuilt),
            without_computed_at(changed.clone())
        );

        let stale = connection
            .lock()
            .unwrap()
            .execute("UPDATE codebase_task_facts SET format_version = 0", [])
            .unwrap();
        assert_eq!(stale, all.len(), "the dropped table was rebuilt");
        let (reformatted, _) = scan(&all);
        assert_eq!(
            without_computed_at(reformatted),
            without_computed_at(changed)
        );
        scan(&[]);
    }
);

analytics_test!(
    codebase_view_of_an_empty_workspace_is_empty_sections,
    |fixture| {
        let server = fixture.server(false);
        let empty = analytics(&server, "");
        assert_eq!(empty["window"], "90d");
        assert_eq!(empty["touched"], "declared_footprint");
        for section in [
            "regressions",
            "lock_contention",
            "footprint_misses",
            "change_coupling",
            "blocked",
            "rejected",
        ] {
            assert_eq!(empty[section]["rows"], json!([]), "{section}: {empty}");
        }
        assert_eq!(empty["lock_contention"]["reservations"], 0);
        assert_eq!(empty["lock_contention"]["denials"], 0);
        // The denial evidence API keeps answering.
        json_ok(server.get(&format!("/api/diagnostics/denials?{WS}")));
    }
);

// The dashboard route and the `orbit.codebase.*` tools read one core
// service: the tool answers the route's rankings and overlap for the same
// selection, and a path's detail names the tasks behind each numerator it
// reports.
analytics_test!(
    codebase_tools_serve_the_dashboard_payload_and_path_detail,
    |fixture| {
        let shared = "src/shared.rs";
        let regressed = done(&fixture, "regressed", &[shared]);
        done(&fixture, "clean", &[shared]);
        let site = "symbol:src/shared.rs#Shared::run:method";
        let regression = add(
            &fixture,
            "regression",
            &["src/fix.rs"],
            sited_link(&regressed, &[site]),
        );
        widen(&fixture, &regression, shared, true);
        let failed = add(&fixture, "run block", &[shared], vec![]);
        block_on_run_failure(&fixture, &failed, "jrun-fail", "-", "tests failed");

        let server = fixture.server(false);
        let mut api = analytics(&server, "window=30d&min_support=1");
        let mut tool = fixture
            .runtime
            .run_tool(
                "orbit.codebase.analytics",
                json!({ "window": "30d", "min_support": 1 }),
            )
            .unwrap();
        assert_eq!(api["cache_ttl_seconds"], 300, "the route caches its scan");
        assert_eq!(tool["cache_ttl_seconds"], 0, "the tool scans per call");
        for payload in [&mut api, &mut tool] {
            let object = payload.as_object_mut().unwrap();
            for scan_time in ["computed_at", "since", "cache_ttl_seconds"] {
                object.remove(scan_time);
            }
        }
        assert_eq!(api, tool, "one service serves the route and the tool");
        assert_eq!(
            api["overlap"]["rows"][0],
            json!({
                "path": shared,
                "rankings": ["regressions", "footprint_misses", "blocked"],
                "top_rate": 0.5,
            }),
            "{api}"
        );

        let detail = fixture
            .runtime
            .run_tool(
                "orbit.codebase.show",
                json!({ "path": shared, "window": "30d", "min_support": 1 }),
            )
            .unwrap();
        assert_eq!(detail["path"], shared);
        let rankings = &detail["rankings"];
        let ids = |ranking: &str| -> Vec<String> {
            rankings[ranking]["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|task| task["task_id"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(ids("regressions"), std::slice::from_ref(&regressed));
        assert_eq!(
            rankings["regressions"]["tasks"][0]["regression_tasks"],
            json!([regression])
        );
        assert_eq!(rankings["regressions"]["tasks"][0]["sites"], json!([site]));
        assert_eq!(
            rankings["regressions"]["sites"],
            json!([{
                "selector": site, "scope": "symbol", "path": shared,
                "symbol": "Shared::run", "kind": "method", "tasks": 1, "links": 1,
                "regressed_tasks": [regressed], "regression_tasks": [regression],
            }]),
            "the detail lists every site under the path with what names it"
        );
        assert_eq!(ids("footprint_misses"), std::slice::from_ref(&regression));
        assert_eq!(ids("blocked"), std::slice::from_ref(&failed));
        for ranking in ["regressions", "footprint_misses", "blocked"] {
            let standing = &rankings[ranking];
            let listed = row(&api[ranking], shared).unwrap();
            assert_eq!(
                (&standing["numerator"], &standing["denominator"]),
                (&listed["numerator"], &listed["denominator"]),
                "{ranking}: the detail reports the ranked counts"
            );
            assert_eq!(
                standing["numerator"].as_u64().unwrap() as usize,
                ids(ranking).len(),
                "{ranking}: one task per numerator count"
            );
            let place = api[ranking]["rows"]
                .as_array()
                .unwrap()
                .iter()
                .position(|listed| listed["path"] == shared)
                .unwrap();
            assert_eq!(standing["rank"], place + 1, "{ranking}: {detail}");
        }
        assert!(rankings["rejected"]["tasks"].as_array().unwrap().is_empty());
        assert_eq!(rankings["lock_contention"], Value::Null);

        // The dashboard's file view reads the same detail over HTTP; a path
        // nothing in the window touched or sited is a 400.
        let mut routed = json_ok(server.get(&format!(
            "/api/codebase/show?{WS}&path={shared}&window=30d&min_support=1"
        )));
        let mut detail = detail.clone();
        for payload in [&mut routed, &mut detail] {
            let object = payload.as_object_mut().unwrap();
            for scan_time in ["computed_at", "since"] {
                object.remove(scan_time);
            }
        }
        assert_eq!(routed, detail, "one service serves the route and the tool");
        let absent = server.get(&format!("/api/codebase/show?{WS}&path=src/absent.rs"));
        assert_eq!(absent.status(), 400);
        let unnamed = server.get(&format!("/api/codebase/show?{WS}"));
        assert_eq!(unnamed.status(), 400, "path is required");

        // A directory key is accepted without its trailing slash.
        let by_dir = fixture
            .runtime
            .run_tool(
                "orbit.codebase.show",
                json!({ "path": "src", "rollup": "dir", "min_support": 1 }),
            )
            .unwrap();
        assert_eq!(by_dir["path"], "src/");
        let unknown = fixture.runtime.run_tool(
            "orbit.codebase.show",
            json!({ "path": "src/absent.rs", "min_support": 1 }),
        );
        assert!(
            matches!(unknown, Err(OrbitError::InvalidInput(_))),
            "{unknown:?}"
        );
    }
);

analytics_test!(
    codebase_regressions_attribute_root_directory_sites_to_known_rollups,
    |fixture| {
        let culprit = done(
            &fixture,
            "root directory culprit",
            &[
                "crates/orbit-core/src/area/a.rs",
                "crates/orbit-core/src/area/b.rs",
            ],
        );
        let screened = fixture
            .runtime
            .add_task_screened(
                TaskAddParams {
                    title: "Fix the root directory culprit".into(),
                    description: "codebase analytics fixture".into(),
                    relations: sited_link(&culprit, &["dir:."]),
                    ..Default::default()
                },
                &TaskWriteScreen::unbound(true),
                None,
                None,
            )
            .unwrap();
        assert_eq!(
            screened.task.relations[0]
                .regression
                .as_ref()
                .unwrap()
                .sites,
            ["dir:."]
        );

        let server = fixture.server(false);
        let by = |rollup: &str| {
            analytics(
                &server,
                &format!("window=all&min_support=1&rollup={rollup}"),
            )
        };

        let files = &by("file")["regressions"];
        assert_eq!(
            files["unsited"],
            json!({ "tasks": 0, "links": 0 }),
            "the admitted root site remains sited: {files}"
        );
        for path in [
            "crates/orbit-core/src/area/a.rs",
            "crates/orbit-core/src/area/b.rs",
        ] {
            assert_eq!(rate_of(files, path), (1, 1), "{files}");
            assert_eq!(
                row(files, path).unwrap()["regressed_tasks"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
        }
        assert_eq!(files["rows"].as_array().unwrap().len(), 2, "{files}");

        let dirs = &by("dir")["regressions"];
        assert_eq!(
            dirs["unsited"],
            json!({ "tasks": 0, "links": 0 }),
            "the root site remains sited at directory rollup: {dirs}"
        );
        assert_eq!(
            rate_of(dirs, "crates/orbit-core/src/area/"),
            (1, 1),
            "the directory row counts the culprit once despite two declared files: {dirs}"
        );
        assert_eq!(dirs["rows"].as_array().unwrap().len(), 1, "{dirs}");

        let crates = &by("crate")["regressions"];
        assert_eq!(
            crates["unsited"],
            json!({ "tasks": 0, "links": 0 }),
            "the root site remains sited at crate rollup: {crates}"
        );
        assert_eq!(rate_of(crates, "crates/orbit-core/"), (1, 1), "{crates}");
        assert_eq!(crates["rows"].as_array().unwrap().len(), 1, "{crates}");
    }
);

// A regressed task counts against the paths its links record as sites, not
// every path it declared: a hub file every task declares takes no blame, a
// site outside the footprint still shows, and links without sites are
// counted as unsited instead of spread over the footprint.
analytics_test!(
    codebase_regressions_count_culprits_at_recorded_sites_not_their_footprint,
    |fixture| {
        let hub = "src/mod.rs";
        let hot = "src/hot.rs";
        let symbol = "symbol:src/hot.rs#Hot::run:function";
        let sited = done(&fixture, "sited twice", &[hub, hot]);
        let first = add(
            &fixture,
            "fix one",
            &["src/fix.rs"],
            sited_link(&sited, &[symbol]),
        );
        let second = add(
            &fixture,
            "fix two",
            &["src/fix.rs"],
            sited_link(&sited, &[symbol, "file:src/hot.rs"]),
        );
        let outside = done(&fixture, "sited outside", &[hub]);
        add(
            &fixture,
            "fix outside",
            &["src/fix.rs"],
            sited_link(&outside, &["file:src/other.rs"]),
        );
        let area = done(
            &fixture,
            "sited directory",
            &[hub, "src/area/a.rs", "src/area/b.rs"],
        );
        add(
            &fixture,
            "fix directory",
            &["src/fix.rs"],
            sited_link(&area, &["dir:src/area"]),
        );
        let unsited = done(&fixture, "unsited", &[hub, hot]);
        add(
            &fixture,
            "fix unsited",
            &["src/fix.rs"],
            regression_from(&unsited),
        );
        done(&fixture, "clean", &[hub, hot]);

        let server = fixture.server(false);
        let by = |query: &str| analytics(&server, &format!("window=30d&min_support=1&{query}"));
        let files = by("rollup=file");
        let section = &files["regressions"];
        assert_eq!(section["attribution"], "sites");
        assert_eq!(
            section["unsited"],
            json!({ "tasks": 1, "links": 1 }),
            "{section}"
        );
        assert!(
            row(section, hub).is_none(),
            "the hub no regression sited takes no blame: {section}"
        );
        assert_eq!(
            rate_of(section, hot),
            (1, 3),
            "two links naming one task count it once"
        );
        let hot_row = row(section, hot).unwrap();
        assert_eq!(hot_row["regressed_tasks"][0]["task_id"], sited.as_str());
        assert_eq!(
            hot_row["regressed_tasks"][0]["sites"],
            json!(["file:src/hot.rs", symbol])
        );
        let sites: Vec<(&str, u64, u64)> = hot_row["sites"]
            .as_array()
            .unwrap()
            .iter()
            .map(|site| {
                (
                    site["selector"].as_str().unwrap(),
                    site["tasks"].as_u64().unwrap(),
                    site["links"].as_u64().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            sites,
            [(symbol, 1, 2), ("file:src/hot.rs", 1, 1)],
            "sites rank by regressed tasks, then links"
        );
        assert_eq!(
            hot_row["sites"][0]["regression_tasks"],
            json!([first, second])
        );
        assert_eq!(
            rate_of(section, "src/other.rs"),
            (1, 1),
            "a culprit sited outside its footprint joins that path's denominator"
        );
        for key in ["src/area/", "src/area/a.rs", "src/area/b.rs"] {
            assert_eq!(
                rate_of(section, key),
                (1, 1),
                "a directory site counts at {key}"
            );
        }

        let dirs = by("rollup=dir");
        assert_eq!(rate_of(&dirs["regressions"], "src/"), (2, 5));
        assert_eq!(rate_of(&dirs["regressions"], "src/area/"), (1, 1));
        let crates = by("rollup=crate");
        assert_eq!(rate_of(&crates["regressions"], "src/"), (3, 5));

        let kept = by("rollup=file&nature=unclassified");
        assert_eq!(
            kept["regressions"]["unsited"],
            json!({ "tasks": 0, "links": 0 }),
            "unsited counts only the links the filter keeps"
        );

        let show = |path: &str| {
            fixture
                .runtime
                .run_tool(
                    "orbit.codebase.show",
                    json!({ "path": path, "window": "30d", "min_support": 1 }),
                )
                .unwrap()
        };
        let detail = show(hot);
        let standing = &detail["rankings"]["regressions"];
        assert_eq!(standing["sites"], hot_row["sites"], "{detail}");
        assert_eq!(standing["unsited"], json!({ "tasks": 1, "links": 1 }));
        let hub_detail = show(hub);
        let hub_standing = &hub_detail["rankings"]["regressions"];
        assert_eq!(
            (&hub_standing["numerator"], &hub_standing["denominator"]),
            (&json!(0), &json!(5))
        );
        assert_eq!(hub_standing["sites"], json!([]));
        assert_eq!(hub_standing["unsited"], json!({ "tasks": 1, "links": 1 }));
    }
);

analytics_test!(
    codebase_regressions_break_down_and_filter_by_link_dimensions,
    |fixture| {
        let repo = init_repo(&fixture);
        let now = Utc::now();
        let area = "src/area.rs";
        let done = |crew: &str, complexity: TaskComplexity| TaskAddParams {
            crew: Some(crew.into()),
            complexity,
            status: Some(TaskStatus::Done),
            ..Default::default()
        };
        let hard = seed_task(
            &fixture,
            "hard",
            &[area],
            done("opus", TaskComplexity::Hard),
        );
        let low = seed_task(&fixture, "low", &[area], done("sol", TaskComplexity::Low));
        seed_task(
            &fixture,
            "clean opus",
            &[area],
            done("opus", TaskComplexity::Low),
        );
        seed_task(
            &fixture,
            "clean sol",
            &[area],
            done("sol", TaskComplexity::Low),
        );

        // A classified review finding whose fix landed in the store
        // with a test and a doc, though it declared the dashboard.
        let reviewed = seed_task(
            &fixture,
            "review finding",
            &["crates/orbit-web/src/view.rs"],
            TaskAddParams {
                relations: classified_link(
                    &hard,
                    Some(RegressionClassification {
                        nature: RegressionNature::BrokeExisting,
                        escape: RegressionEscape::NoTest,
                        severity: DefectSeverity::Medium,
                        severity_source: SeveritySource::Filer,
                        sites: vec![format!("file:{area}")],
                    }),
                ),
                tags: vec!["delivery-code-review".into()],
                crew: Some("sol".into()),
                status: Some(TaskStatus::Done),
                ..Default::default()
            },
        );
        land(
            &fixture,
            &repo,
            &reviewed,
            &[
                "crates/orbit-store/src/rows.rs",
                "crates/orbit-store/tests/rows.rs",
                "docs/rows.md",
            ],
            now,
        );
        // An explicitly unclassified QA finding, read off its footprint.
        let swept = seed_task(
            &fixture,
            "qa finding",
            &["crates/orbit-web/assets/view.js"],
            TaskAddParams {
                relations: classified_link(
                    &hard,
                    Some(RegressionClassification {
                        nature: RegressionNature::Unclassified,
                        escape: RegressionEscape::Unclassified,
                        severity: DefectSeverity::Medium,
                        severity_source: SeveritySource::Filer,
                        sites: vec![format!("symbol:{area}#Area::draw:function")],
                    }),
                ),
                tags: vec!["qa-sweep:dashboard".into()],
                ..Default::default()
            },
        );
        // An operator's link stored without a classification, so without
        // sites: it is attributed to no path.
        seed_task(
            &fixture,
            "operator finding",
            &["docs/guide.md"],
            TaskAddParams {
                relations: classified_link(&low, None),
                origin: Some(TaskOrigin::OperatorRequest),
                ..Default::default()
            },
        );

        // Detail and analytics must classify the same landed fix even
        // when its declared footprint names another layer. Start with
        // Show so its cold scan cannot rely on analytics warming facts.
        let trace_path = fixture.path("detail-query.jsonl");
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_span_events(tracing_subscriber::fmt::format::FmtSpan::NEW)
            .with_span_list(false)
            .with_max_level(tracing::Level::TRACE)
            .with_writer(std::sync::Mutex::new(
                fs::File::create(&trace_path).unwrap(),
            ))
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);
        for layer in [None, Some("store_migration")] {
            let mut query = json!({ "path": area, "window": "30d", "min_support": 1 });
            if let Some(layer) = layer {
                query["layer"] = json!(layer);
            }
            let detail = fixture
                .runtime
                .run_tool("orbit.codebase.show", query.clone())
                .unwrap();
            let trace: Vec<Value> = fs::read_to_string(&trace_path)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .filter(|event| {
                    event["fields"]["message"] == "new"
                        || event["fields"]["codebase_delivery_observation"] == true
                })
                .collect();
            assert_eq!(
                operations(&trace, "codebase_landed_diff"),
                1,
                "Show diffs the receipt once; later scans reuse stored paths: {trace:?}"
            );
            assert_eq!(
                operations(&trace, "task_history_read"),
                7,
                "Show reads each task history once; later scans reuse stored facts: {trace:?}"
            );
            assert!(
                trace
                    .iter()
                    .all(|event| event["fields"]["codebase_delivery_observation"] != true),
                "history-only Show must not observe delivery for receipt-less done tasks: {trace:?}"
            );
            query.as_object_mut().unwrap().remove("path");
            let analytics = fixture
                .runtime
                .run_tool("orbit.codebase.analytics", query)
                .unwrap();
            let standing = &detail["rankings"]["regressions"];
            let ranked = row(&analytics["regressions"], area).unwrap();
            for count in ["numerator", "denominator"] {
                assert_eq!(standing[count], ranked[count], "{layer:?}: {count}");
            }
            let detail_links: Vec<Value> = standing["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|task| task["regressions"].as_array().unwrap().clone())
                .collect();
            let analytics_links: Vec<Value> = ranked["regressed_tasks"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|task| task["regressions"].as_array().unwrap().clone())
                .collect();
            assert_eq!(detail_links, analytics_links, "{layer:?}: matching links");
            let fix = detail_links
                .iter()
                .find(|link| link["task_id"] == reviewed)
                .unwrap();
            assert_eq!(fix["layer"], "store_migration");
            assert_eq!(fix["layer_source"], "landed");
            // Only the sited task counts against the path; the operator's
            // unsited link is kept by the filter but attributed nowhere.
            assert_eq!(detail_links.len(), if layer.is_some() { 1 } else { 2 });
            assert_eq!(
                analytics["regressions"]["matching_links"],
                if layer.is_some() { 1 } else { 3 }
            );
            assert_eq!(standing["numerator"], 1);
        }
        drop(guard);

        let server = fixture.server(false);
        let filtered_analytics =
            |filter: &str| analytics(&server, &format!("window=30d&min_support=1{filter}"));
        let payload = filtered_analytics("");
        let section = &payload["regressions"];
        assert_eq!(
            (
                &section["links"],
                &section["matching_links"],
                &section["regressed"]
            ),
            (&json!(3), &json!(3), &json!(2)),
            "{section}"
        );
        let counts = |pairs: &[(&str, u64)]| -> Vec<(String, u64)> {
            pairs
                .iter()
                .map(|(value, count)| (value.to_string(), *count))
                .collect()
        };
        assert_eq!(
            breakdown(section, "nature"),
            counts(&[("broke_existing", 1), ("unclassified", 1), ("absent", 1)]),
            "a link without a payload is absent, not unclassified"
        );
        assert_eq!(
            breakdown(section, "escape"),
            counts(&[("no_test", 1), ("unclassified", 1), ("absent", 1)])
        );
        assert_eq!(
            breakdown(section, "layer"),
            counts(&[("docs_skills", 1), ("dashboard", 1), ("store_migration", 1)]),
            "landed paths beat the footprint; tests and docs only support a fix"
        );
        assert_eq!(
            breakdown(section, "detected_by"),
            counts(&[("delivery_review", 1), ("qa_sweep", 1), ("operator", 1)])
        );
        let links: Vec<Value> = section["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["path"] == area)
            .unwrap()["regressed_tasks"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|task| task["regressions"].as_array().unwrap().clone())
            .collect();
        let sources: Vec<(&str, &str)> = links
            .iter()
            .map(|link| {
                (
                    link["task_id"].as_str().unwrap(),
                    link["layer_source"].as_str().unwrap(),
                )
            })
            .collect();
        for (task, source) in [(&reviewed, "landed"), (&swept, "footprint")] {
            assert!(sources.contains(&(task.as_str(), source)), "{sources:?}");
        }
        assert_eq!(
            section["cross_tabs"]["nature_escape"],
            json!({
                "row": "nature", "column": "escape",
                "rows": ["broke_existing", "unclassified", "absent"],
                "columns": ["no_test", "unclassified", "absent"],
                "counts": [[1, 0, 0], [0, 1, 0], [0, 0, 1]],
            })
        );
        assert_eq!(
            section["cross_tabs"]["escape_layer"]["columns"],
            json!(["docs_skills", "dashboard", "store_migration"])
        );
        assert_eq!(
            slice_rows(section, "crew"),
            vec![("opus".into(), 1, 2), ("sol".into(), 1, 3)],
            "a crew's rate is its done tasks with a regression"
        );
        let tiers = slice_rows(section, "complexity");
        assert_eq!(tiers[0], ("hard".into(), 1, 1), "{tiers:?}");
        assert!(tiers.contains(&("low".into(), 1, 3)), "{tiers:?}");
        assert_eq!(rate_of(section, area), (1, 4));
        assert_eq!(section["unsited"], json!({ "tasks": 1, "links": 1 }));

        // A filter narrows the path rate, the rates and the cross-tabs;
        // each breakdown still counts its own dimension's other values.
        let payload = filtered_analytics("&nature=broke_existing");
        let section = &payload["regressions"];
        assert_eq!(
            payload["regression_filter"],
            json!({"nature": "broke_existing", "escape": null, "layer": null, "detected_by": null})
        );
        assert_eq!(rate_of(section, area), (1, 4));
        assert_eq!(
            (&section["matching_links"], &section["regressed"]),
            (&json!(1), &json!(1))
        );
        assert_eq!(breakdown(section, "nature").len(), 3, "{section}");
        assert_eq!(
            breakdown(section, "detected_by"),
            counts(&[("delivery_review", 1)])
        );
        assert!(slice_rows(section, "crew").contains(&("sol".into(), 0, 3)));
        assert_eq!(
            section["cross_tabs"]["nature_escape"]["counts"],
            json!([[1]])
        );

        // Under a filter that keeps only the unsited link, the path counts
        // nothing and the detail reports that link's task as unsited.
        let detail = fixture
        .runtime
        .run_tool(
            "orbit.codebase.show",
            json!({ "path": area, "window": "30d", "min_support": 1, "detected_by": "operator" }),
        )
        .unwrap();
        assert_eq!(detail["regression_filter"]["detected_by"], "operator");
        let standing = &detail["rankings"]["regressions"];
        assert_eq!(standing["numerator"], 0, "{detail}");
        assert_eq!(standing["tasks"], json!([]), "{detail}");
        assert_eq!(standing["sites"], json!([]), "{detail}");
        assert_eq!(standing["unsited"], json!({ "tasks": 1, "links": 1 }));

        let bad = codebase_get(&server, "analytics", "layer=frontend");
        assert_eq!(bad.status(), 400, "an unknown layer is refused");
        let bad = fixture
            .runtime
            .run_tool("orbit.codebase.analytics", json!({ "escape": "missed" }));
        assert!(matches!(bad, Err(OrbitError::InvalidInput(_))), "{bad:?}");
    }
);

// Warm latency of the cached view, measured on a copy of a real store: one
// cold scan, then requests across every window, rollup and a lowered support
// inside the cache period. Never point it at a live store. The measurements
// are written to `<root>/codebase-perf.json`.
#[test]
#[ignore = "perf probe over a copied store; set ORBIT_CODEBASE_PERF_ROOT and ORBIT_CODEBASE_PERF_WORKSPACE"]
fn codebase_analytics_warm_p95_on_a_store_copy() {
    let root = std::path::PathBuf::from(
        std::env::var_os("ORBIT_CODEBASE_PERF_ROOT")
            .expect("ORBIT_CODEBASE_PERF_ROOT names a store copy laid out as <root>/global"),
    );
    let workspace = std::env::var("ORBIT_CODEBASE_PERF_WORKSPACE")
        .expect("ORBIT_CODEBASE_PERF_WORKSPACE names the copied workspace id");
    let server = super::support::spawn_server(
        &root,
        false,
        false,
        false,
        &root.join("process.log"),
        None,
        &[],
    );
    let get = |query: &str| {
        let started = std::time::Instant::now();
        let body = json_ok(server.get(&format!(
            "/api/codebase/analytics?workspace={workspace}&{query}"
        )));
        (started.elapsed(), body)
    };
    let (cold, first) = get("window=90d");
    let mut queries = Vec::new();
    for window in ["7d", "30d", "90d", "all"] {
        for rollup in ["file", "dir", "crate"] {
            queries.push(format!("window={window}&rollup={rollup}"));
        }
        queries.push(format!("window={window}&min_support=3"));
    }
    let mut warm = Vec::new();
    for _ in 0..5 {
        for query in &queries {
            let (elapsed, body) = get(query);
            assert_eq!(
                body["computed_at"], first["computed_at"],
                "a request inside the cache period reused the scan"
            );
            warm.push(elapsed);
        }
    }
    warm.sort();
    let p95 = warm[(warm.len() * 95).div_ceil(100) - 1];
    let report = json!({
        "tasks_scanned": first["tasks_scanned"],
        "cold_ms": cold.as_millis(),
        "warm_requests": warm.len(),
        "warm_p50_ms": warm[warm.len() / 2].as_secs_f64() * 1000.0,
        "warm_p95_ms": p95.as_secs_f64() * 1000.0,
        "warm_max_ms": warm.last().map(|max| max.as_secs_f64() * 1000.0),
    });
    fs::write(root.join("codebase-perf.json"), report.to_string()).unwrap();
    assert!(
        p95 < std::time::Duration::from_millis(500),
        "warm p95 over the 500 ms budget: {report}"
    );
}

// Landed diffs, including Markdown without a reservation, drive related
// files; declared-only and bulk history cannot inflate shared support.
analytics_test!(
    codebase_related_uses_landed_diffs_and_filters_support_and_window,
    |fixture| {
        let repo = init_repo(&fixture);
        let now = Utc::now();
        let seed = |title: &str, files: &[&str], declared: &[&str], at: DateTime<Utc>| {
            let task = done(&fixture, title, declared);
            land(&fixture, &repo, &task, files, at);
            task
        };
        let first = seed("first", &["src/a.rs", "docs/b.md"], &["src/a.rs"], now);
        let second = seed(
            "second",
            &["src/a.rs", "docs/b.md", "src/c.rs"],
            &["src/a.rs", "docs/b.md", "src/c.rs"],
            now,
        );
        for (title, files, declared, at) in [
            ("partner only", &["docs/b.md"][..], &["docs/b.md"][..], now),
            (
                "undeclared partner only",
                &["docs/b.md"][..],
                &["src/a.rs"][..],
                now,
            ),
            ("path only", &["src/a.rs"][..], &["src/a.rs"][..], now),
            (
                "old",
                &["src/a.rs", "src/old.rs"][..],
                &["src/a.rs"][..],
                now - Duration::days(40),
            ),
        ] {
            seed(title, files, declared, at);
        }
        let mut bulk: Vec<_> = (0..19).map(|n| format!("bulk/{n}")).collect();
        bulk.extend(["src/a.rs".into(), "docs/b.md".into()]);
        seed(
            "bulk",
            &bulk.iter().map(String::as_str).collect::<Vec<_>>(),
            &["src/a.rs"],
            now,
        );
        reserve(
            &fixture,
            "related-both",
            &first,
            &["src/a.rs", "src/c.rs"],
            now,
        );
        reserve(&fixture, "related-alone", &second, &["src/a.rs"], now);
        let server = fixture.server(false);
        let payload = related(&server, "path=src/a.rs&window=30d&min_support=1&top=1");
        assert_eq!(payload["rows"].as_array().unwrap().len(), 1, "{payload}");
        let row = &payload["rows"][0];
        assert_eq!(row["path"], "docs/b.md");
        assert_eq!(
            row["support"],
            json!({"both":2,"path":3,"partner":4,"either":5})
        );
        assert_eq!(row["jaccard"], 0.4);
        assert!((row["conditional_rate"].as_f64().unwrap() - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(row["footprint_miss_count"], 2);
        assert!(
            row["footprint_miss_tasks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|task| task["task_id"] == first)
        );
        assert!(
            row["reservation_coupling"].is_null(),
            "Markdown needs no reservation to co-change"
        );
        assert_eq!(payload["excluded_large"], 1);
        assert_eq!(
            payload["unavailable_landed_tasks"], 0,
            "an old landing receipt is outside the window even when the task was updated recently"
        );
        let all = related(&server, "path=src/a.rs&window=all&min_support=1");
        assert_eq!(all["path_support"], 4);
        let c = all["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["path"] == "src/c.rs")
            .unwrap();
        assert_eq!(c["reservation_coupling"]["rate"], 0.5);
        let filtered = related(&server, "path=src/a.rs&window=30d&min_support=3");
        assert!(filtered["rows"].as_array().unwrap().is_empty());
        let unknown = related(&server, "path=unknown&min_support=1");
        assert!(unknown["rows"].as_array().unwrap().is_empty());
        for query in ["top=0", "min_support=0", "window=1y", "path=../outside"] {
            assert_eq!(
                server
                    .get(&format!("/api/codebase/related?{WS}&path=src/a.rs&{query}"))
                    .status()
                    .as_u16(),
                400
            );
        }
        let analytics = analytics(&server, "");
        assert!(
            analytics["known_paths"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p == "docs/b.md")
        );
    }
);

// The owner's PR completion and a settlement from a PR merged outside Orbit
// are landing evidence in the one-file view, as an owner landing is; a
// no-diff task without a landing misses nothing; a release cut of up to 20
// paths counts while the coupling ranking keeps its 12-path cap; empty rows
// say why; and facts stored in the previous format are recomputed.
analytics_test!(
    codebase_related_reads_completion_and_settlement_landings_and_explains_empty_rows,
    |fixture| {
        let repo = init_repo(&fixture);
        let now = Utc::now();
        let done = |title: &str, tags: &[&str]| {
            seed_task(
                &fixture,
                title,
                &[],
                TaskAddParams {
                    tags: tags.iter().map(|tag| tag.to_string()).collect(),
                    status: Some(TaskStatus::Done),
                    ..Default::default()
                },
            )
        };
        let completed = done("completed by its pull request", &[]);
        let completed_commit = commit_files(&repo, &completed, &["CHANGELOG.md", "Cargo.toml"]);
        note_pr_completion(
            &fixture,
            &completed,
            now,
            "status_changed",
            format!(
                "completed by run jrun-1; delivered by pull request #41 merged as {completed_commit}"
            ),
        );
        let settled = done("settled from its pull request", &[]);
        let settled_commit = commit_files(
            &repo,
            &settled,
            &["CHANGELOG.md", "Cargo.toml", "server.json"],
        );
        note_pr_completion(
            &fixture,
            &settled,
            now,
            EXTERNAL_MERGE_SETTLED_EVENT,
            format!(
                "settled from pull request #42, merged into agent-main as {settled_commit} outside Orbit"
            ),
        );
        let abbreviated = done("abbreviated commit", &[]);
        let short = commit_files(&repo, &abbreviated, &["CHANGELOG.md", "Cargo.toml"]);
        note_pr_completion(
            &fixture,
            &abbreviated,
            now,
            "status_changed",
            format!(
                "completed; delivered by pull request #43 merged as {}",
                &short[..12]
            ),
        );
        done("finished by hand", &[]);
        done("survey", &["no-diff-expected"]);
        let mut release: Vec<String> = (0..12).map(|n| format!("plugin/{n}.json")).collect();
        release.extend(["CHANGELOG.md", "Cargo.toml", "server.json"].map(String::from));
        let release: Vec<&str> = release.iter().map(String::as_str).collect();
        let cut = done("release cut", &[]);
        land(&fixture, &repo, &cut, &release, now);
        let mut bulk: Vec<String> = (0..20).map(|n| format!("bulk/{n}")).collect();
        bulk.push("CHANGELOG.md".into());
        let bulk: Vec<&str> = bulk.iter().map(String::as_str).collect();
        let sweep = done("bulk sweep", &[]);
        land(&fixture, &repo, &sweep, &bulk, now);
        for (title, files) in [
            ("solo one", &["solo.rs"][..]),
            ("solo two", &["solo.rs"]),
            ("pair x", &["pair.rs", "x.rs"]),
            ("pair y", &["pair.rs", "y.rs"]),
        ] {
            let task = done(title, &[]);
            land(&fixture, &repo, &task, files, now);
        }
        reserve(&fixture, "release-reservation", &cut, &release, now);
        let related = |query: &str| related(&fixture.server(false), &format!("window=30d&{query}"));

        let payload = related("path=CHANGELOG.md&min_support=1");
        assert_eq!(payload["path_support"], 3, "{payload}");
        assert_eq!(payload["excluded_large"], 1, "{payload}");
        assert_eq!(payload["large_footprint_cap"], 20);
        assert_eq!(
            payload["unavailable_landed_tasks"], 2,
            "the abbreviated commit and the hand-finished task lack evidence; the no-diff survey misses nothing: {payload}"
        );
        assert!(payload["empty_reason"].is_null(), "{payload}");
        let partner = payload["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["path"] == "Cargo.toml")
            .unwrap_or_else(|| panic!("Cargo.toml partner: {payload}"));
        assert_eq!(partner["support"]["both"], 3, "{partner}");
        let commits: Vec<(&str, &str)> = partner["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|task| {
                (
                    task["task_id"].as_str().unwrap(),
                    task["commit"].as_str().unwrap(),
                )
            })
            .collect();
        for (task, commit) in [
            (completed.as_str(), completed_commit.as_str()),
            (settled.as_str(), settled_commit.as_str()),
        ] {
            assert!(commits.contains(&(task, commit)), "{task}: {commits:?}");
        }
        assert_eq!(
            partner["reservation_coupling"]["numerator"], 1,
            "a 15-file reservation counts in the one-file view: {partner}"
        );
        let analytics = analytics(&fixture.server(false), "window=30d&min_support=1");
        let coupling = &analytics["change_coupling"];
        assert_eq!(coupling["large_footprint_cap"], 12, "{coupling}");
        assert_eq!(coupling["excluded_large"], 1, "{coupling}");

        for (query, expected) in [
            (
                "path=CHANGELOG.md&min_support=15",
                json!({"kind": "path_below_minimum", "path_support": 3, "min_support": 15}),
            ),
            (
                "path=solo.rs&min_support=2",
                json!({"kind": "no_partner", "path_support": 2}),
            ),
            (
                "path=pair.rs&min_support=2",
                json!({"kind": "partners_below_minimum", "min_support": 2, "partners": 2, "max_shared": 1}),
            ),
            (
                "path=unknown.md&min_support=1",
                json!({"kind": "no_landed_change"}),
            ),
        ] {
            assert_eq!(related(query)["empty_reason"], expected, "{query}");
        }

        // A row stored by the previous format, which read only owner
        // landings, is recomputed rather than reused.
        let store = fixture.runtime.sqlite_store().unwrap();
        let stale = store
        .connection()
        .lock()
        .unwrap()
        .execute(
            "UPDATE codebase_task_facts SET format_version = 1,
               facts_json = json_set(facts_json, '$.receipt', json('null'), '$.landed_paths', json('null'))
             WHERE task_id = ?1",
            [&completed],
        )
        .unwrap();
        assert_eq!(stale, 1, "the scan stored the completed task's facts");
        let recomputed = related("path=CHANGELOG.md&min_support=1");
        assert_eq!(recomputed["path_support"], 3, "{recomputed}");
        assert_eq!(recomputed["unavailable_landed_tasks"], 2, "{recomputed}");
    }
);

analytics_test!(
    lock_contention_survives_audit_gc_and_upgrade_backfill,
    |fixture| {
        let at = Utc::now() - Duration::days(75);
        reserve(&fixture, "old-reservation", "HF-1", &["src/shared.rs"], at);
        super::support::rewind_retention_analytics(&fixture);
        // These old rows predate the upgrade; unscoped and foreign rows must
        // not become local evidence merely because they name the same path.
        for (index, workspace) in [
            (0, Some("ws_http_fixture")),
            (1, None),
            (2, Some("foreign")),
        ] {
            lock_denial_in_workspace(
                &fixture,
                index,
                Some("HF-2"),
                None,
                "src/shared.rs",
                workspace,
            );
        }
        fixture
            .runtime
            .sqlite_store()
            .unwrap()
            .with_transaction(|tx| {
                tx.connection()
                    .execute("UPDATE audit_events SET timestamp = ?1", [at.to_rfc3339()])
                    .map_err(|error| OrbitError::Store(error.to_string()))?;
                Ok(())
            })
            .unwrap();
        super::support::upgrade_retention_analytics(&fixture);
        super::support::upgrade_retention_analytics(&fixture); // backfill runs once
        // A post-upgrade attempt exercises the ongoing durable write, too.
        lock_denial(&fixture, 3, Some("HF-3"), None, "src/shared.rs");
        let read = |window: &str| {
            // A fresh server avoids the dashboard facts cache masking a prune.
            analytics(
                &fixture.server(false),
                &format!("window={window}&min_support=1"),
            )["lock_contention"]
                .clone()
        };
        let before: Vec<_> = ["90d", "all"].map(read).into();
        for section in &before {
            let lock = row(section, "src/shared.rs").unwrap();
            assert_eq!(
                lock["denials"], 2,
                "backfill is idempotent and captures new denials"
            );
            assert_eq!(lock["reservations"], 1);
            assert_eq!(lock["numerator"], 2);
            assert_eq!(lock["denominator"], 3);
        }
        let gc = fixture.runtime.gc_audit(true, Some(60)).unwrap();
        assert_eq!(gc.tables[0].rows_removed, 3);
        for (window, expected) in ["90d", "all"].into_iter().zip(before) {
            let after = read(window);
            let (after_rows, expected_rows) = (
                row(&after, "src/shared.rs").unwrap(),
                row(&expected, "src/shared.rs").unwrap(),
            );
            assert_eq!(after_rows["wait"]["episodes"], 2, "{after}");
            assert_eq!(after_rows["wait"]["waiting"], 2, "{after}");
            assert!(
                after_rows["wait"]["total_ms"].as_i64()
                    >= expected_rows["wait"]["total_ms"].as_i64(),
                "an open wait still measures from its pruned denial: {after}"
            );
            assert_eq!(
                without_open_wait_durations(after),
                without_open_wait_durations(expected),
                "denial counts and rates survive audit GC"
            );
        }
    }
);

// [ORB-18690] Task list rows and the detail serve the same riskiness object
// from the regression facts; a task with no footprint or thin history gets
// an explicit no-estimate state rather than a low probability.
// [ORB-19361] Estimator 2 fits risk on size alone: declared paths before a
// diff exists, lines changed once a landed diff resolves, each with its own
// maximum-likelihood fit, and names no file or crew.
analytics_test!(
    task_riskiness_fits_size_by_basis_and_names_no_estimate_states,
    |fixture| {
        let wide = ["src/b.rs", "src/c.rs", "src/d.rs", "src/e.rs"];
        let mut small = Vec::new();
        let mut large = Vec::new();
        for index in 0..6 {
            small.push(done(&fixture, &format!("small {index}"), &["src/a.rs"]));
            large.push(done(&fixture, &format!("large {index}"), &wide));
        }
        for target in [&small[0], &large[0], &large[1], &large[2]] {
            add(
                &fixture,
                &format!("fix {target}"),
                &["src/fix.rs"],
                regression_from(target),
            );
        }
        // Every done task but small[5] has a landed row.
        land_rows(&fixture, &small[..5], 0, 10);
        land_rows(&fixture, &large, 10, 100);
        let backlog_wide = add(&fixture, "touches four files", &wide, vec![]);
        let unfootprinted = add(&fixture, "no footprint", &[], vec![]);

        // Declared basis: 12 done tasks, sizes 1 (1 of 6 regressed) and 4
        // (3 of 6). Two sizes fit each group's rate exactly.
        let server = fixture.server(false);
        let risky = task_riskiness(&server, &backlog_wide);
        assert_eq!(risky["estimator_version"], 3);
        assert_eq!(risky["state"], "estimated", "{risky}");
        assert_eq!(risky["basis"], "declared_files");
        assert_eq!(risky["basis_fallback"], Value::Null);
        assert_eq!(risky["size"], 4);
        assert_risk_probability(&risky, 0.5);
        assert_eq!(risky["size_band"]["tasks"], 6);
        assert_eq!(risky["size_band"]["regressed"], 3);
        assert_eq!(risky["fit"]["observations"], 12);
        assert_eq!(risky["fit"]["regressed"], 4);
        assert_eq!(risky["support_state"], "full");
        assert_eq!(risky["workspace"]["tasks"], 12);
        assert_eq!(risky["workspace"]["regressed"], 4);
        // Retired attribution stays readable, and empty, for old readers.
        assert_eq!(risky["driving_files"], json!([]));
        assert_eq!(risky["adjustment"], Value::Null);
        assert_eq!(risky["crew"], Value::Null);

        // Diff basis: the 11 landed rows, 10 lines (1 of 5 regressed) and
        // 100 lines (3 of 6).
        let landed = task_riskiness(&server, &large[0]);
        assert_eq!(landed["basis"], "diff_lines", "{landed}");
        assert_eq!(landed["size"], 100);
        assert_risk_probability(&landed, 0.5);
        assert_eq!(landed["fit"]["observations"], 11);
        assert_eq!(landed["size_band"]["tasks"], 6);
        let unresolved = task_riskiness(&server, &small[5]);
        assert_eq!(unresolved["basis"], "declared_files", "{unresolved}");
        assert_eq!(unresolved["basis_fallback"], "no_resolvable_diff");
        assert_eq!(unresolved["size"], 1);
        assert_risk_probability(&unresolved, 1.0 / 6.0);
        assert_eq!(unresolved["size_band"]["tasks"], 12);
        assert_eq!(unresolved["size_band"]["regressed"], 4);

        let none = task_riskiness(&server, &unfootprinted);
        assert_eq!(none["state"], "no_footprint");
        assert_eq!(none["probability"], Value::Null);
        assert_eq!(none["basis"], Value::Null);

        let detail = json_ok(server.get(&format!("/api/tasks/{backlog_wide}?{WS}")));
        assert_eq!(
            detail["riskiness"], risky,
            "list and detail share one estimate"
        );
        drop(server);

        // min_support is the history a fit needs: above both populations,
        // neither basis fits and no number is shown.
        let config = fixture.work.join("config.toml");
        let mut text = fs::read_to_string(&config).unwrap_or_default();
        text.push_str("\n[workflow.regression_risk]\nmin_support = 13\n");
        fs::write(&config, text).unwrap();
        let server = fixture.server(false);
        for id in [&backlog_wide, &large[0]] {
            let thin = task_riskiness(&server, id);
            assert_eq!(thin["state"], "insufficient_history", "{thin}");
            assert_eq!(thin["probability"], Value::Null);
            assert_eq!(thin["basis"], "declared_files");
            assert_eq!(thin["size_band"], Value::Null);
        }
        assert_eq!(
            task_riskiness(&server, &large[0])["basis_fallback"],
            "diff_history_insufficient"
        );
    }
);

// [ORB-19376] Nearby sizes need a steep but finite slope: ten done tasks of
// 10 declared files (1 regressed) and ten of 11 (5 regressed) estimate 0.1
// and 0.5 instead of reporting insufficient_history. The diff-lines basis
// fits the same rates through landed rows of 10 and 11 lines.
analytics_test!(
    task_riskiness_keeps_finite_fit_for_nearby_sizes,
    |fixture| {
        let mut small = Vec::new();
        let mut large = Vec::new();
        for index in 0..10 {
            small.push(done(&fixture, &format!("nearby small {index}"), &NEAR_TEN));
            large.push(done(
                &fixture,
                &format!("nearby large {index}"),
                &NEAR_ELEVEN,
            ));
        }
        add(
            &fixture,
            "fix nearby small",
            &["src/near_fix.rs"],
            regression_from(&small[0]),
        );
        for (index, target) in large.iter().take(5).enumerate() {
            add(
                &fixture,
                &format!("fix nearby large {index}"),
                &["src/near_fix.rs"],
                regression_from(target),
            );
        }
        // The declared fit sees sizes 10 and 11; the diff fit sees landed
        // rows of 10 and 11 lines on the same tasks.
        land_rows(&fixture, &small, 0, 10);
        land_rows(&fixture, &large, 100, 11);
        let probe_ten = add(&fixture, "probe ten files", &NEAR_TEN, vec![]);
        let probe_eleven = add(&fixture, "probe eleven files", &NEAR_ELEVEN, vec![]);

        let server = fixture.server(false);
        let ten = task_riskiness(&server, &probe_ten);
        assert_eq!(ten["state"], "estimated", "{ten}");
        assert_eq!(ten["estimator_version"], 3);
        assert_eq!(ten["basis"], "declared_files");
        assert_eq!(ten["size"], 10);
        assert_risk_probability(&ten, 0.1);
        assert_eq!(ten["size_band"]["tasks"], 20);
        assert_eq!(ten["size_band"]["regressed"], 6);
        assert_eq!(ten["fit"]["observations"], 20);
        assert_eq!(ten["fit"]["regressed"], 6);
        // The reported fit stays in ln(1 + size) units: the saturated
        // two-size model reproduces both group rates exactly.
        let slope = ten["fit"]["slope"].as_f64().unwrap();
        assert!((slope - 21.650667670220933).abs() < 1e-6, "{ten}");
        let intercept = ten["fit"]["intercept"].as_f64().unwrap();
        assert!((intercept + 54.16640098666374).abs() < 1e-6, "{ten}");

        let eleven = task_riskiness(&server, &probe_eleven);
        assert_eq!(eleven["state"], "estimated", "{eleven}");
        assert_eq!(eleven["basis"], "declared_files");
        assert_eq!(eleven["size"], 11);
        assert_risk_probability(&eleven, 0.5);
        assert_eq!(eleven["size_band"]["tasks"], 10);
        assert_eq!(eleven["size_band"]["regressed"], 5);

        let landed = task_riskiness(&server, &small[1]);
        assert_eq!(landed["basis"], "diff_lines", "{landed}");
        assert_eq!(landed["size"], 10);
        assert_risk_probability(&landed, 0.1);
        assert_eq!(landed["fit"]["observations"], 20);
        assert_eq!(landed["fit"]["regressed"], 6);
        assert_eq!(landed["size_band"]["tasks"], 20);
        assert_eq!(landed["size_band"]["regressed"], 6);
    }
);

// [ORB-19376] Genuinely separated sizes still report no estimate: ten clean
// tasks of 10 files and ten regressed tasks of 11 files have no finite fit.
analytics_test!(
    task_riskiness_reports_no_estimate_when_sizes_separate_outcomes,
    |fixture| {
        let mut unregressed = Vec::new();
        let mut large = Vec::new();
        for index in 0..10 {
            unregressed.push(done(&fixture, &format!("split small {index}"), &NEAR_TEN));
            large.push(done(
                &fixture,
                &format!("split large {index}"),
                &NEAR_ELEVEN,
            ));
        }
        assert_eq!(unregressed.len(), 10);
        for (index, target) in large.iter().enumerate() {
            add(
                &fixture,
                &format!("fix split large {index}"),
                &["src/near_fix.rs"],
                regression_from(target),
            );
        }
        let probe = add(&fixture, "probe ten files", &NEAR_TEN, vec![]);

        let server = fixture.server(false);
        let risky = task_riskiness(&server, &probe);
        assert_eq!(risky["estimator_version"], 3);
        assert_eq!(risky["state"], "insufficient_history", "{risky}");
        assert_eq!(risky["probability"], Value::Null);
        assert_eq!(risky["basis"], "declared_files");
        assert_eq!(risky["size"], 10);
        assert_eq!(risky["fit"], Value::Null);
        assert_eq!(risky["size_band"], Value::Null);
    }
);

/// A landed-delivery row of `lines` changed lines for `task_id`; `seq`
/// keeps each row's commit distinct.
fn landed_row(fixture: &Fixture, task_id: &str, seq: usize, lines: u64) {
    use orbit_store::contracts::{
        DiffClassCounts, LandedDiffSource, LandedDiffStats, LandedDiffStoreBackend as _,
    };
    fixture
        .runtime
        .sqlite_store()
        .unwrap()
        .record_landed_diff_stats(&LandedDiffStats {
            workspace_id: "ws_http_fixture".into(),
            task_id: task_id.into(),
            base_commit: format!("{:040x}", 2 * seq),
            landed_commit: format!("{:040x}", 2 * seq + 1),
            landed_at: Utc::now() - Duration::minutes(1),
            recorded_at: Utc::now(),
            job_run_id: None,
            source: LandedDiffSource::Landing,
            crew: None,
            model: None,
            complexity: None,
            task_type: None,
            code: DiffClassCounts {
                added: lines - lines / 4,
                deleted: lines / 4,
                files: 1,
            },
            tests: DiffClassCounts::default(),
            other: DiffClassCounts::default(),
        })
        .unwrap();
}
