//! Composed, synchronous feasibility fixture. This proves reuse of task/audit/job
//! machinery, not production profile admission or detached-worker propagation.

use std::fs;
use std::path::Path;
use std::process::Command;

use orbit_engine::{V2AuditWriter, execute_job_with_resume};
use orbit_types::task::{TaskStatus, TaskType};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::OrbitRuntime;
use crate::application::task::{TaskAddParams, TaskUpdateParams};
use crate::runtime::OrbitRuntimeRoots;

fn digest(path: &Path) -> String {
    format!(
        "{:x}",
        Sha256::digest(fs::read(path).expect("read fixture artifact"))
    )
}

#[test]
fn research_product_fixture_reuses_local_tasks_jobs_and_audit_without_exposing_a_product() {
    const CHILD: &str = "ORBIT_TEST_RESEARCH_COMPOSED_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let home = tempfile::tempdir().expect("isolated child home");
        let mut command = Command::new(std::env::current_exe().expect("test executable"));
        orbit_common::test_env::clear_inherited_authority(|key| {
            command.env_remove(key);
        });
        let output = command
            .args(["--exact", "tests::composition::research_product_fixture_reuses_local_tasks_jobs_and_audit_without_exposing_a_product", "--nocapture"])
            .env(CHILD, "1")
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .current_dir(home.path())
            .output()
            .expect("run isolated fixture");
        assert!(
            output.status.success(),
            "child failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let root = tempfile::tempdir().expect("fixture root");
    let roots = OrbitRuntimeRoots {
        global_root: root.path().join("research-global"),
        shared_root: root.path().join("research-workspace"),
        local_root: root.path().join("research-workspace"),
    };
    let runtime =
        OrbitRuntime::initialize_research_fixture(roots.clone()).expect("research fixture");
    let task = runtime.add_task(TaskAddParams {
        title: "Inspect a synthetic result".into(),
        description: "Deterministic research fixture; no provider.".into(),
        plan: "Read the task through the job engine, validate the fixture receipt, leave review evidence.".into(),
        task_type: Some(TaskType::Chore),
        status: Some(TaskStatus::InProgress),
        ..Default::default()
    }).expect("persist task");
    let mut document: serde_yaml::Value = serde_yaml::from_str(
        r#"
schemaVersion: 2
kind: Job
metadata:
  name: research_fixture
spec:
  state: enabled
  kind: workflow
  steps:
    - id: read_task
      spec:
        type: deterministic
        action: orbit_tool_call
        config:
          tool_name: orbit.task.show
          args:
            id: "{{ input.task_id }}"
            model: codex
"#,
    )
    .expect("fixture job document");
    document["spec"]["steps"][0]["spec"]["config"]["args"]["id"] =
        serde_yaml::Value::from(task.id.clone());
    let job: orbit_types::workflow::activity_job::JobV2 =
        serde_yaml::from_value(document["spec"].clone()).expect("fixture job");
    let writer = V2AuditWriter::with_disk_sinks(
        &runtime.paths().audit_dir,
        runtime.v2_audit_store().expect("audit store"),
        runtime.workspace_id().expect("workspace"),
        "research-fixture-run",
        "research-fixture",
        Some(root.path()),
    )
    .expect("audit writer");
    let outcome = execute_job_with_resume(
        &job,
        json!({"task_id": task.id}),
        "research-fixture-run",
        writer,
        &runtime,
        None,
    )
    .expect("execute existing job engine");
    assert!(outcome.success, "{outcome:?}");
    assert!(!outcome.degraded_audit, "audit must persist");
    assert!(
        outcome.pipeline.to_string().contains(&task.id),
        "job must read the persisted task"
    );

    // The receipt writer/check below deliberately stands in for the downstream
    // research owner adapter. It does NOT claim an implemented delivery gate.
    let note = root.path().join("result.md");
    fs::write(
        &note,
        "Synthetic result: inconclusive; no scientific assessment.\n",
    )
    .expect("note");
    let receipt = json!({"task_id": task.id, "run_id": "research-fixture-run", "result_sha256": digest(&note)});
    let receipt_path = roots.shared_root.join("fixture-receipt.json");
    fs::write(
        &receipt_path,
        serde_json::to_vec(&receipt).expect("receipt JSON"),
    )
    .expect("receipt");
    fs::write(&note, "tampered\n").expect("tamper fixture");
    assert_ne!(receipt["result_sha256"], digest(&note));
    assert_eq!(
        runtime
            .get_task(&task.id)
            .expect("task after tamper")
            .status,
        TaskStatus::InProgress
    );
    fs::write(
        &note,
        "Synthetic result: inconclusive; no scientific assessment.\n",
    )
    .expect("restore exact bytes");
    assert_eq!(receipt["result_sha256"], digest(&note));
    runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Review),
                execution_summary: Some(
                    "Synthetic fixture receipt verified; this is not scientific support.".into(),
                ),
                ..Default::default()
            },
        )
        .expect("persist review evidence");
    drop(runtime);
    let reopened =
        OrbitRuntime::initialize_research_fixture(roots.clone()).expect("reopen fixture");
    assert_eq!(
        reopened.get_task(&task.id).expect("persisted task").status,
        TaskStatus::Review
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(
            &fs::read(receipt_path).expect("persisted receipt")
        )
        .expect("receipt JSON"),
        receipt
    );
    assert!(
        !roots
            .global_root
            .join("resources/jobs/task_pr_pipeline.yaml")
            .exists()
    );
    assert!(
        !roots
            .shared_root
            .join("auto_tasks/code-review.yaml")
            .exists()
    );
}
