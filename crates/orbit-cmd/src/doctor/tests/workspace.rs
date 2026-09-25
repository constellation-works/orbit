use super::*;

pub(super) fn status_of<'a>(
    results: &'a [WorkspaceDoctorResult],
    name: &str,
) -> &'a WorkspaceDoctorResult {
    results
        .iter()
        .find(|row| row.check_name == name)
        .unwrap_or_else(|| panic!("check '{name}' missing from {results:?}"))
}

pub(super) fn workspace_runtime(temp: &tempfile::TempDir) -> OrbitRuntime {
    let global_root = temp.path().join("global");
    let workspace_root = temp.path().join("repo").join(".orbit");
    fs::create_dir_all(&global_root).expect("create global root");
    fs::create_dir_all(&workspace_root).expect("create workspace root");
    OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build runtime")
}

pub(super) fn write_skill(root: &Path, id: &str, purpose: &str) {
    let dir = root.join(id);
    fs::create_dir_all(&dir).expect("create skill dir");
    fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {id}\ndescription: test skill\n---\n\n# Purpose\n\n{purpose}\n"),
    )
    .expect("write skill");
}

pub(super) fn split_root_runtime(temp: &tempfile::TempDir) -> OrbitRuntime {
    let global_root = temp.path().join("global");
    let shared_root = temp.path().join("main").join(".orbit");
    let local_root = temp.path().join("worktree").join(".orbit");
    for root in [&global_root, &shared_root, &local_root] {
        fs::create_dir_all(root).expect("create runtime root");
    }
    OrbitRuntime::from_resolved_roots(&global_root, &shared_root, &local_root)
        .expect("build split-root runtime")
}

