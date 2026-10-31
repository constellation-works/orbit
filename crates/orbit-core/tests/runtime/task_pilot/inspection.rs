//! Status polling reads cached refs without fetching; the real clock sweep
//! still refreshes them. Git's process trace records actual fetch children.

use std::time::Instant;

use orbit_core::application::routines::loader::{DiscoveredWorkspaces, RoutineWorkspaceProvider};
use orbit_core::application::routines::{
    RoutineMachineIdentity, SweepOptions, routine_statuses_with_providers,
};
use orbit_types::workspace::{Workspace as WorkspaceRecord, WorkspaceStatus};

use super::*;

struct SingleWorkspace(OrbitRuntime);

impl RoutineWorkspaceProvider for SingleWorkspace {
    fn discover_workspaces(
        &self,
        _: &Path,
    ) -> Result<DiscoveredWorkspaces, orbit_core::OrbitError> {
        Ok(DiscoveredWorkspaces {
            entries: vec![(
                WorkspaceRecord {
                    id: self.0.workspace_id()?,
                    name: "inspection-fixture".into(),
                    owner_machine_id: None,
                    git_remote: None,
                    ship_mode: None,
                    base_branch: "main".into(),
                    status: WorkspaceStatus::Active,
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                },
                self.0.clone(),
            )],
            ..DiscoveredWorkspaces::default()
        })
    }
}

fn machine() -> RoutineMachineIdentity {
    RoutineMachineIdentity {
        machine_id: "fixture-machine".into(),
        machine_name: "fixture-host".into(),
    }
}

fn automation(workspace: &Workspace, now: chrono::DateTime<Utc>) -> Value {
    let report = routine_statuses_with_providers(
        &workspace.runtime.global_root(),
        machine(),
        &SingleWorkspace(workspace.runtime.clone()),
        now,
    )
    .unwrap();
    assert!(report.load_errors.is_empty(), "{:?}", report.load_errors);
    assert_eq!(report.statuses.len(), 1);
    report.statuses[0].automation.clone().unwrap()
}

fn fetches(trace: &Path) -> usize {
    std::fs::read_to_string(trace)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|event| event["event"] == "start" && event["argv"][1] == "fetch")
        .count()
}

