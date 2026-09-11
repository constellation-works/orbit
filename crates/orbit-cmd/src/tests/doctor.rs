//! Sibling tests for `command/doctor.rs` — workspace self-diagnostics [ORB-10005].

use std::fs;
use std::path::Path;

use chrono::Utc;
use fs2::FileExt;
use orbit_registry::workspace_registry;
use orbit_store::maintenance::task_registry::{
    BindWorkspaceParams, TaskRegistryStore, task_registry_path, task_workspaces_dir,
};
use orbit_types::workflow::{JobRun, JobRunState};
use orbit_types::workspace::{Workspace, WorkspaceStatus};
use sha2::{Digest, Sha256};

use orbit_core::OrbitRuntime;
use orbit_core::runtime::OrbitRuntimeRoots;
use orbit_store::TaskReservationReserveParams;

use crate::doctor::{
    DoctorCommands, WorkspaceDoctorResult, WorkspaceDoctorStatus, collect_lock_files,
    disk_space_check, process_is_alive,
};
use crate::task_store::partition_is_bound;

fn status_of<'a>(results: &'a [WorkspaceDoctorResult], name: &str) -> &'a WorkspaceDoctorResult {
    results
        .iter()
        .find(|row| row.check_name == name)
        .unwrap_or_else(|| panic!("check '{name}' missing from {results:?}"))
}

fn workspace_runtime(temp: &tempfile::TempDir) -> OrbitRuntime {
    let global_root = temp.path().join("global");
    let workspace_root = temp.path().join("repo").join(".orbit");
    fs::create_dir_all(&global_root).expect("create global root");
    fs::create_dir_all(&workspace_root).expect("create workspace root");
    OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build runtime")
}

fn write_skill(root: &Path, id: &str, purpose: &str) {
    let dir = root.join(id);
    fs::create_dir_all(&dir).expect("create skill dir");
    fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {id}\ndescription: test skill\n---\n\n# Purpose\n\n{purpose}\n"),
    )
    .expect("write skill");
}

fn split_root_runtime(temp: &tempfile::TempDir) -> OrbitRuntime {
    let global_root = temp.path().join("global");
    let shared_root = temp.path().join("main").join(".orbit");
    let local_root = temp.path().join("worktree").join(".orbit");
    for root in [&global_root, &shared_root, &local_root] {
        fs::create_dir_all(root).expect("create runtime root");
    }
    OrbitRuntime::from_resolved_roots(&global_root, &shared_root, &local_root)
        .expect("build split-root runtime")
}

fn write_registered_workspace(global_root: &Path, workspace_id: &str, name: &str) {
    let registry_path = workspace_registry::registry_path_for(global_root);
    let mut registry =
        workspace_registry::load_registry_from(&registry_path).expect("load registry");
    let now = Utc::now();
    workspace_registry::register_workspace(
        &mut registry,
        Workspace {
            id: workspace_id.to_string(),
            name: name.to_string(),
            owner_machine_id: None,
            git_remote: None,
            ship_mode: None,
            base_branch: "main".to_string(),
            status: WorkspaceStatus::Active,
            created_at: now,
            updated_at: now,
        },
    )
    .expect("register workspace");
    workspace_registry::save_registry_to(&registry, &registry_path).expect("save registry");
}

/// Bind a checkout in the *task* registry — the id space the partition
/// directories are named after, which a `<slug>-<hash>` id inhabits and the
/// workspace catalog's `ws_*` ids do not [ORB-12119].
fn bind_task_partition(global_root: &Path, workspace_id: &str, slug: &str, repo_root: &Path) {
    let tasks =
        TaskRegistryStore::open(&task_registry_path(global_root)).expect("open task registry");
    tasks
        .bind_workspace(BindWorkspaceParams {
            workspace_id: Some(workspace_id.to_string()),
            slug: slug.to_string(),
            repo_root: repo_root.to_path_buf(),
            workspace_path: repo_root.to_path_buf(),
            orbit_dir: repo_root.join(".orbit"),
            repo_fingerprint: None,
        })
        .expect("bind task-registry workspace");
}

fn write_task_bundle(global_root: &Path, workspace_id: &str, task_id: &str) {
    let bundle = task_workspaces_dir(global_root)
        .join(workspace_id)
        .join(task_id);
    fs::create_dir_all(&bundle).expect("create task bundle dir");
    fs::write(bundle.join("task.yaml"), b"id: dummy\n").expect("write bundle file");
}

/// A partition directory emptied of its bundles, as `workspace teardown` on an
/// older binary left it behind.
fn write_empty_partition(global_root: &Path, workspace_id: &str) {
    fs::create_dir_all(task_workspaces_dir(global_root).join(workspace_id))
        .expect("create empty partition dir");
}

