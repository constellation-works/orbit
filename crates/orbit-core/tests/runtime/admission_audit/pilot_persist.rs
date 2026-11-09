//! Prepare/apply classification of status changes and material drift.

use std::path::{Path, PathBuf};

use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::task::{Task, TaskComplexity, TaskStatus};
use serde_json::{Value, json};
use tempfile::TempDir;

use super::engine_support::runtime_with_workspace_config;
use orbit_core::OrbitRuntime;
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};

const MATERIAL_DETAIL: &str = "task meaning or dependency evidence changed after preparation";

pub(super) struct Workspace {
    _root: TempDir,
    pub(super) runtime: OrbitRuntime,
    pub(super) repo: PathBuf,
}

pub(super) fn workspace(config_toml: Option<&str>) -> Workspace {
    let (root, runtime, _) = runtime_with_workspace_config(config_toml);
    let repo = runtime.paths().repo_root.clone();
    git(&repo, &["init"]);
    git(&repo, &["checkout", "-b", "main"]);
    git(&repo, &["config", "user.name", "Orbit Test"]);
    git(&repo, &["config", "user.email", "orbit-test@example.com"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join(".gitignore"), ".orbit/\n").expect("gitignore");
    std::fs::write(repo.join("README.md"), "fixture\n").expect("readme");
    git(&repo, &["add", ".gitignore", "README.md"]);
    git(&repo, &["commit", "-m", "seed"]);
    Workspace {
        _root: root,
        runtime,
        repo,
    }
}

fn git(repo: &Path, args: &[&str]) {
    let output = crate::git_test_support::command()
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "Orbit Test")
        .env("GIT_AUTHOR_EMAIL", "orbit-test@example.com")
        .env("GIT_COMMITTER_NAME", "Orbit Test")
        .env("GIT_COMMITTER_EMAIL", "orbit-test@example.com")
        .output()
        .unwrap_or_else(|error| panic!("spawn git {}: {error}", args.join(" ")));
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn add_task(runtime: &OrbitRuntime, title: &str, status: TaskStatus) -> Task {
    runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: format!("{title} description"),
            acceptance_criteria: vec![format!("{title} is done")],
            plan: format!("{title} plan"),
            status: Some(status),
            ..Default::default()
        })
        .expect("add task")
}

fn prepare(workspace: &Workspace, task_id: &str) -> Value {
    let path = workspace.repo.canonicalize().expect("canonicalize repo");
    workspace
        .runtime
        .run_deterministic(
            "prepare_task_pilot",
            &json!({}),
            &json!({
                "workspace_path": path,
                "task_ids": [task_id],
                "base_branch": "main",
            }),
            ToolContext::default(),
        )
        .expect("prepare task pilot")
}

fn apply(workspace: &Workspace, prepared: &Value) -> Result<Value, String> {
    apply_naming_blockers(&workspace.runtime, prepared, &[])
}

fn apply_naming_blockers(
    runtime: &OrbitRuntime,
    prepared: &Value,
    blocked_by: &[String],
) -> Result<Value, String> {
    let task = &prepared["tasks"][0];
    let task_id = task["task_id"].as_str().expect("prepared task id");
    let assessment = json!({
        "task_id": task_id,
        "context_files_before": task["context_files_before"],
        "context_files_after": ["file:README.md"],
        "disposition": "selectors",
        "recommended_crew": "opus",
        "recommended_complexity": "low",
        "assessment_rationale": "README.md is the assessed selector.",
        "validation_approach": "Run the prepare and apply actions.",
        "confidence": "high",
        "evidence_gaps": [],
        "reassessment_triggers": [],
        "blocked_by": blocked_by,
        "adr_conflicts": [],
        "utility_warnings": [],
        "surface_warnings": [],
        "duplicate_of": Value::Null,
        "already_landed": Value::Null,
    });
    // The fixture pilot read its checkout: report the observed pair of
    // the prepared revision and its tree.
    let revision = prepared["source"]["source_revision"]
        .as_str()
        .expect("prepared source revision")
        .to_string();
    let tree = {
        let repo = prepared["workspace_path"].as_str().expect("workspace path");
        let output = crate::git_test_support::command()
            .args(["rev-parse", &format!("{revision}^{{tree}}")])
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    };
    runtime
        .run_deterministic(
            "apply_task_pilot_results",
            &json!({}),
            &json!({
                "workspace_path": prepared["workspace_path"],
                "prepared": prepared,
                "results": [{
                    "partition_index": 0,
                    "observed_revision": revision,
                    "observed_tree": tree,
                    "task_ids": [task_id],
                    "tasks": [assessment],
                }],
            }),
            ToolContext::default(),
        )
        .map_err(|error| error.to_string())
}

