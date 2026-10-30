//! Delivery admission freezes a `verify-mock-ui` task's mock digests on
//! every local route, and an active delivery's baseline cannot be
//! rewritten by a refused or repeated attempt (visual-review §2.4).

use std::path::{Path, PathBuf};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_core::application::task::TaskAddParams;
use orbit_core::{CompletionPolicy, OrbitRuntime, ShipMode};
use orbit_engine::RuntimeHost;
use orbit_types::task::{MOCK_DIGESTS_ARTIFACT, MockDigestRecord, TaskStatus, mock_sha256_hex};
use orbit_types::workflow::JobRunTrigger;
use serde_json::json;

use super::engine_support::runtime_with_workspace_config;
use super::review_support::git;
use crate::admission_audit::tool_support::run_tool_as_operator;

const MANIFEST_PATH: &str = "docs/views.toml";
const MANIFEST: &str = r#"
schema_version = 1

[app.web]
themes = ["light", "dark"]

[[view]]
id = "checkout"
app = "web"
"#;
const PNG_ONE: &[u8] = b"\x89PNG\r\n\x1a\nmock-one";
const PNG_TWO: &[u8] = b"\x89PNG\r\n\x1a\nmock-two-replaced";
const CREWS: &str = "[crews.coders]\nmodel = \"code-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n[workflow]\ndefault_crew = \"coders\"\n";

fn mock_workspace(extra_config: &str) -> (tempfile::TempDir, OrbitRuntime, PathBuf) {
    let config = format!("[visual_review]\nmanifests = [\"{MANIFEST_PATH}\"]\n{extra_config}");
    let (root, runtime, repo) = runtime_with_workspace_config(Some(&config));
    git(&repo, &["init"]);
    git(&repo, &["checkout", "-b", "main"]);
    git(&repo, &["config", "user.name", "Orbit Test"]);
    git(&repo, &["config", "user.email", "orbit-test@example.com"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join(".gitignore"), ".orbit/\n").expect("gitignore");
    std::fs::create_dir_all(repo.join("docs")).expect("docs dir");
    std::fs::write(repo.join(MANIFEST_PATH), MANIFEST).expect("view manifest");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "seed view manifest"]);
    (root, runtime, repo)
}

fn attach_mock(runtime: &OrbitRuntime, repo: &Path, id: &str, path: &str, bytes: &[u8]) {
    let source = repo.join("mock-fixture.png");
    std::fs::write(&source, bytes).expect("write mock fixture");
    run_tool_as_operator(
        runtime,
        "orbit.task.artifact.put",
        json!({
            "id": id,
            "source_path": source.to_string_lossy(),
            "path": path,
        }),
    )
    .expect("attach mock");
    std::fs::remove_file(&source).ok();
}

/// A `verify-mock-ui` task in the backlog carrying `mocks/checkout.png`.
fn backlog_task_with_mock(runtime: &OrbitRuntime, repo: &Path, bytes: &[u8]) -> String {
    let id = runtime
        .add_task(TaskAddParams {
            title: "checkout mock freeze".to_string(),
            description: "builds the checkout view against its mock".to_string(),
            acceptance_criteria: vec!["checkout matches the mock".to_string()],
            plan: "build the checkout view".to_string(),
            tags: vec!["verify-mock-ui".to_string()],
            status: Some(TaskStatus::Proposed),
            ..Default::default()
        })
        .expect("add proposed task")
        .id
        .to_string();
    attach_mock(runtime, repo, &id, "mocks/checkout.png", bytes);
    runtime
        .approve_task(&id, None, None)
        .expect("approve valid mocked task to backlog");
    assert_eq!(
        runtime.get_task(&id).expect("read task").status,
        TaskStatus::Backlog
    );
    id
}

fn digest_record(runtime: &OrbitRuntime, id: &str) -> Option<MockDigestRecord> {
    runtime
        .get_task_artifacts(id)
        .expect("read artifacts")
        .into_iter()
        .find(|artifact| artifact.path == MOCK_DIGESTS_ARTIFACT)
        .map(|artifact| {
            serde_json::from_slice(&artifact.content).expect("parse mock digest record")
        })
}