#[test]
fn healthy_fresh_workspace_has_no_failures() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let results = runtime.doctor_workspace().expect("doctor");

    // Nine infrastructure checks plus one definition-artifact row per kind
    // (skills, jobs, activities, auto-tasks, routines).
    assert_eq!(results.len(), 14, "one row per check: {results:?}");
    assert!(
        results
            .iter()
            .all(|row| row.status != WorkspaceDoctorStatus::Error),
        "fresh workspace must not fail any check: {results:?}"
    );
    assert_eq!(
        status_of(&results, "config").status,
        WorkspaceDoctorStatus::Ok
    );
    assert_eq!(
        status_of(&results, "database").status,
        WorkspaceDoctorStatus::Ok
    );
    // Absent subsystems degrade to skip, not error.
    assert_eq!(
        status_of(&results, "semantic-index").status,
        WorkspaceDoctorStatus::Skipped
    );
    assert!(
        results.iter().all(|row| row.check_name != "graph-index"),
        "retired graph state is not a health subsystem: {results:?}"
    );
    assert_eq!(
        status_of(&results, "stale-locks").status,
        WorkspaceDoctorStatus::Ok
    );
    assert_eq!(
        status_of(&results, "job-runs").status,
        WorkspaceDoctorStatus::Ok
    );
    assert_eq!(
        status_of(&results, "task-reservations").status,
        WorkspaceDoctorStatus::Ok
    );
    // No tasks yet → no unresolved relation/dependency targets.
    assert_eq!(
        status_of(&results, "task-relations").status,
        WorkspaceDoctorStatus::Ok
    );
    // No task ever committed on this host → no partitions to flag as orphaned.
    assert_eq!(
        status_of(&results, "orphan-task-stores").status,
        WorkspaceDoctorStatus::Ok
    );
}

#[test]
fn every_warning_or_error_has_structured_remediation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);
    fs::write(
        temp.path().join("repo").join(".orbit").join("config.toml"),
        "not = [valid toml",
    )
    .expect("write broken config");

    let results = runtime.doctor_workspace().expect("doctor");
    let actionable = results.iter().filter(|row| {
        matches!(
            row.status,
            WorkspaceDoctorStatus::Warning | WorkspaceDoctorStatus::Error
        )
    });
    for row in actionable {
        assert!(
            row.remediation
                .as_ref()
                .is_some_and(|value| !value.is_empty()),
            "actionable row needs remediation: {row:?}"
        );
    }
}

#[test]
fn skill_residue_warns_and_names_the_directory() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);
    let skills = temp.path().join("global/skills");
    write_skill(&skills, "orbit", "healthy global skill");
    let residue = skills.join("orbit-search");
    fs::create_dir_all(residue.join("references")).expect("create residue references");
    fs::write(residue.join("references/search.md"), "retired reference\n")
        .expect("write residue reference");
    let empty_residue = skills.join("orbit-task");
    fs::create_dir(&empty_residue).expect("create empty residue");

    let results = runtime.doctor_workspace().expect("doctor");
    let row = status_of(&results, "artifacts-skills");
    assert_eq!(row.status, WorkspaceDoctorStatus::Warning, "{row:?}");
    assert!(row.message.contains("residual"), "{}", row.message);
    assert!(
        row.message.contains(residue.to_string_lossy().as_ref()),
        "DETAILS must name the residue directory: {}",
        row.message
    );
    assert!(
        row.message
            .contains(empty_residue.to_string_lossy().as_ref()),
        "DETAILS must name the empty residue directory: {}",
        row.message
    );
    assert_ne!(row.status, WorkspaceDoctorStatus::Error);
}

#[test]
fn healthy_single_skill_and_workspace_shadow_keep_the_loaded_count() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);
    write_skill(&temp.path().join("global/skills"), "orbit", "global skill");

    let global_only = runtime.doctor_workspace().expect("doctor global catalog");
    let row = status_of(&global_only, "artifacts-skills");
    assert_eq!(row.status, WorkspaceDoctorStatus::Ok, "{row:?}");
    assert!(
        row.message.starts_with("1 skills loaded"),
        "{}",
        row.message
    );
    assert!(row.message.contains("none residual"), "{}", row.message);

    write_skill(
        &temp.path().join("repo/.orbit/skills"),
        "orbit",
        "workspace override",
    );
    let shadowed = runtime.doctor_workspace().expect("doctor layered catalog");
    let row = status_of(&shadowed, "artifacts-skills");
    assert_eq!(row.status, WorkspaceDoctorStatus::Ok, "{row:?}");
    assert!(
        row.message.starts_with("1 skills loaded"),
        "{}",
        row.message
    );
}