fn assert_refused(
    workspace: &Workspace,
    before: &Task,
    output: &Value,
    expected_outcome: &str,
    reason: &str,
    detail: &str,
) {
    assert_eq!(output["applied_count"], 0, "{output}");
    let status = if expected_outcome == "superseded" {
        "succeeded"
    } else {
        "failed"
    };
    assert_eq!(output["status"], status, "{output}");
    let outcome = &output["task_outcomes"][0];
    assert_eq!(outcome["outcome"], expected_outcome, "{outcome}");
    assert_eq!(outcome["reason"], reason, "{outcome}");
    assert_eq!(outcome["detail"], detail, "{outcome}");
    if expected_outcome == "stale" {
        let error = output["error"].as_str().expect("apply error");
        assert!(error.contains(reason) && error.contains(detail), "{error}");
    } else {
        assert!(output["error"].is_null());
    }
    let after = workspace.runtime.get_task(&before.id).expect("task");
    assert_eq!(after.status, before.status);
    assert_eq!(after.title, before.title);
    assert_eq!(after.description, before.description);
    assert_eq!(after.acceptance_criteria, before.acceptance_criteria);
    assert_eq!(after.plan, before.plan);
    assert_eq!(after.context_files, before.context_files);
    assert_eq!(after.complexity, before.complexity);
    assert_eq!(after.crew, before.crew);
    assert_eq!(after.dependencies(), before.dependencies());
    let history = workspace
        .runtime
        .get_task_history(&before.id)
        .expect("history");
    assert!(
        history
            .iter()
            .all(|entry| entry.event != "task_pilot_applied"),
        "apply wrote the task"
    );
}

