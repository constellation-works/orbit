//! What a dispatch admits, through the runtime's public surface.
//!
//! - Backlog admission (`list_backlog_tasks`, the deterministic action every
//!   drain and ship selection runs): its total order, dependency readiness,
//!   and exclusion of work whose files an active task holds.
//! - The operator's exclusive workspace claim [ORB-10709]: workflow
//!   submission refuses everyone but the holder until the claim expires.
//!
//! Every test re-runs itself in a child of this binary with inherited Orbit
//! authority cleared, a disposable `HOME`, and a bounded wait.

#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(missing_docs)]

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use orbit_core::application::task::TaskAddParams;
use orbit_core::{
    CompletionPolicy, OrbitError, OrbitRuntime, ShipMode, Task, TaskComplexity, TaskPriority,
    TaskStatus, TaskType,
};
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::policy::Role;
use orbit_types::tool::{McpCapability, ToolSessionContext};
use orbit_types::workflow::JobRunTrigger;
use serde_json::{Value, json};
use tempfile::TempDir;

/// How long one isolated test may run before it is killed and fails.
const CHILD_DEADLINE: Duration = Duration::from_secs(120);

/// Run `test` alone in a child of this binary with inherited Orbit authority
/// cleared and a disposable `HOME`; `true` inside that child. The parent
/// waits up to [`CHILD_DEADLINE`] and reaps the child on any exit.
fn isolated(test: &str) -> bool {
    const MARKER: &str = "ORBIT_TEST_DISPATCH_ADMISSION_CHILD";
    if std::env::var(MARKER).as_deref() == Ok(test) {
        return true;
    }
    let home = TempDir::new().unwrap();
    let stdout_path = home.path().join("stdout.log");
    let stderr_path = home.path().join("stderr.log");
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    command
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(MARKER, test)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path())
        .stdin(std::process::Stdio::null())
        .stdout(std::fs::File::create(&stdout_path).unwrap())
        .stderr(std::fs::File::create(&stderr_path).unwrap());
    let mut child = ChildGuard(command.spawn().unwrap());
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break Some(status);
        }
        if started.elapsed() > CHILD_DEADLINE {
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    drop(child);
    let read = |path: &Path| {
        let mut text = String::new();
        std::fs::File::open(path)
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        text
    };
    let (stdout, stderr) = (read(&stdout_path), read(&stderr_path));
    let status = status
        .unwrap_or_else(|| panic!("`{test}` ran past {CHILD_DEADLINE:?}:\n{stdout}\n{stderr}"));
    assert!(status.success(), "`{test}` failed:\n{stdout}\n{stderr}");
    assert!(
        stdout.contains("test result: ok. 1 passed;"),
        "the child must run `{test}` itself:\n{stdout}"
    );
    false
}

/// Kills and reaps the isolated child however the parent leaves.
struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn runtime() -> (TempDir, OrbitRuntime, PathBuf) {
    let root = TempDir::new().unwrap();
    let global = root.path().join("home/.orbit");
    let repo = root.path().join("repo");
    let workspace = repo.join(".orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("build runtime");
    (root, runtime, repo)
}

// ---------------------------------------------------------------------------
// Backlog admission
// ---------------------------------------------------------------------------

struct Seed<'a> {
    title: &'a str,
    status: TaskStatus,
    priority: TaskPriority,
    task_type: TaskType,
    tags: &'a [&'a str],
    dependencies: Vec<String>,
    context_files: &'a [&'a str],
}

impl Default for Seed<'_> {
    fn default() -> Self {
        Self {
            title: "fixture",
            status: TaskStatus::Backlog,
            priority: TaskPriority::Medium,
            task_type: TaskType::Chore,
            tags: &[],
            dependencies: Vec::new(),
            context_files: &[],
        }
    }
}

fn seed(runtime: &OrbitRuntime, seed: Seed<'_>) -> Task {
    runtime
        .add_task(TaskAddParams {
            title: seed.title.to_string(),
            description: format!("Fixture task: {}", seed.title),
            acceptance_criteria: vec!["Fixture task is observable.".to_string()],
            plan: "Fixture plan.".to_string(),
            tags: seed.tags.iter().map(ToString::to_string).collect(),
            dependencies: seed.dependencies,
            context_files: seed.context_files.iter().map(ToString::to_string).collect(),
            priority: seed.priority,
            complexity: TaskComplexity::Medium,
            task_type: Some(seed.task_type),
            status: Some(seed.status),
            ..Default::default()
        })
        .expect("seed task")
}

fn list_backlog_tasks(runtime: &OrbitRuntime, input: Value) -> Value {
    runtime
        .run_deterministic(
            "list_backlog_tasks",
            &json!({}),
            &input,
            ToolContext::default(),
        )
        .expect("list backlog tasks")
}