#[test]
fn absent_owner_task_reservation_warning_names_context_reason_and_exact_repair() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);
    let store = runtime.sqlite_store().expect("store");
    let reservation = store
        .reserve_task_reservation(&TaskReservationReserveParams {
            workspace_orbit_dir: runtime.paths().orbit_dir.to_string_lossy().into_owned(),
            workspace_id: None,
            task_ids: vec!["ORB-12345".to_string()],
            requested_files: vec!["file:src/lib.rs".to_string()],
            actor: "test".to_string(),
            ttl_seconds: 3600,
            owner_run_id: Some("jrun-missing".to_string()),
            owner_metadata_json: None,
        })
        .expect("reserve")
        .reservation_id
        .expect("reservation id");

    let results = runtime.doctor_workspace().expect("doctor");
    let row = status_of(&results, "task-reservations");
    assert_eq!(row.status, WorkspaceDoctorStatus::Warning, "{row:?}");
    assert!(row.message.contains(&reservation), "{}", row.message);
    assert!(row.message.contains("ORB-12345"), "{}", row.message);
    assert!(row.message.contains("jrun-missing"), "{}", row.message);
    assert!(row.message.contains("is absent"), "{}", row.message);
    assert_eq!(
        row.remediation.as_deref(),
        Some("Run `orbit doctor --fix-stale-task-locks`.")
    );
    let still_active = store
        .inspect_active_task_reservations(&runtime.paths().orbit_dir.to_string_lossy(), None)
        .expect("inspect after read-only doctor");
    assert!(
        still_active
            .iter()
            .any(|candidate| candidate.reservation_id == reservation),
        "ordinary doctor must not release a diagnosed reservation"
    );
}

#[test]
fn invalid_config_fails_the_config_check() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);

    // Written after runtime construction (an invalid config would fail the
    // bootstrap itself); doctor re-validates the effective file.
    fs::write(
        temp.path().join("repo").join(".orbit").join("config.toml"),
        "not = [valid toml",
    )
    .expect("write broken config");

    let results = runtime.doctor_workspace().expect("doctor");
    let config = status_of(&results, "config");
    assert_eq!(config.status, WorkspaceDoctorStatus::Error, "{config:?}");
    assert!(
        config.message.contains("invalid"),
        "message names the failure: {}",
        config.message
    );
}

#[test]
fn unopenable_store_database_fails_the_database_check() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);

    // Make the store database path unopenable for the probe's fresh
    // connection. (Overwriting the file with garbage is not enough: the
    // runtime's live WAL still serves valid pages to new connections.)
    let db_path = temp.path().join("global").join("orbit.db");
    fs::remove_file(&db_path).expect("remove store db");
    fs::create_dir(&db_path).expect("block store db path");

    let results = runtime.doctor_workspace().expect("doctor");
    let database = status_of(&results, "database");
    assert_eq!(
        database.status,
        WorkspaceDoctorStatus::Error,
        "{database:?}"
    );
    assert!(
        database.message.contains("cannot open store database"),
        "message names the failure: {}",
        database.message
    );
}

#[test]
fn missing_store_database_is_reported_without_recreating_it() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);
    let db_path = temp.path().join("global").join("orbit.db");
    fs::remove_file(&db_path).expect("remove store db");

    let results = runtime.doctor_workspace().expect("doctor");
    let database = status_of(&results, "database");
    assert_eq!(
        database.status,
        WorkspaceDoctorStatus::Error,
        "{database:?}"
    );
    assert!(database.message.contains("cannot open store database"));
    assert!(
        !db_path.exists(),
        "doctor must not recreate a missing database"
    );
}

#[cfg(unix)]
fn reaped_child_pid() -> u32 {
    let mut child = std::process::Command::new("true")
        .spawn()
        .expect("spawn child");
    let pid = child.id();
    child.wait().expect("reap child");
    pid
}

#[cfg(unix)]
fn write_holder_lock(path: &Path, pid: u32, label: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create lock dir");
    }
    fs::write(
        path,
        serde_json::to_string(&serde_json::json!({
            "pid": pid,
            "acquired_at": Utc::now().to_rfc3339(),
            "label": label,
        }))
        .expect("serialize holder"),
    )
    .expect("write lock file");
}

#[cfg(unix)]
#[test]
fn dead_holder_lock_file_is_reported_stale() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);

    let lock_path = temp
        .path()
        .join("repo")
        .join(".orbit")
        .join("state")
        .join(".dead-holder.lock");
    write_holder_lock(&lock_path, reaped_child_pid(), "crashed op");

    let results = runtime.doctor_workspace().expect("doctor");
    let locks = status_of(&results, "stale-locks");
    assert_eq!(locks.status, WorkspaceDoctorStatus::Warning, "{locks:?}");
    assert!(
        locks.message.contains(".dead-holder.lock") && locks.message.contains("crashed op"),
        "message names the stale lock and its op: {}",
        locks.message
    );
}

#[cfg(unix)]
#[test]
fn interrupted_layout_upgrade_is_reported_stale() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);
    let lock_path = temp
        .path()
        .join("repo")
        .join(".orbit")
        .join("state")
        .join("layout.lock");
    write_holder_lock(&lock_path, reaped_child_pid(), "layout upgrade");

    let results = runtime.doctor_workspace().expect("doctor");
    let locks = status_of(&results, "stale-locks");
    assert_eq!(locks.status, WorkspaceDoctorStatus::Warning, "{locks:?}");
    assert!(
        locks.message.contains("layout.lock") && locks.message.contains("layout upgrade"),
        "message names the interrupted layout upgrade: {}",
        locks.message
    );
}

