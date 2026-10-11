//! Codebase analytics (`/api/codebase/analytics`) through the HTTP API, over
//! fixture tasks, relations, history, reservations and lock denials.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use orbit_core::application::task::TaskAddParams;
use orbit_core::{AuditEventInsertParams, AuditEventStatus, OrbitError, TaskStatus};
use orbit_engine::RuntimeHost;
use orbit_types::task::{
    CONTEXT_FILES_WIDENED_EVENT, ContextFilesWidening, ContextWideningStep,
    TASK_ARTIFACT_SCHEMA_VERSION, TASK_EVENTS_FILE_NAME, TaskEventRowV2, TaskRelation,
    TaskRelationType,
};
use serde_json::{Value, json};

use super::support::{Fixture, isolated, json_ok};

const WS: &str = "workspace=ws_http_fixture";

fn add(fixture: &Fixture, title: &str, files: &[&str], relations: Vec<TaskRelation>) -> String {
    add_with_status(fixture, title, files, relations, TaskStatus::Backlog)
}

fn add_with_status(
    fixture: &Fixture,
    title: &str,
    files: &[&str],
    relations: Vec<TaskRelation>,
    status: TaskStatus,
) -> String {
    fixture
        .runtime
        .add_task(TaskAddParams {
            title: title.into(),
            description: "codebase analytics fixture".into(),
            context_files: files.iter().map(|file| format!("file:{file}")).collect(),
            relations,
            status: Some(status),
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string()
}

fn regression_from(target: &str) -> Vec<TaskRelation> {
    vec![TaskRelation {
        relation_type: TaskRelationType::RegressionFrom,
        target: target.parse().unwrap(),
    }]
}

fn bundle_dir(root: &Path, task_id: &str) -> Option<PathBuf> {
    for entry in fs::read_dir(root).ok()? {
        let path = entry.ok()?.path();
        if !path.is_dir() {
            continue;
        }
        if path.file_name().is_some_and(|name| name == task_id)
            && path.join(TASK_EVENTS_FILE_NAME).is_file()
        {
            return Some(path);
        }
        if let Some(found) = bundle_dir(&path, task_id) {
            return Some(found);
        }
    }
    None
}

/// Append one history row to a disposable fixture task, as a lifecycle writer
/// would: the analytics read history, and the transitions they classify
/// (run failures, widening) have no public operator entry point.
fn append_event(
    fixture: &Fixture,
    task_id: &str,
    at: DateTime<Utc>,
    event: &str,
    note: Option<String>,
    to_status: Option<TaskStatus>,
) {
    let dir = bundle_dir(fixture.path("").as_path(), task_id).expect("fixture task bundle");
    let path = dir.join(TASK_EVENTS_FILE_NAME);
    let rows = fs::read_to_string(&path).unwrap().lines().count();
    let row = TaskEventRowV2 {
        schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
        event_id: format!("EV-{:04}", rows + 1),
        at,
        by: "system".into(),
        event_type: event.into(),
        note,
        from_status: None,
        to_status,
    };
    let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(file, "{}", serde_json::to_string(&row).unwrap()).unwrap();
}

/// Log a transition away from and back to backlog, so the bundle's last
/// logged status still matches its envelope.
fn transition_and_return(
    fixture: &Fixture,
    task_id: &str,
    event: &str,
    note: Option<String>,
    to_status: TaskStatus,
) {
    let now = Utc::now();
    append_event(fixture, task_id, now, event, note, Some(to_status));
    append_event(
        fixture,
        task_id,
        now,
        "status_changed",
        None,
        Some(TaskStatus::Backlog),
    );
}

fn widen(fixture: &Fixture, task_id: &str, file: &str) {
    let note = serde_json::to_string(&ContextFilesWidening {
        run_id: "jrun-widen".into(),
        step: ContextWideningStep::Implement,
        activity: "agent_implement".into(),
        selectors: vec![format!("file:{file}")],
    })
    .unwrap();
    append_event(
        fixture,
        task_id,
        Utc::now(),
        CONTEXT_FILES_WIDENED_EVENT,
        Some(note),
        None,
    );
}

fn reserve(fixture: &Fixture, id: &str, task: &str, files: &[&str], at: DateTime<Utc>) {
    reserve_request(fixture, id, &[task], None, files, at);
}

fn reserve_request(
    fixture: &Fixture,
    id: &str,
    tasks: &[&str],
    run: Option<&str>,
    files: &[&str],
    at: DateTime<Utc>,
) {
    let files: Vec<String> = files.iter().map(|file| format!("file:{file}")).collect();
    let row = [
        id.to_string(),
        fixture.runtime.paths().orbit_dir.display().to_string(),
        json!(tasks).to_string(),
        json!(files).to_string(),
        at.to_rfc3339(),
        run.unwrap_or_default().to_string(),
    ];
    fixture
        .runtime
        .sqlite_store()
        .unwrap()
        .with_transaction(|tx| {
            tx.connection()
                .execute(
                    "INSERT INTO task_reservations(reservation_id, workspace_orbit_dir,
                       workspace_id, task_ids_json, files_json, actor, created_at,
                       expires_at, released_at, scope, owner_run_id)
                     VALUES (?1, ?2, 'ws_http_fixture', ?3, ?4, 'fixture', ?5, ?5, ?5, 'files', NULLIF(?6, ''))",
                    row,
                )
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            Ok(())
        })
        .unwrap();
}

fn lock_denial(fixture: &Fixture, index: usize, task: Option<&str>, run: Option<&str>, file: &str) {
    lock_denial_in_workspace(fixture, index, task, run, file, Some("ws_http_fixture"));
}

fn lock_denial_in_workspace(
    fixture: &Fixture,
    index: usize,
    task: Option<&str>,
    run: Option<&str>,
    file: &str,
    workspace: Option<&str>,
) {
    let arguments = json!({
        "actor": "codex",
        "task_ids": task.into_iter().collect::<Vec<_>>(),
        "files": [format!("file:{file}")],
        "conflicts": [{"file": format!("file:{file}"), "held_by": "task", "held_by_id": "HF-999"}],
        "owner_run_id": run,
    });
    fixture
        .runtime
        .record_audit_event(&AuditEventInsertParams {
            execution_id: format!("lock-denial-{index}"),
            command: "task.locks.reserve.denied".into(),
            subcommand: None,
            tool_name: Some("orbit.task.locks.reserve".into()),
            target_type: Some("task_reservation".into()),
            target_id: task.map(str::to_string),
            role: "codex".into(),
            status: AuditEventStatus::Denied,
            exit_code: 1,
            duration_ms: 1,
            working_directory: ".".into(),
            arguments_json: Some(arguments.to_string()),
            stdout_truncated: None,
            stderr_truncated: None,
            error_message: Some("task lock conflict".into()),
            host: None,
            pid: 1,
            session_id: None,
            workspace_id: workspace.map(str::to_string),
            caller_machine_id: None,
            caller_machine_name: None,
            process_machine_id: None,
            process_machine_name: None,
            transport: None,
            effective_capabilities: Default::default(),
            origin_session_id: None,
            mcp_call_id: None,
            lease_id: None,
            task_id: task.map(str::to_string),
            job_run_id: run.map(str::to_string),
            activity_id: None,
            step_index: None,
        })
        .unwrap();
}

#[test]
fn codebase_excludes_foreign_and_unscoped_denials_without_local_activity() {
    isolated(
        "codebase::codebase_excludes_foreign_and_unscoped_denials_without_local_activity",
        || {
            let fixture = Fixture::new();
            for (index, workspace, file) in [
                (0, Some("ws_foreign"), "src/foreign-only.rs"),
                (1, None, "src/legacy-only.rs"),
            ] {
                lock_denial_in_workspace(&fixture, index, None, None, file, workspace);
            }
            let server = fixture.server(false);
            let body = json_ok(server.get(&format!(
                "/api/codebase/analytics?{WS}&window=all&min_support=1"
            )));
            assert_eq!(body["tasks_scanned"], 0);
            assert_eq!(body["lock_contention"]["rows"], json!([]));
            assert_eq!(body["lock_contention"]["below_support"], 0);
        },
    );
}

#[test]
fn codebase_scopes_production_lock_denials_and_support_but_audit_remains_global() {
    isolated(
        "codebase::codebase_scopes_production_lock_denials_and_support_but_audit_remains_global",
        || {
            let fixture = Fixture::new();
            fs::create_dir_all(fixture.runtime.paths().repo_root.join("src")).unwrap();
            fs::write(fixture.runtime.paths().repo_root.join("src/shared.rs"), "").unwrap();
            let holder = add(&fixture, "holder", &["src/shared.rs"], vec![]);
            let waiter = add(&fixture, "waiter", &["src/shared.rs"], vec![]);
            let request = |task: &str| {
                fixture
                    .runtime
                    .run_deterministic(
                        "reserve_locks",
                        &json!({}),
                        &json!({"task_ids": [task], "ttl_seconds": 120}),
                        Default::default(),
                    )
                    .unwrap()
            };
            assert_eq!(request(&holder)["reserved"], true);
            for _ in 0..2 {
                assert_eq!(request(&waiter)["reserved"], false);
            }
            let local = fixture
                .runtime
                .list_audit_events(
                    None,
                    Some("orbit.task.locks.reserve".into()),
                    Some(AuditEventStatus::Denied),
                    None,
                    100,
                )
                .unwrap();
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
            let body = json_ok(server.get(&format!(
                "/api/codebase/analytics?{WS}&window=all&min_support=1"
            )));
            let section = &body["lock_contention"];
            assert_eq!(section["rows"].as_array().unwrap().len(), 1, "{body}");
            let lock = row(section, "src/shared.rs").unwrap();
            assert_eq!(lock["numerator"], 1, "local denial retries count once");
            assert_eq!(lock["denominator"], 2);
            assert_eq!(lock["rate"], 0.5);
            assert_eq!(lock["reservations"], 1);
            assert_eq!(lock["denials"], 2, "only local attempts count");
            let strict = json_ok(server.get(&format!(
                "/api/codebase/analytics?{WS}&window=all&min_support=3"
            )));
            assert_eq!(strict["lock_contention"]["rows"], json!([]));
            assert_eq!(strict["lock_contention"]["below_support"], 1);

            let global = fixture
                .runtime
                .list_audit_events(
                    None,
                    Some("orbit.task.locks.reserve".into()),
                    Some(AuditEventStatus::Denied),
                    None,
                    100,
                )
                .unwrap();
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
        },
    );
}

fn row<'a>(section: &'a Value, path: &str) -> Option<&'a Value> {
    section["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["path"] == path)
}