fn assert_applied(workspace: &Workspace, task_id: &str, output: &Value) {
    assert_eq!(output["status"], "succeeded", "{output}");
    assert_eq!(output["applied_count"], 1, "{output}");
    let task = workspace.runtime.get_task(task_id).expect("task");
    assert_eq!(task.context_files, vec!["file:README.md".to_string()]);
    assert_eq!(task.complexity, Some(TaskComplexity::Low));
    let comments = workspace.runtime.get_task_comments(task_id).unwrap();
    let comment = comments
        .iter()
        .rev()
        .find(|comment| comment.by == "task-pilot")
        .unwrap();
    assert_eq!(
        comment.message.lines().count(),
        1,
        "pilot audits must not consume the human comment budget"
    );
    let reference: Value = serde_json::from_str(&comment.message).unwrap();
    let path = reference["assessment_artifact"]
        .as_str()
        .expect("full pilot audit belongs in an artifact");
    assert!(path.starts_with("pilot/assessment-"));
    assert_eq!(
        reference["operation_id"].as_str(),
        comment.receipt().map(|receipt| receipt.operation_id)
    );
    assert!(reference["summary"].as_str().unwrap().contains("selectors"));
    let audit = crate::task_pilot::pilot_audit(&workspace.runtime, task_id, comment);
    assert_eq!(audit["assessment"]["task_id"], task_id);
    // A normal attachment must not make trusted pilot evidence unreadable.
    // Reproduced through the public update API during artifact QA: replacing
    // the bytes previously succeeded, then even a repair update was refused.
    let replacement = workspace
        .runtime
        .update_task_as_human(
            task_id,
            TaskUpdateParams {
                upsert_artifacts: vec![orbit_types::task::TaskArtifact {
                    path: path.into(),
                    media_type: "application/json".into(),
                    content: b"{}".to_vec(),
                    created_by: None,
                }],
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .unwrap_err();
    assert!(
        replacement.to_string().contains("sealed evidence"),
        "{replacement}"
    );
    assert_eq!(
        crate::task_pilot::pilot_audit(&workspace.runtime, task_id, comment),
        audit
    );

    assert_eq!(
        audit["assessment"]["context_files_after"],
        json!(["file:README.md"])
    );

    let history = workspace
        .runtime
        .get_task_history(task_id)
        .expect("history");
    assert!(
        history
            .iter()
            .any(|entry| entry.event == "task_pilot_applied"),
        "apply did not record the write"
    );
}

fn meaning_edits_name_the_drifted_component_and_write_nothing() {
    let workspace = workspace(None);
    let cases = [
        ("title", "title"),
        ("description", "description"),
        ("criteria", "criteria"),
        ("plan", "plan"),
        ("selectors", "selectors"),
    ];
    for (label, component) in cases {
        let task = add_task(&workspace.runtime, label, TaskStatus::Backlog);
        let prepared = prepare(&workspace, &task.id);
        let edited = workspace
            .runtime
            .force_update_task_with_identity(&task.id, edit_params(label), None, None)
            .expect("edit");
        let output = apply(&workspace, &prepared).expect("apply");
        assert_refused(
            &workspace,
            &edited,
            &output,
            "superseded",
            "material_changed",
            &format!("{MATERIAL_DETAIL}: {component}"),
        );
    }

    let task = add_task(&workspace.runtime, "both", TaskStatus::Backlog);
    let prepared = prepare(&workspace, &task.id);
    let edited = workspace
        .runtime
        .force_update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                title: Some("both retitled".to_string()),
                plan: Some("both replanned".to_string()),
                ..Default::default()
            },
            None,
            None,
        )
        .expect("edit title and plan");
    let output = apply(&workspace, &prepared).expect("apply");
    assert_refused(
        &workspace,
        &edited,
        &output,
        "superseded",
        "material_changed",
        &format!("{MATERIAL_DETAIL}: plan, title"),
    );
}

fn edit_params(label: &str) -> TaskUpdateParams {
    match label {
        "title" => TaskUpdateParams {
            title: Some("retitled".to_string()),
            ..Default::default()
        },
        "description" => TaskUpdateParams {
            description: Some("rewritten description".to_string()),
            ..Default::default()
        },
        "criteria" => TaskUpdateParams {
            acceptance_criteria: Some(vec!["a different criterion".to_string()]),
            ..Default::default()
        },
        "plan" => TaskUpdateParams {
            plan: Some("a different plan".to_string()),
            ..Default::default()
        },
        "selectors" => TaskUpdateParams {
            context_files: Some(vec!["file:README.md".to_string()]),
            ..Default::default()
        },
        _ => panic!("unknown edit {label}"),
    }
}

fn context_files_outside_freshness_stay_context_files_changed() {
    let workspace = workspace(Some(
        "[workflow.task_pilot_freshness]\nmaterial_fields = [\"title\", \"description\", \"criteria\", \"plan\"]\n",
    ));
    let task = add_task(&workspace.runtime, "selectors", TaskStatus::Backlog);
    let prepared = prepare(&workspace, &task.id);
    let edited = workspace
        .runtime
        .force_update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                context_files: Some(vec!["file:README.md".to_string()]),
                ..Default::default()
            },
            None,
            None,
        )
        .expect("edit selectors");
    let output = apply(&workspace, &prepared).expect("apply");
    assert_refused(
        &workspace,
        &edited,
        &output,
        "superseded",
        "context_files_changed",
        "task context_files changed after preparation",
    );
}

fn task_prepared_in_progress_is_not_rewritten() {
    let workspace = workspace(None);
    let task = add_task(&workspace.runtime, "active", TaskStatus::Backlog);
    let mut prepared = prepare(&workspace, &task.id);
    // Older preparations could capture active work. Keep exercising apply's
    // persisted-input fence even though prepare now refuses that selection.
    prepared["tasks"][0]["status"] = json!(TaskStatus::InProgress);
    workspace
        .runtime
        .force_update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::InProgress),
                ..Default::default()
            },
            None,
            None,
        )
        .expect("start");
    let started = workspace.runtime.get_task(&task.id).expect("started");
    let output = apply(&workspace, &prepared).expect("apply");
    assert_refused(
        &workspace,
        &started,
        &output,
        "stale",
        "status_not_mutable",
        "task-pilot does not rewrite in-progress, review, or terminal work",
    );
}