#[cfg(unix)]
#[test]
fn stale_workspace_state_lock_files_are_removed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);
    let stale_locks = [runtime.paths().state_dir.join(".crashed-op.lock")];

    let dead_pid = reaped_child_pid();
    for path in &stale_locks {
        write_holder_lock(path, dead_pid, "crashed op");
    }

    assert_eq!(
        runtime
            .remove_stale_lock_files()
            .expect("remove stale locks"),
        stale_locks.len()
    );
    assert!(
        stale_locks.iter().all(|path| !path.exists()),
        "all dead-holder files must be removed: {stale_locks:?}"
    );
    assert_eq!(
        status_of(&runtime.doctor_workspace().expect("doctor"), "stale-locks").status,
        WorkspaceDoctorStatus::Ok
    );
}

#[cfg(unix)]
#[test]
fn cleanup_preserves_a_lock_held_by_a_live_process() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);
    let lock_path = runtime.paths().state_dir.join(".held-op.lock");
    write_holder_lock(&lock_path, reaped_child_pid(), "stale metadata");

    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .expect("open lock file");
    file.lock_exclusive().expect("hold lock");

    assert_eq!(
        runtime
            .remove_stale_lock_files()
            .expect("clean stale locks"),
        0
    );
    assert!(lock_path.exists(), "a held lock file must remain");
    file.unlock().expect("unlock lock file");
}

#[cfg(unix)]
#[test]
fn live_holder_lock_file_is_not_stale() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);

    let lock_path = temp
        .path()
        .join("repo")
        .join(".orbit")
        .join("state")
        .join(".live-holder.lock");
    write_holder_lock(&lock_path, std::process::id(), "live op");

    let results = runtime.doctor_workspace().expect("doctor");
    assert_eq!(
        status_of(&results, "stale-locks").status,
        WorkspaceDoctorStatus::Ok,
        "a live holder must not be reported stale"
    );
}

#[cfg(unix)]
#[test]
fn orphaned_running_run_is_reported() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let workspace_id = runtime.workspace_id().expect("workspace id");
    let now = Utc::now();
    let run = JobRun {
        run_id: "run-orphan".to_string(),
        job_id: "demo".to_string(),
        attempt: 1,
        state: JobRunState::Running,
        scheduled_at: now,
        started_at: Some(now),
        finished_at: None,
        duration_ms: None,
        created_at: now,
        // No recorded owner: classified `Missing` — conclusively orphaned.
        pid: None,
        pid_start_time: None,
        input: None,
        retry_source_run_id: None,
        knowledge_metrics: None,
        resolved_crew: None,
        crew_model: None,
        steps: Vec::new(),
    };
    runtime
        .sqlite_store()
        .expect("store")
        .upsert_job_run_for_workspace(&workspace_id, &run, None)
        .expect("seed running run");

    let results = runtime.doctor_workspace().expect("doctor");
    let job_runs = status_of(&results, "job-runs");
    assert_eq!(
        job_runs.status,
        WorkspaceDoctorStatus::Warning,
        "{job_runs:?}"
    );
    assert!(
        job_runs.message.contains("run-orphan"),
        "message names the orphaned run: {}",
        job_runs.message
    );
}

/// [ORB-10070] A `pending` run no worker ever claimed, old enough that the
/// claim grace window has passed, is reported as an orphan.
#[test]
fn orphaned_pending_run_is_reported() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let workspace_id = runtime.workspace_id().expect("workspace id");
    let created_at = Utc::now() - chrono::Duration::days(4);
    let run = JobRun {
        run_id: "run-pending-orphan".to_string(),
        job_id: "task_gate_pipeline".to_string(),
        attempt: 1,
        state: JobRunState::Pending,
        scheduled_at: created_at,
        started_at: None,
        finished_at: None,
        duration_ms: None,
        created_at,
        // Never claimed by a worker; far past the unclaimed grace window.
        pid: None,
        pid_start_time: None,
        input: None,
        retry_source_run_id: None,
        knowledge_metrics: None,
        resolved_crew: None,
        crew_model: None,
        steps: Vec::new(),
    };
    runtime
        .sqlite_store()
        .expect("store")
        .upsert_job_run_for_workspace(&workspace_id, &run, None)
        .expect("seed pending run");

    let results = runtime.doctor_workspace().expect("doctor");
    let job_runs = status_of(&results, "job-runs");
    assert_eq!(
        job_runs.status,
        WorkspaceDoctorStatus::Warning,
        "{job_runs:?}"
    );
    assert!(
        job_runs.message.contains("run-pending-orphan"),
        "message names the orphaned pending run: {}",
        job_runs.message
    );
    assert!(
        job_runs
            .message
            .contains("pending run(s) with no live worker"),
        "message explains the pending orphan class: {}",
        job_runs.message
    );
}