fn digest_record_bytes(runtime: &OrbitRuntime, id: &str) -> Option<Vec<u8>> {
    runtime
        .get_task_artifacts(id)
        .expect("read artifacts")
        .into_iter()
        .find(|artifact| artifact.path == MOCK_DIGESTS_ARTIFACT)
        .map(|artifact| artifact.content)
}

/// The shared workflow-admission transition every local leaf starts with
/// (explicit ship, automatic backlog discovery and local drains alike)
/// freezes the exact admitted bytes, keeps them while that delivery is
/// active, and re-freezes for a later delivery.
#[test]
fn workflow_admission_freezes_mock_digests_for_each_delivery() {
    if !crate::admission_audit::isolate(
        module_path!(),
        "workflow_admission_freezes_mock_digests_for_each_delivery",
    ) {
        return;
    }

    let (_root, runtime, repo) = mock_workspace("");
    let id = backlog_task_with_mock(&runtime, &repo, PNG_ONE);
    assert!(digest_record(&runtime, &id).is_none());

    RuntimeHost::admit_task_for_workflow(&runtime, &id, "worktree_setup").expect("admit leaf");
    assert_eq!(
        runtime.get_task(&id).expect("read task").status,
        TaskStatus::InProgress
    );
    let frozen = digest_record(&runtime, &id).expect("admission froze mock digests");
    assert_eq!(frozen.mocks.len(), 1);
    assert_eq!(frozen.mocks[0].mock.artifact, "mocks/checkout.png");
    assert_eq!(frozen.mocks[0].sha256, mock_sha256_hex(PNG_ONE));

    // A mock replaced mid-delivery, and a repeated admission of the same
    // active delivery, leave the frozen baseline untouched.
    attach_mock(&runtime, &repo, &id, "mocks/checkout.png", PNG_TWO);
    RuntimeHost::admit_task_for_workflow(&runtime, &id, "worktree_setup")
        .expect("re-admit active leaf");
    let still = digest_record(&runtime, &id).expect("baseline still frozen");
    assert_eq!(still, frozen, "active delivery baseline was rewritten");

    // A later delivery (the task returned to the backlog first) freezes
    // the mocks it actually admits.
    run_tool_as_operator(
        &runtime,
        "orbit.task.update",
        json!({"id": id, "status": "backlog"}),
    )
    .expect("return task to backlog for a later delivery");
    RuntimeHost::admit_task_for_workflow(&runtime, &id, "worktree_setup")
        .expect("admit later delivery");
    let refrozen = digest_record(&runtime, &id).expect("later delivery froze mock digests");
    assert_eq!(refrozen.mocks[0].sha256, mock_sha256_hex(PNG_TWO));
}

/// Workflow admission refuses a `verify-mock-ui` task whose mocks no
/// longer validate, before it starts and without freezing anything.
#[test]
fn workflow_admission_refuses_invalid_mocks_without_freezing() {
    if !crate::admission_audit::isolate(
        module_path!(),
        "workflow_admission_refuses_invalid_mocks_without_freezing",
    ) {
        return;
    }

    let (_root, runtime, repo) = mock_workspace("");
    let id = backlog_task_with_mock(&runtime, &repo, PNG_ONE);
    attach_mock(&runtime, &repo, &id, "mocks/Checkout.png", PNG_TWO);

    let refused = RuntimeHost::admit_task_for_workflow(&runtime, &id, "worktree_setup")
        .expect_err("invalid mocks refuse workflow admission");
    assert!(
        refused.to_string().contains("invalid_mock_name"),
        "{refused}"
    );
    assert_eq!(
        runtime.get_task(&id).expect("read task").status,
        TaskStatus::Backlog
    );
    assert!(digest_record(&runtime, &id).is_none());
}

fn write_ship_job_asset(runtime: &OrbitRuntime) {
    let asset = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/jobs/task_auto_pipeline.yaml");
    let yaml = std::fs::read_to_string(asset).expect("read shipped ship job asset");
    let jobs_dir = runtime.global_root().join("resources/jobs");
    std::fs::create_dir_all(&jobs_dir).expect("create jobs dir");
    std::fs::write(jobs_dir.join("task_auto_pipeline.yaml"), yaml).expect("write ship job asset");
}