fn routine_status_reads_cached_refs_while_clock_sweep_fetches() {
    let workspace = Workspace::new();
    workspace.install_pilot_job();
    let task = workspace.task("inspection candidate");
    let routines = workspace.runtime.shared_root().join("routines");
    std::fs::create_dir_all(&routines).unwrap();
    std::fs::write(
        routines.join("fixture-pilot.yaml"),
        serde_yaml::to_string(&pilot_routine()).unwrap(),
    )
    .unwrap();
    let remote = workspace.root.path().join("origin.git");
    workspace.git(&["clone", "--bare", ".", remote.to_str().unwrap()]);
    workspace.git(&["remote", "add", "origin", remote.to_str().unwrap()]);
    workspace.git(&["fetch", "origin", "main"]);
    let head = workspace.object("refs/remotes/origin/main");
    let now = Utc::now();

    let trace = workspace
        .root
        .path()
        .canonicalize()
        .unwrap()
        .join("git-trace.jsonl");
    let _env =
        orbit_common::test_env::scoped([("GIT_TRACE2_EVENT", Some(trace.to_str().unwrap()))]);
    let mut samples = Vec::new();
    let mut cached = None;
    for _ in 0..5 {
        let started = Instant::now();
        let status = automation(&workspace, now);
        samples.push(started.elapsed().as_secs_f64());
        if let Some(expected) = &cached {
            assert_eq!(
                &status, expected,
                "polling preserves every automation field"
            );
        } else {
            cached = Some(status);
        }
    }
    let cached = cached.unwrap();
    let inspection_fetches = fetches(&trace);
    let inspection_children = std::fs::read_to_string(&trace)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|event| event["event"] == "start")
        .count();
    assert_eq!(
        inspection_children, 0,
        "routine status reads refs, ancestry and immutable task material without Git subprocesses"
    );
    assert_eq!(cached["reason"], "debouncing");
    assert_eq!(
        cached["state"]["members"]["pending"][&task.id]["source"]["commit"],
        head
    );

    // Preparation has always used the local branch when origin cannot be
    // resolved. A never-fetched ref retains that status, without creating it.
    workspace.git(&["update-ref", "-d", "refs/remotes/origin/main"]);
    let before_missing = fetches(&trace);
    let missing = automation(&workspace, now);
    let missing_fetches = fetches(&trace) - before_missing;
    assert_eq!(
        missing, cached,
        "an unfetched pilot retains its local-source status"
    );

    let before_tick = fetches(&trace);
    orbit_core::test_support::run_sweep_at(
        &workspace.runtime.global_root(),
        SweepOptions::default(),
        machine(),
        &SingleWorkspace(workspace.runtime.clone()),
        now,
    )
    .unwrap();
    let tick_fetches = fetches(&trace) - before_tick;
    assert_eq!(workspace.object("refs/remotes/origin/main"), head);
    let mut sorted = samples.clone();
    sorted.sort_by(f64::total_cmp);
    tracing::info!(
        evidence = %json!({
            "samples_seconds": samples, "p50_seconds": sorted[2],
            "inspection_fetches": inspection_fetches, "missing_ref_fetches": missing_fetches,
            "tick_fetches": tick_fetches, "automation": cached,
        }),
        "routine inspection evidence"
    );
    assert_eq!(
        inspection_fetches, 0,
        "status polling must spawn no git fetch children"
    );
    assert_eq!(
        missing_fetches, 0,
        "an unfetched ref must not make inspection fetch"
    );
    assert!(
        tick_fetches > 0,
        "the non-dry-run clock sweep must still fetch"
    );
}

fn inspection_uses_cached_origin_and_preserves_unfetched_source_reasons() {
    let workspace = Workspace::new();
    let task = workspace.task("cached source candidate");
    let local = workspace.object("HEAD");
    workspace.commit_file("README.md", "remote material\n", "advance origin");
    let origin = workspace.object("HEAD");
    let remote = workspace.root.path().join("origin.git");
    workspace.git(&["clone", "--bare", ".", remote.to_str().unwrap()]);
    workspace.git(&["reset", "--hard", &local]);
    workspace.git(&["remote", "add", "origin", remote.to_str().unwrap()]);
    workspace.git(&["update-ref", "refs/remotes/origin/main", &origin]);
    let now = Utc::now();
    let inspect = |definition: &RoutineDefinition| {
        orbit_core::application::automation::inspect_routine(&workspace.runtime, definition, now)
            .unwrap()
    };
    let source = |diagnostic: orbit_types::workflow::automation::AutomationDiagnostic| {
        diagnostic.state.unwrap().members.unwrap().pending[&task.id]
            .source
            .commit
            .clone()
    };
    assert_eq!(source(inspect(&pilot_routine())), origin);
    workspace.git(&["update-ref", "-d", "refs/remotes/origin/main"]);
    let unfetched = inspect(&pilot_routine());
    assert_eq!(unfetched.reason, "debouncing");
    assert_eq!(source(unfetched), local);
    let delivery: RoutineDefinition = serde_json::from_value(json!({
        "schemaVersion": 1, "name": "fixture-delivery", "enabled": true,
        "target": "job:task_pilot_pipeline",
        "trigger": {"deliveries_landed": {
            "owner_machine": "fixture-machine", "branch": "main", "threshold": 1,
            "max_wait_minutes": 10, "coverage": "landed_code_review_v1",
        }},
    }))
    .unwrap();
    assert_eq!(inspect(&delivery).reason, "awaiting_baseline");
    assert!(
        workspace
            .git(&["for-each-ref", "refs/remotes/origin/main"])
            .is_empty(),
        "inspection must leave a never-fetched ref absent"
    );
}