/// A freshly queued run inside the claim grace window is healthy, not an orphan.
#[test]
fn fresh_pending_run_is_not_reported_as_orphan() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let workspace_id = runtime.workspace_id().expect("workspace id");
    let now = Utc::now();
    let run = JobRun {
        run_id: "run-pending-fresh".to_string(),
        job_id: "task_gate_pipeline".to_string(),
        attempt: 1,
        state: JobRunState::Pending,
        scheduled_at: now,
        started_at: None,
        finished_at: None,
        duration_ms: None,
        created_at: now,
        pid: None,
        pid_start_time: None,
        input: None,
        retry_source_run_id: None,
        knowledge_metrics: None,
        resolved_crew: None,
        crew_model: None,
        steps: Vec::new(),
    };
    runtime
        .sqlite_store()
        .expect("store")
        .upsert_job_run_for_workspace(&workspace_id, &run, None)
        .expect("seed pending run");

    let results = runtime.doctor_workspace().expect("doctor");
    let job_runs = status_of(&results, "job-runs");
    assert_eq!(job_runs.status, WorkspaceDoctorStatus::Ok, "{job_runs:?}");
}

#[cfg(unix)]
#[test]
fn process_liveness_probe_distinguishes_dead_from_live() {
    assert!(process_is_alive(std::process::id()));
    assert!(!process_is_alive(reaped_child_pid()));
}

#[test]
fn disk_space_check_reports_volume_numbers() {
    let temp = tempfile::tempdir().expect("tempdir");
    let row = disk_space_check(temp.path());
    assert_eq!(row.check_name, "disk-space");
    assert!(
        row.message.contains("free of"),
        "message carries free/total detail: {}",
        row.message
    );
    assert_ne!(
        row.status,
        WorkspaceDoctorStatus::Skipped,
        "disk space is always determinable for an existing path"
    );
}

#[test]
fn collect_lock_files_scans_the_store_lock_locations() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);
    let paths = runtime.paths().clone();

    let expected = [paths.state_dir.join(".id_alloc.lock")];
    for path in &expected {
        fs::create_dir_all(path.parent().expect("parent")).expect("create dir");
        fs::write(path, b"{}").expect("write lock file");
    }
    // Non-lock files are ignored.
    fs::write(paths.state_dir.join("notes.txt"), b"x").expect("write non-lock");

    let found = collect_lock_files(&paths);
    for path in &expected {
        assert!(
            found.contains(path),
            "missing {} in {found:?}",
            path.display()
        );
    }
    assert!(
        found
            .iter()
            .all(|path| path.file_name().is_some_and(|n| n != "notes.txt")),
        "non-lock files must be ignored: {found:?}"
    );
}

#[test]
fn retired_graph_cleanup_removes_only_the_two_resolved_locations() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = split_root_runtime(&temp);
    let local_graph = runtime.local_root().join("graph");
    let shared_graph = runtime.shared_root().join("knowledge/graph");
    let unrelated = runtime.shared_root().join("knowledge/keep.txt");
    fs::create_dir_all(&local_graph).expect("create local graph");
    fs::create_dir_all(&shared_graph).expect("create shared graph");
    fs::write(local_graph.join("local.db"), b"retired").expect("write local graph");
    fs::write(shared_graph.join("shared.db"), b"retired").expect("write shared graph");
    fs::write(&unrelated, b"keep").expect("write unrelated state");

    assert_eq!(
        runtime
            .remove_retired_graph_state()
            .expect("remove retired graph state"),
        2
    );
    assert!(!local_graph.exists());
    assert!(!shared_graph.exists());
    assert!(unrelated.exists(), "cleanup must preserve sibling state");
    assert_eq!(
        runtime
            .remove_retired_graph_state()
            .expect("repeat cleanup"),
        0,
        "cleanup is idempotent when both locations are absent"
    );
}

#[test]
fn ordinary_doctor_leaves_retired_graph_locations_untouched() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = split_root_runtime(&temp);
    let local_marker = runtime.local_root().join("graph/local.db");
    let shared_marker = runtime.shared_root().join("knowledge/graph/shared.db");
    for marker in [&local_marker, &shared_marker] {
        fs::create_dir_all(marker.parent().expect("graph parent")).expect("create graph parent");
        fs::write(marker, b"retired").expect("write graph marker");
    }

    let results = runtime.doctor_workspace().expect("doctor");

    assert!(local_marker.exists());
    assert!(shared_marker.exists());
    assert!(results.iter().all(|row| row.check_name != "graph-index"));
}

