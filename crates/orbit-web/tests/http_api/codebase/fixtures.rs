//! Shared codebase analytics task, history, audit and request fixtures.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use orbit_core::application::task::TaskAddParams;
use orbit_core::{
    AuditEventInsertParams, AuditEventStatus, JobRun, JobRunState, OrbitError, TaskStatus,
};
use orbit_engine::RuntimeHost;
use orbit_types::task::{
    CONTEXT_FILES_WIDENED_EVENT, ContextFilesWidening, ContextWideningStep,
    RegressionClassification, TASK_ARTIFACT_SCHEMA_VERSION, TASK_COMMENTS_FILE_NAME,
    TASK_EVENTS_FILE_NAME, TaskCommentRowV2, TaskDecision, TaskEventRowV2, TaskRelation,
    TaskRelationType,
};
use serde_json::{Value, json};

use super::super::support::{Fixture, Server, json_ok};

use super::WS;

pub(in super::super) fn add(
    fixture: &Fixture,
    title: &str,
    files: &[&str],
    relations: Vec<TaskRelation>,
) -> String {
    add_with_status(fixture, title, files, relations, TaskStatus::Backlog)
}

pub(in super::super) fn add_with_status(
    fixture: &Fixture,
    title: &str,
    files: &[&str],
    relations: Vec<TaskRelation>,
    status: TaskStatus,
) -> String {
    seed_task(
        fixture,
        title,
        files,
        TaskAddParams {
            relations,
            status: Some(status),
            ..Default::default()
        },
    )
}

pub(super) fn done(fixture: &Fixture, title: &str, files: &[&str]) -> String {
    add_with_status(fixture, title, files, vec![], TaskStatus::Done)
}

/// Keep raw responses available for status/header checks; query defaults remain
/// the API's defaults unless the scenario explicitly supplies them.
pub(super) fn codebase_get(
    server: &Server,
    endpoint: &str,
    query: &str,
) -> reqwest::blocking::Response {
    server.get(&format!("/api/codebase/{endpoint}?{WS}&{query}"))
}

pub(super) fn analytics(server: &Server, query: &str) -> Value {
    json_ok(codebase_get(server, "analytics", query))
}

pub(super) fn related(server: &Server, query: &str) -> Value {
    json_ok(codebase_get(server, "related", query))
}

pub(super) fn denied_events(fixture: &Fixture) -> Vec<orbit_core::AuditEvent> {
    fixture
        .runtime
        .list_audit_events(
            None,
            Some("orbit.task.locks.reserve".into()),
            Some(AuditEventStatus::Denied),
            None,
            100,
        )
        .unwrap()
}

pub(super) fn request_lock(fixture: &Fixture, task: &str) -> Value {
    fixture
        .runtime
        .run_deterministic(
            "reserve_locks",
            &json!({}),
            &json!({"task_ids": [task], "ttl_seconds": 120}),
            Default::default(),
        )
        .unwrap()
}

pub(super) fn regression_from(target: &str) -> Vec<TaskRelation> {
    classified_link(target, None)
}