fn proposed_to_backlog_still_applies() {
    let workspace = workspace(None);
    let task = add_task(&workspace.runtime, "promote", TaskStatus::Proposed);
    let prepared = prepare(&workspace, &task.id);
    workspace
        .runtime
        .force_update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            },
            None,
            None,
        )
        .expect("move to backlog");
    let output = apply(&workspace, &prepared).expect("apply");
    assert_applied(&workspace, &task.id, &output);
    assert_eq!(
        workspace.runtime.get_task(&task.id).expect("task").status,
        TaskStatus::Backlog
    );
}

fn malformed_component_map_fails_apply_without_writing() {
    let workspace = workspace(None);
    let task = add_task(&workspace.runtime, "malformed", TaskStatus::Backlog);
    let before = workspace.runtime.get_task(&task.id).expect("task");
    let mut prepared = prepare(&workspace, &task.id);
    prepared["tasks"][0]["material_components"] = json!("not-an-object");
    let error = apply(&workspace, &prepared).expect_err("malformed components");
    assert!(error.contains("material_components"), "{error}");
    assert_eq!(
        workspace
            .runtime
            .get_task(&task.id)
            .expect("task")
            .complexity,
        before.complexity
    );
    let history = workspace
        .runtime
        .get_task_history(&task.id)
        .expect("history");
    assert!(
        history
            .iter()
            .all(|entry| entry.event != "task_pilot_applied")
    );
}

const CREW_AND_DEPENDENCIES: &str = "\
[workflow.task_pilot_freshness]
material_fields = [\"title\", \"description\", \"criteria\", \"plan\", \"selectors\", \"crew\", \"dependencies\"]
";

fn configured_crew_and_dependency_meaning_drift_are_named() {
    let workspace = workspace(Some(CREW_AND_DEPENDENCIES));
    let task = workspace
        .runtime
        .add_task(TaskAddParams {
            title: "crew".to_string(),
            description: "crew description".to_string(),
            acceptance_criteria: vec!["crew is resolved".to_string()],
            plan: "crew plan".to_string(),
            status: Some(TaskStatus::Backlog),
            crew: Some("opus".to_string()),
            ..Default::default()
        })
        .expect("add crewed task");
    let prepared = prepare(&workspace, &task.id);
    let edited = workspace
        .runtime
        .force_update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                crew: Some(Some("luna".to_string())),
                ..Default::default()
            },
            None,
            None,
        )
        .expect("change crew");
    let output = apply(&workspace, &prepared).expect("apply");
    assert_refused(
        &workspace,
        &edited,
        &output,
        "superseded",
        "material_changed",
        &format!("{MATERIAL_DETAIL}: crew"),
    );

    let dependency = add_task(&workspace.runtime, "dependency", TaskStatus::Backlog);
    let main = workspace
        .runtime
        .add_task(TaskAddParams {
            title: "dependent".to_string(),
            description: "dependent description".to_string(),
            acceptance_criteria: vec!["dependency meaning is stable".to_string()],
            plan: "dependent plan".to_string(),
            status: Some(TaskStatus::Backlog),
            dependencies: vec![dependency.id.clone()],
            ..Default::default()
        })
        .expect("add dependent task");
    let prepared = prepare(&workspace, &main.id);
    workspace
        .runtime
        .force_update_task_with_identity(
            &dependency.id,
            TaskUpdateParams {
                description: Some("dependency meaning changed".to_string()),
                ..Default::default()
            },
            None,
            None,
        )
        .expect("edit dependency");
    let before = workspace.runtime.get_task(&main.id).expect("dependent");
    let output = apply(&workspace, &prepared).expect("apply");
    assert_refused(
        &workspace,
        &before,
        &output,
        "stale",
        "material_changed",
        &format!("{MATERIAL_DETAIL}: dependencies"),
    );
}