/// libgit2 refuses repository extensions it does not implement, such as a
/// partial clone's. Status then reads the same refs and material through Git,
/// so every automation field is unchanged, and a branch that does not resolve
/// reports Git's own text from either reader.
fn inspection_of_a_repository_libgit2_cannot_open_matches_the_in_process_read() {
    let workspace = Workspace::new();
    let task = workspace.task("fallback candidate");
    workspace.commit_file("AGENTS.md", "repository instructions\n", "instructions");
    workspace.commit_file("nested/CLAUDE.md", "nested instructions\n", "nested");
    let local = workspace.object("HEAD");
    workspace.commit_file("README.md", "remote material\n", "advance origin");
    let origin = workspace.object("HEAD");
    let remote = workspace.root.path().join("origin.git");
    workspace.git(&["clone", "--bare", ".", remote.to_str().unwrap()]);
    workspace.git(&["reset", "--hard", &local]);
    workspace.git(&["remote", "add", "origin", remote.to_str().unwrap()]);
    workspace.git(&["update-ref", "refs/remotes/origin/main", &origin]);
    let now = Utc::now();
    let inspect = |definition: &RoutineDefinition| {
        orbit_core::application::automation::inspect_routine(&workspace.runtime, definition, now)
            .unwrap()
    };
    let delivery = |branch: &str| -> RoutineDefinition {
        serde_json::from_value(json!({
            "schemaVersion": 1, "name": "fixture-delivery", "enabled": true,
            "target": "job:task_pilot_pipeline",
            "trigger": {"deliveries_landed": {
                "owner_machine": "fixture-machine", "branch": branch, "threshold": 1,
                "max_wait_minutes": 10, "coverage": "landed_code_review_v1",
            }},
        }))
        .unwrap()
    };
    let observe = || {
        (
            json!(inspect(&pilot_routine())),
            inspect(&delivery("main")).reason,
            inspect(&delivery("no-such-branch")).reason,
            inspect(&delivery("bad..name")).reason,
        )
    };

    let in_process = observe();
    assert_eq!(
        in_process.0["state"]["members"]["pending"][&task.id]["source"]["commit"],
        origin
    );
    assert_eq!(in_process.1, "awaiting_baseline");
    assert_eq!(
        in_process.2,
        "evidence_unavailable: git rev-parse --verify --end-of-options \
         refs/heads/no-such-branch^{commit}: fatal: Needed a single revision"
    );
    assert_eq!(
        in_process.3,
        "evidence_unavailable: git check-ref-format --branch bad..name: \
         fatal: 'bad..name' is not a valid branch name"
    );

    workspace.git(&["config", "core.repositoryformatversion", "1"]);
    workspace.git(&["config", "extensions.partialClone", "origin"]);
    assert!(
        git2::Repository::open(&workspace.repo).is_err(),
        "the fixture must be a repository libgit2 refuses"
    );
    assert_eq!(
        observe(),
        in_process,
        "Git reads the refused repository to the same status"
    );
}

/// Every task-commit lock (host and partition) under `root`.
fn task_commit_locks(root: &Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with("task-commit.lock"))
            {
                found.push(path);
            }
        }
    }
    found
}

