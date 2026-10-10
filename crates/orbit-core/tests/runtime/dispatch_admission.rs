//! What a dispatch admits, through the runtime's public surface.
//!
//! - Backlog admission (`list_backlog_tasks`, the deterministic action every
//!   drain and ship selection runs): its total order, dependency readiness,
//!   exclusion of work whose files an active task holds, and the bounded
//!   surface a lock-blocked high-priority task reserves [ORB-14310].
//! - The operator's exclusive workspace claim [ORB-10709]: workflow
//!   submission refuses everyone but the holder until the claim expires.
//! - Review admission and settlement provenance [ORB-13916]: deterministic
//!   evidence is system-authored while reviewer writes retain their identity.
//! - Host resource throttling [ORB-13901]: under an injected probe, sustained
//!   pressure holds drain waves and ship discovery, warns through readiness,
//!   run show and MCP, and lifts below the resume mark. CPU pressure alone
//!   still admits CPU-light auto-tasks within a reserved budget [ORB-14624].
//! - Frozen-batch expiry [ORB-14624]: a task whose frozen delivery batch
//!   nears its deadline sorts ahead of same-priority backlog.
//! - Unknown footprints [ORB-15191]: a multi-slot drain or ship waits for the
//!   task pilot to prepare empty-context backlog work, else runs it alone.
//!
//! Every test re-runs itself in a child of this binary with inherited Orbit
//! authority cleared, a disposable `HOME`, and a bounded wait.

#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(missing_docs)]

use std::collections::BTreeSet;
use std::io::Write;
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
use orbit_types::task::{CONTEXT_FILES_WIDENED_EVENT, ContextFilesWidening, ContextWideningStep};
use orbit_types::tool::{McpCapability, McpTransport, ToolSessionContext};
use orbit_types::workflow::{JobRunState, JobRunTrigger, PipelineState};
use serde_json::{Value, json};
use tempfile::TempDir;

mod replay_crew;
mod reservation_grants;
mod unknown_footprint;

/// Run `test` alone in a child of this binary with inherited Orbit authority
/// cleared and a disposable `HOME`; `true` inside that child. The parent
/// waits under the shared child-test hang guard and reaps the child on any
/// exit.
pub(super) fn isolated(test: &str) -> bool {
    isolated_with_ignored(test, false)
}

/// The same isolation for an opt-in measurement selected with `--ignored`.
#[cfg(target_os = "linux")]
pub(super) fn isolated_ignored(test: &str) -> bool {
    isolated_with_ignored(test, true)
}

fn isolated_with_ignored(test: &str, ignored: bool) -> bool {
    const MARKER: &str = "ORBIT_TEST_DISPATCH_ADMISSION_CHILD";
    if std::env::var(MARKER).as_deref() == Ok(test) {
        return true;
    }
    let home = TempDir::new().unwrap();
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
        .current_dir(home.path());
    if ignored {
        command.arg("--ignored");
    }
    let logs = TempDir::new().unwrap();
    let output = orbit_common::test_env::run_child_test(&mut command, &qualified, logs.path());
    orbit_common::test_env::assert_child_test_passed(
        &qualified,
        output.status,
        &output.stdout,
        &output.stderr,
    );
    if ignored {
        std::io::stdout().write_all(&output.stdout).unwrap();
    }
    false
}

fn runtime() -> (TempDir, OrbitRuntime, PathBuf) {
    let root = TempDir::new().unwrap();
    let global = root.path().join("home/.orbit");
    let repo = root.path().join("repo");
    let workspace = repo.join(".orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace)
        .expect("build runtime")
        .with_host_resource_probe(PressureProbe::calm());
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
    context_files: Option<&'a [&'a str]>,
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
            context_files: None,
        }
    }
}