fn dependency_status_alone_still_applies_and_is_not_named_with_a_title_edit() {
    let workspace = workspace(Some(CREW_AND_DEPENDENCIES));
    let dependency = add_task(&workspace.runtime, "status dependency", TaskStatus::Backlog);
    let main = workspace
        .runtime
        .add_task(TaskAddParams {
            title: "status main".to_string(),
            description: "status main description".to_string(),
            acceptance_criteria: vec!["dependency status can move".to_string()],
            plan: "status main plan".to_string(),
            status: Some(TaskStatus::Backlog),
            dependencies: vec![dependency.id.clone()],
            ..Default::default()
        })
        .expect("add dependent task");
    let prepared = prepare(&workspace, &main.id);
    workspace
        .runtime
        .force_update_task_with_identity(
            &dependency.id,
            TaskUpdateParams {
                status: Some(TaskStatus::InProgress),
                ..Default::default()
            },
            None,
            None,
        )
        .expect("start dependency");
    let output = apply(&workspace, &prepared).expect("apply");
    assert_applied(&workspace, &main.id, &output);

    let dependency = add_task(&workspace.runtime, "mixed dependency", TaskStatus::Backlog);
    let main = workspace
        .runtime
        .add_task(TaskAddParams {
            title: "mixed main".to_string(),
            description: "mixed main description".to_string(),
            acceptance_criteria: vec!["title drift is not a dependency edit".to_string()],
            plan: "mixed main plan".to_string(),
            status: Some(TaskStatus::Backlog),
            dependencies: vec![dependency.id.clone()],
            ..Default::default()
        })
        .expect("add mixed task");
    let prepared = prepare(&workspace, &main.id);
    workspace
        .runtime
        .force_update_task_with_identity(
            &dependency.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Done),
                ..Default::default()
            },
            None,
            None,
        )
        .expect("finish dependency");
    let edited = workspace
        .runtime
        .force_update_task_with_identity(
            &main.id,
            TaskUpdateParams {
                title: Some("mixed main retitled".to_string()),
                ..Default::default()
            },
            None,
            None,
        )
        .expect("retitle");
    let output = apply(&workspace, &prepared).expect("apply");
    assert_refused(
        &workspace,
        &edited,
        &output,
        "superseded",
        "material_changed",
        &format!("{MATERIAL_DETAIL}: title"),
    );
}

/// A proposed task whose pilot assessment artifact has gone missing (the
/// state a restore without attachments leaves behind) must hold only its own
/// readout. List and show keep serving every task, and the broken task reports
/// its audit as unavailable instead of failing the page.
fn missing_assessment_artifact_degrades_only_its_own_readout() {
    let workspace = workspace(None);
    let broken = add_task(&workspace.runtime, "broken audit", TaskStatus::Proposed);
    let healthy = add_task(&workspace.runtime, "healthy", TaskStatus::Proposed);
    let output = apply(&workspace, &prepare(&workspace, &broken.id)).expect("apply");
    assert_eq!(output["status"], "succeeded", "{output}");
    let comment = workspace
        .runtime
        .get_task_comments(&broken.id)
        .expect("comments")
        .into_iter()
        .rev()
        .find(|comment| comment.by == "task-pilot")
        .expect("pilot receipt");
    let reference: Value = serde_json::from_str(&comment.message).expect("reference");
    let path = reference["assessment_artifact"]
        .as_str()
        .expect("assessment artifact path")
        .to_string();
    crate::artifact_tools::drop_artifact_manifest_entry(
        &workspace.runtime.global_root(),
        &broken.id,
        &path,
    );
    assert!(
        workspace
            .runtime
            .get_task_artifact(&broken.id, &path)
            .expect("artifact read")
            .is_none(),
        "the assessment artifact must now be missing"
    );

    let listed = workspace
        .runtime
        .run_tool("orbit.task.list", json!({"status": "proposed"}))
        .expect("list must survive one unreadable pilot audit");
    let rows = listed["tasks"].as_array().expect("task rows");
    let row = |id: &str| {
        rows.iter()
            .find(|row| row["id"] == id)
            .unwrap_or_else(|| panic!("{id} missing from list: {listed}"))
    };
    assert_eq!(row(&healthy.id)["pilot_warnings"], json!([]), "{listed}");
    assert_eq!(
        row(&broken.id)["pilot_warnings"][0]["kind"],
        "pilot_assessment_unavailable",
        "{listed}"
    );

    let shown = workspace
        .runtime
        .run_tool("orbit.task.show", json!({"id": broken.id}))
        .expect("show must survive one unreadable pilot audit");
    assert_eq!(
        shown["pilot_warnings"][0]["kind"], "pilot_assessment_unavailable",
        "{shown}"
    );
    assert_eq!(
        shown["resolvable_pilot_admission_hold"],
        json!(false),
        "{shown}"
    );
}