fn admitted(output: &Value) -> Vec<String> {
    output["task_ids"]
        .as_array()
        .expect("task_ids array")
        .iter()
        .map(|id| id.as_str().expect("task id").to_string())
        .collect()
}

/// Automatic dispatch order is total: critical work first, then the
/// corrective band (bugs and exact review-finding tags), then priority, with
/// creation order and then the task id breaking every remaining tie.
#[test]
fn backlog_admission_orders_critical_then_corrective_then_priority_then_age() {
    if !isolated("backlog_admission_orders_critical_then_corrective_then_priority_then_age") {
        return;
    }
    let (_root, runtime, _repo) = runtime();
    let medium_first = seed(
        &runtime,
        Seed {
            title: "medium a",
            ..Seed::default()
        },
    );
    let high = seed(
        &runtime,
        Seed {
            title: "high feature",
            priority: TaskPriority::High,
            task_type: TaskType::Feature,
            ..Seed::default()
        },
    );
    let low_bug = seed(
        &runtime,
        Seed {
            title: "low bug",
            priority: TaskPriority::Low,
            task_type: TaskType::Bug,
            ..Seed::default()
        },
    );
    let review_finding = seed(
        &runtime,
        Seed {
            title: "review finding",
            tags: &["code-review"],
            ..Seed::default()
        },
    );
    let near_miss_tag = seed(
        &runtime,
        Seed {
            title: "near-miss tag",
            tags: &["code-review-sweep"],
            ..Seed::default()
        },
    );
    let critical = seed(
        &runtime,
        Seed {
            title: "critical feature",
            priority: TaskPriority::Critical,
            task_type: TaskType::Feature,
            ..Seed::default()
        },
    );
    let medium_second = seed(
        &runtime,
        Seed {
            title: "medium b",
            ..Seed::default()
        },
    );

    let expected = vec![
        critical.id.clone(),
        review_finding.id,
        low_bug.id,
        high.id,
        medium_first.id,
        near_miss_tag.id,
        medium_second.id,
    ];
    assert_eq!(admitted(&list_backlog_tasks(&runtime, json!({}))), expected);
    assert_eq!(
        admitted(&list_backlog_tasks(&runtime, json!({ "max_tasks": 1 }))),
        vec![critical.id],
        "a bounded selection takes the head of the same order"
    );
}

/// A backlog task is admitted only once every dependency is done.
#[test]
fn backlog_admission_waits_for_every_dependency_to_be_done() {
    if !isolated("backlog_admission_waits_for_every_dependency_to_be_done") {
        return;
    }
    let (_root, runtime, _repo) = runtime();
    let done = seed(
        &runtime,
        Seed {
            title: "done dependency",
            status: TaskStatus::Done,
            ..Seed::default()
        },
    );
    let ready = seed(
        &runtime,
        Seed {
            title: "ready dependent",
            dependencies: vec![done.id.clone()],
            ..Seed::default()
        },
    );
    let mut blocked = BTreeSet::new();
    let mut unfinished = Vec::new();
    for status in [
        TaskStatus::Proposed,
        TaskStatus::Backlog,
        TaskStatus::InProgress,
        TaskStatus::Review,
    ] {
        let dependency = seed(
            &runtime,
            Seed {
                title: "unfinished dependency",
                status,
                ..Seed::default()
            },
        );
        let dependent = seed(
            &runtime,
            Seed {
                title: "blocked dependent",
                dependencies: vec![done.id.clone(), dependency.id.clone()],
                ..Seed::default()
            },
        );
        blocked.insert(dependent.id);
        unfinished.push(dependency.id);
    }

    let selected = admitted(&list_backlog_tasks(&runtime, json!({})));
    assert!(selected.contains(&ready.id), "{selected:?}");
    assert!(
        selected.contains(&unfinished[1]),
        "a backlog dependency is itself ready: {selected:?}"
    );
    let leaked = selected
        .iter()
        .filter(|id| blocked.contains(*id))
        .collect::<Vec<_>>();
    assert!(
        leaked.is_empty(),
        "admitted before its dependencies were done: {leaked:?}"
    );
}