fn seed(runtime: &OrbitRuntime, seed: Seed<'_>) -> Task {
    let context_files = match seed.context_files {
        Some(selectors) => selectors.iter().map(ToString::to_string).collect(),
        None => {
            let name = format!(
                "fixture-{}.txt",
                runtime.list_task_metadata().unwrap().len()
            );
            std::fs::write(runtime.paths().repo_root.join(&name), "fixture\n").unwrap();
            vec![format!("file:{name}")]
        }
    };
    runtime
        .add_task(TaskAddParams {
            title: seed.title.to_string(),
            description: format!("Fixture task: {}", seed.title),
            acceptance_criteria: vec!["Fixture task is observable.".to_string()],
            plan: "Fixture plan.".to_string(),
            tags: seed.tags.iter().map(ToString::to_string).collect(),
            dependencies: seed.dependencies,
            context_files,
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

/// Selector-free backlog work is admitted on the first auto/ship pass, even
/// while another task holds the workspace, and reserves no context locks.
/// With no pilot to prepare it and no other leaf in flight it goes alone, so
/// only work that edits nothing shares its wave ([`unknown_footprint`]
/// covers the waits).
#[test]
fn empty_context_is_admitted_on_auto_ship_and_readiness_without_locks() {
    if !isolated("empty_context_is_admitted_on_auto_ship_and_readiness_without_locks") {
        return;
    }
    let (_root, runtime, _repo) = runtime();
    seed(
        &runtime,
        Seed {
            status: TaskStatus::InProgress,
            context_files: Some(&["dir:."]),
            ..Seed::default()
        },
    );
    let selector_free = seed(
        &runtime,
        Seed {
            context_files: Some(&[]),
            ..Seed::default()
        },
    );
    let no_diff = seed(
        &runtime,
        Seed {
            title: "side effects only",
            context_files: Some(&[]),
            tags: &["no-diff-expected"],
            ..Seed::default()
        },
    );
    for input in [
        json!({}),
        json!({"task_ids": [selector_free.id, no_diff.id]}),
    ] {
        let output = list_backlog_tasks(&runtime, input);
        assert_eq!(
            admitted(&output),
            vec![selector_free.id.clone(), no_diff.id.clone()],
            "{output}"
        );
        assert_eq!(output["excluded"], json!([]), "{output}");
    }
    let drain = running_run(&runtime, "workspace_auto_pipeline", json!({}));
    let wave = runtime
        .run_deterministic(
            "classify_workspace_auto_tasks",
            &json!({}),
            &json!({"run_id": drain, "max_active_leaf_runs": 2}),
            ToolContext::default(),
        )
        .unwrap();
    assert_eq!(
        wave["loose_task_ids"],
        json!([selector_free.id, no_diff.id]),
        "{wave}"
    );
    let shown = as_operator(&runtime, "orbit.workflow.run.show", json!({"id": drain}));
    assert_eq!(shown["drain_last_pass"]["excluded_total"], 0, "{shown}");
    let readiness = runtime
        .workspace_auto_readiness(&[], None, 50, &[])
        .unwrap();
    for id in [&selector_free.id, &no_diff.id] {
        let ready = readiness_task(&readiness, id);
        assert_eq!(ready["eligible"], true, "{ready}");
        assert_eq!(ready["reason"], "ready", "{ready}");
        let reservation = reserve_locks(&runtime, id);
        assert_eq!(reservation["reserved"], true, "{reservation}");
        assert_eq!(reservation["reserved_files"], json!([]), "{reservation}");
    }
}

/// The shipped documentation chore mints with a locking scope, so a drain can
/// admit it without pilot preparation and withhold overlapping work.
#[test]
fn doc_duties_mints_an_admissible_locking_context() {
    if !isolated("doc_duties_mints_an_admissible_locking_context") {
        return;
    }
    let (_root, runtime, _repo) = runtime();
    let definitions = runtime.paths().local_dir.join("auto_tasks");
    std::fs::create_dir_all(&definitions).unwrap();
    std::fs::write(
        definitions.join("doc-duties.yaml"),
        include_str!("../../assets/auto_tasks/doc-duties.yaml"),
    )
    .unwrap();
    let minted = runtime.auto_task_mint("doc-duties").unwrap();
    assert!(
        !minted.context_files.is_empty(),
        "the documentation chore must reserve its edits"
    );
    assert_eq!(
        admitted(&list_backlog_tasks(&runtime, json!({}))),
        vec![minted.id.clone()]
    );
    let reservation = reserve_locks(&runtime, &minted.id);
    assert_eq!(reservation["reserved"], true, "{reservation}");
    assert!(
        !reservation["reserved_files"].as_array().unwrap().is_empty(),
        "{reservation}"
    );
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
            context_files: Some(&["crates/foo/src/lib.rs"]),
            ..Seed::default()
        },
    );
    let locked = seed(
        &runtime,
        Seed {
            title: "locked",
            context_files: Some(&["crates/foo/src/lib.rs"]),
            ..Seed::default()
        },
    );
    let free = seed(
        &runtime,
        Seed {
            title: "free",
            context_files: Some(&["crates/bar/src/lib.rs"]),
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

/// Write each workspace-relative fixture file so its selector expands.
fn write_files(repo: &Path, files: &[&str]) {
    for file in files {
        let path = repo.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "fixture\n").unwrap();
    }
}

/// Move an active holder out of the lock surface, as a finished delivery does.
fn release(runtime: &OrbitRuntime, task_id: &str) {
    for status in [TaskStatus::Review, TaskStatus::Done] {
        runtime
            .update_task_as_human(
                task_id,
                orbit_core::application::task::TaskUpdateParams {
                    status: Some(status),
                    execution_summary: Some("Fixture delivery finished.".into()),
                    ..Default::default()
                },
                "fixture operator".into(),
            )
            .unwrap();
    }
}

fn excluded_entry<'a>(output: &'a Value, task: &str) -> &'a Value {
    output["excluded"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == task)
        .unwrap_or_else(|| panic!("{task} is not excluded: {output:#}"))
}

/// A critical task that needs two locks, released one at a time, is admitted
/// ahead of the lower-ranked task overlapping the lock that frees first. That
/// task used to take the freed lock on every pass, so the critical task never
/// saw all of its locks free at once [ORB-14310]. Work that does not overlap
/// the reserved surface keeps admitting throughout.
#[test]
fn a_lock_blocked_critical_task_is_admitted_ahead_of_overlapping_work_as_locks_free() {
    if !isolated("a_lock_blocked_critical_task_is_admitted_ahead_of_overlapping_work_as_locks_free")
    {
        return;
    }
    let (_root, runtime, repo) = runtime();
    write_files(&repo, &["a.rs", "b.rs", "free.rs"]);
    let holder_a = seed(
        &runtime,
        Seed {
            title: "holder a",
            status: TaskStatus::InProgress,
            context_files: Some(&["a.rs"]),
            ..Seed::default()
        },
    );
    let holder_b = seed(
        &runtime,
        Seed {
            title: "holder b",
            status: TaskStatus::InProgress,
            context_files: Some(&["b.rs"]),
            ..Seed::default()
        },
    );
    // Older than the critical task, so age alone would put it first.
    let overlapping = seed(
        &runtime,
        Seed {
            title: "overlapping",
            context_files: Some(&["b.rs"]),
            ..Seed::default()
        },
    );
    let critical = seed(
        &runtime,
        Seed {
            title: "critical",
            priority: TaskPriority::Critical,
            task_type: TaskType::Feature,
            context_files: Some(&["a.rs", "b.rs"]),
            ..Seed::default()
        },
    );
    let unrelated = seed(
        &runtime,
        Seed {
            title: "unrelated",
            context_files: Some(&["free.rs"]),
            ..Seed::default()
        },
    );
    let drain = running_run(&runtime, "workspace_auto_pipeline", json!({}));

    // Both locks held: each waits on its holder, and the critical task's
    // reservation covers nothing the lock filter has not already withheld.
    let wave = classify(&runtime, &drain);
    assert_eq!(wave["loose_task_ids"], json!([unrelated.id]), "{wave:#}");
    let output = list_backlog_tasks(&runtime, json!({}));
    assert_eq!(
        excluded_entry(&output, &overlapping.id)["reason"],
        "context_lock_conflict"
    );

    // The first lock frees. The overlapping task would take it; the
    // reservation withholds it and names the critical task.
    release(&runtime, &holder_b.id);
    let wave = classify(&runtime, &drain);
    assert_eq!(
        wave["loose_task_ids"],
        json!([unrelated.id]),
        "only non-overlapping work admits while the critical task waits: {wave:#}"
    );
    let output = list_backlog_tasks(&runtime, json!({}));
    assert_eq!(admitted(&output), vec![unrelated.id.clone()]);
    let withheld = excluded_entry(&output, &overlapping.id);
    assert_eq!(withheld["reason"], "surface_reserved", "{output:#}");
    assert_eq!(
        withheld["conflicts"],
        json!([{"requested_file": "file:b.rs", "locking_task_id": critical.id}])
    );
    assert!(
        withheld["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains(&critical.id)),
        "{withheld}"
    );
    let reserving = excluded_entry(&output, &critical.id);
    assert_eq!(reserving["reason"], "context_lock_conflict");
    assert_eq!(
        reserving["conflicts"],
        json!([{"requested_file": "file:a.rs", "locking_task_id": holder_a.id}])
    );
    assert!(reserving["detail"].is_string(), "{reserving}");

    // The drain's persisted pass and readiness report the same wait.
    let pass = runtime
        .read_run_state(&drain)
        .unwrap()
        .unwrap()
        .drain_last_pass
        .expect("last pass");
    let recorded = pass
        .excluded
        .iter()
        .find(|task| task.task_id == overlapping.id)
        .expect("the pass records the withheld task");
    assert_eq!(recorded.reason.as_deref(), Some("surface_reserved"));
    assert_eq!(recorded.blocked_by, vec![critical.id.clone()]);
    let readiness = runtime
        .workspace_auto_readiness(&[], None, 50, &[])
        .unwrap();
    let waiting = readiness_task(&readiness, &overlapping.id);
    assert_eq!(waiting["eligible"], false);
    assert_eq!(waiting["reason"], "surface_reserved");
    assert_eq!(waiting["blocking_task_ids"], json!([critical.id]));
    assert_eq!(readiness_task(&readiness, &unrelated.id)["reason"], "ready");

    // The second lock frees: the critical task takes the wave ahead of the
    // overlapping task, which now defers behind it.
    release(&runtime, &holder_a.id);
    let wave = classify(&runtime, &drain);
    assert_eq!(
        wave["loose_task_ids"],
        json!([critical.id, unrelated.id]),
        "{wave:#}"
    );
    let deferred = wave["deferred_conflicts"].as_array().unwrap();
    assert_eq!(deferred.len(), 1, "{wave:#}");
    assert_eq!(deferred[0]["task_id"], overlapping.id);
    assert_eq!(deferred[0]["blocking_task_ids"], json!([critical.id]));
}

/// Reservations are bounded so a stuck task cannot freeze the queue
/// [ORB-14310]: only critical and high-priority lock-blocked tasks reserve, at
/// most two per pass in dispatch order, and a task ranked ahead of a reserving
/// task is never withheld by it.
#[test]
fn surface_reservations_are_bounded_by_priority_count_and_rank() {
    if !isolated("surface_reservations_are_bounded_by_priority_count_and_rank") {
        return;
    }
    let (_root, runtime, repo) = runtime();
    write_files(
        &repo,
        &[
            "held/h1.rs",
            "held/h2.rs",
            "held/h3.rs",
            "held/m.rs",
            "h1.rs",
            "h2.rs",
            "h3.rs",
            "m.rs",
        ],
    );
    seed(
        &runtime,
        Seed {
            title: "holder",
            status: TaskStatus::InProgress,
            context_files: Some(&["dir:held"]),
            ..Seed::default()
        },
    );
    let lock_blocked = |title, priority, files: &'static [&'static str]| {
        seed(
            &runtime,
            Seed {
                title,
                priority,
                task_type: TaskType::Feature,
                context_files: Some(files),
                ..Seed::default()
            },
        )
    };
    let high_1 = lock_blocked("high 1", TaskPriority::High, &["held/h1.rs", "h1.rs"]);
    let high_2 = lock_blocked("high 2", TaskPriority::High, &["held/h2.rs", "h2.rs"]);
    let high_3 = lock_blocked("high 3", TaskPriority::High, &["held/h3.rs", "h3.rs"]);
    let medium = lock_blocked("medium", TaskPriority::Medium, &["held/m.rs", "m.rs"]);
    let low = |title, files: &'static [&'static str]| {
        seed(
            &runtime,
            Seed {
                title,
                priority: TaskPriority::Low,
                context_files: Some(files),
                ..Seed::default()
            },
        )
    };
    let behind_1 = low("behind high 1", &["h1.rs"]);
    let behind_2 = low("behind high 2", &["h2.rs"]);
    let behind_3 = low("behind high 3", &["h3.rs"]);
    let behind_medium = low("behind medium", &["m.rs"]);
    let ahead = seed(
        &runtime,
        Seed {
            title: "critical ahead",
            priority: TaskPriority::Critical,
            context_files: Some(&["h2.rs"]),
            ..Seed::default()
        },
    );

    let output = list_backlog_tasks(&runtime, json!({}));

    assert_eq!(
        admitted(&output),
        vec![ahead.id, behind_3.id, behind_medium.id],
        "a third high task and a medium task reserve nothing; a critical task \
         ranked ahead of a reservation is not withheld by it: {output:#}"
    );
    for (withheld, reserving) in [(&behind_1, &high_1), (&behind_2, &high_2)] {
        let entry = excluded_entry(&output, &withheld.id);
        assert_eq!(entry["reason"], "surface_reserved", "{output:#}");
        assert_eq!(entry["conflicts"][0]["locking_task_id"], reserving.id);
    }
    for (task, reserves) in [
        (&high_1, true),
        (&high_2, true),
        (&high_3, false),
        (&medium, false),
    ] {
        let entry = excluded_entry(&output, &task.id);
        assert_eq!(entry["reason"], "context_lock_conflict");
        assert_eq!(entry["detail"].is_string(), reserves, "{entry}");
    }
}

