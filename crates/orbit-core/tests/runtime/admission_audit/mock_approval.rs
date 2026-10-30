//! Proposal approval validates a `verify-mock-ui` task's mocks against the
//! base view manifest before it leaves `proposed`, through every entry point
//! that promotes a proposal to backlog (visual-review §2.3).

use std::path::Path;

use orbit_common::OrbitError;
use orbit_core::OrbitRuntime;
use orbit_core::application::task::TaskAddParams;
use orbit_types::desktop::{DesktopTaskFields, DesktopTaskOperation, DesktopTaskRequest};
use orbit_types::task::TaskStatus;
use serde_json::json;

use super::desktop_support::session;
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
const PNG_FIXTURE: &[u8] = b"\x89PNG\r\n\x1a\nmock";

type ApprovalEntry = fn(&OrbitRuntime, &str) -> Result<(), OrbitError>;

fn tool_backlog_transition(runtime: &OrbitRuntime, id: &str) -> Result<(), OrbitError> {
    run_tool_as_operator(
        runtime,
        "orbit.task.update",
        json!({"id": id, "status": "backlog"}),
    )
    .map(|_| ())
}

fn operator_approval(runtime: &OrbitRuntime, id: &str) -> Result<(), OrbitError> {
    runtime
        .approve_task_screened(id, None, None, None, None)
        .map(|_| ())
}

fn automatic_promotion(runtime: &OrbitRuntime, id: &str) -> Result<(), OrbitError> {
    runtime.approve_task(id, None, None).map(|_| ())
}

/// The desktop approval: an `Edit` to `backlog`, read against the task's
/// current revision through the in-process desktop API.
fn desktop_backlog_edit(runtime: &OrbitRuntime, id: &str) -> Result<(), OrbitError> {
    let operator = session(true);
    let revision = runtime.desktop_task_snapshot(id, &operator)?.revision;
    runtime
        .desktop_task_write(
            DesktopTaskRequest {
                request_id: "desktop-proposal-approval".into(),
                operation: DesktopTaskOperation::Edit {
                    id: id.to_string(),
                    expected_revision: revision,
                    fields: DesktopTaskFields {
                        status: Some(TaskStatus::Backlog),
                        ..Default::default()
                    },
                },
            },
            None,
            None,
            &operator,
        )
        .map(|_| ())
}

const APPROVAL_ENTRY_POINTS: [(&str, ApprovalEntry); 4] = [
    (
        "task tool proposed-to-backlog transition",
        tool_backlog_transition,
    ),
    ("operator approval (--approve)", operator_approval),
    ("automatic proposal promotion", automatic_promotion),
    ("desktop proposal edit to backlog", desktop_backlog_edit),
];

/// A git-backed workspace. The base branch commits [`MANIFEST`] at
/// [`MANIFEST_PATH`] when `commit_manifest` is set, and the workspace config
/// lists `manifests` under `[visual_review]`.
fn mock_workspace(
    manifests: &[&str],
    commit_manifest: bool,
) -> (tempfile::TempDir, OrbitRuntime, std::path::PathBuf) {
    let config = format!(
        "[visual_review]\nmanifests = {}\n",
        serde_json::to_string(manifests).expect("manifest list")
    );
    let (root, runtime, repo) = runtime_with_workspace_config(Some(&config));
    git(&repo, &["init"]);
    git(&repo, &["checkout", "-b", "main"]);
    git(&repo, &["config", "user.name", "Orbit Test"]);
    git(&repo, &["config", "user.email", "orbit-test@example.com"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join(".gitignore"), ".orbit/\n").expect("gitignore");
    if commit_manifest {
        std::fs::create_dir_all(repo.join("docs")).expect("docs dir");
        std::fs::write(repo.join(MANIFEST_PATH), MANIFEST).expect("view manifest");
    }
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "seed view manifest"]);
    (root, runtime, repo)
}