pub(in super::super) fn bundle_dir(root: &Path, task_id: &str) -> Option<PathBuf> {
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
pub(in super::super) fn append_event(
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
pub(super) fn append_decided_event(
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
pub(in super::super) fn append_row(
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
pub(super) fn transition_and_return(
    fixture: &Fixture,
    task_id: &str,
    event: &str,
    note: Option<String>,
    to_status: TaskStatus,
) {
    transition_by_and_return(fixture, task_id, "system", event, note, to_status);
}

/// [`transition_and_return`], the transition written by `by`.
pub(super) fn transition_by_and_return(
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
pub(super) fn widen(fixture: &Fixture, task_id: &str, file: &str, typed: bool) {
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
pub(super) fn git(repo: &Path, args: &[&str]) -> String {
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
pub(in super::super) fn init_repo(fixture: &Fixture) -> PathBuf {
    let repo = fixture.work.parent().unwrap().to_path_buf();
    git(&repo, &["init", "-q"]);
    fs::write(repo.join("seed"), "seed").unwrap();
    git(&repo, &["add", "seed"]);
    git(&repo, &["commit", "-qm", "seed"]);
    repo
}

/// Commit `files` as `task`'s landed diff and record its owner landing at
/// `at`, as the landing evidence the landed-diff facts read.
pub(in super::super) fn land(
    fixture: &Fixture,
    repo: &Path,
    task: &str,
    files: &[&str],
    at: DateTime<Utc>,
) {
    let commit = commit_files(repo, task, files);
    append_event(
        fixture,
        task,
        at,
        "landing_completed",
        Some(json!({"delivery": {"merge_commit": commit}}).to_string()),
        Some(TaskStatus::Done),
    );
}

/// Commit `files`, each written with `task`'s ID, and return the commit.
pub(super) fn commit_files(repo: &Path, task: &str, files: &[&str]) -> String {
    for file in files {
        fs::create_dir_all(repo.join(file).parent().unwrap()).unwrap();
        fs::write(repo.join(file), task).unwrap();
    }
    git(repo, &[&["add", "--"][..], files].concat());
    git(repo, &["commit", "-qm", task]);
    git(repo, &["rev-parse", "HEAD"])
}

pub(in super::super) fn reserve(
    fixture: &Fixture,
    id: &str,
    task: &str,
    files: &[&str],
    at: DateTime<Utc>,
) {
    reserve_request(fixture, id, &[task], None, files, at);
}

pub(in super::super) fn reserve_request(
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

pub(super) fn lock_denial(
    fixture: &Fixture,
    index: usize,
    task: Option<&str>,
    run: Option<&str>,
    file: &str,
) {
    lock_denial_in_workspace(fixture, index, task, run, file, Some("ws_http_fixture"));
}

pub(super) fn lock_denial_in_workspace(
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

pub(super) fn row<'a>(section: &'a Value, path: &str) -> Option<&'a Value> {
    section["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["path"] == path)
}

pub(super) fn rate_of(section: &Value, path: &str) -> (u64, u64) {
    let row = row(section, path).unwrap_or_else(|| panic!("no row for {path}: {section}"));
    (
        row["numerator"].as_u64().unwrap(),
        row["denominator"].as_u64().unwrap(),
    )
}

pub(super) fn operations(trace: &[Value], name: &str) -> usize {
    trace
        .iter()
        .filter(|event| event["span"]["name"] == name)
        .count()
}

/// The note an owner's `claim_failed` entry carries for a claimed leaf's
/// failure settlement.
pub(super) fn claim_failed_note(evidence: &str, class: &str, reason: &str) -> String {
    format!(
        "claimed leaf failed: host=mac-follower, run_id=jrun-leaf, evidence={evidence}, \
         failure_class={class}, failed_step=implement, error={reason}"
    )
}

/// The task ids whose facts one server's scans computed, in order.
pub(super) fn computed_tasks(trace: &[Value]) -> Vec<String> {
    let mut tasks: Vec<String> = trace
        .iter()
        .filter(|event| event["span"]["name"] == "codebase_task_facts_computed")
        .map(|event| event["span"]["task_id"].as_str().unwrap().to_string())
        .collect();
    tasks.sort();
    tasks
}

/// Every analytics field but when the scan ran.
pub(super) fn without_computed_at(mut body: Value) -> Value {
    body.as_object_mut().unwrap().remove("computed_at");
    body
}

/// A fixture task with its crew, complexity, tags and regression link.
pub(super) fn seed_task(
    fixture: &Fixture,
    title: &str,
    files: &[&str],
    params: TaskAddParams,
) -> String {
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

pub(super) fn classified_link(
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
pub(super) fn breakdown(section: &Value, dimension: &str) -> Vec<(String, u64)> {
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
pub(super) fn slice_rows(section: &Value, slice: &str) -> Vec<(String, u64, u64)> {
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

/// A task-pilot run of `crew` and `model`, active from `started` to
/// `finished`, with its frozen activity draw, before invocation evidence.
pub(in super::super) fn pilot_run(
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
    let mut state =
        orbit_types::workflow::PipelineState::new(run_id.into(), run.job_id.clone(), json!({}));
    let key = orbit_types::workflow::CrewPurpose::TaskPilot.config_key();
    state.activity_crew_draws.insert(
        key.into(),
        orbit_types::workflow::ActivityCrewDraw {
            crew: crew.into(),
            source: key.into(),
            eligible_pool: Vec::new(),
        },
    );
    fixture.runtime.write_run_state(run_id, &state).unwrap();
}

/// A pilot run with invocation evidence under its actual worker step id.
pub(in super::super) fn seed_pilot_run(
    fixture: &Fixture,
    run_id: &str,
    crew: &str,
    model: &str,
    started: DateTime<Utc>,
    finished: DateTime<Utc>,
) {
    pilot_run(fixture, run_id, crew, model, started, finished);
    fixture
        .runtime
        .insert_invocation_trace_record(&orbit_core::InvocationInsertParams {
            job_run_id: run_id.into(),
            account: None,
            activity_id: "pilot".into(),
            agent: "fixture".into(),
            provider: None,
            model: Some(model.into()),
            task_ids: Vec::new(),
            trace: Default::default(),
        })
        .unwrap();
}

/// Append a task-pilot receipt comment in the shape the pilot's atomic apply
/// writes: the audit body sealed under its operation id, naming its pilot run
/// unless `run_id` is `None` (a receipt written before that field).
pub(in super::super) fn pilot_receipt(
    fixture: &Fixture,
    task_id: &str,
    at: DateTime<Utc>,
    run_id: Option<&str>,
    before: &[&str],
    after: &[&str],
) {
    pilot_receipt_with(fixture, task_id, at, run_id, before, after, json!({}));
}

/// [`pilot_receipt`] whose assessment also carries `findings`, such as
/// `{"utility_warnings": [...]}`.
pub(in super::super) fn pilot_receipt_with(
    fixture: &Fixture,
    task_id: &str,
    at: DateTime<Utc>,
    run_id: Option<&str>,
    before: &[&str],
    after: &[&str],
    findings: Value,
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
    for (field, value) in findings.as_object().into_iter().flatten() {
        body["assessment"][field] = value.clone();
    }
    if let Some(run_id) = run_id {
        body["pilot_run_id"] = json!(run_id);
    }
    let row = TaskCommentRowV2 {
        schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
        comment_id: format!("C-{:04}", rows + 1),
        at,
        by: "task-pilot".into(),
        body: body.to_string(),
        operation_id: Some(orbit_common::security::release::sha256_hex(
            format!("{task_id}:{}", rows + 1).as_bytes(),
        )),
    };
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(file, "{}", serde_json::to_string(&row).unwrap()).unwrap();
}

/// `section` without wait durations, which grow with the scan time while a
/// task is still waiting, so two reads compare only what the prune can change.
pub(super) fn without_open_wait_durations(mut section: Value) -> Value {
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

/// Failure evidence used by the blocked-cause HTTP scenario.
pub(super) const WORK_FAILURES: &[&str] = &[
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

/// Failure evidence used by the blocked-cause HTTP scenario.
pub(super) const INFRA_FAILURES: &[(&str, &str, &str)] = &[
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