/// A writer queued for the task store holds back every ordinary read that
/// arrives after it. Routine status decides nothing, so it reads past that
/// writer: held turnstiles, not load, fix the interleaving here, and an
/// ordinary read started meanwhile is still waiting when status returns.
#[test]
fn routine_status_does_not_wait_behind_a_queued_task_commit_writer() {
    use fs2::FileExt;

    if !super::super::dispatch_admission::isolated(
        "task_pilot::inspection::routine_status_does_not_wait_behind_a_queued_task_commit_writer",
    ) {
        return;
    }
    let workspace = Workspace::new();
    workspace.install_pilot_job();
    let task = workspace.task("queued writer candidate");
    let routines = workspace.runtime.shared_root().join("routines");
    std::fs::create_dir_all(&routines).unwrap();
    std::fs::write(
        routines.join("fixture-pilot.yaml"),
        serde_yaml::to_string(&pilot_routine()).unwrap(),
    )
    .unwrap();
    let now = Utc::now();
    let unqueued = automation(&workspace, now);
    assert_eq!(unqueued["reason"], "debouncing");
    assert!(unqueued["state"]["members"]["pending"][&task.id].is_object());

    let locks = task_commit_locks(&workspace.runtime.global_root());
    assert_eq!(locks.len(), 2, "host and partition locks: {locks:?}");
    let turnstiles: Vec<_> = locks
        .iter()
        .map(|lock| {
            let mut name = lock.file_name().unwrap().to_owned();
            name.push(".turnstile");
            let turnstile = std::fs::File::create(lock.with_file_name(name)).unwrap();
            turnstile.lock_exclusive().unwrap();
            turnstile
        })
        .collect();

    let ordinary = {
        let runtime = workspace.runtime.clone();
        let id = task.id.clone();
        std::thread::spawn(move || runtime.get_task(&id).map(|task| task.id))
    };
    let queued = automation(&workspace, now);
    let ordinary_waiting = !ordinary.is_finished();
    drop(turnstiles);
    assert_eq!(
        queued, unqueued,
        "status reads past the queued writer and reports the same automation"
    );
    assert!(
        ordinary_waiting,
        "an ordinary read waits behind the queued writer"
    );
    assert_eq!(ordinary.join().unwrap().unwrap(), task.id);
}

/// `preparation_eligible` is the only state kind whose target job this
/// fixture's catalog resolves.
fn state_routine_yaml(name: &str, enabled: bool) -> String {
    format!(
        r#"schemaVersion: 1
name: {name}
enabled: {enabled}
target: job:task_pilot_pipeline
trigger:
  state:
    kind: preparation_eligible
    owner_machine: fixture-machine
    branch: main
    debounce_minutes: 2
    max_wait_minutes: 10
    max_items: 50
    retries: 1
    deadline_minutes: 90
policy:
  timeout_minutes: 90
  overlap: forbid
  retries: {{ max: 1, backoff_minutes: 5 }}
"#
    )
}

/// The sweep skips a state routine for duplicate ownership when an enabled
/// same-kind peer shares its target. `routine show` and `list` must report that
/// skip, not the evaluator's dry-run reason. A disabled routine owns nothing and
/// keeps its own reason. [ORB-17918]
#[test]
fn status_reports_the_duplicate_skip_the_sweep_records_for_a_state_routine() {
    let workspace = Workspace::new();
    workspace.install_pilot_job();
    let routines = workspace.repo.join(".orbit/routines");
    std::fs::create_dir_all(&routines).unwrap();
    for (name, enabled) in [("pilot-a", true), ("pilot-b", true), ("parked", false)] {
        std::fs::write(
            routines.join(format!("{name}.yaml")),
            state_routine_yaml(name, enabled),
        )
        .unwrap();
    }

    let report = routine_statuses_with_providers(
        &workspace.runtime.global_root(),
        machine(),
        &SingleWorkspace(workspace.runtime.clone()),
        Utc::now(),
    )
    .unwrap();
    assert!(report.load_errors.is_empty(), "{:?}", report.load_errors);
    let reason = |name: &str| {
        let status = report
            .statuses
            .iter()
            .find(|status| status.routine.definition.name == name)
            .unwrap();
        status.automation.as_ref().unwrap()["reason"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(reason("pilot-a"), "duplicate_routine_ownership");
    assert_eq!(reason("pilot-b"), "duplicate_routine_ownership");
    assert_ne!(reason("parked"), "duplicate_routine_ownership");
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    routine_status_reads_cached_refs_while_clock_sweep_fetches_scenarios,
    routine_status_reads_cached_refs_while_clock_sweep_fetches,
    inspection_uses_cached_origin_and_preserves_unfetched_source_reasons,
    inspection_of_a_repository_libgit2_cannot_open_matches_the_in_process_read
);
