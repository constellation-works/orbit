//! Prepare/apply classification of status changes and material drift.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, mpsc};
use std::thread;
use std::time::Duration;

use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::task::{Task, TaskStatus};
use serde_json::{Value, json};
use tempfile::TempDir;

use super::super::persist::lock_hook::{self, Point};
use crate::OrbitRuntime;
use crate::application::task::{TaskAddParams, TaskUpdateParams};
use crate::application::test_support::runtime_with_workspace_config;

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
    git_out(repo, args);
}

pub(super) fn git_out(repo: &Path, args: &[&str]) -> String {
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
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// The observed-revision proof a pilot that read `prepared`'s checkout
/// reports: the prepared `source_revision` and its tree.
pub(super) fn proof(prepared: &Value) -> (String, String) {
    let revision = prepared["source"]["source_revision"]
        .as_str()
        .expect("prepared source revision")
        .to_string();
    let repo = prepared["workspace_path"].as_str().expect("workspace path");
    let tree = git_out(
        Path::new(repo),
        &["rev-parse", &format!("{revision}^{{tree}}")],
    );
    (revision, tree)
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
    let (revision, tree) = proof(prepared);
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

/// Two applications whose proposed edges close a cycle only together, each
/// acyclic alone: existing B -> C and D -> A, proposed A -> B and C -> D.
/// The second discovers its graph before the first writes and locks it after,
/// so its discovery is stale by the time it holds its locks. Locking only the
/// assessed task and its direct prerequisites let each walk wait on a task
/// the other held until both timed out [ORB-15408].
#[test]
fn overlapping_pilot_graphs_lock_in_order_and_refuse_the_closing_edge() {
    let workspace = workspace(None);
    let mut ids: Vec<String> = ["first", "second", "third", "fourth"]
        .into_iter()
        .map(|title| add_task(&workspace.runtime, title, TaskStatus::Backlog).id)
        .collect();
    ids.sort();
    let [a, b, c, d] = <[String; 4]>::try_from(ids).expect("four tasks");
    for (task, dependency) in [(&b, &c), (&d, &a)] {
        workspace
            .runtime
            .update_task(
                task,
                TaskUpdateParams {
                    dependencies: Some(vec![dependency.clone()]),
                    ..Default::default()
                },
            )
            .expect("existing edge");
    }
    let first_prepared = prepare(&workspace, &a);
    let second_prepared = prepare(&workspace, &c);

    let paths = workspace.runtime.paths().clone();
    let open = || OrbitRuntime::from_roots(&paths.global_dir, &paths.orbit_dir).expect("runtime");
    let discovered = Arc::new(Barrier::new(2));
    let (first_locked, first_holds_locks) = mpsc::channel();
    let second_discoveries = Arc::new(AtomicUsize::new(0));
    let (first, second) = thread::scope(|scope| {
        let first = scope.spawn(|| {
            let runtime = open();
            let discovered = Arc::clone(&discovered);
            let mut points = Vec::new();
            lock_hook::set(Some(Box::new(move |point| {
                if points.contains(&point) {
                    return;
                }
                points.push(point);
                match point {
                    Point::Discovered => {
                        discovered.wait();
                    }
                    Point::Locked => first_locked.send(()).expect("signal first locks"),
                }
            })));
            let output =
                apply_naming_blockers(&runtime, &first_prepared, &[format!("Wait for {b}")]);
            lock_hook::set(None);
            output
        });
        let second = scope.spawn(|| {
            let runtime = open();
            let discovered = Arc::clone(&discovered);
            let discoveries = Arc::clone(&second_discoveries);
            lock_hook::set(Some(Box::new(move |point| {
                if point != Point::Discovered || discoveries.fetch_add(1, Ordering::SeqCst) > 0 {
                    return;
                }
                discovered.wait();
                first_holds_locks
                    .recv_timeout(Duration::from_secs(120))
                    .expect("first application holds its locks");
            })));
            let output =
                apply_naming_blockers(&runtime, &second_prepared, &[format!("Wait for {d}")]);
            lock_hook::set(None);
            output
        });
        (
            first.join().expect("first application"),
            second.join().expect("second application"),
        )
    });

    let first = first.expect("first application runs");
    assert_eq!(first["status"], "succeeded", "{first}");
    assert_eq!(first["applied_count"], 1, "{first}");
    let second = second.expect("second application runs");
    assert_eq!(second["status"], "failed", "{second}");
    assert_eq!(second["applied_count"], 0, "{second}");
    let error = second["error"].as_str().expect("second application error");
    assert!(
        error.contains("dependency cycle"),
        "the closing edge is refused by the cycle check, not a lock timeout: {error}"
    );
    assert_eq!(
        second_discoveries.load(Ordering::SeqCst),
        2,
        "the second application rediscovers the graph the first one grew"
    );
    let dependencies = |id: &String| workspace.runtime.get_task(id).expect("task").dependencies();
    assert_eq!(dependencies(&a), std::slice::from_ref(&b));
    assert_eq!(dependencies(&b), std::slice::from_ref(&c));
    assert!(dependencies(&c).is_empty());
    assert_eq!(dependencies(&d), std::slice::from_ref(&a));
}

/// An explore brief is the pilot's own product, stored apart from its
/// receipt: a valid brief is written as the system-written artifact the
/// receipt names, and an invalid sibling's brief is dropped with its reason
/// while its assessment still applies, unrepaired [ORB-18539].
#[test]
fn pilot_apply_writes_a_valid_explore_brief_and_drops_an_invalid_one() {
    use orbit_types::task::{ArtifactWriter, EXPLORE_BRIEF_ARTIFACT, TaskArtifact};

    let workspace = workspace(Some("[workflow.explore_brief]\nenabled = true\n"));
    std::fs::create_dir_all(workspace.repo.join("src")).expect("src dir");
    std::fs::write(workspace.repo.join("src/lib.rs"), "pub fn greet() {}\n").expect("lib");
    git(&workspace.repo, &["add", "src/lib.rs"]);
    git(&workspace.repo, &["commit", "-m", "lib"]);
    let valid = add_task(&workspace.runtime, "valid brief", TaskStatus::Backlog).id;
    let dropped = add_task(&workspace.runtime, "dropped brief", TaskStatus::Backlog).id;
    let path = workspace.repo.canonicalize().expect("canonicalize repo");
    let prepared = workspace
        .runtime
        .run_deterministic(
            "prepare_task_pilot",
            &json!({}),
            &json!({
                "workspace_path": path,
                "task_ids": [valid, dropped],
                "base_branch": "main",
            }),
            ToolContext::default(),
        )
        .expect("prepare task pilot");
    assert_eq!(
        prepared["partitions"][0]["explore_brief_task_ids"],
        json!([valid, dropped]),
        "{prepared}"
    );
    let assessment = |task_id: &str, what: &str| {
        json!({
            "task_id": task_id,
            "context_files_after": ["file:src/lib.rs"],
            "disposition": "selectors",
            "recommended_crew": "opus",
            "recommended_complexity": "low",
            "assessment_rationale": "src/lib.rs holds greet.",
            "validation_approach": "Run the apply action.",
            "confidence": "high",
            "evidence_gaps": [],
            "reassessment_triggers": [],
            "blocked_by": [],
            "adr_conflicts": [],
            "utility_warnings": [],
            "surface_warnings": [],
            "duplicate_of": Value::Null,
            "already_landed": Value::Null,
            "explore_brief": {
                "change": [{"target": "symbol:src/lib.rs#greet:function", "what": what}],
                "pattern": [{"target": "file:README.md", "why": "The fixture's only doc."}],
                "gotchas": [{"text": "Keep the public name.", "cite": "file:src/lib.rs"}],
                "out_of_scope": ["Renaming the crate."],
                "confidence": "high",
            },
        })
    };
    let (revision, tree) = proof(&prepared);
    let output = workspace
        .runtime
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
                    "task_ids": [valid, dropped],
                    "tasks": [
                        assessment(&valid, "Return a greeting."),
                        assessment(&dropped, "Return a greeting from src/lib.rs:1."),
                    ],
                }],
            }),
            ToolContext::default(),
        )
        .expect("apply task pilot");
    assert_eq!(output["status"], "succeeded", "{output}");
    assert_eq!(
        output["applied_count"], 2,
        "an invalid brief never fails its task: {output}"
    );
    assert_eq!(
        output["repair_count"], 0,
        "an invalid brief spends no repair: {output}"
    );

    let receipt = |task_id: &str| {
        let task = workspace.runtime.get_task(task_id).expect("task");
        let comment = workspace
            .runtime
            .get_task_comments(task_id)
            .expect("comments")
            .into_iter()
            .rev()
            .find(|comment| comment.by == "task-pilot")
            .expect("pilot receipt");
        let operation_id = comment.receipt().expect("receipt").operation_id.to_string();
        let audit: Value = serde_json::from_str(&comment.message).expect("audit json");
        (task, operation_id, audit)
    };

    let (task, operation_id, audit) = receipt(&valid);
    assert_eq!(task.context_files, ["file:src/lib.rs"]);
    let manifest = workspace
        .runtime
        .get_task_artifact_manifest(&valid)
        .expect("manifest");
    let entry = manifest
        .iter()
        .find(|entry| entry.path == EXPLORE_BRIEF_ARTIFACT)
        .expect("the valid brief is stored");
    assert_eq!(entry.writer, Some(ArtifactWriter::System));
    let artifact = workspace
        .runtime
        .get_task_artifact(&valid, EXPLORE_BRIEF_ARTIFACT)
        .expect("read brief")
        .expect("brief artifact");
    let brief: Value = serde_json::from_slice(&artifact.content).expect("brief json");
    assert_eq!(brief["receipt_operation_id"], operation_id, "{brief}");
    assert_eq!(brief["inspection_revision"], revision, "{brief}");
    assert_eq!(brief["observed_tree"], tree, "{brief}");
    assert_eq!(
        brief["material_fingerprint"], audit["applied_material"]["material"][0],
        "the brief is keyed to the material its receipt sealed: {brief}"
    );
    assert_eq!(
        brief["brief"]["change"][0]["target"],
        "symbol:src/lib.rs#greet:function"
    );
    assert_eq!(audit["explore_brief"]["written"], EXPLORE_BRIEF_ARTIFACT);
    assert_eq!(audit["explore_brief"]["bytes"], artifact.content.len());
    assert!(
        audit["assessment"].get("explore_brief").is_none(),
        "the receipt never carries the brief itself: {audit}"
    );

    let (task, _, audit) = receipt(&dropped);
    assert_eq!(
        task.context_files,
        ["file:src/lib.rs"],
        "the assessment applied"
    );
    let reason = audit["explore_brief"]["dropped"]
        .as_str()
        .unwrap_or_else(|| panic!("dropped reason in the audit: {audit}"));
    assert!(reason.contains("line number"), "{reason}");
    assert!(
        workspace
            .runtime
            .get_task_artifact(&dropped, EXPLORE_BRIEF_ARTIFACT)
            .expect("read brief")
            .is_none(),
        "a dropped brief is not stored"
    );

    let forged = workspace.runtime.update_task(
        &dropped,
        TaskUpdateParams {
            upsert_artifacts: vec![TaskArtifact {
                path: "./Explore-Brief.json".to_string(),
                content: b"{}".to_vec(),
                media_type: "application/json".to_string(),
                created_by: None,
            }],
            ..Default::default()
        },
    );
    let error = forged
        .expect_err("only the pilot writes the brief")
        .to_string();
    assert!(error.contains("reserved"), "{error}");
}