fn rate_of(section: &Value, path: &str) -> (u64, u64) {
    let row = row(section, path).unwrap_or_else(|| panic!("no row for {path}: {section}"));
    (
        row["numerator"].as_u64().unwrap(),
        row["denominator"].as_u64().unwrap(),
    )
}

fn operations(trace: &[Value], name: &str) -> usize {
    trace
        .iter()
        .filter(|event| event["span"]["name"] == name)
        .count()
}

#[test]
fn codebase_sections_count_distinct_tasks_and_split_causes() {
    isolated(
        "codebase::codebase_sections_count_distinct_tasks_and_split_causes",
        || {
            let fixture = Fixture::new();
            let now = Utc::now();

            // Regressions: three edges on one done task, a second done task.
            let regressed = add_with_status(
                &fixture,
                "regressed",
                &["src/flaky.rs"],
                vec![],
                TaskStatus::Done,
            );
            add_with_status(
                &fixture,
                "clean",
                &["src/flaky.rs"],
                vec![],
                TaskStatus::Done,
            );
            for index in 0..3 {
                add(
                    &fixture,
                    &format!("regression {index}"),
                    &["src/fix.rs"],
                    regression_from(&regressed),
                );
            }

            // Footprint misses: one task widened twice onto the same path.
            let widened = add(&fixture, "widened", &["src/widened.rs"], vec![]);
            widen(&fixture, &widened, "src/widened.rs");
            widen(&fixture, &widened, "src/widened.rs");
            add(&fixture, "planned", &["src/widened.rs"], vec![]);

            // Blocked: auth infrastructure, unclassified, and a run failure.
            let auth = add(&fixture, "auth block", &["src/blocked.rs"], vec![]);
            transition_and_return(&fixture, &auth, "workflow_run_failed", Some("workflow run failed: job=task_pr_pipeline, run_id=jrun-auth, error_code=provider_unavailable, error=OAuth token expired".into()), TaskStatus::Blocked);
            let unknown = add(&fixture, "manual block", &["src/blocked.rs"], vec![]);
            transition_and_return(
                &fixture,
                &unknown,
                "status_changed",
                None,
                TaskStatus::Blocked,
            );
            let failed = add(&fixture, "run block", &["src/blocked.rs"], vec![]);
            transition_and_return(&fixture, &failed, "workflow_run_failed", Some("workflow run failed: job=task_pr_pipeline, run_id=jrun-fail, error_code=-, error=tests failed".into()), TaskStatus::Blocked);

            // Rejected: one after a run, one at proposal, one live task with a run.
            let ran = add_with_status(
                &fixture,
                "rejected after run",
                &["src/rejected.rs"],
                vec![],
                TaskStatus::Rejected,
            );
            append_event(
                &fixture,
                &ran,
                now,
                "started",
                None,
                Some(TaskStatus::InProgress),
            );
            append_event(
                &fixture,
                &ran,
                now,
                "review_rejected",
                None,
                Some(TaskStatus::Rejected),
            );
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
            let low = json_ok(server.get(&format!(
                "/api/codebase/analytics?{WS}&window=all&min_support=1"
            )));

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
                "both events parsed from the note JSON"
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
            let default = json_ok(server.get(&format!("/api/codebase/analytics?{WS}&window=all")));
            assert_eq!(default["min_support"], 15);
            assert!(row(&default["regressions"], "src/flaky.rs").is_none());
            assert!(default["regressions"]["below_support"].as_u64().unwrap() >= 1);
            let strict = json_ok(server.get(&format!(
                "/api/codebase/analytics?{WS}&window=all&min_support=4"
            )));
            assert!(
                strict["change_coupling"]["rows"]
                    .as_array()
                    .unwrap()
                    .is_empty(),
                "pairs reserved together fewer than min_support times are omitted"
            );
        },
    );
}