fn unreadable_assessment_degrades_show_and_refuses_admission() {
    for oversized in [false, true] {
        let workspace = workspace(None);
        let task = add_task(&workspace.runtime, "unreadable audit", TaskStatus::Proposed);
        let output = apply(&workspace, &prepare(&workspace, &task.id)).expect("apply");
        assert_eq!(output["status"], "succeeded", "{output}");
        let comment = workspace
            .runtime
            .get_task_comments(&task.id)
            .expect("comments")
            .into_iter()
            .rev()
            .find(|comment| comment.by == "task-pilot")
            .expect("pilot receipt");
        let reference: Value = serde_json::from_str(&comment.message).expect("reference");
        let path = reference["assessment_artifact"]
            .as_str()
            .expect("artifact path");
        let blob = crate::artifact_tools::find_artifact_blob(
            &workspace.runtime.global_root(),
            &task.id,
            path,
        );
        if oversized {
            let content =
                vec![b'x'; orbit_types::task::MAX_TASK_ARTIFACT_CONTENT_BYTES as usize + 1];
            std::fs::write(&blob, &content).expect("oversize blob");
            let manifest_path = blob
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("manifest.yaml");
            let mut manifest: orbit_types::task::ArtifactManifestV2 =
                serde_yaml::from_str(&std::fs::read_to_string(&manifest_path).expect("manifest"))
                    .expect("parse manifest");
            let entry = manifest
                .files
                .iter_mut()
                .find(|entry| entry.path == path)
                .expect("entry");
            entry.size_bytes = content.len() as u64;
            entry.sha256 = orbit_common::security::release::sha256_hex(&content);
            std::fs::write(manifest_path, serde_yaml::to_string(&manifest).unwrap())
                .expect("write manifest");
        } else {
            std::fs::remove_file(&blob).expect("remove blob");
            std::fs::create_dir(&blob).expect("replace blob with directory");
        }
        let error = workspace
            .runtime
            .has_resolvable_pilot_admission_hold(&task.id)
            .expect_err("admission must fail closed on unreadable evidence");
        if oversized {
            assert!(
                matches!(error, orbit_common::OrbitError::InvalidInput(_)),
                "{error:?}"
            );
        } else {
            assert!(
                matches!(error, orbit_common::OrbitError::Io(_)),
                "{error:?}"
            );
        }
        let shown = workspace
            .runtime
            .run_tool("orbit.task.show", json!({"id": task.id}))
            .expect("show must survive unreadable pilot evidence");
        let warnings = shown["pilot_warnings"].as_array().expect("warnings");
        assert_eq!(warnings.len(), 1, "{shown}");
        assert_eq!(
            warnings[0]["kind"], "pilot_assessment_unavailable",
            "{shown}"
        );
        assert_eq!(shown["resolvable_pilot_admission_hold"], false, "{shown}");
    }
}