fn seed_running_ship_run(runtime: &OrbitRuntime, task_id: &str) -> String {
    let jobs = crate::admission_audit::store_support::jobs(runtime);
    let run = jobs
        .insert_job_run(
            "task_auto_pipeline",
            1,
            Utc::now(),
            Some(json!({"task_ids": [task_id], "mode": "local"})),
            None,
        )
        .expect("insert running ship run");
    jobs.mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .expect("start ship run");
    run.run_id
}

fn ship(
    runtime: &OrbitRuntime,
    task_id: &str,
) -> Result<orbit_core::PipelineInvokeResult, OrbitError> {
    runtime.submit_ship_run(
        ShipMode::Local,
        Some("main"),
        &[task_id.to_string()],
        CompletionPolicy::Review,
        &[],
        Some("test"),
        None,
        JobRunTrigger::cli(),
    )
}

/// An explicit ship freezes the named task's digests keyed to its run, and
/// a duplicate ship refused as already in flight cannot rewrite that
/// baseline with newer mock bytes.
#[test]
fn explicit_ship_freezes_and_a_refused_duplicate_cannot_rewrite_it() {
    if !crate::admission_audit::isolate(
        module_path!(),
        "explicit_ship_freezes_and_a_refused_duplicate_cannot_rewrite_it",
    ) {
        return;
    }

    let (root, runtime, repo) = mock_workspace(CREWS);
    let kit_home = root.path().join("kit-home");
    std::fs::create_dir_all(&kit_home).expect("kit home");
    crate::host_ui_routing::write_kit(&kit_home, true);
    let runtime = runtime.with_browser_kit_home(&kit_home);
    write_ship_job_asset(&runtime);
    orbit_core::test_support::install_substitute_pipeline_worker([root
        .path()
        .join("worker-must-not-start")
        .to_string_lossy()
        .into_owned()]);

    let id = backlog_task_with_mock(&runtime, &repo, PNG_ONE);
    // The fixture worker cannot start; an admitted submission persists its
    // run and may then report that startup failure. The freeze is part of
    // admission, before the worker would start.
    match ship(&runtime, &id) {
        Ok(_) | Err(OrbitError::Execution(_)) => {}
        Err(other) => panic!("explicit ship admits the task: {other:?}"),
    }
    let frozen = digest_record(&runtime, &id).expect("explicit ship froze mock digests");
    assert_eq!(frozen.mocks[0].sha256, mock_sha256_hex(PNG_ONE));
    let admission_id = frozen
        .admission_id
        .clone()
        .filter(|value| value.starts_with("ship:"))
        .expect("baseline is keyed to the ship admission");
    let runs = runtime
        .list_job_runs(orbit_core::application::job::JobRunListParams::default())
        .expect("list runs");
    assert!(
        runs.iter().any(|run| {
            run.input
                .as_ref()
                .and_then(|input| input.get("__mock_admissions"))
                .and_then(|admissions| admissions.get(id.as_str()))
                .and_then(|token| token.as_str())
                == Some(admission_id.as_str())
        }),
        "the admitted run carries the freeze token: {runs:?}"
    );
    let frozen_bytes = digest_record_bytes(&runtime, &id);

    // The delivery is in flight; a duplicate ship with newer mock bytes
    // is refused and leaves the baseline byte-identical.
    seed_running_ship_run(&runtime, &id);
    attach_mock(&runtime, &repo, &id, "mocks/checkout.png", PNG_TWO);
    let refused = ship(&runtime, &id).expect_err("duplicate ship is refused");
    assert!(
        matches!(refused, OrbitError::ShipRunInFlight { .. }),
        "expected the in-flight refusal, got {refused:?}"
    );
    assert_eq!(
        digest_record_bytes(&runtime, &id),
        frozen_bytes,
        "a refused duplicate rewrote the active baseline"
    );
}