fn proposal_with_mocks(runtime: &OrbitRuntime, repo: &Path, mocks: &[&str]) -> String {
    let id = runtime
        .add_task(TaskAddParams {
            title: "checkout mock review".to_string(),
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
    for path in mocks {
        let source = repo.join("mock-fixture.png");
        std::fs::write(&source, PNG_FIXTURE).expect("write mock fixture");
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
    id
}

/// Every entry point refuses a fresh proposal carrying `mocks`, naming
/// `detail`, and leaves that proposal `proposed`.
fn assert_refused_at_every_entry_point(
    runtime: &OrbitRuntime,
    repo: &Path,
    mocks: &[&str],
    detail: &str,
) {
    for (entry, approve) in APPROVAL_ENTRY_POINTS {
        let id = proposal_with_mocks(runtime, repo, mocks);
        let error = approve(runtime, &id).expect_err(entry);
        let message = error.to_string();
        assert!(
            message.contains("cannot move to backlog") && message.contains(detail),
            "{entry} refusal should name `{detail}`, got: {message}"
        );
        let status = runtime.get_task(&id).expect("read proposal").status;
        assert_eq!(
            status,
            TaskStatus::Proposed,
            "{entry} changed status despite the refusal"
        );
    }
}

#[test]
fn proposal_approval_refuses_a_verify_mock_ui_task_without_mocks() {
    if !crate::admission_audit::isolate(
        module_path!(),
        "proposal_approval_refuses_a_verify_mock_ui_task_without_mocks",
    ) {
        return;
    }

    let (_root, runtime, repo) = mock_workspace(&[MANIFEST_PATH], true);
    assert_refused_at_every_entry_point(&runtime, &repo, &[], "no_mocks");
}

#[test]
fn proposal_approval_refuses_a_verify_mock_ui_task_with_an_invalid_mock_name() {
    if !crate::admission_audit::isolate(
        module_path!(),
        "proposal_approval_refuses_a_verify_mock_ui_task_with_an_invalid_mock_name",
    ) {
        return;
    }

    let (_root, runtime, repo) = mock_workspace(&[MANIFEST_PATH], true);
    assert_refused_at_every_entry_point(
        &runtime,
        &repo,
        &["mocks/Checkout.png"],
        "invalid_mock_name",
    );
}

#[test]
fn proposal_approval_refuses_a_verify_mock_ui_task_when_the_manifest_is_unusable() {
    if !crate::admission_audit::isolate(
        module_path!(),
        "proposal_approval_refuses_a_verify_mock_ui_task_when_the_manifest_is_unusable",
    ) {
        return;
    }

    // Configured but absent from the base commit.
    let (_root, runtime, repo) = mock_workspace(&[MANIFEST_PATH], false);
    assert_refused_at_every_entry_point(&runtime, &repo, &["mocks/checkout.png"], MANIFEST_PATH);

    // No manifest configured at all.
    let (_root, runtime, repo) = mock_workspace(&[], true);
    assert_refused_at_every_entry_point(
        &runtime,
        &repo,
        &["mocks/checkout.png"],
        "no_view_manifest",
    );
}

/// The ordinary requeue (`someday → backlog`) shares the promotion check with
/// approval, so it keeps refusing a task whose mocks do not pass.
#[test]
fn an_ordinary_requeue_to_backlog_still_refuses_a_task_without_mocks() {
    if !crate::admission_audit::isolate(
        module_path!(),
        "an_ordinary_requeue_to_backlog_still_refuses_a_task_without_mocks",
    ) {
        return;
    }

    let (_root, runtime, _repo) = mock_workspace(&[MANIFEST_PATH], true);
    let id = runtime
        .add_task(TaskAddParams {
            title: "checkout mock requeue".to_string(),
            description: "requeues the checkout view from someday".to_string(),
            acceptance_criteria: vec!["checkout matches the mock".to_string()],
            plan: "build the checkout view".to_string(),
            tags: vec!["verify-mock-ui".to_string()],
            status: Some(TaskStatus::Someday),
            ..Default::default()
        })
        .expect("add someday task")
        .id
        .to_string();
    let error = tool_backlog_transition(&runtime, &id).expect_err("requeue without mocks");
    let message = error.to_string();
    assert!(
        message.contains("cannot move to backlog") && message.contains("no_mocks"),
        "requeue refusal should name `no_mocks`, got: {message}"
    );
    let status = runtime.get_task(&id).expect("read someday task").status;
    assert_eq!(
        status,
        TaskStatus::Someday,
        "requeue changed status despite the refusal"
    );
}

#[test]
fn a_valid_mocked_proposal_still_approves_at_every_entry_point() {
    if !crate::admission_audit::isolate(
        module_path!(),
        "a_valid_mocked_proposal_still_approves_at_every_entry_point",
    ) {
        return;
    }

    let (_root, runtime, repo) = mock_workspace(&[MANIFEST_PATH], true);
    for (entry, approve) in APPROVAL_ENTRY_POINTS {
        let id = proposal_with_mocks(&runtime, &repo, &["mocks/checkout.png"]);
        approve(&runtime, &id)
            .unwrap_or_else(|error| panic!("{entry} refused a valid mocked proposal: {error}"));
        let status = runtime.get_task(&id).expect("read approved task").status;
        assert_eq!(status, TaskStatus::Backlog, "{entry} did not approve");
    }
}