fn dangling_or_corrupt_pilot_blob_degrades_show() {
    // 0: blob absent with manifest retained. 1: same-length bytes whose
    // sha256 no longer matches the manifest. 2: appended bytes whose size
    // (and sha256) no longer match the manifest.
    for variant in 0..3 {
        let workspace = workspace(None);
        let task = add_task(&workspace.runtime, "dangling audit", TaskStatus::Proposed);
        let output = apply(&workspace, &prepare(&workspace, &task.id)).expect("apply");
        assert_eq!(output["status"], "succeeded", "{output}");
        let comment = workspace
            .runtime
            .get_task_comments(&task.id)
            .expect("comments")
            .into_iter()
            .rev()
            .find(|comment| comment.by == "task-pilot")
            .expect("pilot receipt");
        let reference: Value = serde_json::from_str(&comment.message).expect("reference");
        let path = reference["assessment_artifact"]
            .as_str()
            .expect("artifact path");
        let blob = crate::artifact_tools::find_artifact_blob(
            &workspace.runtime.global_root(),
            &task.id,
            path,
        );
        match variant {
            0 => std::fs::remove_file(&blob).expect("remove blob"),
            1 => {
                let mut bytes = std::fs::read(&blob).expect("read blob");
                bytes[0] ^= 0xff;
                std::fs::write(&blob, &bytes).expect("tamper blob");
            }
            _ => {
                let mut bytes = std::fs::read(&blob).expect("read blob");
                bytes.push(b'x');
                std::fs::write(&blob, &bytes).expect("append blob");
            }
        }
        // Store integrity and admission stay fail-closed.
        assert!(
            matches!(
                workspace.runtime.get_task(&task.id),
                Err(orbit_common::OrbitError::TaskBundleCorrupt { .. })
            ),
            "variant {variant}: full bundle read must refuse damaged evidence"
        );
        workspace
            .runtime
            .has_resolvable_pilot_admission_hold(&task.id)
            .expect_err("variant: admission must refuse damaged evidence");
        let shown = workspace
            .runtime
            .run_tool("orbit.task.show", json!({"id": task.id}))
            .unwrap_or_else(|error| {
                panic!("variant {variant}: show must degrade damaged pilot evidence: {error}")
            });
        let warnings = shown["pilot_warnings"].as_array().expect("warnings");
        assert_eq!(warnings.len(), 1, "variant {variant}: {shown}");
        assert_eq!(
            warnings[0]["kind"], "pilot_assessment_unavailable",
            "variant {variant}: {shown}"
        );
        assert_eq!(
            shown["resolvable_pilot_admission_hold"], false,
            "variant {variant}: {shown}"
        );
    }

    // Unrelated structural corruption stays an explicit show error.
    let workspace = workspace(None);
    let task = add_task(
        &workspace.runtime,
        "structurally corrupt",
        TaskStatus::Proposed,
    );
    let output = apply(&workspace, &prepare(&workspace, &task.id)).expect("apply");
    assert_eq!(output["status"], "succeeded", "{output}");
    let comment = workspace
        .runtime
        .get_task_comments(&task.id)
        .expect("comments")
        .into_iter()
        .rev()
        .find(|comment| comment.by == "task-pilot")
        .expect("pilot receipt");
    let reference: Value = serde_json::from_str(&comment.message).expect("reference");
    let path = reference["assessment_artifact"]
        .as_str()
        .expect("artifact path");
    let blob =
        crate::artifact_tools::find_artifact_blob(&workspace.runtime.global_root(), &task.id, path);
    let bundle_dir = blob
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("bundle dir");
    std::fs::write(bundle_dir.join("task.yaml"), ": [invalid envelope\n")
        .expect("corrupt envelope");
    workspace
        .runtime
        .run_tool("orbit.task.show", json!({"id": task.id}))
        .expect_err("structural corruption must remain an explicit show error");
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    meaning_edits_name_the_drifted_component_and_write_nothing_scenarios,
    meaning_edits_name_the_drifted_component_and_write_nothing,
    context_files_outside_freshness_stay_context_files_changed,
    task_prepared_in_progress_is_not_rewritten,
    proposed_to_backlog_still_applies,
    malformed_component_map_fails_apply_without_writing,
    configured_crew_and_dependency_meaning_drift_are_named,
    dependency_status_alone_still_applies_and_is_not_named_with_a_title_edit,
    missing_assessment_artifact_degrades_only_its_own_readout,
    unreadable_assessment_degrades_show_and_refuses_admission,
    dangling_or_corrupt_pilot_blob_degrades_show
);
