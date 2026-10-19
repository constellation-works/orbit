#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use orbit_core::application::task::TaskAddParams;
use orbit_core::{ActorIdentity, JobRun, JobRunState, OrbitRuntime};
use orbit_types::task::{
    TASK_ARTIFACT_SCHEMA_VERSION, TASK_COMMENTS_FILE_NAME, TASK_EVENTS_FILE_NAME, TaskCommentRowV2,
    TaskEventRowV2, TaskRelation, TaskRelationType, TaskStatus,
};
use serde_json::json;

use crate::isolated_cli_fixture::Fixture;

/// `orbit codebase analytics` prints the tool's rankings, and `orbit codebase
/// show` follows a ranked path's rate to the tasks behind it.
#[test]
fn codebase_cli_ranks_paths_and_lists_the_tasks_behind_a_count() {
    if crate::isolated_test::run_in_child(
        "codebase_cli::codebase_cli_ranks_paths_and_lists_the_tasks_behind_a_count",
    ) {
        return;
    }

    let fixture = Fixture::new();
    let shown = fixture.json(&["workspace", "show", "--format", "json"]);
    let workspace_root = Path::new(shown["orbit_root"].as_str().unwrap());
    let (regressed, regression) = {
        // A managed run exports worker authority that would bind the fixture
        // runtime to the live store.
        let _authority = orbit_common::test_env::unset(
            orbit_common::test_env::INHERITED_AUTHORITY_ENV
                .iter()
                .copied(),
        );
        let runtime = OrbitRuntime::from_roots(&fixture.root, workspace_root)
            .unwrap()
            .with_actor(ActorIdentity::human("codebase-fixture"));
        let add = |title: &str, status: TaskStatus, relations: Vec<TaskRelation>| {
            runtime
                .add_task(TaskAddParams {
                    title: title.into(),
                    description: "codebase analytics fixture".into(),
                    context_files: vec!["file:src/lib.rs".into()],
                    relations,
                    status: Some(status),
                    ..Default::default()
                })
                .unwrap()
                .id
                .to_string()
        };
        let regressed = add("regressed", TaskStatus::Done, vec![]);
        add("clean", TaskStatus::Done, vec![]);
        let regression = add(
            "regression",
            TaskStatus::Backlog,
            vec![TaskRelation {
                relation_type: TaskRelationType::RegressionFrom,
                target: regressed.parse().unwrap(),
                regression: None,
            }],
        );
        (regressed, regression)
    };

    let selection = ["--window", "30d", "--min-support", "1"];
    let mut analytics =
        fixture.json(&[&["codebase", "analytics"][..], &selection, &["--json"]].concat());
    assert_eq!(
        analytics["regressions"]["rows"][0]["path"], "src/lib.rs",
        "{analytics}"
    );
    assert_eq!(analytics["regressions"]["rows"][0]["numerator"], 1);
    assert_eq!(analytics["regressions"]["rows"][0]["denominator"], 2);
    let input = json!({ "window": "30d", "min_support": 1 }).to_string();
    let mut tool = fixture.json(&[
        "tool",
        "run",
        "orbit.codebase.analytics",
        "--full",
        "--input",
        &input,
    ]);
    for payload in [&mut analytics, &mut tool] {
        let object = payload.as_object_mut().unwrap();
        object.remove("computed_at");
        object.remove("since");
    }
    assert_eq!(analytics, tool, "the command prints the tool's payload");

    let detail = fixture.json(
        &[
            &["codebase", "show", "src/lib.rs"][..],
            &selection,
            &["--json"],
        ]
        .concat(),
    );
    let regressions = &detail["rankings"]["regressions"];
    assert_eq!(regressions["rank"], 1);
    assert_eq!(regressions["numerator"], 1);
    assert_eq!(regressions["tasks"][0]["task_id"], regressed.as_str());
    assert_eq!(
        regressions["tasks"][0]["regression_tasks"],
        json!([regression])
    );
    assert_eq!(regressions["tasks"].as_array().map(Vec::len), Some(1));

    // The human view lists the regressed task with the task that regressed it.
    let human = fixture
        .command(&[&["codebase", "show", "src/lib.rs"][..], &selection].concat())
        .env("NO_COLOR", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let human = String::from_utf8(human).unwrap();
    assert!(
        human.lines().any(|line| {
            line.trim_start().starts_with(regressed.as_str()) && line.contains(regression.as_str())
        }),
        "the regressed task and its regression are listed:\n{human}"
    );

    let refused = fixture
        .command(&["codebase", "show", "src/absent.rs"])
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let refused = String::from_utf8_lossy(&refused);
    assert!(
        refused.contains("src/absent.rs"),
        "a path nothing touched is refused by name: {refused}"
    );
}

/// The public CLI reads landed paths rather than inferring them from context.
#[test]
fn codebase_related_cli_reports_landed_markdown_partner() {
    if crate::isolated_test::run_in_child(
        "codebase_cli::codebase_related_cli_reports_landed_markdown_partner",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let shown = fixture.json(&["workspace", "show", "--json"]);
    let workspace_root = Path::new(shown["orbit_root"].as_str().unwrap());
    let task = {
        let _authority = orbit_common::test_env::unset(
            orbit_common::test_env::INHERITED_AUTHORITY_ENV
                .iter()
                .copied(),
        );
        let runtime = OrbitRuntime::from_roots(&fixture.root, workspace_root)
            .unwrap()
            .with_actor(ActorIdentity::human("codebase-fixture"));
        runtime
            .add_task(TaskAddParams {
                title: "landed fixture".into(),
                context_files: vec!["file:src/lib.rs".into()],
                status: Some(TaskStatus::Done),
                ..Default::default()
            })
            .unwrap()
            .id
            .to_string()
    };
    fs::create_dir_all(fixture.repo.join("src")).unwrap();
    fs::write(fixture.repo.join("src/lib.rs"), "pub fn example() {}\n").unwrap();
    fs::write(fixture.repo.join("design.md"), "design\n").unwrap();
    crate::git_repo::run(&fixture.repo, &["add", "src/lib.rs", "design.md"]);
    crate::git_repo::run(
        &fixture.repo,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "landed fixture",
        ],
    );
    let commit = crate::git_repo::run(&fixture.repo, &["rev-parse", "HEAD"]);
    record_landing(&fixture.root, &task, &commit);
    let payload = fixture.json(&[
        "codebase",
        "related",
        "src/lib.rs",
        "--top",
        "1",
        "--min-support",
        "1",
        "--json",
    ]);
    assert_eq!(payload["window"], "30d");
    assert_eq!(payload["rows"].as_array().unwrap().len(), 1);
    let partner = &payload["rows"][0];
    assert_eq!(partner["path"], "design.md");
    assert_eq!(partner["jaccard"], 1.0);
    assert_eq!(partner["conditional_rate"], 1.0);
    assert_eq!(partner["support"]["both"], 1);
    assert_eq!(partner["footprint_miss_count"], 1);
    assert!(partner["reservation_coupling"].is_null());
    assert_eq!(partner["tasks"][0]["task_id"], task);
}

/// The bundle directory of task `id` under `root`.
fn bundle(root: &Path, id: &str) -> Option<PathBuf> {
    for entry in fs::read_dir(root).ok()? {
        let path = entry.ok()?.path();
        if !path.is_dir() {
            continue;
        }
        if path.file_name().is_some_and(|name| name == id)
            && path.join(TASK_EVENTS_FILE_NAME).is_file()
        {
            return Some(path);
        }
        if let Some(found) = bundle(&path, id) {
            return Some(found);
        }
    }
    None
}

fn append_row(path: PathBuf, row: &impl serde::Serialize) {
    writeln!(
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap(),
        "{}",
        serde_json::to_string(row).unwrap()
    )
    .unwrap();
}

/// Record a verified local landing of `commit` for task `id`.
fn record_landing(root: &Path, id: &str, commit: &str) {
    let path = bundle(root, id).unwrap().join(TASK_EVENTS_FILE_NAME);
    let rows = fs::read_to_string(&path).unwrap().lines().count();
    let row = TaskEventRowV2 { schema_version: TASK_ARTIFACT_SCHEMA_VERSION, event_id: format!("EV-{:04}", rows+1), at: chrono::Utc::now(), by: "system".into(), event_type: "landing_completed".into(), note: Some(json!({"delivery":"local_candidate","candidate":commit,"verified":"landing_ref_contains_candidate"}).to_string()), from_status: None, to_status: Some(TaskStatus::Done), operation_id: None, decision: None };
    append_row(path, &row);
}

/// `orbit codebase analytics` grades a task-pilot crew's selectors against
/// the landed diff, in its JSON and in its text table.
#[test]
fn codebase_cli_ranks_task_pilot_crews_against_the_landed_diff() {
    if crate::isolated_test::run_in_child(
        "codebase_cli::codebase_cli_ranks_task_pilot_crews_against_the_landed_diff",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let shown = fixture.json(&["workspace", "show", "--json"]);
    let workspace_root = Path::new(shown["orbit_root"].as_str().unwrap());
    let now = chrono::Utc::now();
    let task = {
        let _authority = orbit_common::test_env::unset(
            orbit_common::test_env::INHERITED_AUTHORITY_ENV
                .iter()
                .copied(),
        );
        let runtime = OrbitRuntime::from_roots(&fixture.root, workspace_root)
            .unwrap()
            .with_actor(ActorIdentity::human("codebase-fixture"));
        let started = now - chrono::Duration::minutes(10);
        let run = JobRun {
            run_id: "jrun-pilot-cli".into(),
            job_id: "task_pilot_pipeline".into(),
            attempt: 1,
            state: JobRunState::Success,
            scheduled_at: started,
            created_at: started,
            started_at: Some(started),
            finished_at: Some(now),
            duration_ms: None,
            pid: None,
            pid_start_time: None,
            input: None,
            retry_source_run_id: None,
            knowledge_metrics: None,
            resolved_crew: Some("luna".into()),
            crew_model: Some("luna-model".into()),
            steps: Vec::new(),
            executed_on: None,
        };
        runtime
            .sqlite_store()
            .unwrap()
            .upsert_job_run_for_workspace(&runtime.workspace_id().unwrap(), &run, None)
            .unwrap();
        runtime
            .add_task(TaskAddParams {
                title: "piloted fixture".into(),
                status: Some(TaskStatus::Done),
                ..Default::default()
            })
            .unwrap()
            .id
            .to_string()
    };
    let before = ["file:src/lib.rs"];
    let after = ["dir:src", "file:docs/unused.md"];
    let receipt = TaskCommentRowV2 {
        schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
        comment_id: "C-0001".into(),
        at: now - chrono::Duration::minutes(5),
        by: "task-pilot".into(),
        body: json!({
            "assessment": {"context_files_before": before, "context_files_after": after},
            "context_files_before": before,
            "pilot_run_id": "jrun-pilot-cli",
        })
        .to_string(),
        operation_id: Some(format!("{:064x}", 1)),
    };
    append_row(
        bundle(&fixture.root, &task)
            .unwrap()
            .join(TASK_COMMENTS_FILE_NAME),
        &receipt,
    );
    fs::create_dir_all(fixture.repo.join("src")).unwrap();
    fs::write(fixture.repo.join("src/lib.rs"), "pub fn example() {}\n").unwrap();
    fs::write(fixture.repo.join("src/main.rs"), "fn main() {}\n").unwrap();
    crate::git_repo::run(&fixture.repo, &["add", "src"]);
    crate::git_repo::run(
        &fixture.repo,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "piloted fixture",
        ],
    );
    let commit = crate::git_repo::run(&fixture.repo, &["rev-parse", "HEAD"]);
    record_landing(&fixture.root, &task, &commit);

    let selection = [
        "codebase",
        "analytics",
        "--window",
        "30d",
        "--min-support",
        "1",
    ];
    let payload = fixture.json(&[&selection[..], &["--json"]].concat());
    let row = &payload["task_pilot"]["rows"][0];
    assert_eq!(
        (row["crew"].as_str(), row["model"].as_str(), &row["support"]),
        (Some("luna"), Some("luna-model"), &json!(1)),
        "{payload}"
    );
    assert_eq!(
        row["recall"]["rate"], 1.0,
        "dir:src covers both landed files"
    );
    assert_eq!(
        row["precision"]["rate"], 0.5,
        "docs/unused.md covered nothing"
    );
    assert_eq!(row["author"]["recall"]["rate"], 0.5);
    assert_eq!(
        row["over_declarations"][0]["selectors"],
        json!(["file:docs/unused.md"])
    );

    let human = fixture
        .command(&selection)
        .env("NO_COLOR", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let human = String::from_utf8(human).unwrap();
    assert!(
        human.lines().any(|line| line.contains("luna")
            && line.contains("luna-model")
            && line.contains("67%")),
        "the text table ranks the crew by F1:\n{human}"
    );
}