#[cfg(unix)]
#[test]
fn retired_graph_cleanup_unlinks_boundaries_without_following_them() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = split_root_runtime(&temp);
    let outside = temp.path().join("outside");
    fs::create_dir_all(&outside).expect("create outside");
    let outside_marker = outside.join("keep.db");
    fs::write(&outside_marker, b"keep").expect("write outside marker");
    let local_graph = runtime.local_root().join("graph");
    let shared_graph = runtime.shared_root().join("knowledge/graph");
    fs::create_dir_all(shared_graph.parent().expect("knowledge parent"))
        .expect("create knowledge parent");
    std::os::unix::fs::symlink(&outside, &local_graph).expect("link local graph");
    std::os::unix::fs::symlink(&outside, &shared_graph).expect("link shared graph");

    assert_eq!(
        runtime
            .remove_retired_graph_state()
            .expect("remove graph links"),
        2
    );
    assert!(
        outside_marker.exists(),
        "cleanup must not follow graph symlinks"
    );
    assert!(fs::symlink_metadata(local_graph).is_err());
    assert!(fs::symlink_metadata(shared_graph).is_err());
}

#[test]
fn workspace_retired_backend_warns_artifacts_activities_with_repair_command() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);
    let activities = temp
        .path()
        .join("repo")
        .join(".orbit")
        .join("resources")
        .join("activities");
    fs::create_dir_all(&activities).expect("create workspace activities");
    let path = activities.join("epic_orchestrator.yaml");
    fs::write(
        &path,
        "schemaVersion: 2\nkind: Activity\nmetadata:\n  name: epic_orchestrator\nspec:\n  type: agent_loop\n  description: fixture\n  instruction: do the work\n  backend: http\n",
    )
    .expect("write retired backend activity");

    let results = runtime.doctor_workspace().expect("doctor");
    let row = status_of(&results, "artifacts-activities");
    assert_eq!(row.status, WorkspaceDoctorStatus::Warning, "{row:?}");
    assert!(
        row.message.contains("epic_orchestrator.yaml"),
        "{}",
        row.message
    );
    assert!(
        row.message.contains("spec.backend: http"),
        "{}",
        row.message
    );
    assert!(
        row.message.contains("schemaVersion 2 parse failed"),
        "{}",
        row.message
    );
    assert_eq!(
        row.remediation.as_deref(),
        Some("Run `orbit doctor --fix-retired-activity-backends`.")
    );

    let repaired = runtime
        .repair_retired_activity_backends()
        .expect("repair retired backends");
    assert_eq!(repaired.repaired, vec![path.clone()]);
    assert!(repaired.skipped.is_empty(), "{repaired:?}");
    assert!(
        !fs::read_to_string(&path)
            .expect("read repaired activity")
            .contains("backend:"),
        "repair must remove only the backend key"
    );

    let after = runtime.doctor_workspace().expect("doctor after repair");
    assert_eq!(
        status_of(&after, "artifacts-activities").status,
        WorkspaceDoctorStatus::Ok,
        "{after:?}"
    );
}

#[test]
fn stale_shipped_activity_default_names_the_refresh_remediation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let workspace_root = temp.path().join("repo/.orbit");
    let runtime = OrbitRuntime::initialize_from_resolved_roots(
        OrbitRuntimeRoots {
            global_root: global_root.clone(),
            shared_root: workspace_root.clone(),
            local_root: workspace_root,
        },
        None,
    )
    .expect("initialize runtime with defaults");
    let activities_dir = global_root.join("resources/activities");
    let path = activities_dir.join("agent_implement.yaml");
    let current = fs::read_to_string(&path).expect("read current activity");
    let stale = current.replacen("  tools:\n", "  tools:\n    - fs.read\n", 1);
    assert_ne!(stale, current, "fixture must contain the retired tool");
    fs::write(&path, &stale).expect("write stale activity");

    let manifest_path = activities_dir.join(".orbit-managed-assets.json");
    let mut manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&manifest_path).expect("read managed manifest"))
            .expect("parse managed manifest");
    manifest["assets"]["agent_implement"] =
        serde_json::Value::String(format!("{:x}", Sha256::digest(stale.as_bytes())));
    fs::write(
        &manifest_path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&manifest).expect("serialize managed manifest")
        ),
    )
    .expect("write stale managed manifest");

    let results = runtime.doctor_workspace().expect("doctor");
    let row = status_of(&results, "artifacts-activities");
    assert_eq!(row.status, WorkspaceDoctorStatus::Error, "{row:?}");
    assert!(row.message.contains("stale"), "{}", row.message);
    assert!(row.message.contains("older release"), "{}", row.message);
    assert_eq!(row.remediation.as_deref(), Some("Run `orbit init`."));
}