pub(super) fn write_registered_workspace(global_root: &Path, workspace_id: &str, name: &str) {
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

pub(super) fn write_registered_checkout(global_root: &Path, workspace_id: &str, repo_root: &Path) {
    let registry_path = workspace_registry::registry_path_for(global_root);
    let mut registry =
        workspace_registry::load_registry_from(&registry_path).expect("load registry");
    workspace_registry::register_checkout(
        &mut registry,
        WorkspaceCheckout::owner(
            workspace_id.to_string(),
            repo_root.to_path_buf(),
            repo_root.join(".orbit"),
        ),
    )
    .expect("register checkout");
    workspace_registry::save_registry_to(&registry, &registry_path).expect("save registry");
}

/// Bind a checkout in the *task* registry — the id space the partition
/// directories are named after, which a `<slug>-<hash>` id inhabits and the
/// workspace catalog's `ws_*` ids do not [ORB-12119].
pub(super) fn bind_task_partition(
    global_root: &Path,
    workspace_id: &str,
    slug: &str,
    repo_root: &Path,
) {
    bind_task_partition_at(
        global_root,
        workspace_id,
        slug,
        repo_root,
        &repo_root.join(".orbit"),
    );
}

pub(super) fn bind_task_partition_at(
    global_root: &Path,
    workspace_id: &str,
    slug: &str,
    repo_root: &Path,
    orbit_dir: &Path,
) {
    let tasks =
        TaskRegistryStore::open(&task_registry_path(global_root)).expect("open task registry");
    tasks
        .bind_workspace(BindWorkspaceParams {
            partition_id: Some(workspace_id.to_string()),
            slug: slug.to_string(),
            repo_root: repo_root.to_path_buf(),
            workspace_path: repo_root.to_path_buf(),
            orbit_dir: orbit_dir.to_path_buf(),
            repo_fingerprint: None,
        })
        .expect("bind task-registry workspace");
}

pub(super) fn register_task_workspace(global_root: &Path, workspace_id: &str, slug: &str) {
    let tasks =
        TaskRegistryStore::open(&task_registry_path(global_root)).expect("open task registry");
    tasks
        .register_workspace(RegisterWorkspaceParams {
            partition_id: workspace_id.to_string(),
            slug: slug.to_string(),
            repo_fingerprint: None,
        })
        .expect("register path-free task workspace");
}

pub(super) fn write_registered_shared_root_checkout(
    global_root: &Path,
    workspace_id: &str,
    repo_root: &Path,
) {
    let registry_path = workspace_registry::registry_path_for(global_root);
    let mut registry =
        workspace_registry::load_registry_from(&registry_path).expect("load registry");
    workspace_registry::register_checkout(
        &mut registry,
        WorkspaceCheckout::owner(
            workspace_id.to_string(),
            repo_root.to_path_buf(),
            global_root.to_path_buf(),
        ),
    )
    .expect("register shared-root checkout");
    workspace_registry::save_registry_to(&registry, &registry_path).expect("save registry");
}

pub(super) fn write_task_bundle(global_root: &Path, workspace_id: &str, task_id: &str) {
    let bundle = task_workspaces_dir(global_root)
        .join(workspace_id)
        .join(task_id);
    fs::create_dir_all(&bundle).expect("create task bundle dir");
    fs::write(bundle.join("task.yaml"), b"id: dummy\n").expect("write bundle file");
}

pub(super) fn write_unpublished_stub(
    global_root: &Path,
    workspace_id: &str,
    task_id: &str,
) -> std::path::PathBuf {
    let bundle = task_workspaces_dir(global_root)
        .join(workspace_id)
        .join(task_id);
    fs::create_dir_all(&bundle).expect("create stub dir");
    fs::write(bundle.join(".task.yaml.lock"), []).expect("write stub lock");
    bundle
}

pub(super) fn write_unresolved_bundle(
    global_root: &Path,
    workspace_id: &str,
    task_id: &str,
) -> std::path::PathBuf {
    let bundle = task_workspaces_dir(global_root)
        .join(workspace_id)
        .join(task_id);
    fs::create_dir_all(&bundle).expect("create unresolved bundle dir");
    fs::write(bundle.join("events.jsonl"), b"{}\n").expect("write retained events");
    bundle
}

/// A partition directory emptied of its bundles, as `workspace teardown` on an
/// older binary left it behind.
pub(super) fn write_empty_partition(global_root: &Path, workspace_id: &str) {
    fs::create_dir_all(task_workspaces_dir(global_root).join(workspace_id))
        .expect("create empty partition dir");
}

#[test]
pub(super) fn healthy_fresh_workspace_has_no_failures() {
    let home = tempfile::tempdir().expect("tempdir");
    let home_path = home.path().to_string_lossy().into_owned();
    let _env = orbit_common::test_env::scoped([("HOME", Some(home_path.as_str()))]);
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let results = runtime.doctor_workspace().expect("doctor");

    // Thirteen infrastructure checks plus one definition-artifact row per kind
    // (skills, jobs, activities, auto-tasks, routines).
    assert_eq!(results.len(), 18, "one row per check: {results:?}");
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
        status_of(&results, "search-index").status,
        WorkspaceDoctorStatus::Ok
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
    assert_eq!(
        status_of(&results, "empty-task-stubs").status,
        WorkspaceDoctorStatus::Ok
    );
    assert_eq!(
        status_of(&results, "unresolved-task-bundles").status,
        WorkspaceDoctorStatus::Ok
    );
}

#[test]
pub(super) fn every_warning_or_error_has_structured_remediation() {
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
pub(super) fn skill_residue_warns_and_names_the_directory() {
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
pub(super) fn healthy_single_skill_and_workspace_shadow_keep_the_loaded_count() {
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
pub(super) fn absent_owner_task_reservation_warning_names_context_reason_and_exact_repair() {
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
pub(super) fn invalid_config_fails_the_config_check() {
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
pub(super) fn ignored_optional_crew_effort_is_a_config_warning() {
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);

    fs::write(
        temp.path().join("repo").join(".orbit").join("config.toml"),
        "[workflow]\ndefault_crew = \"astra\"\n\n[crews.astra]\nmodel = \"gpt-6-astra\"\nprovider = \"codex\"\neffort = \"hard\"\n",
    )
    .expect("write config with invalid optional effort");

    let results = runtime.doctor_workspace().expect("doctor");
    let config_rows: Vec<_> = results
        .iter()
        .filter(|row| row.check_name == "config")
        .collect();
    assert_eq!(config_rows.len(), 1, "{results:?}");
    let config = config_rows[0];
    assert_eq!(config.status, WorkspaceDoctorStatus::Warning, "{config:?}");
    assert!(
        config.message.contains("ignoring [crews.astra].effort"),
        "finding names the ignored property: {}",
        config.message
    );
    assert!(
        config.message.contains("hard"),
        "finding names the offending value: {}",
        config.message
    );
    let remediation = config.remediation.as_deref().expect("corrective edit");
    assert!(
        remediation.contains("[crews.astra].effort"),
        "{remediation}"
    );
    assert!(
        remediation.contains("low, medium, high, xhigh, max")
            || remediation.contains("remove the key"),
        "{remediation}"
    );
}

#[test]
pub(super) fn unopenable_store_database_fails_the_database_check() {
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
pub(super) fn missing_store_database_is_reported_without_recreating_it() {
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
pub(super) fn reaped_child_pid() -> u32 {
    let mut child = std::process::Command::new("true")
        .spawn()
        .expect("spawn child");
    let pid = child.id();
    child.wait().expect("reap child");
    pid
}

#[cfg(unix)]
pub(super) fn write_holder_lock(path: &Path, pid: u32, label: &str) {
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
pub(super) fn dead_holder_lock_file_is_reported_stale() {
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
pub(super) fn interrupted_layout_upgrade_is_reported_stale() {
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
pub(super) fn stale_workspace_state_lock_files_are_removed() {
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
pub(super) fn cleanup_preserves_a_lock_held_by_a_live_process() {
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
pub(super) fn live_holder_lock_file_is_not_stale() {
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
pub(super) fn orphaned_running_run_is_reported() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let workspace_id = runtime.workspace_id().expect("workspace id");
    let now = Utc::now();
    let run = JobRun {
        executed_on: None,
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
pub(super) fn orphaned_pending_run_is_reported() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let workspace_id = runtime.workspace_id().expect("workspace id");
    let created_at = Utc::now() - chrono::Duration::days(4);
    let run = JobRun {
        executed_on: None,
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
pub(super) fn fresh_pending_run_is_not_reported_as_orphan() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let workspace_id = runtime.workspace_id().expect("workspace id");
    let now = Utc::now();
    let run = JobRun {
        executed_on: None,
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
pub(super) fn process_liveness_probe_distinguishes_dead_from_live() {
    assert!(process_is_alive(std::process::id()));
    assert!(!process_is_alive(reaped_child_pid()));
}

#[test]
pub(super) fn disk_space_check_reports_volume_numbers() {
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
pub(super) fn collect_lock_files_scans_the_store_lock_locations() {
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