/// An in-progress `no-diff-expected` task does not exclude overlapping backlog
/// work. An ordinary holder still does, and the tagged task still waits on
/// its own dependency and on that ordinary lock [ORB-14247].
#[test]
fn no_diff_expected_does_not_hold_a_context_lock() {
    if !isolated("no_diff_expected_does_not_hold_a_context_lock") {
        return;
    }
    let (_root, runtime, repo) = runtime();
    for file in [
        "crates/review/lib.rs",
        "crates/ordinary/lib.rs",
        "crates/elsewhere/lib.rs",
    ] {
        let path = repo.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "fixture\n").unwrap();
    }
    let review = seed(
        &runtime,
        Seed {
            title: "review holder",
            status: TaskStatus::InProgress,
            tags: &["no-diff-expected"],
            context_files: Some(&["dir:crates/review"]),
            ..Seed::default()
        },
    );
    let repair = seed(
        &runtime,
        Seed {
            title: "overlapping repair",
            context_files: Some(&["file:crates/review/lib.rs"]),
            ..Seed::default()
        },
    );
    let ordinary = seed(
        &runtime,
        Seed {
            title: "ordinary holder",
            status: TaskStatus::InProgress,
            context_files: Some(&["file:crates/ordinary/lib.rs"]),
            ..Seed::default()
        },
    );
    let ordinary_overlap = seed(
        &runtime,
        Seed {
            title: "ordinary overlap",
            context_files: Some(&["file:crates/ordinary/lib.rs"]),
            ..Seed::default()
        },
    );
    let tagged_overlap = seed(
        &runtime,
        Seed {
            title: "tagged task still waits on an ordinary lock",
            tags: &["no-diff-expected"],
            context_files: Some(&["file:crates/ordinary/lib.rs"]),
            ..Seed::default()
        },
    );
    let unfinished = seed(
        &runtime,
        Seed {
            title: "unfinished dependency",
            status: TaskStatus::InProgress,
            ..Seed::default()
        },
    );
    let tagged_dependent = seed(
        &runtime,
        Seed {
            title: "tagged task still waits on its dependency",
            tags: &["no-diff-expected"],
            dependencies: vec![unfinished.id.clone()],
            context_files: Some(&["file:crates/elsewhere/lib.rs"]),
            ..Seed::default()
        },
    );

    let output = list_backlog_tasks(&runtime, json!({}));
    assert_eq!(admitted(&output), vec![repair.id.clone()], "{output}");
    let excluded = output["excluded"].as_array().expect("excluded");
    assert!(
        excluded.iter().all(|entry| entry["id"] != repair.id),
        "the repair overlapping only the no-diff holder must not be excluded: {output}"
    );
    assert!(
        excluded.iter().all(|entry| {
            entry["conflicts"].as_array().is_none_or(|conflicts| {
                conflicts
                    .iter()
                    .all(|conflict| conflict["locking_task_id"] != review.id)
            })
        }),
        "no context_lock_conflict names the no-diff-expected task: {output}"
    );
    let ordinary_exclusion = excluded
        .iter()
        .find(|entry| entry["id"] == ordinary_overlap.id)
        .expect("ordinary overlap is excluded");
    assert_eq!(ordinary_exclusion["reason"], "context_lock_conflict");
    assert_eq!(
        ordinary_exclusion["conflicts"],
        json!([{
            "requested_file": ordinary_overlap.context_files[0],
            "locking_task_id": ordinary.id
        }])
    );
    let tagged_exclusion = excluded
        .iter()
        .find(|entry| entry["id"] == tagged_overlap.id)
        .expect("a tagged task still conflicts with an ordinary holder");
    assert_eq!(tagged_exclusion["reason"], "context_lock_conflict");
    assert_eq!(
        tagged_exclusion["conflicts"][0]["locking_task_id"],
        ordinary.id
    );

    let readiness = runtime
        .workspace_auto_readiness(&[], None, 50, &[])
        .expect("readiness");
    let repair_ready = readiness_task(&readiness, &repair.id);
    assert_eq!(repair_ready["eligible"], true, "{repair_ready}");
    assert_eq!(repair_ready["reason"], "ready", "{repair_ready}");
    let ordinary_waiting = readiness_task(&readiness, &ordinary_overlap.id);
    assert_eq!(ordinary_waiting["reason"], "context_lock_conflict");
    assert_eq!(
        ordinary_waiting["conflicts"][0]["locking_task_id"],
        ordinary.id
    );
    let dependent = readiness_task(&readiness, &tagged_dependent.id);
    assert_eq!(dependent["reason"], "unmet_dependency", "{dependent}");
    assert!(
        !admitted(&output).contains(&tagged_dependent.id),
        "a tagged task with an unfinished dependency is not admitted"
    );

    let review_grant = reserve_locks(&runtime, &review.id);
    assert_eq!(review_grant["reserved"], true, "{review_grant}");
    assert_eq!(review_grant["reserved_files"], json!([]), "{review_grant}");
    assert!(
        review_grant["reservation_id"].as_str().is_some(),
        "release still has a reservation id: {review_grant}"
    );
    let repair_grant = reserve_locks(&runtime, &repair.id);
    assert_eq!(
        repair_grant["reserved"], true,
        "a drain's lock grant is not blocked by the no-diff reservation: {repair_grant}"
    );
    let ordinary_grant = reserve_locks(&runtime, &ordinary_overlap.id);
    assert_eq!(ordinary_grant["reserved"], false, "{ordinary_grant}");
    assert!(
        ordinary_grant["conflicts"]
            .as_array()
            .is_some_and(|conflicts| {
                conflicts
                    .iter()
                    .any(|conflict| conflict["held_by_id"] == ordinary.id)
            }),
        "an ordinary holder still denies the grant: {ordinary_grant}"
    );
}