/// A backlog task whose files an in-progress task holds is withheld and
/// reported with the holder, while unrelated work is still admitted.
#[test]
fn backlog_admission_excludes_work_locked_by_an_active_task() {
    if !isolated("backlog_admission_excludes_work_locked_by_an_active_task") {
        return;
    }
    let (_root, runtime, repo) = runtime();
    for file in ["crates/foo/src/lib.rs", "crates/bar/src/lib.rs"] {
        let path = repo.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "fixture\n").unwrap();
    }
    let holder = seed(
        &runtime,
        Seed {
            title: "holder",
            status: TaskStatus::InProgress,
            context_files: &["crates/foo/src/lib.rs"],
            ..Seed::default()
        },
    );
    let locked = seed(
        &runtime,
        Seed {
            title: "locked",
            context_files: &["crates/foo/src/lib.rs"],
            ..Seed::default()
        },
    );
    let free = seed(
        &runtime,
        Seed {
            title: "free",
            context_files: &["crates/bar/src/lib.rs"],
            ..Seed::default()
        },
    );

    let output = list_backlog_tasks(&runtime, json!({}));

    assert_eq!(admitted(&output), vec![free.id]);
    assert_eq!(
        output["excluded"],
        json!([{
            "id": locked.id,
            "reason": "context_lock_conflict",
            "conflicts": [{
                "requested_file": locked.context_files[0],
                "locking_task_id": holder.id
            }]
        }])
    );
}

// ---------------------------------------------------------------------------
// Operator workspace claim
// ---------------------------------------------------------------------------

fn as_operator(runtime: &OrbitRuntime, tool: &str, input: Value) -> Value {
    runtime
        .run_tool_with_context_and_role(
            tool,
            input,
            Role::Admin,
            ToolContext {
                session_context: ToolSessionContext {
                    effective_capabilities: BTreeSet::from([McpCapability::Operator]),
                    ..ToolSessionContext::default()
                },
                ..ToolContext::default()
            },
        )
        .unwrap_or_else(|error| panic!("{tool}: {error}"))
}

/// Submit a discovery-mode ship run. This fixture deploys no job asset, so a
/// submission that passes the claim gate fails next on the missing asset.
fn ship(runtime: &OrbitRuntime, claim_token: Option<&str>) -> OrbitError {
    runtime
        .submit_ship_run(
            ShipMode::Local,
            Some("main"),
            &[],
            CompletionPolicy::Review,
            &[],
            Some("test"),
            claim_token,
            JobRunTrigger::cli(),
        )
        .expect_err("a fixture without job assets never submits a run")
}

/// While an operator holds the claim, dispatch is refused to everyone else
/// with the holder and expiry named, and the holder's own token passes.
#[test]
fn a_held_workspace_claim_gates_dispatch_to_its_holder() {
    if !isolated("a_held_workspace_claim_gates_dispatch_to_its_holder") {
        return;
    }
    let (_root, runtime, _repo) = runtime();
    assert!(
        matches!(ship(&runtime, None), OrbitError::NotFound { .. }),
        "no claim, no gate"
    );
    let grant = as_operator(
        &runtime,
        "orbit.workspace.claim.acquire",
        json!({ "model": "claude", "machine_id": "machine-1", "session_id": "session-1" }),
    );
    assert_eq!(grant["acquired"], json!(true));
    let token = grant["claim_token"].as_str().expect("claim token");

    for stranger in [None, Some("wsclaim-some-other-token")] {
        let error = ship(&runtime, stranger);
        let OrbitError::WorkspaceClaimHeld(claim) = &error else {
            panic!("dispatch with {stranger:?} must be refused, got {error:?}");
        };
        assert_eq!(claim.operation, "orbit.workflow.ship");
        assert_eq!(claim.holder, "claude");
        assert!(
            !claim.expires_at.is_empty(),
            "a refusal names when the claim lapses"
        );
    }
    let resume = runtime
        .submit_resume_run("jrun-does-not-exist", Some("test"), None)
        .expect_err("resume is gated before run lookup");
    assert!(
        matches!(resume, OrbitError::WorkspaceClaimHeld(_)),
        "resume takes the same gate: {resume:?}"
    );

    let holder = ship(&runtime, Some(token));
    assert!(
        matches!(holder, OrbitError::NotFound { .. }),
        "the holder passes the gate, got {holder:?}"
    );
}

/// An expired claim stops gating dispatch with no release.
#[test]
fn an_expired_workspace_claim_stops_gating_dispatch() {
    if !isolated("an_expired_workspace_claim_stops_gating_dispatch") {
        return;
    }
    let (_root, runtime, _repo) = runtime();
    as_operator(
        &runtime,
        "orbit.workspace.claim.acquire",
        json!({ "model": "claude", "ttl_seconds": 1 }),
    );
    assert!(matches!(
        ship(&runtime, None),
        OrbitError::WorkspaceClaimHeld(_)
    ));

    // Bounded wait for the one-second lease to lapse.
    let deadline = Instant::now() + Duration::from_secs(10);
    let after = loop {
        let error = ship(&runtime, None);
        if !matches!(error, OrbitError::WorkspaceClaimHeld(_)) || Instant::now() > deadline {
            break error;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(
        matches!(after, OrbitError::NotFound { .. }),
        "an expired claim must stop gating dispatch, got {after:?}"
    );
}