#[test]
fn missing_shipped_activity_default_is_an_error_not_healthy() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let workspace_root = temp.path().join("repo/.orbit");
    let runtime = OrbitRuntime::initialize_from_resolved_roots(
        OrbitRuntimeRoots {
            global_root: global_root.clone(),
            shared_root: workspace_root.clone(),
            local_root: workspace_root,
        },
        None,
    )
    .expect("initialize runtime with defaults");
    let path = global_root.join("resources/activities/git_merge.yaml");
    std::fs::remove_file(&path).expect("delete shipped default");

    let results = runtime.doctor_workspace().expect("doctor");
    let row = status_of(&results, "artifacts-activities");
    assert_eq!(row.status, WorkspaceDoctorStatus::Error, "{row:?}");
    assert!(row.message.contains("missing"), "{}", row.message);
    assert!(row.message.contains("git_merge"), "{}", row.message);
    assert_eq!(row.remediation.as_deref(), Some("Run `orbit init`."));
    assert!(
        results
            .iter()
            .any(|row| row.status == WorkspaceDoctorStatus::Error),
        "a missing shipped default must not leave the workspace looking healthy: {results:?}"
    );
}

/// [ORB-12109] A task-store partition whose workspace is still registered on
/// this host is healthy, not an orphan.
#[test]
fn registered_task_store_partition_is_not_an_orphan() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);
    let global_root = temp.path().join("global");
    write_registered_workspace(&global_root, "ws_registered", "registered");
    write_task_bundle(&global_root, "ws_registered", "ORB-1");

    let results = runtime.doctor_workspace().expect("doctor");
    let row = status_of(&results, "orphan-task-stores");
    assert_eq!(row.status, WorkspaceDoctorStatus::Ok, "{row:?}");
    assert!(
        row.message.contains("1 task-store partition"),
        "{}",
        row.message
    );
}

/// [ORB-12109] A task-store partition whose workspace id no longer resolves
/// in the registry — left behind by `workspace teardown` on an older binary,
/// or by deleting a checkout without running teardown — is named with its
/// path and an exact repair command. Emptied of bundles, it carries nothing to
/// recover, so the repair is the right next step [ORB-12131].
#[test]
fn orphan_task_store_partition_is_reported_with_path_and_remediation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);
    let global_root = temp.path().join("global");
    write_registered_workspace(&global_root, "ws_registered", "registered");
    write_task_bundle(&global_root, "ws_registered", "ORB-1");
    write_empty_partition(&global_root, "ws_orphan");

    let results = runtime.doctor_workspace().expect("doctor");
    let row = status_of(&results, "orphan-task-stores");
    assert_eq!(row.status, WorkspaceDoctorStatus::Warning, "{row:?}");
    assert!(row.message.contains("ws_orphan"), "{}", row.message);
    assert!(
        row.message.contains(
            &task_workspaces_dir(&global_root)
                .join("ws_orphan")
                .to_string_lossy()
                .into_owned()
        ),
        "message names the orphaned partition path: {}",
        row.message
    );
    assert!(row.message.contains("0 task bundle(s)"), "{}", row.message);
    assert!(
        !row.message.contains("ws_registered"),
        "registered partition must not be reported: {}",
        row.message
    );
    assert_eq!(
        row.remediation.as_deref(),
        Some("Run `orbit doctor --fix-orphan-task-stores --confirm`.")
    );
}

/// [ORB-12119] A partition bound in the task registry under a derived
/// `<slug>-<hash>` id is live task state, even though that id is absent from
/// the workspace catalog, which knows the same checkout as `ws_*`. Comparing
/// partition names against catalog ids alone flagged every such partition —
/// the host's real task stores — as orphaned.
#[test]
fn task_registry_bound_partition_is_not_an_orphan() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);
    let global_root = temp.path().join("global");
    let drifted_root = temp.path().join("drifted");
    fs::create_dir_all(drifted_root.join(".orbit")).expect("create drifted checkout");

    write_registered_workspace(&global_root, "ws_drifted", "drifted");
    bind_task_partition(&global_root, "drifted-a1b2c3", "drifted", &drifted_root);
    write_task_bundle(&global_root, "drifted-a1b2c3", "ORB-1");

    let results = runtime.doctor_workspace().expect("doctor");
    let row = status_of(&results, "orphan-task-stores");
    assert_eq!(
        row.status,
        WorkspaceDoctorStatus::Ok,
        "a bound partition is live task state: {row:?}"
    );
}

/// A task-registry binding to a deleted checkout is stale rather than a live
/// claim. Doctor reports it, and the confirmed repair removes its partition
/// and the binding's task data.
#[test]
fn stale_task_registry_binding_is_reported_and_removed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);
    let global_root = temp.path().join("global");
    let deleted_root = temp.path().join("deleted");
    fs::create_dir_all(deleted_root.join(".orbit")).expect("create deleted checkout");

    bind_task_partition(&global_root, "deleted-a1b2c3", "deleted", &deleted_root);
    write_task_bundle(&global_root, "deleted-a1b2c3", "ORB-2");
    fs::remove_dir_all(&deleted_root).expect("delete checkout");

    let results = runtime.doctor_workspace().expect("doctor");
    let row = status_of(&results, "orphan-task-stores");
    assert_eq!(row.status, WorkspaceDoctorStatus::Warning, "{row:?}");
    assert!(row.message.contains("deleted-a1b2c3"), "{}", row.message);
    assert!(row.message.contains("1 task bundle(s)"), "{}", row.message);
    assert_eq!(
        row.remediation.as_deref(),
        Some("Run `orbit doctor --fix-orphan-task-stores --confirm`.")
    );

    let removed = runtime
        .remove_orphan_task_stores()
        .expect("remove stale orphan task store");
    assert_eq!(removed, 1);
    assert!(
        !task_workspaces_dir(&global_root)
            .join("deleted-a1b2c3")
            .exists()
    );
    assert!(!partition_is_bound(&global_root, "deleted-a1b2c3").expect("read binding"));
}