fn reserve_locks(runtime: &OrbitRuntime, task_id: &str) -> Value {
    runtime
        .run_deterministic(
            "reserve_locks",
            &json!({}),
            &json!({ "task_ids": [task_id], "ttl_seconds": 120 }),
            ToolContext::default(),
        )
        .expect("reserve locks")
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
    for stranger in [None, Some("wrong-token")] {
        let foreground = runtime
            .replay_job_run_with_claim("missing", stranger)
            .unwrap_err();
        let detached = runtime
            .submit_replay_run("missing", None, stranger, JobRunTrigger::dashboard())
            .unwrap_err();
        for error in [foreground, detached] {
            assert!(
                matches!(error, OrbitError::WorkspaceClaimHeld(_)),
                "{error:?}"
            );
        }
    }
    assert!(matches!(
        runtime.replay_job_run("missing").unwrap_err(),
        OrbitError::WorkspaceClaimHeld(_)
    ));
    assert!(matches!(
        runtime
            .replay_job_run_with_claim("missing", Some(token))
            .unwrap_err(),
        OrbitError::NotFound { .. }
    ));
    assert!(matches!(
        runtime
            .submit_replay_run("missing", None, Some(token), JobRunTrigger::dashboard())
            .unwrap_err(),
        OrbitError::NotFound { .. }
    ));

    let holder = ship(&runtime, Some(token));
    assert!(
        matches!(holder, OrbitError::NotFound { .. }),
        "the holder passes the gate, got {holder:?}"
    );
}

