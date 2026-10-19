//! Codebase analytics (`/api/codebase/analytics`) through the HTTP API, over
//! fixture tasks, relations, history, reservations, lock denials, landed
//! commits and task-pilot receipts.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use orbit_core::application::task::TaskAddParams;
use orbit_core::{
    AuditEventInsertParams, AuditEventStatus, JobRun, JobRunState, OrbitError, TaskStatus,
};
use orbit_engine::RuntimeHost;
use orbit_types::task::{
    CONTEXT_FILES_WIDENED_EVENT, ContextFilesWidening, ContextWideningStep,
    RegressionClassification, RegressionEscape, RegressionNature, TASK_ARTIFACT_SCHEMA_VERSION,
    TASK_COMMENTS_FILE_NAME, TASK_EVENTS_FILE_NAME, TaskCommentRowV2, TaskComplexity, TaskDecision,
    TaskEventRowV2, TaskOrigin, TaskRelation, TaskRelationType,
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
        regression: None,
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
    append_decided_event(fixture, task_id, at, event, note, to_status, None);
}

/// [`append_event`], recording `decision` on the row.
fn append_decided_event(
    fixture: &Fixture,
    task_id: &str,
    at: DateTime<Utc>,
    event: &str,
    note: Option<String>,
    to_status: Option<TaskStatus>,
    decision: Option<TaskDecision>,
) {
    append_row(
        fixture, task_id, at, "system", event, note, to_status, decision,
    );
}

