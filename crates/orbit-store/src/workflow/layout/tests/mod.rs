//! Sibling tests for the workspace-layout migration registry (ORB-10012).

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use orbit_common::OrbitError;
#[cfg(unix)]
use orbit_common::fs::io::create_dir_symlink;
use orbit_types::task::TaskStatus;

use crate::contracts::{
    BreakingMigration, COMPATIBILITY_RECORD_FORMAT, CompatibilityRecord, MigrationCompatibility,
    StateComponent,
};

use super::{
    LAYOUT_MIGRATIONS, LayoutMigration, SUPPORTED_LAYOUT_VERSION, current_layout_version,
    layout_forward_compatible_open, pending_layout_migrations, pending_with, upgrade_lock_path,
    upgrade_with, upgrade_workspace_layout,
};
use crate::driver::file::task_bundle::read_bundle_at;
use crate::fs::lock::read_lock_holder;

fn temp_orbit_dir() -> tempfile::TempDir {
    tempfile::tempdir().expect("create temp .orbit dir")
}

fn marker_contents(orbit_dir: &Path) -> String {
    fs::read_to_string(orbit_dir.join("state").join("layout.version")).expect("read marker")
}

#[cfg(unix)]
const CRASH_CHILD_TEST: &str = "workflow::layout::tests::crash_during_layout_upgrade_child";

#[cfg(unix)]
fn blocking_apply(_orbit_dir: &Path) -> Result<(), OrbitError> {
    let (Ok(ready_path), Ok(_lock_path)) = (
        std::env::var("ORBIT_LAYOUT_CRASH_READY"),
        std::env::var("ORBIT_LAYOUT_CRASH_LOCK"),
    ) else {
        return Ok(());
    };
    fs::write(ready_path, b"migration started").expect("write migration readiness");
    std::thread::sleep(std::time::Duration::from_secs(60));
    Ok(())
}

#[cfg(unix)]
const INTERRUPTED_REGISTRY: &[LayoutMigration] = &[LayoutMigration {
    version: 1,
    name: "blocking migration",
    compat: MigrationCompatibility::Additive,
    description: "wait for the test process to be interrupted",
    apply: blocking_apply,
}];

#[cfg(unix)]
#[test]
#[ignore = "helper process for interrupted_layout_upgrade_leaves_stale_holder_metadata"]
fn crash_during_layout_upgrade_child() {
    let (Ok(orbit_dir), Ok(lock_path), Ok(ready_path)) = (
        std::env::var("ORBIT_LAYOUT_CRASH_ORBIT_DIR"),
        std::env::var("ORBIT_LAYOUT_CRASH_LOCK"),
        std::env::var("ORBIT_LAYOUT_CRASH_READY"),
    ) else {
        return;
    };
    let _ = lock_path;
    let _ = ready_path;
    upgrade_with(Path::new(&orbit_dir), INTERRUPTED_REGISTRY).expect("child upgrade");
}