#[test]
fn codebase_block_causes_require_failure_evidence() {
    isolated(
        "codebase::codebase_block_causes_require_failure_evidence",
        || {
            let fixture = Fixture::new();
            let work_failures = [
                "assertion failed in sandbox_profile_regression",
                "error[E0425]: cannot find value in module sandbox",
                "assertion failed in bwrap_launch_regression",
                "compiler error in bubblewrap::profile",
                "assertion failed in seatbelt_policy_test",
                "assertion failed in authentication_regression",
                "compiler error in oauth::authenticate",
                "assertion failed in unauthorized_request",
                "assertion failed in unauthorized",
                "compiler error in authentication_error",
                "assertion failed in enospc_recovery",
                "compiler error in provider_limit_regression",
                "assertion failed in invalid_token_parser",
                "compiler error in authentication_failed_regression",
            ];
            for (index, error) in work_failures.iter().enumerate() {
                let path = format!("src/work_{index}.rs");
                let task = add(&fixture, error, &[&path], vec![]);
                transition_and_return(
                    &fixture,
                    &task,
                    "workflow_run_failed",
                    Some(format!(
                        "workflow run failed: job=task_pr_pipeline, run_id=jrun-work-{index}, error_code=execution_failed, error={error}"
                    )),
                    TaskStatus::Blocked,
                );
            }
            let infra_failures = [
                (
                    "sandbox",
                    "execution_failed",
                    "bwrap: No permissions to create new namespace",
                ),
                (
                    "sandbox",
                    "execution_failed",
                    "cli subprocess exited with code 71 sandbox-exec could not apply Orbit's macOS sandbox profile (`sandbox_apply: Operation not permitted`), so the CLI never started",
                ),
                (
                    "sandbox",
                    "execution_failed",
                    "sandbox-exec cannot apply a profile",
                ),
                (
                    "sandbox",
                    "execution_failed",
                    "Orbit linux-bwrap policy denied the attempted write: read-only mount",
                ),
                (
                    "sandbox",
                    "provider_unavailable",
                    "Orbit's macOS sandbox hid the stored credential and the login is not necessarily expired",
                ),
                ("auth", "provider_unavailable", "OAuth token expired"),
                (
                    "auth",
                    "provider_unavailable",
                    "No authentication information found",
                ),
                (
                    "auth",
                    "provider_unavailable",
                    "Authentication required. Please run agent login first",
                ),
                ("auth", "execution_failed", "HTTP 401 Unauthorized"),
                (
                    "auth",
                    "provider_unavailable",
                    "codex provider authentication failure (HTTP 403): credentials rejected",
                ),
                ("auth", "provider_unavailable", "invalid API key"),
                (
                    "auth",
                    "execution_failed",
                    "[provider_unavailable] provider=codex Unauthorized",
                ),
                ("auth", "provider_unavailable", "authentication_error"),
                (
                    "disk",
                    "execution_failed",
                    "write failed: No space left on device (ENOSPC)",
                ),
                ("disk", "execution_failed", "disk quota exceeded"),
                ("disk", "execution_failed", "disk full"),
                (
                    "provider_limit",
                    "provider_limit",
                    "account usage exhausted",
                ),
                ("provider_limit", "provider_capacity", "no capacity"),
                (
                    "provider_limit",
                    "execution_failed",
                    "[provider_limit] provider=codex usage exhausted",
                ),
                (
                    "provider_unavailable",
                    "provider_unavailable",
                    "CLI unusable",
                ),
                (
                    "provider_unavailable",
                    "provider_refusal",
                    "content refused",
                ),
            ];
            let mut expected_causes = std::collections::BTreeMap::new();
            for (index, (cause, code, error)) in infra_failures.iter().enumerate() {
                let path = format!("src/infra_{index}.rs");
                let task = add(&fixture, error, &[&path], vec![]);
                transition_and_return(
                    &fixture,
                    &task,
                    "workflow_run_failed",
                    Some(format!(
                        "workflow run failed: job=task_pr_pipeline, run_id=jrun-infra-{index}, error_code={code}, error={error}"
                    )),
                    TaskStatus::Blocked,
                );
                *expected_causes.entry(*cause).or_insert(0_u64) += 1;
            }
            let unknown = add(&fixture, "manual block", &["src/unknown.rs"], vec![]);
            transition_and_return(
                &fixture,
                &unknown,
                "status_changed",
                Some("needs investigation of sandbox authentication module".into()),
                TaskStatus::Blocked,
            );
            let server = fixture.server(false);
            let response = json_ok(server.get(&format!(
                "/api/codebase/analytics?{WS}&window=all&min_support=1"
            )));
            let blocked = &response["blocked"];
            for index in 0..work_failures.len() {
                assert_eq!(rate_of(blocked, &format!("src/work_{index}.rs")), (1, 1));
            }
            for index in 0..infra_failures.len() {
                assert!(row(blocked, &format!("src/infra_{index}.rs")).is_none());
            }
            assert_eq!(rate_of(blocked, "src/unknown.rs"), (0, 1));
            assert_eq!(row(blocked, "src/unknown.rs").unwrap()["unclassified"], 1);
            expected_causes.insert("run_failure", work_failures.len() as u64);
            expected_causes.insert("unclassified", 1);
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
        },
    );
}