/// [`append_decided_event`] by the actor `by`.
#[allow(clippy::too_many_arguments)]
fn append_row(
    fixture: &Fixture,
    task_id: &str,
    at: DateTime<Utc>,
    by: &str,
    event: &str,
    note: Option<String>,
    to_status: Option<TaskStatus>,
    decision: Option<TaskDecision>,
) {
    let dir = bundle_dir(fixture.path("").as_path(), task_id).expect("fixture task bundle");
    let path = dir.join(TASK_EVENTS_FILE_NAME);
    let rows = fs::read_to_string(&path).unwrap().lines().count();
    let row = TaskEventRowV2 {
        schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
        event_id: format!("EV-{:04}", rows + 1),
        at,
        by: by.into(),
        event_type: event.into(),
        note,
        from_status: None,
        to_status,
        operation_id: None,
        decision,
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
    transition_by_and_return(fixture, task_id, "system", event, note, to_status);
}

/// [`transition_and_return`], the transition written by `by`.
fn transition_by_and_return(
    fixture: &Fixture,
    task_id: &str,
    by: &str,
    event: &str,
    note: Option<String>,
    to_status: TaskStatus,
) {
    let now = Utc::now();
    append_row(
        fixture,
        task_id,
        now,
        by,
        event,
        note,
        Some(to_status),
        None,
    );
    append_event(
        fixture,
        task_id,
        now,
        "status_changed",
        None,
        Some(TaskStatus::Backlog),
    );
}

/// Record a widening onto `file`, as a typed decision beside its note, or,
/// unless `typed`, as an older binary wrote it: the note alone.
fn widen(fixture: &Fixture, task_id: &str, file: &str, typed: bool) {
    let widening = ContextFilesWidening {
        run_id: "jrun-widen".into(),
        step: ContextWideningStep::Implement,
        activity: "agent_implement".into(),
        selectors: vec![format!("file:{file}")],
    };
    let note = serde_json::to_string(&widening).unwrap();
    append_decided_event(
        fixture,
        task_id,
        Utc::now(),
        CONTEXT_FILES_WIDENED_EVENT,
        Some(note),
        None,
        typed.then_some(TaskDecision::ContextFilesWidened(widening)),
    );
}

/// Run git in the fixture repository and return its trimmed stdout.
fn git(repo: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
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

/// A Git repository at the fixture's checkout with one seed commit.
fn init_repo(fixture: &Fixture) -> PathBuf {
    let repo = fixture.work.parent().unwrap().to_path_buf();
    git(&repo, &["init", "-q"]);
    fs::write(repo.join("seed"), "seed").unwrap();
    git(&repo, &["add", "seed"]);
    git(&repo, &["commit", "-qm", "seed"]);
    repo
}

/// Commit `files` as `task`'s landed diff and record its owner landing at
/// `at`, as the landing evidence the landed-diff facts read.
fn land(fixture: &Fixture, repo: &Path, task: &str, files: &[&str], at: DateTime<Utc>) {
    for file in files {
        fs::create_dir_all(repo.join(file).parent().unwrap()).unwrap();
        fs::write(repo.join(file), task).unwrap();
    }
    git(repo, &[&["add", "--"][..], files].concat());
    git(repo, &["commit", "-qm", task]);
    let commit = git(repo, &["rev-parse", "HEAD"]);
    append_event(
        fixture,
        task,
        at,
        "landing_completed",
        Some(json!({"delivery": {"merge_commit": commit}}).to_string()),
        Some(TaskStatus::Done),
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

#[test]
fn codebase_lock_wait_spans_first_denial_to_admission_once_per_task() {
    isolated(
        "codebase::codebase_lock_wait_spans_first_denial_to_admission_once_per_task",
        || {
            let fixture = Fixture::new();
            fs::create_dir_all(fixture.runtime.paths().repo_root.join("src")).unwrap();
            fs::write(fixture.runtime.paths().repo_root.join("src/shared.rs"), "").unwrap();
            let holder = add(&fixture, "holder", &["src/shared.rs"], vec![]);
            let waiter = add(&fixture, "waiter", &["src/shared.rs"], vec![]);
            let leaver = add(&fixture, "leaver", &["src/shared.rs"], vec![]);
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
                let mut times: Vec<_> = fixture
                    .runtime
                    .list_audit_events(
                        None,
                        Some("orbit.task.locks.reserve".into()),
                        Some(AuditEventStatus::Denied),
                        None,
                        100,
                    )
                    .unwrap()
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
            let body = json_ok(server.get(&format!(
                "/api/codebase/analytics?{WS}&window=all&min_support=1"
            )));
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
            let millis =
                |at: DateTime<Utc>| at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
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

            // Footprint misses: one task widened twice onto the same path,
            // once typed and once in the note-only shape of older history.
            let widened = add(&fixture, "widened", &["src/widened.rs"], vec![]);
            widen(&fixture, &widened, "src/widened.rs", true);
            widen(&fixture, &widened, "src/widened.rs", false);
            add(&fixture, "planned", &["src/widened.rs"], vec![]);

            // Blocked: auth infrastructure, unclassified, and a run failure.
            let auth = add(&fixture, "auth block", &["src/blocked.rs"], vec![]);
            transition_and_return(&fixture, &auth, "workflow_run_failed", Some("workflow run failed: job=task_pr_pipeline, run_id=jrun-auth, error_code=provider_unavailable, error=OAuth token expired".into()), TaskStatus::Blocked);
            let unknown = add(&fixture, "unrecorded block", &["src/blocked.rs"], vec![]);
            transition_and_return(
                &fixture,
                &unknown,
                "claim_failed",
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
            let manual = add(&fixture, "manual block", &["src/manual.rs"], vec![]);
            transition_and_return(
                &fixture,
                &manual,
                "status_changed",
                Some("needs investigation of sandbox authentication module".into()),
                TaskStatus::Blocked,
            );
            let unknown = add(&fixture, "unknown block", &["src/unknown.rs"], vec![]);
            transition_and_return(
                &fixture,
                &unknown,
                "dependency_blocked",
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
        },
    );
}

/// The note an owner's `claim_failed` entry carries for a claimed leaf's
/// failure settlement.
fn claim_failed_note(evidence: &str, class: &str, reason: &str) -> String {
    format!(
        "claimed leaf failed: host=mac-follower, run_id=jrun-leaf, evidence={evidence}, \
         failure_class={class}, failed_step=implement, error={reason}"
    )
}

/// [ORB-16793] Every recorded cause of a block is classified from its event
/// and typed fields: a claimed leaf's settlement classes, run failure
/// handoffs naming `run=`, base conflicts (counted only on the paths they
/// recorded), review and validation-environment stops, and manual blocks by
/// actor kind. Only an entry that records no cause stays unclassified.
#[test]
fn codebase_blocked_ranking_classifies_every_recorded_cause() {
    isolated(
        "codebase::codebase_blocked_ranking_classifies_every_recorded_cause",
        || {
            let fixture = Fixture::new();
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
            // Counted against the footprint.
            claim(
                "src/claim_candidate.rs",
                "failure",
                "candidate",
                "cargo test failed",
            );
            claim(
                "src/claim_recovery.rs",
                "final_recovery",
                "candidate",
                "escalated",
            );
            block(
                "src/handoff_failed.rs",
                "system",
                "pr_failure_handoff",
                Some(
                    "failure handoff published PR #7: run=jrun-h, failed_step=validate, \
                      original_base=b0, target_base=b0, conflicts=none reported"
                        .into(),
                ),
            );
            block(
                "src/review_gate.rs",
                "system",
                "review_gate_escalation",
                Some("review gate escalated: run=jrun-r, decision=reject".into()),
            );
            block(
                "src/run_named.rs",
                "system",
                "legacy_failure",
                Some("legacy failure: run=jrun-l, failed_step=implement".into()),
            );
            // Infrastructure, by cause.
            claim(
                "src/claim_auth.rs",
                "provider_unavailable",
                "provider",
                "OAuth token expired",
            );
            claim(
                "src/claim_provider.rs",
                "failure",
                "provider",
                "CLI unusable",
            );
            claim(
                "src/claim_forge.rs",
                "forge_unavailable",
                "transient",
                "push refused",
            );
            claim(
                "src/claim_red.rs",
                "baseline_red",
                "baseline_red",
                "make test red on base",
            );
            block(
                "src/validation_env.rs",
                "system",
                "validation_environment_blocked",
                Some("required validation lacked a tool in its environment: run=jrun-v".into()),
            );
            // Not failures.
            claim(
                "src/claim_hold.rs",
                "evidence_hold",
                "-",
                "awaiting evidence",
            );
            claim(
                "src/claim_cancel.rs",
                "failure",
                "operator_cancel",
                "operator cancelled",
            );
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
            block(
                "src/sync_base.rs",
                "system",
                "pr_failure_handoff",
                Some(
                    "failure handoff published PR #9: run=jrun-s, failed_step=sync_base, \
                      original_base=b0, target_base=b1, conflicts=none reported"
                        .into(),
                ),
            );
            // Manual blocks, by who made them.
            block("src/by_operator.rs", "human:dan", "status_changed", None);
            block("src/by_agent.rs", "claude", "status_changed", None);
            block(
                "src/revoked.rs",
                "hm_owner",
                "claim_revoked",
                Some("recover".into()),
            );
            block("src/forced.rs", "system", "forced", None);
            // No recorded cause: a note-less claim failure with no settlement,
            // and a field that only ends in `run`.
            block("src/claim_unknown.rs", "hm_follower", "claim_failed", None);
            block(
                "src/source_run.rs",
                "system",
                "candidate_resumed",
                Some("resumed: source_run=jrun-old".into()),
            );

            let server = fixture.server(false);
            let response = json_ok(server.get(&format!(
                "/api/codebase/analytics?{WS}&window=all&min_support=1"
            )));
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

/// The dashboard route and the `orbit.codebase.*` tools read one core
/// service: the tool answers the route's rankings and overlap for the same
/// selection, and a path's detail names the tasks behind each numerator it
/// reports.
#[test]
fn codebase_tools_serve_the_dashboard_payload_and_path_detail() {
    isolated(
        "codebase::codebase_tools_serve_the_dashboard_payload_and_path_detail",
        || {
            let fixture = Fixture::new();
            let shared = "src/shared.rs";
            let regressed =
                add_with_status(&fixture, "regressed", &[shared], vec![], TaskStatus::Done);
            add_with_status(&fixture, "clean", &[shared], vec![], TaskStatus::Done);
            let regression = add(
                &fixture,
                "regression",
                &["src/fix.rs"],
                regression_from(&regressed),
            );
            widen(&fixture, &regression, shared, true);
            let failed = add(&fixture, "run block", &[shared], vec![]);
            transition_and_return(&fixture, &failed, "workflow_run_failed", Some("workflow run failed: job=task_pr_pipeline, run_id=jrun-fail, error_code=-, error=tests failed".into()), TaskStatus::Blocked);

            let server = fixture.server(false);
            let mut api = json_ok(server.get(&format!(
                "/api/codebase/analytics?{WS}&window=30d&min_support=1"
            )));
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
        },
    );
}

/// A fixture task with its crew, complexity, tags and regression link.
fn seed_task(fixture: &Fixture, title: &str, files: &[&str], params: TaskAddParams) -> String {
    fixture
        .runtime
        .add_task(TaskAddParams {
            title: title.into(),
            description: "codebase analytics fixture".into(),
            context_files: files.iter().map(|file| format!("file:{file}")).collect(),
            ..params
        })
        .unwrap()
        .id
        .to_string()
}

fn classified_link(
    target: &str,
    classification: Option<RegressionClassification>,
) -> Vec<TaskRelation> {
    vec![TaskRelation {
        relation_type: TaskRelationType::RegressionFrom,
        target: target.parse().unwrap(),
        regression: classification,
    }]
}

/// One breakdown's nonzero counts by value.
fn breakdown(section: &Value, dimension: &str) -> Vec<(String, u64)> {
    section["breakdowns"][dimension]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["links"] != 0)
        .map(|row| {
            (
                row["value"].as_str().unwrap().to_string(),
                row["links"].as_u64().unwrap(),
            )
        })
        .collect()
}

/// A slice's `(key, numerator, denominator)` rows.
fn slice_rows(section: &Value, slice: &str) -> Vec<(String, u64, u64)> {
    section[format!("by_{slice}")]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row[slice].as_str().unwrap().to_string(),
                row["numerator"].as_u64().unwrap(),
                row["denominator"].as_u64().unwrap(),
            )
        })
        .collect()
}

#[test]
fn codebase_regressions_break_down_and_filter_by_link_dimensions() {
    isolated(
        "codebase::codebase_regressions_break_down_and_filter_by_link_dimensions",
        || {
            let fixture = Fixture::new();
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
                        }),
                    ),
                    tags: vec!["qa-sweep:dashboard".into()],
                    ..Default::default()
                },
            );
            // An operator's link stored without a classification.
            let filed = seed_task(
                &fixture,
                "operator finding",
                &["docs/guide.md"],
                TaskAddParams {
                    relations: classified_link(&low, None),
                    origin: Some(TaskOrigin::OperatorRequest),
                    ..Default::default()
                },
            );

            let server = fixture.server(false);
            let analytics = |filter: &str| {
                json_ok(server.get(&format!(
                    "/api/codebase/analytics?{WS}&window=30d&min_support=1{filter}"
                )))
            };
            let payload = analytics("");
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
            for (task, source) in [
                (&reviewed, "landed"),
                (&swept, "footprint"),
                (&filed, "footprint"),
            ] {
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
            assert_eq!(rate_of(section, area), (2, 4));

            // A filter narrows the path rate, the rates and the cross-tabs;
            // each breakdown still counts its own dimension's other values.
            let payload = analytics("&nature=broke_existing");
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

            // The detail lists only the regressed tasks and links that match.
            let detail = fixture
                .runtime
                .run_tool(
                    "orbit.codebase.show",
                    json!({ "path": area, "window": "30d", "min_support": 1, "detected_by": "operator" }),
                )
                .unwrap();
            assert_eq!(detail["regression_filter"]["detected_by"], "operator");
            let standing = &detail["rankings"]["regressions"];
            assert_eq!(standing["numerator"], 1, "{detail}");
            let tasks = standing["tasks"].as_array().unwrap();
            assert_eq!(tasks.len(), 1, "{detail}");
            assert_eq!(tasks[0]["task_id"], low.as_str());
            assert_eq!(tasks[0]["regression_tasks"], json!([filed]));
            assert_eq!(tasks[0]["regressions"][0]["nature"], "absent");

            let bad = server.get(&format!("/api/codebase/analytics?{WS}&layer=frontend"));
            assert_eq!(bad.status(), 400, "an unknown layer is refused");
            let bad = fixture
                .runtime
                .run_tool("orbit.codebase.analytics", json!({ "escape": "missed" }));
            assert!(matches!(bad, Err(OrbitError::InvalidInput(_))), "{bad:?}");
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

/// Landed diffs, including Markdown without a reservation, drive related
/// files; declared-only and bulk history cannot inflate shared support.
#[test]
fn codebase_related_uses_landed_diffs_and_filters_support_and_window() {
    isolated(
        "codebase::codebase_related_uses_landed_diffs_and_filters_support_and_window",
        || {
            let fixture = Fixture::new();
            let repo = init_repo(&fixture);
            let now = Utc::now();
            let seed = |title: &str, files: &[&str], declared: &[&str], at: DateTime<Utc>| {
                let task = add_with_status(&fixture, title, declared, vec![], TaskStatus::Done);
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
            seed("partner only", &["docs/b.md"], &["docs/b.md"], now);
            seed(
                "undeclared partner only",
                &["docs/b.md"],
                &["src/a.rs"],
                now,
            );
            seed("path only", &["src/a.rs"], &["src/a.rs"], now);
            seed(
                "old",
                &["src/a.rs", "src/old.rs"],
                &["src/a.rs"],
                now - Duration::days(40),
            );
            let mut bulk: Vec<_> = (0..13).map(|n| format!("bulk/{n}")).collect();
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
            let payload = json_ok(server.get(&format!(
                "/api/codebase/related?{WS}&path=src/a.rs&window=30d&min_support=1&top=1"
            )));
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
            let all = json_ok(server.get(&format!(
                "/api/codebase/related?{WS}&path=src/a.rs&window=all&min_support=1"
            )));
            assert_eq!(all["path_support"], 4);
            let c = all["rows"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["path"] == "src/c.rs")
                .unwrap();
            assert_eq!(c["reservation_coupling"]["rate"], 0.5);
            let filtered = json_ok(server.get(&format!(
                "/api/codebase/related?{WS}&path=src/a.rs&window=30d&min_support=3"
            )));
            assert!(filtered["rows"].as_array().unwrap().is_empty());
            let unknown = json_ok(server.get(&format!(
                "/api/codebase/related?{WS}&path=unknown&min_support=1"
            )));
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
            let analytics = json_ok(server.get(&format!("/api/codebase/analytics?{WS}")));
            assert!(
                analytics["known_paths"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|p| p == "docs/b.md")
            );
        },
    );
}

/// A task-pilot run of `crew` and `model`, active from `started` to
/// `finished`, as the run record the ranking attributes assessments to.
fn seed_pilot_run(
    fixture: &Fixture,
    run_id: &str,
    crew: &str,
    model: &str,
    started: DateTime<Utc>,
    finished: DateTime<Utc>,
) {
    let run = JobRun {
        run_id: run_id.into(),
        job_id: "task_pilot_pipeline".into(),
        attempt: 1,
        state: JobRunState::Success,
        scheduled_at: started,
        created_at: started,
        started_at: Some(started),
        finished_at: Some(finished),
        duration_ms: None,
        pid: None,
        pid_start_time: None,
        input: None,
        retry_source_run_id: None,
        knowledge_metrics: None,
        resolved_crew: Some(crew.into()),
        crew_model: Some(model.into()),
        steps: Vec::new(),
        executed_on: None,
    };
    let workspace_id = fixture.runtime.workspace_id().unwrap();
    fixture
        .runtime
        .sqlite_store()
        .unwrap()
        .upsert_job_run_for_workspace(&workspace_id, &run, None)
        .unwrap();
}

/// Append a task-pilot receipt comment in the shape the pilot's atomic apply
/// writes: the audit body sealed under its operation id, naming its pilot run
/// unless `run_id` is `None` (a receipt written before that field).
fn pilot_receipt(
    fixture: &Fixture,
    task_id: &str,
    at: DateTime<Utc>,
    run_id: Option<&str>,
    before: &[&str],
    after: &[&str],
) {
    let dir = bundle_dir(fixture.path("").as_path(), task_id).expect("fixture task bundle");
    let path = dir.join(TASK_COMMENTS_FILE_NAME);
    let rows = fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .count();
    let mut body = json!({
        "assessment": {
            "task_id": task_id,
            "context_files_before": before,
            "context_files_after": after,
            "disposition": "selectors",
        },
        "context_files_before": before,
    });
    if let Some(run_id) = run_id {
        body["pilot_run_id"] = json!(run_id);
    }
    let row = TaskCommentRowV2 {
        schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
        comment_id: format!("C-{:04}", rows + 1),
        at,
        by: "task-pilot".into(),
        body: body.to_string(),
        operation_id: Some(format!("{:064x}", rows + 1 + task_id.len() * 1000)),
    };
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(file, "{}", serde_json::to_string(&row).unwrap()).unwrap();
}

/// Each pilot crew and model is graded against the landed diffs of the tasks
/// it assessed: dir and wildcard selectors cover the files beneath them, a
/// mirrored skill and a CLI golden count through their sources, the filer's
/// selectors are the baseline, a receipt from before the run link is matched
/// to the one pilot run active then, and no-diff, bulk, unassessed,
/// unattributed and out-of-window tasks are left out.
#[test]
fn codebase_task_pilot_grades_each_crew_against_landed_diffs() {
    isolated(
        "codebase::codebase_task_pilot_grades_each_crew_against_landed_diffs",
        || {
            let fixture = Fixture::new();
            let repo = init_repo(&fixture);
            let now = Utc::now();
            seed_pilot_run(
                &fixture,
                "jrun-pilot-luna",
                "luna",
                "luna-model",
                now - Duration::minutes(60),
                now - Duration::minutes(50),
            );
            seed_pilot_run(
                &fixture,
                "jrun-pilot-haiku",
                "haiku",
                "claude-haiku-5-5",
                now - Duration::minutes(120),
                now - Duration::minutes(110),
            );
            let piloted = |title: &str,
                           files: &[&str],
                           run: Option<&str>,
                           before: &[&str],
                           after: &[&str],
                           receipt_at: DateTime<Utc>,
                           landed_at: DateTime<Utc>| {
                let task = add_with_status(&fixture, title, &[], vec![], TaskStatus::Done);
                pilot_receipt(&fixture, &task, receipt_at, run, before, after);
                land(&fixture, &repo, &task, files, landed_at);
                task
            };
            let luna = Some("jrun-pilot-luna");
            let haiku = Some("jrun-pilot-haiku");
            let at = now - Duration::minutes(55);
            // luna: every landed file covered; one of five selectors unused.
            let skills = piloted(
                "web and skills",
                &[
                    "src/web/app.js",
                    "src/web/style.css",
                    "crates/orbit-core/assets/skills/orbit/SKILL.md",
                    "plugin/skills/orbit/SKILL.md",
                ],
                luna,
                &["file:src/web/app.js"],
                &[
                    "dir:src/web",
                    "file:crates/orbit-core/assets/skills/orbit/SKILL.md",
                    "file:docs/unused.md",
                ],
                at,
                now,
            );
            let cli = piloted(
                "cli and goldens",
                &[
                    "src/lib.rs",
                    "src/main.rs",
                    "crates/orbit-cli/src/command/codebase.rs",
                    "crates/orbit-cli/tests/help_goldens/codebase/root.txt",
                ],
                luna,
                &[],
                &[
                    "file:src/*.rs",
                    "file:crates/orbit-cli/src/command/codebase.rs",
                ],
                at,
                now,
            );
            widen(&fixture, &cli, "src/extra.rs", true);
            // haiku: half its prediction right where the filer was exact,
            // then a whole directory where the filer named the wrong file.
            let missed = piloted(
                "half right",
                &["src/x.rs", "src/y.rs"],
                haiku,
                &["file:src/x.rs", "file:src/y.rs"],
                &["file:src/x.rs", "file:src/z.rs"],
                now - Duration::minutes(115),
                now,
            );
            piloted(
                "legacy receipt",
                &["docs/guide.md"],
                None,
                &["file:docs/other.md"],
                &["dir:docs"],
                now - Duration::minutes(115),
                now,
            );
            // Left out: no-diff, bulk, unassessed, unattributed, too old.
            let no_diff = fixture
                .runtime
                .add_task(TaskAddParams {
                    title: "no diff".into(),
                    description: "codebase analytics fixture".into(),
                    tags: vec!["no-diff-expected".into()],
                    status: Some(TaskStatus::Done),
                    ..Default::default()
                })
                .unwrap()
                .id
                .to_string();
            pilot_receipt(&fixture, &no_diff, at, luna, &[], &["file:src/n.rs"]);
            land(&fixture, &repo, &no_diff, &["src/n.rs"], now);
            let bulk: Vec<String> = (0..13).map(|n| format!("bulk/{n}.rs")).collect();
            piloted(
                "bulk",
                &bulk.iter().map(String::as_str).collect::<Vec<_>>(),
                luna,
                &[],
                &["dir:bulk"],
                at,
                now,
            );
            let unassessed = add_with_status(&fixture, "unassessed", &[], vec![], TaskStatus::Done);
            land(&fixture, &repo, &unassessed, &["src/u.rs"], now);
            piloted(
                "deleted run",
                &["src/d.rs"],
                Some("jrun-deleted"),
                &[],
                &["file:src/d.rs"],
                at,
                now,
            );
            piloted(
                "old",
                &["src/old.rs"],
                luna,
                &[],
                &["file:src/other.rs"],
                now - Duration::days(40),
                now - Duration::days(40),
            );

            let server = fixture.server(false);
            let payload = json_ok(server.get(&format!(
                "/api/codebase/analytics?{WS}&window=30d&min_support=1"
            )));
            let section = &payload["task_pilot"];
            assert_eq!(section["assessed"], 4, "{section}");
            assert_eq!(section["excluded_no_diff"], 1, "{section}");
            assert_eq!(section["excluded_large"], 1, "{section}");
            assert_eq!(section["without_assessment"], 1, "{section}");
            assert_eq!(section["unattributed"], 1, "{section}");
            assert_eq!(section["below_support"], 0);
            let rows = section["rows"].as_array().unwrap();
            assert_eq!(
                rows.iter()
                    .map(|row| (
                        row["crew"].as_str().unwrap(),
                        row["model"].as_str().unwrap()
                    ))
                    .collect::<Vec<_>>(),
                [("luna", "luna-model"), ("haiku", "claude-haiku-5-5")],
                "ranked by F1"
            );
            let close = |value: &Value, expected: f64| {
                assert!(
                    (value.as_f64().unwrap() - expected).abs() < 1e-9,
                    "{value} != {expected}"
                );
            };
            let luna_row = &rows[0];
            assert_eq!(luna_row["support"], 2);
            assert_eq!(
                (
                    &luna_row["recall"]["numerator"],
                    &luna_row["recall"]["denominator"]
                ),
                (&json!(8), &json!(8)),
                "dir, wildcard, mirrored and golden files are covered: {luna_row}"
            );
            assert_eq!(
                (
                    &luna_row["precision"]["numerator"],
                    &luna_row["precision"]["denominator"]
                ),
                (&json!(4), &json!(5))
            );
            close(&luna_row["f1"], 2.0 * 0.8 / 1.8);
            assert_eq!(luna_row["author"]["recall"]["numerator"], 1);
            assert_eq!(luna_row["author"]["precision"]["denominator"], 1);
            close(&luna_row["author"]["f1"], 2.0 * 0.125 / 1.125);
            close(
                &luna_row["lift"]["f1"],
                2.0 * 0.8 / 1.8 - 2.0 * 0.125 / 1.125,
            );
            assert_eq!(luna_row["widenings"], 1);
            close(&luna_row["widenings_per_task"], 0.5);
            assert_eq!(luna_row["misses"], json!([]));
            assert_eq!(
                luna_row["over_declarations"],
                json!([{"task_id": skills, "title": "web and skills", "selectors": ["file:docs/unused.md"]}])
            );

            let haiku_row = &rows[1];
            assert_eq!(
                haiku_row["support"], 2,
                "the legacy receipt is matched by its run's window"
            );
            close(&haiku_row["recall"]["rate"], 2.0 / 3.0);
            close(&haiku_row["precision"]["rate"], 2.0 / 3.0);
            close(&haiku_row["author"]["f1"], 2.0 / 3.0);
            close(&haiku_row["lift"]["f1"], 0.0);
            assert_eq!(
                haiku_row["misses"],
                json!([{"task_id": missed, "title": "half right", "paths": ["src/y.rs"]}])
            );
            assert_eq!(
                haiku_row["over_declarations"][0]["selectors"],
                json!(["file:src/z.rs"])
            );

            let supported = json_ok(server.get(&format!(
                "/api/codebase/analytics?{WS}&window=30d&min_support=3"
            )));
            assert_eq!(supported["task_pilot"]["rows"], json!([]));
            assert_eq!(supported["task_pilot"]["below_support"], 2);
            let all = json_ok(server.get(&format!(
                "/api/codebase/analytics?{WS}&window=all&min_support=1"
            )));
            assert_eq!(
                all["task_pilot"]["assessed"], 5,
                "the 40-day-old landing counts only in a window that reaches it"
            );
        },
    );
}

#[test]
fn lock_contention_survives_audit_gc_and_upgrade_backfill() {
    isolated(
        "codebase::lock_contention_survives_audit_gc_and_upgrade_backfill",
        || {
            let fixture = Fixture::new();
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
                json_ok(fixture.server(false).get(&format!(
                    "/api/codebase/analytics?{WS}&window={window}&min_support=1"
                )))["lock_contention"]
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
        },
    );
}

/// `section` without wait durations, which grow with the scan time while a
/// task is still waiting, so two reads compare only what the prune can change.
fn without_open_wait_durations(mut section: Value) -> Value {
    for rows in ["rows", "rows_by_wait"] {
        for row in section[rows].as_array_mut().into_iter().flatten() {
            if let Some(wait) = row["wait"].as_object_mut() {
                for duration in ["median_ms", "max_ms", "total_ms"] {
                    wait.remove(duration);
                }
            }
        }
    }
    section
}
