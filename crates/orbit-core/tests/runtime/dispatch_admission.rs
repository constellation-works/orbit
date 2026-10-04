//! What a dispatch admits, through the runtime's public surface.
//!
//! - Backlog admission (`list_backlog_tasks`, the deterministic action every
//!   drain and ship selection runs): its total order, dependency readiness,
//!   and exclusion of work whose files an active task holds.
//! - The operator's exclusive workspace claim [ORB-10709]: workflow
//!   submission refuses everyone but the holder until the claim expires.
//! - Review admission and settlement provenance [ORB-13916]: deterministic
//!   evidence is system-authored while reviewer writes retain their identity.
//! - Host resource throttling [ORB-13901]: under an injected probe, sustained
//!   pressure holds drain waves and ship discovery, warns through readiness,
//!   run show and MCP, and lifts below the resume mark.
//!
//! Every test re-runs itself in a child of this binary with inherited Orbit
//! authority cleared, a disposable `HOME`, and a bounded wait.

#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(missing_docs)]

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

use orbit_core::application::distributed::DrainEntryPoint;
use orbit_core::application::task::TaskAddParams;
use orbit_core::runtime::host_resource::{DiskSample, HostResourceProbe, HostResourceSample};
use orbit_core::{
    CompletionPolicy, OrbitError, OrbitRuntime, ShipMode, Task, TaskComplexity, TaskPriority,
    TaskStatus, TaskType,
};
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::policy::Role;
use orbit_types::tool::{McpCapability, McpTransport, ToolSessionContext};
use orbit_types::workflow::{JobRunState, JobRunTrigger, PipelineState};
use serde_json::{Value, json};
use tempfile::TempDir;

/// How long one isolated test may run before it is killed and fails.
const CHILD_DEADLINE: Duration = Duration::from_secs(120);