/// [ORB-12119] The fix deletes only partitions no registry claims: a
/// task-registry binding, a workspace-catalog entry, and the synthetic
/// `--root` data-dir partition each keep their bundles.
#[test]
fn fix_orphan_task_stores_keeps_every_claimed_partition() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);
    let global_root = temp.path().join("global");
    let drifted_root = temp.path().join("drifted");
    fs::create_dir_all(drifted_root.join(".orbit")).expect("create drifted checkout");

    write_registered_workspace(&global_root, "ws_registered", "registered");
    write_task_bundle(&global_root, "ws_registered", "ORB-1");
    bind_task_partition(&global_root, "drifted-a1b2c3", "drifted", &drifted_root);
    write_task_bundle(&global_root, "drifted-a1b2c3", "ORB-2");
    // Every `--root <data-dir>` write lands here, and no registry ever records it.
    write_task_bundle(&global_root, "ws_unbound-data-dir", "ORB-3");
    write_empty_partition(&global_root, "ws_orphan");

    let removed = runtime
        .remove_orphan_task_stores()
        .expect("remove orphan task stores");
    assert_eq!(removed, 1, "only the unclaimed partition is removed");

    let partitions = task_workspaces_dir(&global_root);
    assert!(!partitions.join("ws_orphan").exists());
    for claimed in ["ws_registered", "drifted-a1b2c3", "ws_unbound-data-dir"] {
        assert!(
            partitions.join(claimed).is_dir(),
            "claimed partition '{claimed}' must survive the fix"
        );
    }

    let results = runtime.doctor_workspace().expect("doctor after fix");
    assert_eq!(
        status_of(&results, "orphan-task-stores").status,
        WorkspaceDoctorStatus::Ok,
        "{results:?}"
    );
}

/// [ORB-12109] `--fix-orphan-task-stores` deletes only the unregistered
/// partition, leaving the registered one untouched, and doctor is healthy
/// again afterward — the teardown-then-doctor regression path.
#[test]
fn fix_orphan_task_stores_removes_only_the_unregistered_partition() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);
    let global_root = temp.path().join("global");
    write_registered_workspace(&global_root, "ws_registered", "registered");
    write_task_bundle(&global_root, "ws_registered", "ORB-1");
    write_empty_partition(&global_root, "ws_orphan");

    let removed = runtime
        .remove_orphan_task_stores()
        .expect("remove orphan task stores");
    assert_eq!(removed, 1);
    assert!(!task_workspaces_dir(&global_root).join("ws_orphan").exists());
    assert!(
        task_workspaces_dir(&global_root)
            .join("ws_registered")
            .exists()
    );

    let results = runtime.doctor_workspace().expect("doctor after fix");
    assert_eq!(
        status_of(&results, "orphan-task-stores").status,
        WorkspaceDoctorStatus::Ok,
        "{results:?}"
    );
}

/// [ORB-12131] A partition that still holds task bundles is reported without
/// pointing the operator at the deletion repair: the same picture is what a
/// lost `tasks/index.sqlite` paints for every live checkout, and `orbit task
/// reindex` restores those bundles.
#[test]
fn populated_unclaimed_partition_warns_toward_reindex_not_deletion() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);
    let populated = task_workspaces_dir(&runtime.global_root())
        .join("elsewhere-d4e5f6")
        .join("ORB-77");
    fs::create_dir_all(&populated).expect("create task bundle in an unclaimed partition");

    let row = status_of(
        &runtime.doctor_workspace().expect("doctor"),
        "orphan-task-stores",
    )
    .clone();
    assert_eq!(row.status, WorkspaceDoctorStatus::Warning, "{row:?}");
    assert!(
        row.message.contains("elsewhere-d4e5f6") && row.message.contains("1 task bundle(s)"),
        "message names the partition and its bundles: {}",
        row.message
    );
    let remediation = row.remediation.expect("actionable row has remediation");
    assert!(
        remediation.contains("orbit task reindex"),
        "remediation points at recovery: {remediation}"
    );
    assert!(
        !remediation.contains("--fix-orphan-task-stores --confirm"),
        "remediation must not advertise the deletion repair: {remediation}"
    );

    assert_eq!(
        runtime.remove_orphan_task_stores().expect("run the repair"),
        0
    );
    assert!(
        populated.is_dir(),
        "the repair must not delete task bundles"
    );
}