#[test]
fn codebase_contention_counts_file_only_runs_and_deduplicates_retries() {
    isolated(
        "codebase::codebase_contention_counts_file_only_runs_and_deduplicates_retries",
        || {
            let fixture = Fixture::new();
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
                let body = json_ok(server.get(&format!(
                    "/api/codebase/analytics?{WS}&window=all&min_support={support}"
                )));
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
            let strict = json_ok(server.get(&format!(
                "/api/codebase/analytics?{WS}&window=all&min_support=3"
            )));
            assert_eq!(strict["lock_contention"]["below_support"], 3);
            assert_eq!(
                strict["lock_contention"]["rows"].as_array().unwrap().len(),
                1
            );
            assert!(row(&strict["lock_contention"], "src/unowned-lock.rs").is_some());
        },
    );
}

#[test]
fn codebase_window_rollup_and_cache_reuse_one_scan() {
    isolated(
        "codebase::codebase_window_rollup_and_cache_reuse_one_scan",
        || {
            let fixture = Fixture::new();
            let old = add_with_status(
                &fixture,
                "old",
                &["crates/alpha/src/old.rs"],
                vec![],
                TaskStatus::Done,
            );
            append_event(
                &fixture,
                &old,
                Utc::now() - Duration::days(60),
                "status_changed",
                None,
                Some(TaskStatus::Done),
            );
            let new = add_with_status(
                &fixture,
                "new",
                &["crates/alpha/tests/new.rs"],
                vec![],
                TaskStatus::Done,
            );
            for target in [&old, &new] {
                add(
                    &fixture,
                    "regression",
                    &["README.md"],
                    regression_from(target),
                );
            }

            let server = fixture.counted_task_server();
            let query = |params: &str| {
                json_ok(server.get(&format!(
                    "/api/codebase/analytics?{WS}&min_support=1&{params}"
                )))
            };
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

            let refused = server.get(&format!("/api/codebase/analytics?{WS}&window=1y"));
            assert_eq!(refused.status().as_u16(), 400);
            let refused = server.get(&format!("/api/codebase/analytics?{WS}&rollup=module"));
            assert_eq!(refused.status().as_u16(), 400);
        },
    );
}

#[test]
fn codebase_view_of_an_empty_workspace_is_empty_sections() {
    isolated(
        "codebase::codebase_view_of_an_empty_workspace_is_empty_sections",
        || {
            let fixture = Fixture::new();
            let server = fixture.server(false);
            let empty = json_ok(server.get(&format!("/api/codebase/analytics?{WS}")));
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
        },
    );
}

/// Warm latency of the cached view, measured on a copy of a real store: one
/// cold scan, then requests across every window, rollup and a lowered support
/// inside the cache period. Never point it at a live store. The measurements
/// are written to `<root>/codebase-perf.json`.
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