/// Run `test` alone in a child of this binary with inherited Orbit authority
/// cleared and a disposable `HOME`; `true` inside that child. The parent
/// waits up to [`CHILD_DEADLINE`] and reaps the child on any exit.
pub(super) fn isolated(test: &str) -> bool {
    const MARKER: &str = "ORBIT_TEST_DISPATCH_ADMISSION_CHILD";
    if std::env::var(MARKER).as_deref() == Ok(test) {
        return true;
    }
    let home = TempDir::new().unwrap();
    let stdout_path = home.path().join("stdout.log");
    let stderr_path = home.path().join("stderr.log");
    // libtest names a test by its module path below the crate root.
    let qualified = if test.contains("::") {
        test.to_string()
    } else {
        format!(
            "{}::{test}",
            module_path!().split_once("::").expect("test module").1
        )
    };
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    command
        .args(["--exact", &qualified, "--nocapture", "--test-threads=1"])
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

/// ORB-13916: deterministic gate evidence must not look like a human
/// intervention, including replay and the coupled-repair selector write.
#[test]
fn review_gate_writes_system_provenance_without_borrowing_the_operator() {
    if !isolated("review_gate_writes_system_provenance_without_borrowing_the_operator") {
        return;
    }
    use chrono::Utc;
    use orbit_core::ActorIdentity;
    use orbit_core::application::task::TaskUpdateParams;
    use orbit_types::workflow::{
        REVIEW_CONTRACT_VERSION, REVIEW_GATE_ARTIFACT, REVIEW_MANIFEST_ARTIFACT,
        REVIEW_REPORT_ARTIFACT, ReviewAdmission, ReviewCertificate, ReviewVerdict,
    };

    for verdict in [
        ReviewVerdict::Accept,
        ReviewVerdict::Reject,
        ReviewVerdict::AcceptWithFixes,
    ] {
        let root = TempDir::new().unwrap();
        let global = root.path().join("home/.orbit");
        let repo = root.path().join("repo");
        let workspace = repo.join(".orbit");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(
            workspace.join("config.toml"),
            "[crews.reviewers]\nmodel = \"review-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n[workflow]\ndefault_crew = \"reviewers\"\n[operation]\nreview_policy = \"before-pr\"\nreview_crew = \"reviewers\"\n",
        )
        .unwrap();
        let runtime = OrbitRuntime::from_roots(&global, &workspace)
            .unwrap()
            .with_actor(ActorIdentity::human("human:daniel"));
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .unwrap();
            assert!(output.status.success(), "git {args:?}: {output:?}");
            String::from_utf8(output.stdout).unwrap().trim().to_string()
        };
        git(&["init", "-b", "main"]);
        git(&["config", "user.name", "Orbit Test"]);
        git(&["config", "user.email", "orbit-test@example.com"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join(".gitignore"), ".orbit/\n").unwrap();
        std::fs::write(repo.join("src.txt"), "before\n").unwrap();
        std::fs::write(repo.join("coupled.txt"), "before\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-m", "seed"]);
        let task = runtime
            .add_task(TaskAddParams {
                title: "Review provenance fixture".into(),
                description: "Exercise the deterministic review gate.".into(),
                acceptance_criteria: vec!["System evidence has system provenance.".into()],
                plan: "Change src.txt.".into(),
                context_files: vec!["file:src.txt".into()],
                status: Some(TaskStatus::InProgress),
                ..Default::default()
            })
            .unwrap();
        git(&["checkout", "-b", "candidate"]);
        std::fs::write(repo.join("src.txt"), "implemented\n").unwrap();
        git(&["add", "src.txt"]);
        git(&["commit", "-m", &format!("feat: implement [{}]", task.id)]);
        let policy = runtime.operation_policy();
        let admission = ReviewAdmission {
            contract_version: REVIEW_CONTRACT_VERSION,
            policy_version: policy.version,
            timing: policy.review_policy.value.timing(),
            timing_source: policy.review_policy.source.label().into(),
            crew: policy.review_crew.value.clone(),
            crew_source: policy.review_crew.source.label().into(),
            budget: policy.review_budget(),
            captured_at: Utc::now(),
        };
        let run = runtime
            .insert_job_run(
                "task_pr_pipeline",
                1,
                Utc::now(),
                Some(json!({"review": admission})),
                None,
            )
            .unwrap();
        runtime
            .update_task_with_identity(
                &task.id,
                TaskUpdateParams {
                    job_run_id: Some(Some(run.run_id.clone())),
                    ..Default::default()
                },
                Some("codex".into()),
                None,
            )
            .unwrap();
        let history_before = runtime.get_task_history(&task.id).unwrap();
        let mut input = json!({
            "job_run_id": run.run_id,
            "completed_task_ids": [task.id],
            "workspace_path": repo.canonicalize().unwrap(),
            "base": "main",
            "base_sync": "local",
            "mode": "pr",
            "allowed_crews": [],
        });
        let admitted = runtime
            .run_deterministic(
                "review_gate_admit",
                &json!({}),
                &input,
                ToolContext::default(),
            )
            .unwrap();
        assert_eq!(admitted["applies"], true);
        let repaired = verdict == ReviewVerdict::AcceptWithFixes;
        if repaired {
            std::fs::write(repo.join("coupled.txt"), "reviewer repair\n").unwrap();
        }
        let report = json!({
            "schema_version": REVIEW_CONTRACT_VERSION,
            "attempt_id": admitted["attempt_id"],
            "verdict": verdict,
            "summary": "Checked the fixture.",
            "findings": if repaired || verdict == ReviewVerdict::Reject {
                json!([{
                    "id": "F1", "severity": "medium", "summary": "Coupled repair",
                    "paths": ["coupled.txt"],
                    "disposition": {"kind": if repaired { "repaired" } else { "open" }},
                }])
            } else { json!([]) },
            "validation": [{"command": "fixture check", "outcome": "passed", "role": "required"}],
            "escalation": null,
        });
        // The public agent tools share the owner-write helper with the old
        // gate implementation; they must keep attributing reviewer writes.
        let scratch = workspace.join("tmp");
        std::fs::create_dir_all(&scratch).unwrap();
        let report_path = scratch.join(REVIEW_REPORT_ARTIFACT);
        std::fs::write(&report_path, report.to_string()).unwrap();
        runtime
            .run_tool(
                "orbit.task.artifact.put",
                json!({
                    "id": task.id, "model": "codex", "path": REVIEW_REPORT_ARTIFACT,
                    "source_path": report_path,
                }),
            )
            .unwrap();
        runtime
            .run_tool(
                "orbit.task.update",
                json!({
                    "id": task.id, "model": "codex", "comment": "Reviewer report ready.",
                }),
            )
            .unwrap();
        input["admission"] = admitted;
        for _ in 0..2 {
            let settled = runtime.run_deterministic(
                "review_gate_settle",
                &json!({}),
                &input,
                ToolContext::default(),
            );
            assert_eq!(settled.is_ok(), verdict.passed(), "{settled:?}");
        }
        let certificate: ReviewCertificate = serde_json::from_slice(
            &runtime
                .get_task_artifact(&task.id, REVIEW_GATE_ARTIFACT)
                .unwrap()
                .unwrap()
                .content,
        )
        .unwrap();
        assert_eq!(certificate.verdict, verdict, "{certificate:?}");
        assert_eq!(certificate.reviewer.crew, "reviewers");
        assert_eq!(
            certificate.selectors_widened,
            if repaired {
                vec!["file:coupled.txt".to_string()]
            } else {
                vec![]
            }
        );
        let manifest = runtime.get_task_artifact_manifest(&task.id).unwrap();
        for (path, author) in [
            (REVIEW_MANIFEST_ARTIFACT, "system"),
            (REVIEW_GATE_ARTIFACT, "system"),
            (REVIEW_REPORT_ARTIFACT, "codex"),
        ] {
            assert_eq!(
                manifest
                    .iter()
                    .find(|file| file.path == path)
                    .unwrap()
                    .created_by,
                author,
                "ORB-13916: {path} must retain its actual writer"
            );
            assert_eq!(
                runtime
                    .get_task_artifact(&task.id, path)
                    .unwrap()
                    .unwrap()
                    .created_by
                    .as_deref(),
                Some(author)
            );
        }
        let comments = runtime.get_task_comments(&task.id).unwrap();
        assert_eq!(comments.len(), 2, "replay must not duplicate settlement");
        assert_eq!(comments[0].by, "codex");
        assert_eq!(
            comments[1].by, "system",
            "ORB-13916: gate is not a human intervention"
        );
        // Gate settlement does not create synthetic history stubs. Existing
        // human creation history must survive without new human entries.
        assert_eq!(runtime.get_task_history(&task.id).unwrap(), history_before);
        if repaired {
            assert_eq!(
                runtime.get_task(&task.id).unwrap().context_files,
                ["file:src.txt", "file:coupled.txt"]
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Host resource throttle
// ---------------------------------------------------------------------------

/// A host whose CPU and memory readings and sample time a test sets; every
/// filesystem reads 10%.
pub(super) struct PressureProbe {
    reading: Mutex<(Option<f64>, Option<f64>, DateTime<Utc>)>,
    samples: AtomicUsize,
}

impl PressureProbe {
    pub(super) fn calm() -> Arc<Self> {
        Arc::new(Self {
            reading: Mutex::new((Some(10.0), Some(10.0), Utc::now())),
            samples: AtomicUsize::new(0),
        })
    }

    /// Memory at `percent`, observed at `at`.
    pub(super) fn memory(&self, percent: f64, at: DateTime<Utc>) {
        let mut reading = self.reading.lock().unwrap();
        reading.1 = Some(percent);
        reading.2 = at;
    }

    pub(super) fn cpu(&self, percent: Option<f64>) {
        self.reading.lock().unwrap().0 = percent;
    }

    /// Hold memory above its 90% high mark across the ten-second sustain
    /// window, so the next admission check throttles. Returns when the
    /// pressure began.
    pub(super) fn sustain_memory(&self, runtime: &OrbitRuntime, percent: f64) -> DateTime<Utc> {
        let since = Utc::now() - chrono::Duration::seconds(12);
        self.memory(percent, since);
        assert!(
            runtime.resource_admission().throttle.is_none(),
            "one sample is not sustained"
        );
        self.memory(percent, Utc::now());
        since
    }

    fn samples(&self) -> usize {
        self.samples.load(Ordering::SeqCst)
    }
}

impl HostResourceProbe for PressureProbe {
    fn sample(&self, paths: &[PathBuf]) -> HostResourceSample {
        self.samples.fetch_add(1, Ordering::SeqCst);
        let (cpu, memory, at) = *self.reading.lock().unwrap();
        HostResourceSample {
            sampled_at: at,
            cpu_percent: cpu,
            memory_percent: memory,
            disks: paths
                .iter()
                .map(|path| DiskSample {
                    path: path.clone(),
                    used_percent: Some(10.0),
                })
                .collect(),
        }
    }
}

/// A running `job` run, as its worker leaves it, with `input`.
fn running_run(runtime: &OrbitRuntime, job: &str, input: Value) -> String {
    let jobs = orbit_store::compose::workspace_job_run_store(
        runtime.sqlite_store().unwrap(),
        runtime.workspace_id().unwrap(),
    );
    let run = jobs
        .insert_job_run(job, 1, Utc::now(), Some(input), None)
        .expect("run");
    runtime
        .write_run_state(
            &run.run_id,
            &PipelineState::new(run.run_id.clone(), run.job_id, json!({})),
        )
        .expect("run state");
    jobs.mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .expect("running");
    run.run_id
}

fn classify(runtime: &OrbitRuntime, drain: &str) -> Value {
    runtime
        .run_deterministic(
            "classify_workspace_auto_tasks",
            &json!({}),
            &json!({"run_id": drain, "max_active_leaf_runs": 4}),
            ToolContext::default(),
        )
        .expect("classify")
}

fn readiness_task<'a>(readiness: &'a Value, task: &str) -> &'a Value {
    readiness["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["task_id"] == task)
        .expect("task in readiness")
}

/// A separate probe process establishes sustained pressure without any drain.
/// Fresh ship and sweep runtimes must recover it before pipeline submission.
#[test]
fn fresh_runtime_discovery_recovers_host_pressure_without_a_drain() {
    const TEST: &str = "fresh_runtime_discovery_recovers_host_pressure_without_a_drain";
    const WARM_ROOT: &str = "ORBIT_TEST_PRESSURE_WARM_ROOT";
    if !isolated(TEST) {
        return;
    }
    if let Some(root) = std::env::var_os(WARM_ROOT) {
        let root = PathBuf::from(root);
        let probe = PressureProbe::calm();
        let runtime = OrbitRuntime::from_roots(&root.join("global"), &root.join("repo/.orbit"))
            .unwrap()
            .with_host_resource_probe(probe.clone());
        probe.sustain_memory(&runtime, 95.0);
        assert!(runtime.resource_admission().throttle.is_some());
        return;
    }

    let root = TempDir::new().unwrap();
    let global = root.path().join("global");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let qualified = format!("dispatch_admission::{TEST}");
    let mut warm = std::process::Command::new(std::env::current_exe().unwrap());
    warm.args(["--exact", &qualified, "--nocapture", "--test-threads=1"])
        .env(WARM_ROOT, root.path());
    let output = orbit_common::process::run_bounded_capped(&mut warm, CHILD_DEADLINE, 64 * 1024)
        .expect("bounded independent probe process");
    orbit_common::test_env::assert_child_test_passed(
        &qualified,
        output.status,
        &output.stdout,
        &output.stderr,
    );

    let probe = PressureProbe::calm();
    probe.memory(95.0, Utc::now());
    let open = |workspace: &Path| {
        OrbitRuntime::from_roots(&global, workspace)
            .unwrap()
            .with_host_resource_probe(probe.clone())
    };
    let runtime = open(&workspace);
    let queued = seed(&runtime, Seed::default());
    let refused = ship(&runtime, None);
    assert!(
        matches!(refused, OrbitError::PolicyDenied(ref reason) if reason.starts_with("resource_throttled:")),
        "fresh discovery must refuse before looking up/submitting its pipeline: {refused}"
    );
    // Sweep opens its own runtime too, and shares history across workspace roots.
    let other_workspace = root.path().join("other/.orbit");
    std::fs::create_dir_all(&other_workspace).unwrap();
    let sweep = open(&other_workspace);
    let decision = sweep
        .drain_entry_admission(DrainEntryPoint::ShipSweep, &[], true)
        .unwrap();
    assert_eq!(decision.refusal.unwrap().code(), "resource_throttled");
    for runtime in [&runtime, &sweep] {
        let jobs = orbit_store::compose::workspace_job_run_store(
            runtime.sqlite_store().unwrap(),
            runtime.workspace_id().unwrap(),
        );
        for job in [
            "task_auto_pipeline",
            "workspace_auto_pipeline",
            "workspace_pull_pipeline",
        ] {
            assert!(
                jobs.list_job_runs(job).unwrap().is_empty(),
                "no submitted pipeline or active drain"
            );
        }
    }
    let explicit = runtime
        .drain_entry_admission(
            DrainEntryPoint::ExplicitShip,
            std::slice::from_ref(&queued.id),
            false,
        )
        .unwrap();
    assert!(explicit.refusal.is_none());
    assert!(
        explicit.resource_throttle.is_some(),
        "explicit selection carries the throttle warning"
    );
    let explicit_submission = runtime
        .submit_ship_run(
            ShipMode::Local,
            Some("main"),
            std::slice::from_ref(&queued.id),
            CompletionPolicy::Review,
            &[],
            Some("test"),
            None,
            JobRunTrigger::cli(),
        )
        .expect_err("the fixture has no delivery job asset");
    assert!(matches!(explicit_submission, OrbitError::NotFound { .. }));

    // Hysteresis survives a fresh open but recovery is immediate below resume.
    probe.memory(85.0, Utc::now());
    assert!(open(&workspace).resource_admission().throttle.is_some());
    probe.memory(70.0, Utc::now());
    assert!(open(&workspace).resource_admission().throttle.is_none());

    // Re-establish a hold, then prove unknown/stale readings clear shared history.
    probe.sustain_memory(&runtime, 95.0);
    assert!(runtime.resource_admission().throttle.is_some());
    {
        let mut reading = probe.reading.lock().unwrap();
        reading.0 = None;
        reading.1 = None;
    }
    let unknown = open(&workspace).admission_resource_throttle();
    assert!(unknown.throttle.is_none());
    assert!(
        unknown
            .unknown
            .iter()
            .any(|reason| reason == "memory unavailable")
    );
    assert!(
        runtime
            .drain_entry_admission(DrainEntryPoint::ShipSweep, &[], true)
            .unwrap()
            .refusal
            .is_none()
    );
    assert!(matches!(
        ship(&open(&workspace), None),
        OrbitError::NotFound { .. }
    ));
    probe.cpu(Some(10.0));
    probe.sustain_memory(&runtime, 95.0);
    assert!(runtime.resource_admission().throttle.is_some());
    probe.memory(95.0, Utc::now() - chrono::Duration::seconds(20));
    let stale = open(&workspace).admission_resource_throttle();
    assert!(stale.throttle.is_none());
    assert!(stale.unknown.iter().any(|reason| reason == "memory stale"));
    assert!(matches!(
        ship(&open(&workspace), None),
        OrbitError::NotFound { .. }
    ));
}

/// Sustained memory pressure holds the local drain's wave, ship discovery
/// and readiness until memory falls below its resume mark; a live child keeps
/// running throughout, and unknown CPU telemetry never holds anything.
#[test]
fn sustained_pressure_holds_local_admission_until_it_clears_below_resume() {
    if !isolated("sustained_pressure_holds_local_admission_until_it_clears_below_resume") {
        return;
    }
    let probe = PressureProbe::calm();
    let (_root, runtime, _repo) = runtime();
    let runtime = runtime.with_host_resource_probe(probe.clone());
    let running = seed(
        &runtime,
        Seed {
            title: "running",
            context_files: &["file:src/running.rs"],
            ..Seed::default()
        },
    );
    let queued = seed(
        &runtime,
        Seed {
            title: "queued",
            context_files: &["file:src/queued.rs"],
            ..Seed::default()
        },
    );
    let drain = running_run(&runtime, "workspace_auto_pipeline", json!({}));
    let child = running_run(
        &runtime,
        "task_auto_pipeline",
        json!({"task_ids": [running.id]}),
    );

    // Unknown CPU telemetry fails open and is reported.
    probe.cpu(None);
    let open = classify(&runtime, &drain);
    assert_eq!(open["loose_task_ids"], json!([queued.id]), "{open}");
    assert_eq!(open["resource_throttle"], Value::Null, "{open}");
    assert_eq!(
        open["resource_telemetry_unknown"],
        json!(["cpu unavailable"]),
        "{open}"
    );
    let readiness = runtime
        .workspace_auto_readiness(&[], None, 50, &[])
        .unwrap();
    assert_eq!(
        readiness["capacity"]["resource_telemetry_unknown"],
        json!(["cpu unavailable"])
    );
    probe.cpu(Some(10.0));

    // A spike that has not been sustained does not throttle.
    probe.memory(95.0, Utc::now() - chrono::Duration::seconds(12));
    let spike = classify(&runtime, &drain);
    assert_eq!(spike["loose_task_ids"], json!([queued.id]), "{spike}");

    probe.memory(95.0, Utc::now());
    let held = classify(&runtime, &drain);
    assert_eq!(held["loose_task_ids"], json!([]), "{held}");
    assert_eq!(held["free_slots"], 0, "{held}");
    let pressure = &held["resource_throttle"]["resources"][0];
    assert_eq!(pressure["resource"], "memory", "{held}");
    assert_eq!(pressure["percent"], 95.0, "{held}");
    assert_eq!(pressure["high_percent"], 90, "{held}");
    assert_eq!(pressure["resume_percent"], 80, "{held}");
    assert!(pressure["since"].is_string(), "{held}");
    assert_eq!(held["sleep_seconds"], 30, "a throttled drain polls: {held}");
    let pass = runtime
        .read_run_state(&drain)
        .unwrap()
        .unwrap()
        .drain_last_pass
        .expect("last pass");
    let recorded = pass
        .resource_throttle
        .expect("the pass records the throttle");
    assert_eq!(recorded.resources[0].resource, "memory");

    // Readiness and MCP name the resource, value, threshold and since-when.
    let readiness = runtime
        .workspace_auto_readiness(&[], None, 50, &[])
        .unwrap();
    assert_eq!(readiness["capacity"]["free_slots"], 0);
    assert_eq!(
        readiness["capacity"]["resource_throttle"]["resources"][0]["resource"],
        "memory"
    );
    let waiting = readiness_task(&readiness, &queued.id);
    assert_eq!(waiting["reason"], "resource_throttled", "{waiting}");
    assert!(
        waiting["detail"]
            .as_str()
            .is_some_and(|detail| detail.starts_with("memory 95% \u{2265} 90% since ")),
        "{waiting}"
    );
    let operator = ToolContext {
        session_context: ToolSessionContext {
            transport: Some(McpTransport::Local),
            effective_capabilities: BTreeSet::from([McpCapability::Operator]),
            ..ToolSessionContext::default()
        },
        ..ToolContext::default()
    };
    let status = runtime
        .run_tool_with_context_and_role(
            "orbit.workflow.auto",
            json!({"workspace": runtime.workspace_id().unwrap(), "action": "status"}),
            Role::Admin,
            operator.clone(),
        )
        .expect("mcp status");
    assert_eq!(
        status["capacity"]["resource_throttle"]["resources"][0]["high_percent"], 90,
        "{status:#}"
    );
    let shown = as_operator(&runtime, "orbit.workflow.run.show", json!({"id": drain}));
    assert_eq!(
        shown["drain_last_pass"]["resource_throttle"]["resources"][0]["resource"], "memory",
        "{shown:#}"
    );

    // Ship discovery stands down; an explicit selection is admitted and warned.
    let refused = ship(&runtime, None);
    assert!(
        matches!(&refused, OrbitError::PolicyDenied(reason) if reason.starts_with("resource_throttled: Admissions throttled: memory 95%")),
        "{refused}"
    );
    let explicit = runtime
        .drain_entry_admission(
            DrainEntryPoint::ExplicitShip,
            std::slice::from_ref(&queued.id),
            false,
        )
        .unwrap();
    assert!(explicit.refusal.is_none(), "{:?}", explicit.refusal);
    assert!(explicit.resource_throttle.is_some());

    // Between the resume and high marks the hold continues.
    probe.memory(85.0, Utc::now());
    let band = classify(&runtime, &drain);
    assert_eq!(band["loose_task_ids"], json!([]), "{band}");

    // Running work was never touched.
    let jobs = orbit_store::compose::workspace_job_run_store(
        runtime.sqlite_store().unwrap(),
        runtime.workspace_id().unwrap(),
    );
    assert_eq!(
        jobs.get_job_run(&child).unwrap().unwrap().state,
        JobRunState::Running
    );

    probe.memory(70.0, Utc::now());
    let resumed = classify(&runtime, &drain);
    assert_eq!(resumed["loose_task_ids"], json!([queued.id]), "{resumed}");
    assert_eq!(resumed["resource_throttle"], Value::Null, "{resumed}");
    let readiness = runtime
        .workspace_auto_readiness(&[], None, 50, &[])
        .unwrap();
    assert_eq!(readiness["capacity"]["resource_throttle"], Value::Null);
    assert_eq!(readiness_task(&readiness, &queued.id)["reason"], "ready");
    assert!(
        runtime
            .read_run_state(&drain)
            .unwrap()
            .unwrap()
            .drain_last_pass
            .unwrap()
            .resource_throttle
            .is_none()
    );
}

/// With `workflow.resource_throttle.enabled = false` admission is what it was
/// before the throttle: no sample is taken for it and pressure holds nothing.
#[test]
fn a_disabled_throttle_admits_under_pressure_without_sampling() {
    if !isolated("a_disabled_throttle_admits_under_pressure_without_sampling") {
        return;
    }
    let root = TempDir::new().unwrap();
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(
        global.join("config.toml"),
        "[workflow.resource_throttle]\nenabled = false\n",
    )
    .unwrap();
    let probe = PressureProbe::calm();
    let runtime = OrbitRuntime::from_roots(&global, &workspace)
        .expect("build runtime")
        .with_host_resource_probe(probe.clone());
    let task = seed(&runtime, Seed::default());
    let drain = running_run(&runtime, "workspace_auto_pipeline", json!({}));
    probe.memory(99.0, Utc::now() - chrono::Duration::seconds(12));
    classify(&runtime, &drain);
    probe.memory(99.0, Utc::now());
    let wave = classify(&runtime, &drain);
    assert_eq!(wave["loose_task_ids"], json!([task.id]), "{wave}");
    assert_eq!(wave["resource_throttle"], Value::Null);
    assert_eq!(wave["resource_telemetry_unknown"], json!([]));
    let readiness = runtime
        .workspace_auto_readiness(&[], None, 50, &[])
        .unwrap();
    assert_eq!(readiness["capacity"]["resource_throttle"], Value::Null);
    assert_eq!(probe.samples(), 0, "a disabled throttle samples nothing");
}