#[test]
fn a_replica_refuses_foreground_and_detached_replay_before_persisting_a_run() {
    if !isolated("a_replica_refuses_foreground_and_detached_replay_before_persisting_a_run") {
        return;
    }
    let (_root, runtime, _repo) = runtime();
    let source = running_run(
        &runtime,
        "replay_fixture",
        json!({"task_ids": ["some-task"]}),
    );
    let replica = runtime.with_coordination_write_owner(Some("owner-machine".into()));
    let before = replica.list_job_runs(Default::default()).unwrap();
    let foreground = replica.replay_job_run(&source).unwrap_err();
    let detached = replica
        .submit_replay_run(&source, None, None, JobRunTrigger::dashboard())
        .unwrap_err();
    for error in [foreground, detached] {
        assert!(
            matches!(error, OrbitError::CapabilityRefused(_)),
            "{error:?}"
        );
    }
    assert_eq!(replica.list_job_runs(Default::default()).unwrap(), before);
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
        REVIEW_REPORT_ARTIFACT, ReviewAdmission, ReviewCertificate, ReviewTiming, ReviewVerdict,
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
            "[crews.reviewers]\nmodel = \"review-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n[workflow]\ndefault_crew = \"reviewers\"\n[operation]\nreview_crew = \"reviewers\"\n[review]\nbefore_pr = true\n",
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
            timing: ReviewTiming::BeforePr,
            timing_source: policy.review_before_pr.source.label().into(),
            crew: policy.review_crew.value.first().cloned(),
            crew_pool: Vec::new(),
            crew_source: policy.review_crew.source.label().into(),
            budget: policy.review_budget(),
            required_validation_commands: Some(
                runtime.workflow_required_validation_commands().to_vec(),
            ),
            baseline_commands: runtime.review_baseline_commands().to_vec(),
            captured_at: Utc::now(),
            host_evidence: Vec::new(),
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
            // ORB-13990: a repair no finding names still passes and widens.
            std::fs::write(repo.join("undeclared.txt"), "reviewer companion\n").unwrap();
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
            "validation": [{"id": "V1", "command": "fixture check", "outcome": "passed", "role": "required"}],
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
                vec![
                    "file:coupled.txt".to_string(),
                    "file:undeclared.txt".to_string(),
                ]
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
        // human creation history must survive without new human entries; a
        // repair outside the selectors records the semantic scope update and
        // the widening provenance, both attributed to the system.
        let history = runtime.get_task_history(&task.id).unwrap();
        assert_eq!(history[..history_before.len()], history_before[..]);
        let added = &history[history_before.len()..];
        if repaired {
            let [updated, widened] = added else {
                panic!("expected a scope update and its widening entry, got {added:?}");
            };
            assert_eq!(updated.event, "updated");
            assert_eq!(
                updated.by, "system",
                "ORB-14296: the semantic scope update must not borrow the operator's identity"
            );
            assert_eq!(widened.by, "system");
            assert_eq!(widened.event, CONTEXT_FILES_WIDENED_EVENT);
            let widening =
                ContextFilesWidening::from_note(widened.note.as_deref().unwrap()).unwrap();
            assert_eq!(widening.run_id, run.run_id);
            assert_eq!(widening.step, ContextWideningStep::Review);
            assert_eq!(widening.activity, "review_gate_settle");
            assert_eq!(
                widening.selectors,
                ["file:coupled.txt", "file:undeclared.txt"]
            );
        } else {
            assert!(added.is_empty(), "{added:?}");
        }
        if repaired {
            assert_eq!(
                runtime.get_task(&task.id).unwrap().context_files,
                ["file:src.txt", "file:coupled.txt", "file:undeclared.txt"]
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Host resource throttle
// ---------------------------------------------------------------------------

/// A host whose CPU, memory and filesystem readings and sample time a test
/// sets; every filesystem reads the same, 10% unless a test raises it.
pub(super) struct PressureProbe {
    reading: Mutex<(Option<f64>, Option<f64>, DateTime<Utc>)>,
    disk: Mutex<f64>,
    samples: AtomicUsize,
}

impl PressureProbe {
    pub(super) fn calm() -> Arc<Self> {
        Arc::new(Self {
            reading: Mutex::new((Some(10.0), Some(10.0), Utc::now())),
            disk: Mutex::new(10.0),
            samples: AtomicUsize::new(0),
        })
    }

    /// Hold `set` above its high mark across the ten-second sustain window:
    /// one sample twelve seconds ago, then one now.
    fn sustain(&self, runtime: &OrbitRuntime, set: impl Fn(&Self)) {
        set(self);
        self.reading.lock().unwrap().2 = Utc::now() - chrono::Duration::seconds(12);
        assert!(
            runtime.resource_admission().throttle.is_none(),
            "one sample is not sustained"
        );
        self.reading.lock().unwrap().2 = Utc::now();
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
        let disk = *self.disk.lock().unwrap();
        HostResourceSample {
            sampled_at: at,
            cpu_percent: cpu,
            memory_percent: memory,
            disks: paths
                .iter()
                .map(|path| DiskSample {
                    path: path.clone(),
                    used_percent: Some(disk),
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
    let logs = TempDir::new().unwrap();
    let output = orbit_common::test_env::run_child_test(&mut warm, &qualified, logs.path());
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
    probe.memory(87.0, Utc::now());
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
            context_files: Some(&["file:src/running.rs"]),
            ..Seed::default()
        },
    );
    let queued = seed(
        &runtime,
        Seed {
            title: "queued",
            context_files: Some(&["file:src/queued.rs"]),
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
    assert_eq!(pressure["resume_percent"], 85, "{held}");
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
        waiting["detail"].as_str().is_some_and(
            |detail| detail.starts_with("memory 95% (throttled at \u{2265} 90% since ")
        ),
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
    probe.memory(87.0, Utc::now());
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

// ---------------------------------------------------------------------------
// CPU-light admission and frozen-batch expiry [ORB-14624]
// ---------------------------------------------------------------------------

/// The tags an after-landing review auto-task carries.
const LIGHT_TAGS: &[&str] = &["code-review", "no-diff-expected", "auto-task:code-review"];

/// CPU pressure from cargo-heavy leaves used to hold a review auto-task for
/// hours. While CPU alone holds admissions, `no-diff-expected` auto-task
/// leaves start up to the reserved light budget and an implementation leaf
/// still waits; once the budget is taken a further light leaf waits too, and
/// readiness names the light-budget reason.
#[test]
fn a_cpu_only_throttle_admits_light_auto_tasks_within_the_reserved_budget() {
    if !isolated("a_cpu_only_throttle_admits_light_auto_tasks_within_the_reserved_budget") {
        return;
    }
    let probe = PressureProbe::calm();
    let (_root, runtime, _repo) = runtime();
    let runtime = runtime.with_host_resource_probe(probe.clone());
    let implementation = seed(
        &runtime,
        Seed {
            title: "implementation",
            priority: TaskPriority::High,
            ..Seed::default()
        },
    );
    // Tagged `no-diff-expected` but filed by hand: the exemption is for
    // automated reading work, not for anything that promises no diff.
    let manual_no_diff = seed(
        &runtime,
        Seed {
            title: "manual no-diff",
            tags: &["no-diff-expected"],
            ..Seed::default()
        },
    );
    let light: Vec<Task> = ["review one", "review two", "review three"]
        .into_iter()
        .map(|title| {
            seed(
                &runtime,
                Seed {
                    title,
                    tags: LIGHT_TAGS,
                    ..Seed::default()
                },
            )
        })
        .collect();
    let drain = running_run(&runtime, "workspace_auto_pipeline", json!({}));

    probe.sustain(&runtime, |probe| probe.cpu(Some(195.0)));
    let wave = classify(&runtime, &drain);
    assert_eq!(
        wave["resource_throttle"]["resources"][0]["resource"], "cpu",
        "{wave}"
    );
    assert_eq!(
        wave["loose_task_ids"],
        json!([light[0].id, light[1].id]),
        "the reserved budget admits light leaves in queue order and nothing else: {wave}"
    );
    assert_eq!(wave["free_slots"], 2, "{wave}");
    assert_eq!(
        wave["cpu_light_budget"],
        json!({"reserved": 2, "active": 0, "remaining": 2, "applies": true}),
        "{wave}"
    );

    let readiness = runtime
        .workspace_auto_readiness(&[], None, 50, &[])
        .unwrap();
    assert_eq!(readiness["capacity"]["free_slots"], 2, "{readiness:#}");
    let first = readiness_task(&readiness, &light[0].id);
    assert_eq!(first["reason"], "ready", "{first}");
    assert_eq!(first["cpu_light"], true, "{first}");
    for refused in [&implementation.id, &manual_no_diff.id] {
        let entry = readiness_task(&readiness, refused);
        assert_eq!(entry["reason"], "resource_throttled", "{entry}");
        assert_eq!(entry["cpu_light"], Value::Null, "{entry}");
    }

    // The admitted pair are live leaves now; the budget is spent.
    for task in &light[..2] {
        running_run(
            &runtime,
            "task_auto_pipeline",
            json!({"task_ids": [task.id]}),
        );
    }
    let full = classify(&runtime, &drain);
    assert_eq!(full["loose_task_ids"], json!([]), "{full}");
    assert_eq!(full["free_slots"], 0, "{full}");
    assert_eq!(
        full["cpu_light_budget"],
        json!({"reserved": 2, "active": 2, "remaining": 0, "applies": true}),
        "{full}"
    );
    let readiness = runtime
        .workspace_auto_readiness(&[], None, 50, &[])
        .unwrap();
    assert_eq!(
        readiness["capacity"]["cpu_light_budget"]["remaining"], 0,
        "{readiness:#}"
    );
    let waiting = readiness_task(&readiness, &light[2].id);
    assert_eq!(waiting["reason"], "cpu_light_budget_full", "{waiting}");
    assert!(
        waiting["detail"]
            .as_str()
            .is_some_and(|detail| detail.starts_with("2 of 2 reserved CPU-light leaves")),
        "{waiting}"
    );
    assert_eq!(
        readiness_task(&readiness, &implementation.id)["reason"],
        "resource_throttled"
    );

    // Below the resume mark every slot opens again.
    probe.cpu(Some(10.0));
    probe.reading.lock().unwrap().2 = Utc::now();
    let resumed = classify(&runtime, &drain);
    assert_eq!(resumed["resource_throttle"], Value::Null, "{resumed}");
    assert_eq!(
        resumed["loose_task_ids"],
        json!([light[2].id, implementation.id]),
        "{resumed}"
    );
    assert_eq!(resumed["cpu_light_budget"]["applies"], false, "{resumed}");
}

/// Memory and disk pressure hold light leaves like any other: an agent
/// session costs memory and its artifacts cost disk, whatever CPU does.
#[test]
fn memory_and_disk_pressure_still_hold_light_auto_tasks() {
    if !isolated("memory_and_disk_pressure_still_hold_light_auto_tasks") {
        return;
    }
    let probe = PressureProbe::calm();
    let (_root, runtime, _repo) = runtime();
    let runtime = runtime.with_host_resource_probe(probe.clone());
    let light = seed(
        &runtime,
        Seed {
            title: "review",
            tags: LIGHT_TAGS,
            ..Seed::default()
        },
    );
    let drain = running_run(&runtime, "workspace_auto_pipeline", json!({}));

    let assert_held = |pressure: &str| {
        let wave = classify(&runtime, &drain);
        assert_eq!(wave["loose_task_ids"], json!([]), "{pressure}: {wave}");
        assert_eq!(wave["free_slots"], 0, "{pressure}: {wave}");
        assert_eq!(
            wave["cpu_light_budget"]["applies"], false,
            "{pressure}: {wave}"
        );
        let readiness = runtime
            .workspace_auto_readiness(&[], None, 50, &[])
            .unwrap();
        let entry = readiness_task(&readiness, &light.id);
        assert_eq!(entry["reason"], "resource_throttled", "{pressure}: {entry}");
        assert_eq!(entry["cpu_light"], true, "{pressure}: {entry}");
    };

    // Memory beside CPU holds the light leaf a CPU-only throttle admits.
    probe.sustain(&runtime, |probe| {
        probe.cpu(Some(195.0));
        probe.memory(95.0, Utc::now());
    });
    assert_held("memory and cpu");

    probe.cpu(Some(10.0));
    probe.memory(10.0, Utc::now());
    let open = classify(&runtime, &drain);
    assert_eq!(open["loose_task_ids"], json!([light.id]), "{open}");

    probe.sustain(&runtime, |probe| probe.memory(95.0, Utc::now()));
    assert_held("memory");

    probe.memory(10.0, Utc::now());
    probe.sustain(&runtime, |probe| *probe.disk.lock().unwrap() = 95.0);
    assert_held("disk");
}

/// A frozen delivery batch within two hours of its deadline sorts ahead of
/// same-priority backlog, including corrective work, so it is not left to
/// expire in the queue; one further from its deadline keeps its place.
#[test]
fn a_frozen_batch_near_its_deadline_sorts_ahead_of_same_priority_backlog() {
    if !isolated("a_frozen_batch_near_its_deadline_sorts_ahead_of_same_priority_backlog") {
        return;
    }
    let (_root, runtime, _repo) = runtime();
    let runtime = runtime.with_automation_machine_identity(Some("hm_fixture".to_string()));
    let older = seed(
        &runtime,
        Seed {
            title: "older chore",
            ..Seed::default()
        },
    );
    let corrective = seed(
        &runtime,
        Seed {
            title: "corrective",
            task_type: TaskType::Bug,
            ..Seed::default()
        },
    );
    let high = seed(
        &runtime,
        Seed {
            title: "high priority",
            priority: TaskPriority::High,
            task_type: TaskType::Bug,
            ..Seed::default()
        },
    );
    let expiring = seed(
        &runtime,
        Seed {
            title: "expiring full review",
            tags: &["no-diff-expected", "auto-task:full-review"],
            ..Seed::default()
        },
    );
    let distant = seed(
        &runtime,
        Seed {
            title: "distant review",
            tags: &["no-diff-expected", "auto-task:friction-curation"],
            ..Seed::default()
        },
    );
    admitted_frozen_batch(
        &runtime,
        "code-review",
        &expiring.id,
        chrono::Duration::minutes(90),
    );
    admitted_frozen_batch(
        &runtime,
        "friction-curation",
        &distant.id,
        chrono::Duration::hours(5),
    );

    let order = admitted(&list_backlog_tasks(&runtime, json!({})));
    assert_eq!(
        order,
        vec![
            high.id.clone(),
            expiring.id.clone(),
            corrective.id.clone(),
            older.id.clone(),
            distant.id.clone(),
        ],
        "a higher priority still leads; the expiring batch leads its priority"
    );

    let readiness = runtime
        .workspace_auto_readiness(&[], None, 50, &[])
        .unwrap();
    let listed: Vec<&str> = readiness["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|task| task["task_id"].as_str().unwrap())
        .collect();
    assert_eq!(listed, order.iter().map(String::as_str).collect::<Vec<_>>());
    assert!(
        readiness_task(&readiness, &expiring.id)["frozen_batch_deadline"].is_string(),
        "{readiness:#}"
    );
    assert_eq!(
        readiness_task(&readiness, &distant.id)["frozen_batch_deadline"],
        Value::Null
    );
}

/// A surface reservation follows frozen-batch expiry in the queue's order:
/// expiry can promote ordinary work ahead of higher-priority work, or break a
/// same-priority tie with older corrective work [ORB-15110].
#[test]
fn surface_reservations_follow_frozen_batch_expiry_order() {
    if !isolated("surface_reservations_follow_frozen_batch_expiry_order") {
        return;
    }
    for (reserver_type, expiring_priority) in [
        (TaskType::Feature, TaskPriority::Medium),
        (TaskType::Bug, TaskPriority::High),
    ] {
        let (_root, runtime, repo) = runtime();
        let runtime = runtime.with_automation_machine_identity(Some("hm_fixture".to_string()));
        write_files(&repo, &["held.rs", "foo/expiring.rs", "foo/ordinary.rs"]);
        let holder = seed(
            &runtime,
            Seed {
                title: "lock holder",
                status: TaskStatus::InProgress,
                context_files: Some(&["held.rs"]),
                ..Seed::default()
            },
        );
        let reserver = seed(
            &runtime,
            Seed {
                title: "older high-priority reserver",
                priority: TaskPriority::High,
                task_type: reserver_type,
                context_files: Some(&["held.rs", "dir:foo"]),
                ..Seed::default()
            },
        );
        let expiring = seed(
            &runtime,
            Seed {
                title: "expiring overlapping work",
                priority: expiring_priority,
                task_type: TaskType::Feature,
                context_files: Some(&["foo/expiring.rs"]),
                ..Seed::default()
            },
        );
        let ordinary = seed(
            &runtime,
            Seed {
                title: "ordinary overlapping work",
                context_files: Some(&["foo/ordinary.rs"]),
                ..Seed::default()
            },
        );
        let before = list_backlog_tasks(&runtime, json!({}));
        assert_eq!(
            excluded_entry(&before, &expiring.id)["reason"],
            "surface_reserved",
            "without expiry the reserver ranks ahead: {before:#}"
        );
        admitted_frozen_batch(
            &runtime,
            "code-review",
            &expiring.id,
            chrono::Duration::minutes(90),
        );

        let output = list_backlog_tasks(&runtime, json!({}));
        assert_eq!(
            admitted(&output),
            vec![expiring.id.clone()],
            "expiry-aware order must let overlapping work ahead of the reserver admit: {output:#}"
        );
        let waiting = excluded_entry(&output, &reserver.id);
        assert_eq!(waiting["reason"], "context_lock_conflict", "{waiting}");
        assert_eq!(waiting["conflicts"][0]["locking_task_id"], holder.id);
        let withheld = excluded_entry(&output, &ordinary.id);
        assert_eq!(withheld["reason"], "surface_reserved", "{withheld}");
        assert_eq!(withheld["conflicts"][0]["locking_task_id"], reserver.id);

        let drain = running_run(&runtime, "workspace_auto_pipeline", json!({}));
        let wave = classify(&runtime, &drain);
        assert_eq!(wave["loose_task_ids"], json!([expiring.id]), "{wave:#}");
        let readiness = runtime
            .workspace_auto_readiness(&[], None, 50, &[])
            .unwrap();
        let ready = readiness_task(&readiness, &expiring.id);
        assert_eq!(ready["eligible"], true, "{ready}");
        assert_eq!(ready["reason"], "ready", "{ready}");
        assert!(ready["frozen_batch_deadline"].is_string(), "{ready}");

        // With both batches expiring, priority or age puts the reserver
        // first again: expiry must be considered on both sides.
        admitted_frozen_batch(
            &runtime,
            "friction-curation",
            &reserver.id,
            chrono::Duration::minutes(90),
        );
        let both_expiring = list_backlog_tasks(&runtime, json!({}));
        assert_eq!(
            excluded_entry(&both_expiring, &expiring.id)["reason"],
            "surface_reserved",
            "the reserver's expiry must also affect ranking: {both_expiring:#}"
        );
    }
}

/// Record `task_id` as the admitted action of a frozen batch on this
/// workspace's `name` consumer, due `remaining` from now — the state delivery
/// automation leaves after minting the batch's task.
pub(super) fn admitted_frozen_batch(
    runtime: &OrbitRuntime,
    name: &str,
    task_id: &str,
    remaining: chrono::Duration,
) {
    use orbit_types::workflow::automation::{
        AutomationState, BatchAttempt, BatchState, CoverageBatch, CoverageClass, SourceRevision,
    };
    let consumer = format!(
        "{}/{}/auto-task/{name}",
        runtime.automation_machine_identity().unwrap(),
        runtime.workspace_id().unwrap()
    );
    let revision = |commit: &str| SourceRevision {
        commit: commit.to_string(),
        tree: format!("{commit}-tree"),
    };
    let baseline = AutomationState {
        members: None,
        consumer: consumer.clone(),
        epoch: "epoch".into(),
        trigger: None,
        repository: "fixture-repo".into(),
        branch: "main".into(),
        generation: 0,
        baseline: revision("base"),
        observed: revision("base"),
        covered: revision("base"),
        pending_commits: vec![],
        pending: vec![],
        waived: vec![],
        excluded: vec![],
        unresolved: Default::default(),
        associations: Default::default(),
        lookup_retries: Default::default(),
        active: None,
        stall: None,
    };
    let store = runtime.automation_store().unwrap();
    assert!(store.automation_initialize(&baseline).unwrap());
    let mut admitted = baseline.clone();
    admitted.generation = 1;
    admitted.observed = revision("landing");
    admitted.pending_commits = vec!["landing".into()];
    admitted.active = Some(BatchAttempt {
        batch: CoverageBatch {
            schema_version: 1,
            id: format!("batch-{name}"),
            consumer,
            epoch: "epoch".into(),
            repository: "fixture-repo".into(),
            branch: "main".into(),
            coverage: CoverageClass::LandedCodeReviewV1,
            from_exclusive: revision("base"),
            through_inclusive: revision("landing"),
            commits: vec!["landing".into()],
            deliveries: vec![],
            exclusions: vec![],
            created_at: Utc::now() - chrono::Duration::hours(8),
            max_attempts: 1,
            retry_until: Utc::now() + remaining,
        },
        input_digest: "input-digest".into(),
        attempt: 1,
        action_key: format!("automation:batch-{name}:1"),
        action_id: Some(task_id.to_string()),
        state: BatchState::Admitted,
        reason: None,
        retry_after: None,
        reissue: None,
    });
    assert!(store.automation_commit(&baseline, &admitted, None).unwrap());
}