#[cfg(unix)]
#[test]
fn interrupted_layout_upgrade_leaves_stale_holder_metadata() {
    let temp = temp_orbit_dir();
    let lock_path = upgrade_lock_path(temp.path());
    let ready_path = temp.path().join("migration-ready");
    let exe = std::env::current_exe().expect("current test exe");
    let mut child = std::process::Command::new(exe)
        .args(["--exact", CRASH_CHILD_TEST, "--ignored"])
        .env("ORBIT_LAYOUT_CRASH_ORBIT_DIR", temp.path())
        .env("ORBIT_LAYOUT_CRASH_LOCK", &lock_path)
        .env("ORBIT_LAYOUT_CRASH_READY", &ready_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn layout-upgrade child");

    let start = std::time::Instant::now();
    while !ready_path.exists() {
        if start.elapsed() > std::time::Duration::from_secs(20) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("layout-upgrade child never entered migration");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    let child_pid = child.id();
    child.kill().expect("SIGKILL layout-upgrade child");
    child.wait().expect("reap layout-upgrade child");

    let holder = read_lock_holder(&lock_path).expect("crashed holder metadata remains");
    assert_eq!(holder.pid, child_pid);
    assert_eq!(holder.label, "layout upgrade");
    assert_eq!(current_layout_version(temp.path()).expect("version"), 0);

    // The interrupted migration can be resumed, and its clean release clears
    // the metadata that the crash intentionally left behind.
    upgrade_with(temp.path(), INTERRUPTED_REGISTRY).expect("resume upgrade");
    assert_eq!(current_layout_version(temp.path()).expect("version"), 1);
    assert!(read_lock_holder(&lock_path).is_none());
}

#[cfg(unix)]
#[test]
fn layout_marker_parent_is_private_under_permissive_umask() {
    use std::os::unix::fs::PermissionsExt;

    const CHILD_MARKER: &str = "ORBIT_TEST_PRIVATE_LAYOUT_MARKER";
    if std::env::var_os(CHILD_MARKER).is_none() {
        let status = std::process::Command::new("sh")
            .args(["-c", "umask 000; exec \"$@\"", "sh"])
            .arg(std::env::current_exe().expect("current test executable"))
            .arg("layout_marker_parent_is_private_under_permissive_umask")
            .env(CHILD_MARKER, "1")
            .status()
            .expect("run test under permissive umask");
        assert!(status.success(), "permissive-umask child failed");
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let orbit_dir = temp.path().join("workspace/.orbit");
    super::write_marker(&orbit_dir, 1).expect("write layout marker");

    let state = orbit_dir.join("state");
    for directory in [&orbit_dir, &state] {
        let mode = fs::metadata(directory)
            .expect("layout directory metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "{} has mode {mode:04o}", directory.display());
    }
}

// ── shipping registry ──

#[test]
fn shipping_registry_is_strictly_increasing_and_matches_supported_version() {
    let mut previous = 0u32;
    for migration in LAYOUT_MIGRATIONS {
        assert!(
            migration.version > previous,
            "registry must be strictly increasing: v{} after v{previous}",
            migration.version
        );
        previous = migration.version;
    }
    assert_eq!(
        previous, SUPPORTED_LAYOUT_VERSION,
        "SUPPORTED_LAYOUT_VERSION must equal the newest registry entry"
    );
}

#[test]
fn fresh_workspace_adopts_the_baseline_and_stamps_the_marker() {
    let temp = temp_orbit_dir();

    assert_eq!(current_layout_version(temp.path()).expect("version"), 0);
    let pending = pending_layout_migrations(temp.path()).expect("pending");
    assert_eq!(pending.len(), LAYOUT_MIGRATIONS.len());
    assert_eq!(pending[0].name, "baseline");

    let report = upgrade_workspace_layout(temp.path()).expect("upgrade");
    assert_eq!(report.from_version, 0);
    assert_eq!(report.to_version, SUPPORTED_LAYOUT_VERSION);
    assert_eq!(report.applied.len(), LAYOUT_MIGRATIONS.len());
    assert_eq!(
        marker_contents(temp.path()).trim(),
        SUPPORTED_LAYOUT_VERSION.to_string()
    );
    assert_eq!(
        current_layout_version(temp.path()).expect("version"),
        SUPPORTED_LAYOUT_VERSION
    );
    assert!(
        read_lock_holder(&upgrade_lock_path(temp.path())).is_none(),
        "a completed layout upgrade must not leave stale holder metadata"
    );
}

#[test]
fn current_workspace_is_a_no_op_with_no_pending_migrations() {
    let temp = temp_orbit_dir();
    upgrade_workspace_layout(temp.path()).expect("first upgrade");

    let report = upgrade_workspace_layout(temp.path()).expect("second upgrade");
    assert_eq!(report.from_version, SUPPORTED_LAYOUT_VERSION);
    assert_eq!(report.to_version, SUPPORTED_LAYOUT_VERSION);
    assert!(report.applied.is_empty());
    assert!(
        pending_layout_migrations(temp.path())
            .expect("pending")
            .is_empty()
    );
}

#[test]
fn newer_marker_refuses_with_downgrade_guard() {
    let temp = temp_orbit_dir();
    fs::create_dir_all(temp.path().join("state")).expect("mkdir state");
    fs::write(temp.path().join("state").join("layout.version"), "99\n").expect("write marker");

    let error = upgrade_workspace_layout(temp.path()).expect_err("must refuse newer layout");
    assert!(matches!(error, OrbitError::Migration(_)), "{error:?}");
    let message = error.to_string();
    assert!(message.contains("layout version 99"), "{message}");
    assert!(message.contains("upgrade orbit"), "{message}");

    // The marker is untouched and the pre-flight remains inspectable.
    assert_eq!(current_layout_version(temp.path()).expect("version"), 99);
    assert!(
        pending_layout_migrations(temp.path())
            .expect("pending")
            .is_empty()
    );
}

// ── forward compatibility (ORB-12434) ──

/// State a workspace as a newer binary would have left it: marker plus the
/// companion compatibility record naming the registry's breaking migrations.
fn stamp_newer_workspace(orbit_dir: &Path, version: u32, breaking: &[(u32, &str)]) {
    fs::create_dir_all(orbit_dir.join("state")).expect("mkdir state");
    fs::write(
        orbit_dir.join("state").join("layout.version"),
        format!("{version}\n"),
    )
    .expect("write marker");
    let record = CompatibilityRecord {
        format: COMPATIBILITY_RECORD_FORMAT,
        version,
        breaking: breaking
            .iter()
            .map(|(version, name)| BreakingMigration {
                version: *version,
                name: (*name).to_string(),
            })
            .collect(),
    };
    fs::write(
        orbit_dir.join("state").join("layout.compat"),
        format!("{}\n", record.encode().expect("encode record")),
    )
    .expect("write compatibility record");
}

fn state_bytes(orbit_dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    for entry in fs::read_dir(orbit_dir.join("state")).expect("read state dir") {
        let entry = entry.expect("state dir entry");
        if entry.path().is_file() {
            files.insert(
                entry.file_name().to_string_lossy().to_string(),
                fs::read(entry.path()).expect("read state file"),
            );
        }
    }
    files
}

#[test]
fn applying_a_migration_records_the_compatibility_contract() {
    let temp = temp_orbit_dir();
    upgrade_workspace_layout(temp.path()).expect("upgrade");

    let raw = fs::read_to_string(temp.path().join("state").join("layout.compat"))
        .expect("read compatibility record");
    let record = CompatibilityRecord::decode(raw.trim()).expect("decode compatibility record");
    assert_eq!(record.format, COMPATIBILITY_RECORD_FORMAT);
    assert_eq!(record.version, SUPPORTED_LAYOUT_VERSION);
    let expected: Vec<BreakingMigration> = LAYOUT_MIGRATIONS
        .iter()
        .filter(|migration| migration.compat.is_breaking())
        .map(|migration| BreakingMigration {
            version: migration.version,
            name: migration.name.to_string(),
        })
        .collect();
    assert_eq!(record.breaking, expected);
}

#[test]
fn layout_newer_by_additive_migrations_opens_read_only_without_rewriting_state() {
    let temp = temp_orbit_dir();
    let newer = SUPPORTED_LAYOUT_VERSION + 2;
    // Every breaking migration in the newer registry is one this binary
    // already has, so only additive work separates the two.
    stamp_newer_workspace(
        temp.path(),
        newer,
        &[(SUPPORTED_LAYOUT_VERSION, "remove-task-checkout-projections")],
    );
    let before = state_bytes(temp.path());

    let report = upgrade_workspace_layout(temp.path()).expect("newer-but-additive layout opens");
    assert_eq!(report.from_version, newer);
    assert_eq!(report.to_version, newer);
    assert!(report.applied.is_empty(), "an older binary applies nothing");
    let forward = report
        .forward_compatible
        .expect("the report must record the read-only open");
    assert_eq!(forward.component, StateComponent::WorkspaceLayout);
    assert_eq!(forward.state_version, newer);
    assert_eq!(forward.supported_version, SUPPORTED_LAYOUT_VERSION);
    assert_eq!(forward.min_reader_version, SUPPORTED_LAYOUT_VERSION);

    assert_eq!(
        state_bytes(temp.path()),
        before,
        "a read-only open must not rewrite any layout state"
    );
    assert_eq!(
        layout_forward_compatible_open(temp.path()).expect("inspection"),
        Some(forward)
    );
}

#[test]
fn layout_newer_by_a_breaking_migration_refuses_and_names_it() {
    let temp = temp_orbit_dir();
    let newer = SUPPORTED_LAYOUT_VERSION + 2;
    stamp_newer_workspace(
        temp.path(),
        newer,
        &[
            (SUPPORTED_LAYOUT_VERSION + 1, "relocate-run-state"),
            (newer, "drop-legacy-events"),
        ],
    );
    let before = state_bytes(temp.path());

    let error = upgrade_workspace_layout(temp.path()).expect_err("breaking-newer must refuse");
    let message = error.to_string();
    assert!(
        message.contains(&format!("layout version {newer}")),
        "{message}"
    );
    // The first breaking migration this binary lacks, not the newest.
    assert!(
        message.contains(&format!(
            "v{} (relocate-run-state)",
            SUPPORTED_LAYOUT_VERSION + 1
        )),
        "{message}"
    );
    assert!(!message.contains("drop-legacy-events"), "{message}");
    assert!(message.contains("upgrade orbit"), "{message}");

    assert_eq!(state_bytes(temp.path()), before);
    assert_eq!(
        layout_forward_compatible_open(temp.path()).expect("inspection"),
        None
    );
}

#[test]
fn newer_layout_with_a_stale_or_unreadable_record_still_refuses() {
    let temp = temp_orbit_dir();
    let newer = SUPPORTED_LAYOUT_VERSION + 1;

    // Stale: the record describes an older version, leaving the migrations
    // in between unclassified.
    stamp_newer_workspace(temp.path(), newer, &[]);
    fs::write(
        temp.path().join("state").join("layout.compat"),
        format!(
            "{}\n",
            CompatibilityRecord {
                format: COMPATIBILITY_RECORD_FORMAT,
                version: SUPPORTED_LAYOUT_VERSION,
                breaking: Vec::new(),
            }
            .encode()
            .expect("encode")
        ),
    )
    .expect("write stale record");
    let error = upgrade_workspace_layout(temp.path()).expect_err("stale record must refuse");
    assert!(
        error.to_string().contains("only describes version"),
        "{error}"
    );

    // Unreadable: present but not a record this binary can evaluate.
    fs::write(
        temp.path().join("state").join("layout.compat"),
        "not-a-record\n",
    )
    .expect("write corrupt record");
    let error = upgrade_workspace_layout(temp.path()).expect_err("corrupt record must refuse");
    assert!(error.to_string().contains("unreadable"), "{error}");
}

#[test]
fn corrupt_marker_is_a_typed_error_naming_the_file() {
    let temp = temp_orbit_dir();
    fs::create_dir_all(temp.path().join("state")).expect("mkdir state");
    fs::write(temp.path().join("state").join("layout.version"), "banana").expect("write marker");

    let error = upgrade_workspace_layout(temp.path()).expect_err("must refuse corrupt marker");
    let message = error.to_string();
    assert!(message.contains("layout.version"), "{message}");
    assert!(message.contains("banana"), "{message}");
}

// ── shipping migrations ──

fn seed_task_bundle(orbit_dir: &Path, id: &str, status: &str) -> std::path::PathBuf {
    let bundle_dir = orbit_dir.join("tasks").join(id);
    fs::create_dir_all(bundle_dir.join("artifacts")).expect("create task bundle");
    fs::write(
        bundle_dir.join("task.yaml"),
        format!(
            "schema_version: 1\nid: {id}\ntitle: Legacy task\nstatus: {status}\ntype: bug\npriority: medium\ncreated_at: 2026-07-01T00:00:00Z\nupdated_at: 2026-07-01T00:00:00Z\n"
        ),
    )
    .expect("write task envelope");
    for document in [
        "description.md",
        "acceptance.md",
        "plan.md",
        "execution-summary.md",
    ] {
        fs::write(bundle_dir.join(document), "").expect("write task document");
    }
    fs::write(
        bundle_dir.join("events.jsonl"),
        format!(
            "{{\"schema_version\":1,\"event_id\":\"EV-0001\",\"at\":\"2026-07-01T00:00:00Z\",\"by\":\"codex\",\"type\":\"created\",\"to_status\":\"{status}\"}}\n\
             {{\"schema_version\":1,\"event_id\":\"EV-0002\",\"at\":\"2026-07-01T00:01:00Z\",\"by\":\"codex\",\"type\":\"updated\",\"from_status\":\"{status}\"}}\n"
        ),
    )
    .expect("write task events");
    fs::write(bundle_dir.join("comments.jsonl"), "").expect("write task comments");
    bundle_dir
}

fn workspace_bytes(root: &Path) -> BTreeMap<std::path::PathBuf, Vec<u8>> {
    fn visit(root: &Path, current: &Path, files: &mut BTreeMap<std::path::PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(current).expect("read workspace fixture") {
            let path = entry.expect("workspace fixture entry").path();
            if path.is_dir() {
                visit(root, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(root)
                        .expect("relative fixture path")
                        .to_path_buf(),
                    fs::read(&path).expect("read fixture file"),
                );
            }
        }
    }

    let mut files = BTreeMap::new();
    visit(root, root, &mut files);
    files
}

#[test]
fn legacy_friction_task_fails_before_layout_upgrade_and_opens_after() {
    let temp = temp_orbit_dir();
    let bundle_dir = seed_task_bundle(temp.path(), "ORB-00001", "friction");
    fs::create_dir_all(temp.path().join("state")).expect("create state");
    fs::write(temp.path().join("state/layout.version"), "1\n").expect("stamp v1");

    let before = read_bundle_at(&bundle_dir).expect_err("removed status must fail");
    assert!(before.to_string().contains("friction"), "{before}");

    let report = upgrade_workspace_layout(temp.path()).expect("apply v2 migration");
    assert_eq!(report.from_version, 1);
    assert_eq!(report.to_version, 3);
    assert_eq!(report.applied.len(), 2);
    assert_eq!(report.applied[0].name, "archive-friction-tasks");

    let after = read_bundle_at(&bundle_dir).expect("migrated task bundle opens");
    assert_eq!(after.envelope.status, TaskStatus::Archived);
    assert_eq!(
        after.events.first().and_then(|event| event.to_status),
        Some(TaskStatus::Archived)
    );
    assert_eq!(
        after.events.last().and_then(|event| event.from_status),
        Some(TaskStatus::Archived)
    );
}

#[test]
fn friction_migration_is_idempotent_and_safe_to_replay_before_marker_advance() {
    let temp = temp_orbit_dir();
    let bundle_dir = seed_task_bundle(temp.path(), "ORB-00002", "friction");
    fs::create_dir_all(temp.path().join("state")).expect("create state");
    fs::write(temp.path().join("state/layout.version"), "1\n").expect("stamp v1");

    // Simulate a crash after apply returned but before the marker advanced.
    (LAYOUT_MIGRATIONS[1].apply)(temp.path()).expect("first apply");
    let after_first_apply = workspace_bytes(temp.path());
    assert_eq!(current_layout_version(temp.path()).expect("version"), 1);

    let report = upgrade_workspace_layout(temp.path()).expect("replay after interruption");
    assert_eq!(report.applied.len(), 2);
    assert_eq!(report.applied[0].version, 2);
    assert_eq!(
        read_bundle_at(&bundle_dir).expect("bundle").envelope.status,
        TaskStatus::Archived
    );

    let after_marker_advance = workspace_bytes(temp.path());
    let rerun = upgrade_workspace_layout(temp.path()).expect("idempotent rerun");
    assert!(rerun.applied.is_empty());
    assert_eq!(workspace_bytes(temp.path()), after_marker_advance);

    // The replay changed only the marker; task bundle bytes were already final
    // after the first application.
    for (path, bytes) in after_first_apply {
        if path != Path::new("state/layout.version") {
            assert_eq!(after_marker_advance.get(&path), Some(&bytes));
        }
    }
}

#[test]
fn dry_run_metadata_lists_plain_friction_task_outcome() {
    let temp = temp_orbit_dir();
    fs::create_dir_all(temp.path().join("state")).expect("create state");
    fs::write(temp.path().join("state/layout.version"), "1\n").expect("stamp v1");

    let pending = pending_layout_migrations(temp.path()).expect("pending");
    assert_eq!(pending.len(), 2);
    assert_eq!(pending[0].version, 2);
    assert_eq!(pending[0].name, "archive-friction-tasks");
    assert!(pending[0].description.contains("status 'friction'"));
    assert!(pending[0].description.contains("'archived'"));
    assert!(pending[0].description.contains("preserving the task"));
}

#[cfg(unix)]
#[test]
fn legacy_task_projection_cleanup_is_guarded_and_idempotent() {
    let temp = temp_orbit_dir();
    let tasks_dir = temp.path().join("tasks");
    let canonical_root = temp.path().join("global/tasks/workspaces/ws_orbit");
    let live_target = canonical_root.join("ORB-00001");
    let dangling_target = canonical_root.join("ORB-00002");
    fs::create_dir_all(&live_target).expect("create canonical target");
    fs::write(live_target.join("task.yaml"), "canonical").expect("write canonical target");
    fs::create_dir_all(&tasks_dir).expect("create projection directory");
    create_dir_symlink(&live_target, &tasks_dir.join("ORB-00001")).expect("create live projection");
    create_dir_symlink(&dangling_target, &tasks_dir.join("ORB-00002"))
        .expect("create dangling projection");

    let unknown_target = temp.path().join("unrelated-target");
    fs::create_dir_all(&unknown_target).expect("create unknown target");
    let unknown_link = tasks_dir.join("ORB-00003");
    create_dir_symlink(&unknown_target, &unknown_link).expect("create unknown link");
    fs::write(tasks_dir.join("ORB-00004"), "ordinary file").expect("write ordinary file");
    fs::create_dir(tasks_dir.join("ORB-00005")).expect("create ordinary directory");
    create_dir_symlink(&live_target, &tasks_dir.join("not-a-task")).expect("create unrelated link");

    (LAYOUT_MIGRATIONS[2].apply)(temp.path()).expect("clean legacy projections");
    (LAYOUT_MIGRATIONS[2].apply)(temp.path()).expect("repeat cleanup");

    assert!(fs::symlink_metadata(tasks_dir.join("ORB-00001")).is_err());
    assert!(fs::symlink_metadata(tasks_dir.join("ORB-00002")).is_err());
    assert_eq!(
        fs::read_to_string(live_target.join("task.yaml")).expect("read canonical target"),
        "canonical"
    );
    assert!(!dangling_target.exists());
    assert!(unknown_link.is_symlink());
    assert!(tasks_dir.join("ORB-00004").is_file());
    assert!(tasks_dir.join("ORB-00005").is_dir());
    assert!(tasks_dir.join("not-a-task").is_symlink());
    assert!(unknown_target.is_dir());
    assert!(
        tasks_dir.is_dir(),
        "non-empty projection parent is retained"
    );
}

#[cfg(unix)]
#[test]
fn legacy_cleanup_does_not_follow_a_symlinked_tasks_parent() {
    let temp = temp_orbit_dir();
    let outside = tempfile::tempdir().expect("outside tempdir");
    let target = outside.path().join("ORB-00001");
    fs::create_dir(&target).expect("create outside entry");
    create_dir_symlink(outside.path(), &temp.path().join("tasks"))
        .expect("create symlinked parent");

    (LAYOUT_MIGRATIONS[2].apply)(temp.path()).expect("cleanup symlinked parent");

    assert!(temp.path().join("tasks").is_symlink());
    assert!(target.is_dir());
}

#[cfg(unix)]
#[test]
fn legacy_cleanup_removes_an_empty_projection_directory() {
    let temp = temp_orbit_dir();
    let tasks_dir = temp.path().join("tasks");
    let target = temp
        .path()
        .join("global/tasks/workspaces/ws_orbit/ORB-00001");
    fs::create_dir_all(&target).expect("create canonical target");
    fs::create_dir(&tasks_dir).expect("create projection directory");
    create_dir_symlink(&target, &tasks_dir.join("ORB-00001")).expect("create projection");

    (LAYOUT_MIGRATIONS[2].apply)(temp.path()).expect("cleanup projection");

    assert!(!tasks_dir.exists());
    assert!(target.is_dir());
}

#[test]
fn legacy_review_thread_sidecars_are_ignored_when_bundle_opens() {
    let temp = temp_orbit_dir();
    let bundle_dir = seed_task_bundle(temp.path(), "ORB-00003", "backlog");
    let review_threads = bundle_dir.join("review-threads");
    fs::create_dir_all(&review_threads).expect("create legacy review threads");
    fs::write(
        review_threads.join("RT-0001.yaml"),
        "schema_version: 1\nthread_id: RT-0001\nstatus: open\nmessages: []\ncreated_at: 2026-07-01T00:00:00Z\nupdated_at: 2026-07-01T00:00:00Z\n",
    )
    .expect("write legacy review metadata");
    fs::write(review_threads.join("RT-0001.md"), "Legacy review body.\n")
        .expect("write legacy review body");

    let bundle = read_bundle_at(&bundle_dir).expect("legacy bundle opens");
    assert_eq!(bundle.envelope.status, TaskStatus::Backlog);
    assert!(
        review_threads.is_dir(),
        "opening leaves ignored sidecars untouched"
    );
}

// ── test-only v2 registry: exercises a real layout change end to end ──

fn toy_v2_apply(orbit_dir: &Path) -> Result<(), OrbitError> {
    // Idempotent rename: move legacy `notes.txt` under `notes/` (staged
    // write-new-then-swap shape a real migration would use).
    let legacy = orbit_dir.join("notes.txt");
    let target_dir = orbit_dir.join("notes");
    fs::create_dir_all(&target_dir).map_err(|e| OrbitError::Io(e.to_string()))?;
    if legacy.exists() {
        fs::rename(&legacy, target_dir.join("notes.txt"))
            .map_err(|e| OrbitError::Io(e.to_string()))?;
    }
    Ok(())
}

fn failing_apply(_orbit_dir: &Path) -> Result<(), OrbitError> {
    Err(OrbitError::Execution("boom".to_string()))
}

const TOY_V2_REGISTRY: &[LayoutMigration] = &[
    LayoutMigration {
        version: 1,
        name: "baseline",
        compat: MigrationCompatibility::Additive,
        description: "adopt the versioned layout",
        apply: |_| Ok(()),
    },
    LayoutMigration {
        version: 2,
        name: "notes-into-subdir",
        compat: MigrationCompatibility::Additive,
        description: "move notes.txt under notes/",
        apply: toy_v2_apply,
    },
];

#[test]
fn toy_v2_migration_applies_in_order_and_advances_the_marker() {
    let temp = temp_orbit_dir();
    fs::write(temp.path().join("notes.txt"), "hello").expect("write legacy file");

    let pending = pending_with(temp.path(), TOY_V2_REGISTRY).expect("pending");
    assert_eq!(
        pending.iter().map(|m| m.version).collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(pending[1].description, "move notes.txt under notes/");

    let report = upgrade_with(temp.path(), TOY_V2_REGISTRY).expect("upgrade");
    assert_eq!(report.from_version, 0);
    assert_eq!(report.to_version, 2);
    assert_eq!(
        report
            .applied
            .iter()
            .map(|m| m.name.as_str())
            .collect::<Vec<_>>(),
        vec!["baseline", "notes-into-subdir"]
    );
    assert_eq!(marker_contents(temp.path()).trim(), "2");
    assert!(!temp.path().join("notes.txt").exists());
    assert_eq!(
        fs::read_to_string(temp.path().join("notes").join("notes.txt")).expect("moved file"),
        "hello"
    );

    // Idempotent rerun: nothing pending, nothing re-applied.
    let rerun = upgrade_with(temp.path(), TOY_V2_REGISTRY).expect("rerun");
    assert!(rerun.applied.is_empty());
}

#[test]
fn upgrade_applies_only_migrations_newer_than_the_marker() {
    let temp = temp_orbit_dir();
    // Already on v1: only the v2 entry should run.
    fs::create_dir_all(temp.path().join("state")).expect("mkdir state");
    fs::write(temp.path().join("state/layout.version"), "1\n").expect("stamp v1");
    fs::write(temp.path().join("notes.txt"), "hello").expect("write legacy file");

    let report = upgrade_with(temp.path(), TOY_V2_REGISTRY).expect("upgrade");
    assert_eq!(report.from_version, 1);
    assert_eq!(report.to_version, 2);
    assert_eq!(report.applied.len(), 1);
    assert_eq!(report.applied[0].name, "notes-into-subdir");
}

#[test]
fn failed_migration_keeps_the_marker_at_the_last_applied_version() {
    let temp = temp_orbit_dir();
    const FAILING_REGISTRY: &[LayoutMigration] = &[
        LayoutMigration {
            version: 1,
            name: "baseline",
            compat: MigrationCompatibility::Additive,
            description: "adopt",
            apply: |_| Ok(()),
        },
        LayoutMigration {
            version: 2,
            name: "explodes",
            compat: MigrationCompatibility::Additive,
            description: "always fails",
            apply: failing_apply,
        },
    ];

    let error = upgrade_with(temp.path(), FAILING_REGISTRY).expect_err("v2 must fail");
    let message = error.to_string();
    assert!(message.contains("v2"), "{message}");
    assert!(message.contains("explodes"), "{message}");
    // v1 landed and was recorded; the failed v2 did not advance the marker,
    // so a fixed binary resumes exactly at v2.
    assert_eq!(current_layout_version(temp.path()).expect("version"), 1);

    let report = upgrade_with(temp.path(), TOY_V2_REGISTRY).expect("resume with fixed registry");
    assert_eq!(report.from_version, 1);
    assert_eq!(report.applied.len(), 1);
    assert_eq!(report.applied[0].version, 2);
}

#[test]
fn non_increasing_registry_is_rejected() {
    let temp = temp_orbit_dir();
    const BROKEN_REGISTRY: &[LayoutMigration] = &[
        LayoutMigration {
            version: 2,
            name: "two",
            compat: MigrationCompatibility::Additive,
            description: "",
            apply: |_| Ok(()),
        },
        LayoutMigration {
            version: 2,
            name: "two-again",
            compat: MigrationCompatibility::Additive,
            description: "",
            apply: |_| Ok(()),
        },
    ];

    let error = upgrade_with(temp.path(), BROKEN_REGISTRY).expect_err("must reject registry");
    assert!(error.to_string().contains("strictly increasing"), "{error}");
    let error = pending_with(temp.path(), BROKEN_REGISTRY).expect_err("must reject registry");
    assert!(error.to_string().contains("strictly increasing"), "{error}");
}
