//! Identity allocation and coordinated admission through the store's public
//! composition.
//!
//! - Task ids come from the host registry's allocator: monotonic, never
//!   wrapped past the ceiling, and exhausted cleanly, including when an import
//!   lands the maximum id.
//! - Job-run ids are never minted twice, even after the run they named is
//!   archived or deleted and the store reopened; local pull admission holds
//!   one slot per admitted request under a shared ceiling, which is the
//!   drain's live worker limit and nothing else.
//! - The owner's commit boundary admits at most one claim per request and per
//!   task under concurrent pulls, and a handoff is authorized only by the
//!   policy its claim was admitted under and only over unchanged evidence.
//! - Claimed evidence stores canonical artifact paths, and racing review
//!   report revisions are each retained in the store-owned report history.
//!
//! Every test re-runs itself in a child of this binary with inherited Orbit
//! authority cleared, a disposable `HOME`, and a bounded wait.

#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(missing_docs)]

use std::collections::{BTreeMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use orbit_common::OrbitError;
use orbit_store::Store;
use orbit_store::compose::{
    CoordinatedWorkspaceBackends, workspace_coordinated_backends, workspace_job_run_store,
};
use orbit_store::contracts::{
    ActiveTaskReservation, AdmissionIdentity, AdmissionLookup, AdmissionReceipt, AdmissionRequest,
    AdmissionReviewContract, AdmissionRunContext, AdmissionShipContract, ClaimEvidence,
    ClaimInvocation, ClaimMutation, ClaimMutationResult, ClaimRun, ClaimWorkerUpdate,
    ExecutionClaim, ExecutionClaimPhase, ExecutionLocation, HandoffObservation,
    HandoffReviewObservation, HandoffReviewRefusal, JobRunStoreBackend, PullDestination,
    TaskArtifactUpdateParams, TaskCreateParams,
};
use orbit_store::maintenance::task_registry::{
    BindWorkspaceParams, TaskRegistryStore, WorkspaceCheckoutBinding, task_registry_path,
};
use orbit_store::workflow::task::{
    AttachmentPolicy, AttachmentPolicyKind, ExportSelection, ImportConflictPolicy,
    PublicationCallerRole, PublicationInspectRequest, PublicationPublishRequest,
    PublicationRestoreMode, PublicationRestoreRequest, ScannerFailureBehavior, export_tasks,
    import_tasks, publish_task_snapshot, restore_publication,
};
use orbit_types::task::{
    CONTEXT_FILES_WIDENED_EVENT, ContextFilesWidening, ContextWideningStep, ORB_TASK_ID_MAX,
    TASK_ACCEPTANCE_FILE_NAME, TASK_ARTIFACT_SCHEMA_VERSION, TASK_COMMENTS_FILE_NAME,
    TASK_DESCRIPTION_FILE_NAME, TASK_ENVELOPE_FILE_NAME, TASK_EVENTS_FILE_NAME,
    TASK_EXECUTION_SUMMARY_FILE_NAME, TASK_PLAN_FILE_NAME, TaskArtifact, TaskCommentRowV2,
    TaskComplexity, TaskEventRowV2, TaskPriority, TaskStatus, TaskType,
};
use orbit_types::workflow::handoff::{
    HandoffArtifactRef, HandoffCandidate, HandoffDelivery, HandoffReview, HandoffReviewDisposition,
    HandoffReviewEvidence, HandoffValidationLog, LandingStartRequest, LandingStartState,
    TaskHandoff,
};
use orbit_types::workflow::{
    CommitIdentity, JobRunState, PipelineState, REVIEW_CONTRACT_VERSION, REVIEW_GATE_ARTIFACT,
    REVIEW_REPORT_ARTIFACT, REVIEW_REPORT_HISTORY_ARTIFACT, ReviewBudget, ReviewCertificate,
    ReviewConsumption, ReviewReportHistory, ReviewTiming, ReviewValidation, ReviewVerdict,
    ReviewerIdentity, ValidationOutcome, ValidationRole, automation::SourceRevision,
};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

#[path = "allocation_admission/dependencies.rs"]
mod dependencies;
#[path = "allocation_admission/reservation_grants.rs"]
mod reservation_grants;

/// How long one isolated test may run before it is killed and fails.
const CHILD_DEADLINE: Duration = Duration::from_secs(120);

/// Run `test` alone in a child of this binary with inherited Orbit authority
/// cleared and a disposable `HOME`; `true` inside that child. The parent
/// waits up to [`CHILD_DEADLINE`] and reaps the child on any exit.
fn isolated(test: &str) -> bool {
    const MARKER: &str = "ORBIT_TEST_STORE_ALLOCATION_CHILD";
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

fn is_exhausted<T: std::fmt::Debug>(result: Result<T, OrbitError>) -> bool {
    matches!(result, Err(OrbitError::Store(ref message)) if message.contains("exhausted"))
}

// ---------------------------------------------------------------------------
// Task id allocation
// ---------------------------------------------------------------------------

const PARTITION_ID: &str = "orbit-test-123456";

fn bind(registry: &TaskRegistryStore, root: &Path, partition_id: &str) -> WorkspaceCheckoutBinding {
    let repo = root.join(partition_id);
    let orbit_dir = repo.join(".orbit");
    std::fs::create_dir_all(&orbit_dir).unwrap();
    registry
        .bind_workspace(BindWorkspaceParams {
            partition_id: Some(partition_id.to_string()),
            slug: "Orbit Test".to_string(),
            repo_root: repo.clone(),
            workspace_path: repo,
            orbit_dir,
            repo_fingerprint: None,
        })
        .expect("bind workspace")
}

fn bind_with_fingerprint(
    registry: &TaskRegistryStore,
    root: &Path,
    partition_id: &str,
    fingerprint: &str,
) -> WorkspaceCheckoutBinding {
    let repo = root.join(partition_id);
    let orbit_dir = repo.join(".orbit");
    std::fs::create_dir_all(&orbit_dir).unwrap();
    registry
        .bind_workspace(BindWorkspaceParams {
            partition_id: Some(partition_id.to_string()),
            slug: "Orbit Test".to_string(),
            repo_root: repo.clone(),
            workspace_path: repo,
            orbit_dir,
            repo_fingerprint: Some(fingerprint.to_string()),
        })
        .expect("bind workspace")
}

#[test]
fn task_ids_are_monotonic_across_workspaces_and_batches() {
    if !isolated("task_ids_are_monotonic_across_workspaces_and_batches") {
        return;
    }
    let root = TempDir::new().unwrap();
    let registry = TaskRegistryStore::open(&task_registry_path(root.path())).unwrap();
    let first = bind(&registry, root.path(), "first-aaaaaa");
    let second = bind(&registry, root.path(), "second-bbbbbb");

    let mut minted = vec![
        registry.allocate_task_id(&first.partition_id).unwrap(),
        registry.allocate_task_id(&second.partition_id).unwrap(),
    ];
    minted.extend(registry.allocate_task_ids(&first.partition_id, 3).unwrap());
    minted.push(registry.allocate_task_id(&second.partition_id).unwrap());
    assert_eq!(
        minted,
        [
            "ORB-00000",
            "ORB-00001",
            "ORB-00002",
            "ORB-00003",
            "ORB-00004",
            "ORB-00005"
        ],
        "one host-wide counter serves every workspace and batch, in order"
    );

    // Reopening the registry (a later process) continues from the counter.
    drop(registry);
    let registry = TaskRegistryStore::open(&task_registry_path(root.path())).unwrap();
    assert_eq!(
        registry.allocate_task_id(&first.partition_id).unwrap(),
        "ORB-00006"
    );
    // Seeding never lowers the counter.
    assert!(registry.seed_allocator_start(3).is_err());
    assert_eq!(
        registry.allocate_task_id(&first.partition_id).unwrap(),
        "ORB-00007"
    );
}

/// The last id is minted once; every later allocation is refused as
/// exhausted, never wrapped to a low id that may already be taken. A batch
/// that would cross the ceiling is refused whole and leaves the counter.
#[test]
fn task_id_allocation_exhausts_at_the_ceiling_without_wrapping() {
    if !isolated("task_id_allocation_exhausts_at_the_ceiling_without_wrapping") {
        return;
    }
    let root = TempDir::new().unwrap();
    let registry = TaskRegistryStore::open(&task_registry_path(root.path())).unwrap();
    let workspace = bind(&registry, root.path(), PARTITION_ID);
    registry.seed_allocator_start(ORB_TASK_ID_MAX - 1).unwrap();

    assert!(
        is_exhausted(registry.allocate_task_ids(&workspace.partition_id, 3)),
        "a reservation past the ceiling is refused whole"
    );
    assert_eq!(
        registry.allocator_next_number().unwrap(),
        ORB_TASK_ID_MAX - 1
    );
    assert_eq!(
        registry.allocate_task_id(&workspace.partition_id).unwrap(),
        format!("ORB-{}", ORB_TASK_ID_MAX - 1)
    );
    assert_eq!(
        registry.allocate_task_id(&workspace.partition_id).unwrap(),
        format!("ORB-{ORB_TASK_ID_MAX}")
    );
    for _ in 0..2 {
        assert!(
            is_exhausted(registry.allocate_task_id(&workspace.partition_id)),
            "the allocator must stay exhausted rather than wrap"
        );
    }
    assert!(is_exhausted(registry.allocator_next_number()));
    assert!(is_exhausted(registry.seed_allocator_start(ORB_TASK_ID_MAX)));

    drop(registry);
    let registry = TaskRegistryStore::open(&task_registry_path(root.path())).unwrap();
    assert!(
        is_exhausted(registry.allocate_task_id(&workspace.partition_id)),
        "exhaustion survives a reopen"
    );
}

/// Importing a task that holds the maximum id leaves the target's allocator
/// exhausted, not wrapped to `ORB-00000`.
#[test]
fn importing_the_maximum_task_id_exhausts_the_target_allocator() {
    if !isolated("importing_the_maximum_task_id_exhausts_the_target_allocator") {
        return;
    }
    let source_root = TempDir::new().unwrap();
    let target_root = TempDir::new().unwrap();
    let archive = source_root.path().join("tasks.tar.zst");
    std::fs::write(&archive, b"previous backup").unwrap();
    let max_id = format!("ORB-{ORB_TASK_ID_MAX}");

    let source = Coordinated::open(source_root.path());
    source
        .registry
        .seed_allocator_start(ORB_TASK_ID_MAX)
        .unwrap();
    assert_eq!(source.create_task("final id").id, max_id);
    let destination_dir = source_root.path().join("archive-directory");
    std::fs::create_dir(&destination_dir).unwrap();
    let entries_before = std::fs::read_dir(source_root.path()).unwrap().count();
    export_tasks(
        &source.registry,
        PARTITION_ID,
        ExportSelection::All,
        &destination_dir,
        Utc.with_ymd_and_hms(2026, 10, 3, 12, 0, 0).unwrap(),
    )
    .expect_err("publishing an archive over a directory must fail");
    assert!(destination_dir.is_dir());
    assert_eq!(std::fs::read(&archive).unwrap(), b"previous backup");
    assert_eq!(
        std::fs::read_dir(source_root.path()).unwrap().count(),
        entries_before,
        "failed publication must remove its staging file"
    );
    export_tasks(
        &source.registry,
        PARTITION_ID,
        ExportSelection::All,
        &archive,
        Utc.with_ymd_and_hms(2026, 10, 3, 12, 0, 0).unwrap(),
    )
    .expect("export");
    assert_eq!(
        std::fs::read_dir(source_root.path()).unwrap().count(),
        entries_before,
        "successful replacement must leave no staging file"
    );

    let target = TaskRegistryStore::open(&task_registry_path(target_root.path())).unwrap();
    let outcome =
        import_tasks(&target, &archive, None, ImportConflictPolicy::Fail).expect("import");
    assert_eq!(outcome.tasks[0].final_id, max_id);
    assert!(target.find_task_binding(&max_id).unwrap().is_some());
    assert!(is_exhausted(target.allocator_next_number()));
    assert!(
        is_exhausted(target.allocate_task_id(PARTITION_ID)),
        "the next id after an imported maximum must be refused, not wrapped"
    );
}

/// Restoring a publication containing both local and foreign-prefix tasks
/// advances the local allocator only past local task numbers, leaving it at
/// `max(previous, max_local + 1)`. A publication containing only foreign-prefix
/// tasks preserves the prior allocator and does not error.
#[test]
fn publication_restore_advances_allocator_only_for_local_task_numbers() {
    if !isolated("publication_restore_advances_allocator_only_for_local_task_numbers") {
        return;
    }
    const FINGERPRINT: &str = "ssh://source.test/orbit.git";

    // 1. Create a foreign registry with prefix "DANI" and task DANI-90000.
    let foreign_root = TempDir::new().unwrap();
    let foreign_registry =
        TaskRegistryStore::open(&task_registry_path(foreign_root.path())).unwrap();
    foreign_registry.set_task_prefix("DANI").unwrap();
    let foreign_binding = bind_with_fingerprint(
        &foreign_registry,
        foreign_root.path(),
        "ws_foreign",
        FINGERPRINT,
    );
    let foreign_store = Store::open(&foreign_root.path().join("state.sqlite")).unwrap();
    let foreign_backends = workspace_coordinated_backends(
        foreign_registry.clone(),
        foreign_binding.partition_id.clone(),
        foreign_store,
    )
    .unwrap();
    foreign_registry.seed_allocator_start(90000).unwrap();
    let foreign = Coordinated {
        registry: foreign_registry.clone(),
        backends: foreign_backends,
        orbit_dir: foreign_binding.orbit_dir,
    };
    let dani_task = foreign.create_task("foreign dani task");
    assert_eq!(dani_task.id, "DANI-90000");

    let dani_archive = foreign_root.path().join("dani.tar.zst");
    export_tasks(
        &foreign_registry,
        &foreign_binding.partition_id,
        ExportSelection::All,
        &dani_archive,
        Utc.with_ymd_and_hms(2026, 10, 3, 12, 0, 0).unwrap(),
    )
    .expect("export dani");

    // 2. Create source workspace (default ORB prefix) with ORB-00005, and import DANI-90000 into it.
    let source_root = TempDir::new().unwrap();
    let source = Coordinated::open_with_fingerprint(source_root.path(), "ws_source", FINGERPRINT);
    source.registry.seed_allocator_start(5).unwrap();
    let orb_task = source.create_task("orb task 5");
    assert_eq!(orb_task.id, "ORB-00005");

    let imported = import_tasks(
        &source.registry,
        &dani_archive,
        Some("ws_source"),
        ImportConflictPolicy::Fail,
    )
    .expect("import dani into source");
    assert_eq!(imported.tasks[0].final_id, "DANI-90000");

    // 3. Publish source workspace containing both ORB-00005 and DANI-90000.
    let pub_dir = TempDir::new().unwrap();
    let bare = pub_dir.path().join("publication.git");
    let cache = pub_dir.path().join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    let mut git_init = std::process::Command::new("git");
    git_init.args([
        "init",
        "--bare",
        "--quiet",
        "-b",
        "main",
        bare.to_str().unwrap(),
    ]);
    assert!(git_init.status().unwrap().success());

    publish_task_snapshot(
        &source.registry,
        PublicationPublishRequest {
            workspace_id: "ws_source".to_string(),
            task_workspace_id: "ws_source".to_string(),
            source_repository_fingerprint: FINGERPRINT.to_string(),
            publication_id: "pub_mixed".to_string(),
            authority_machine_id: "hm_owner".to_string(),
            local_machine_id: "hm_owner".to_string(),
            caller_role: PublicationCallerRole::Owner,
            publication_remote: bare.to_str().unwrap().to_string(),
            publication_branch: "main".to_string(),
            cache_dir: cache,
            published_at: Utc.with_ymd_and_hms(2026, 10, 4, 12, 0, 0).unwrap(),
            last_success: None,
        },
        &AttachmentPolicy {
            kind: AttachmentPolicyKind::Omit,
            max_file_bytes: 1024,
            max_total_bytes: 1024,
            deny_patterns: Vec::new(),
            scanner_failure_behavior: ScannerFailureBehavior::AllowUnchecked,
        },
        None,
    )
    .expect("publish mixed snapshot");

    // Case A: Target has previous allocator < 6 (previous = 2).
    // Restoring must leave allocator at max(2, 6) = 6.
    let target_root_a = TempDir::new().unwrap();
    let target_a = TaskRegistryStore::open(&task_registry_path(target_root_a.path())).unwrap();
    bind_with_fingerprint(&target_a, target_root_a.path(), "ws_target_a", FINGERPRINT);
    target_a.seed_allocator_start(2).unwrap();
    assert_eq!(target_a.allocator_next_number().unwrap(), 2);

    let restore_cache_a = target_root_a.path().join("restore_cache");
    std::fs::create_dir_all(&restore_cache_a).unwrap();
    let outcome_a = restore_publication(
        &target_a,
        PublicationRestoreRequest {
            task_workspace_id: "ws_target_a".to_string(),
            publication: PublicationInspectRequest {
                workspace_id: "ws_source".to_string(),
                source_repository_fingerprint: FINGERPRINT.to_string(),
                publication_id: "pub_mixed".to_string(),
                authority_machine_id: "hm_owner".to_string(),
                publication_remote: bare.to_str().unwrap().to_string(),
                publication_branch: "main".to_string(),
                cache_dir: restore_cache_a,
                commit: None,
            },
            mode: PublicationRestoreMode::EmptyDestination,
        },
    )
    .expect("restore publication");
    assert_eq!(outcome_a.restored_task_ids.len(), 2);
    assert_eq!(
        target_a.allocator_next_number().unwrap(),
        6,
        "ORB-14129: restoring ORB-00005 and DANI-90000 with previous=2 must set allocator to max(2, 6) = 6"
    );
    assert_eq!(
        target_a.allocate_task_id("ws_target_a").unwrap(),
        "ORB-00006",
        "ORB-14129: next minted task after restore must be ORB-00006"
    );

    // Case B: Target has previous allocator > 6 (previous = 10).
    // Restoring must leave allocator at max(10, 6) = 10.
    let target_root_b = TempDir::new().unwrap();
    let target_b = TaskRegistryStore::open(&task_registry_path(target_root_b.path())).unwrap();
    bind_with_fingerprint(&target_b, target_root_b.path(), "ws_target_b", FINGERPRINT);
    target_b.seed_allocator_start(10).unwrap();
    assert_eq!(target_b.allocator_next_number().unwrap(), 10);

    let restore_cache_b = target_root_b.path().join("restore_cache");
    std::fs::create_dir_all(&restore_cache_b).unwrap();
    let outcome_b = restore_publication(
        &target_b,
        PublicationRestoreRequest {
            task_workspace_id: "ws_target_b".to_string(),
            publication: PublicationInspectRequest {
                workspace_id: "ws_source".to_string(),
                source_repository_fingerprint: FINGERPRINT.to_string(),
                publication_id: "pub_mixed".to_string(),
                authority_machine_id: "hm_owner".to_string(),
                publication_remote: bare.to_str().unwrap().to_string(),
                publication_branch: "main".to_string(),
                cache_dir: restore_cache_b,
                commit: None,
            },
            mode: PublicationRestoreMode::EmptyDestination,
        },
    )
    .expect("restore publication");
    assert_eq!(outcome_b.restored_task_ids.len(), 2);
    assert_eq!(
        target_b.allocator_next_number().unwrap(),
        10,
        "ORB-14129: restoring ORB-00005 and DANI-90000 with previous=10 must preserve allocator at max(10, 6) = 10"
    );
    assert_eq!(
        target_b.allocate_task_id("ws_target_b").unwrap(),
        "ORB-00010",
        "ORB-14129: next minted task after restore must be ORB-00010"
    );

    // Case C: Publication with only foreign task IDs (DANI-90000).
    // Must succeed without error and preserve previous allocator.
    let foreign_only_pub_dir = TempDir::new().unwrap();
    let foreign_only_bare = foreign_only_pub_dir.path().join("foreign_only.git");
    let foreign_only_cache = foreign_only_pub_dir.path().join("cache");
    std::fs::create_dir_all(&foreign_only_cache).unwrap();
    let mut git_init_foreign = std::process::Command::new("git");
    git_init_foreign.args([
        "init",
        "--bare",
        "--quiet",
        "-b",
        "main",
        foreign_only_bare.to_str().unwrap(),
    ]);
    assert!(git_init_foreign.status().unwrap().success());

    publish_task_snapshot(
        &foreign_registry,
        PublicationPublishRequest {
            workspace_id: "ws_foreign".to_string(),
            task_workspace_id: "ws_foreign".to_string(),
            source_repository_fingerprint: FINGERPRINT.to_string(),
            publication_id: "pub_foreign_only".to_string(),
            authority_machine_id: "hm_foreign".to_string(),
            local_machine_id: "hm_foreign".to_string(),
            caller_role: PublicationCallerRole::Owner,
            publication_remote: foreign_only_bare.to_str().unwrap().to_string(),
            publication_branch: "main".to_string(),
            cache_dir: foreign_only_cache,
            published_at: Utc.with_ymd_and_hms(2026, 10, 4, 12, 0, 0).unwrap(),
            last_success: None,
        },
        &AttachmentPolicy {
            kind: AttachmentPolicyKind::Omit,
            max_file_bytes: 1024,
            max_total_bytes: 1024,
            deny_patterns: Vec::new(),
            scanner_failure_behavior: ScannerFailureBehavior::AllowUnchecked,
        },
        None,
    )
    .expect("publish foreign only");

    let target_root_c = TempDir::new().unwrap();
    let target_c = TaskRegistryStore::open(&task_registry_path(target_root_c.path())).unwrap();
    bind_with_fingerprint(&target_c, target_root_c.path(), "ws_target_c", FINGERPRINT);
    target_c.seed_allocator_start(4).unwrap();
    assert_eq!(target_c.allocator_next_number().unwrap(), 4);

    let restore_cache_c = target_root_c.path().join("restore_cache");
    std::fs::create_dir_all(&restore_cache_c).unwrap();
    let outcome_c = restore_publication(
        &target_c,
        PublicationRestoreRequest {
            task_workspace_id: "ws_target_c".to_string(),
            publication: PublicationInspectRequest {
                workspace_id: "ws_foreign".to_string(),
                source_repository_fingerprint: FINGERPRINT.to_string(),
                publication_id: "pub_foreign_only".to_string(),
                authority_machine_id: "hm_foreign".to_string(),
                publication_remote: foreign_only_bare.to_str().unwrap().to_string(),
                publication_branch: "main".to_string(),
                cache_dir: restore_cache_c,
                commit: None,
            },
            mode: PublicationRestoreMode::EmptyDestination,
        },
    )
    .expect("ORB-14129: restore of publication containing only foreign task IDs must succeed");
    assert_eq!(outcome_c.restored_task_ids, vec!["DANI-90000"]);
    assert_eq!(
        target_c.allocator_next_number().unwrap(),
        4,
        "ORB-14129: publication with only foreign task IDs must preserve prior allocator"
    );
    assert_eq!(
        target_c.allocate_task_id("ws_target_c").unwrap(),
        "ORB-00004",
        "ORB-14129: next minted task must continue from prior allocator"
    );
}

// ---------------------------------------------------------------------------
// Job-run ids and pull occupancy
// ---------------------------------------------------------------------------

fn minute_stem(run_id: &str) -> &str {
    run_id.rsplit_once('-').expect("run id has a sequence").0
}

fn finish(jobs: &dyn JobRunStoreBackend, run_id: &str) {
    let at = Utc::now();
    assert!(
        jobs.mark_job_run_running(run_id, at, 42)
            .unwrap()
            .owns_execution()
    );
    assert!(
        jobs.finalize_job_run(run_id, JobRunState::Success, at, Some(1))
            .unwrap()
    );
}

/// [ORB-13599] Archive and delete remove a run's row; the id must stay
/// reserved so the next run minted in the same minute, by this process or a
/// later one, never takes it.
#[test]
fn removed_job_run_ids_are_never_reallocated() {
    if !isolated("removed_job_run_ids_are_never_reallocated") {
        return;
    }
    let root = TempDir::new().unwrap();
    let db = root.path().join("orbit.db");
    let mut minted = Vec::new();
    for delete in [false, true] {
        // The id stem is the wall-clock minute; retry the cycle if it crossed
        // a minute boundary, which would make the comparison vacuous.
        let (removed, next) = (0..3)
            .find_map(|_| {
                let jobs = workspace_job_run_store(Store::open(&db).unwrap(), "ws_a");
                let removed = jobs
                    .insert_job_run("job-a", 1, Utc::now(), None, None)
                    .unwrap();
                finish(jobs.as_ref(), &removed.run_id);
                if delete {
                    jobs.delete_job_run(&removed.run_id).unwrap();
                } else {
                    jobs.archive_job_run(&removed.run_id).unwrap();
                }
                assert!(jobs.get_job_run(&removed.run_id).unwrap().is_none());
                drop(jobs);

                let reopened = workspace_job_run_store(Store::open(&db).unwrap(), "ws_a");
                let next = reopened
                    .insert_job_run("job-a", 1, Utc::now(), None, None)
                    .unwrap();
                (minute_stem(&removed.run_id) == minute_stem(&next.run_id))
                    .then_some((removed.run_id, next.run_id))
            })
            .expect("two runs minted within one minute");
        assert_ne!(next, removed, "a removed run's id was minted again");
        minted.extend([removed, next]);
    }
    let unique = minted.iter().collect::<HashSet<_>>();
    assert_eq!(
        unique.len(),
        minted.len(),
        "every id minted once: {minted:?}"
    );
}

fn pull_request(run_id: &str, request_id: &str) -> AdmissionRequest {
    AdmissionRequest {
        request_id: request_id.into(),
        caller_version: "1".into(),
        caller_schema: orbit_store::contracts::DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
        caller_fingerprint: None,
        caller_before_pr: false,
        review_gate: false,
        run_context: AdmissionRunContext {
            run_id: run_id.into(),
            job_name: "workspace_auto_pipeline".into(),
            machine_name: None,
        },
        ship: AdmissionShipContract {
            mode: "local".into(),
            base_branch: "main".into(),
            landing_branch: "main".into(),
            before_pr: false,
            completion: "review".into(),
            authorization_reference: None,
            review: None,
        },
        crews: None,
        os: None,
    }
}

/// Twelve independent connections race for local pull admission under a
/// ceiling of three: exactly three are admitted and the store's occupancy
/// reading counts each admitted request as one slot.
#[test]
fn concurrent_pull_admission_obeys_the_shared_ceiling() {
    if !isolated("concurrent_pull_admission_obeys_the_shared_ceiling") {
        return;
    }
    const RACERS: usize = 12;
    const CEILING: usize = 3;
    let root = TempDir::new().unwrap();
    let db = root.path().join("pull.db");
    let jobs = workspace_job_run_store(Store::open(&db).unwrap(), "ws");
    let parent = jobs
        .insert_job_run("workspace_auto_pipeline", 1, Utc::now(), None, None)
        .unwrap();
    jobs.write_run_state(
        &parent.run_id,
        &PipelineState::new(parent.run_id.clone(), parent.job_id, serde_json::json!({})),
    )
    .unwrap();
    let destination = PullDestination {
        owner_machine_id: "owner".into(),
        owner_workspace_id: "ws".into(),
        selector: "owner/ws".into(),
        execution_machine_id: "owner".into(),
    };
    // Bootstrap the feature before independent connections race for admission.
    jobs.local_pull_admissions().unwrap();

    let barrier = Arc::new(Barrier::new(RACERS));
    let racers = (0..RACERS)
        .map(|index| {
            let jobs = workspace_job_run_store(Store::open(&db).unwrap(), "ws");
            let destination = destination.clone();
            let request = pull_request(&parent.run_id, &format!("request-{index}"));
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                jobs.allocate_pull_request(&destination, &request, CEILING)
                    .unwrap()
                    .is_some()
            })
        })
        .collect::<Vec<_>>();
    let admitted = racers
        .into_iter()
        .map(|racer| usize::from(racer.join().unwrap()))
        .sum::<usize>();

    assert_eq!(admitted, CEILING);
    assert_eq!(jobs.local_pull_admissions().unwrap().len(), CEILING);
    assert_eq!(jobs.drain_leaf_occupancy().unwrap().occupied, CEILING);
    assert_eq!(
        jobs.drain_leaf_occupancy_for_run(&parent.run_id)
            .unwrap()
            .inherited,
        Some(0)
    );
    let replacement = jobs
        .insert_job_run("workspace_auto_pipeline", 1, Utc::now(), None, None)
        .unwrap();
    assert_eq!(
        jobs.drain_leaf_occupancy_for_run(&replacement.run_id)
            .unwrap()
            .inherited,
        Some(CEILING)
    );
    let late = pull_request(&parent.run_id, "late");
    assert!(
        jobs.allocate_pull_request(&destination, &late, CEILING)
            .unwrap()
            .is_none(),
        "a full ceiling admits nothing more"
    );
}

/// The drain's own limit is the only ceiling on pulled leaves: one drain
/// admits well past the ten a leaf definition used to allow, under the limit
/// an operator retuned on the drain's state rather than the one it was
/// submitted with.
#[test]
fn pull_admission_is_bound_only_by_the_drains_live_limit() {
    if !isolated("pull_admission_is_bound_only_by_the_drains_live_limit") {
        return;
    }
    const SUBMITTED: usize = 3;
    const RETUNED: u32 = 24;
    let root = TempDir::new().unwrap();
    let jobs = workspace_job_run_store(Store::open(&root.path().join("pull.db")).unwrap(), "ws");
    let parent = jobs
        .insert_job_run("workspace_pull_pipeline", 1, Utc::now(), None, None)
        .unwrap();
    let mut state = PipelineState::new(
        parent.run_id.clone(),
        parent.job_id.clone(),
        serde_json::json!({}),
    );
    assert!(state.set_drain_worker_limit(RETUNED, SUBMITTED as u32, "cli".into(), None, None));
    jobs.write_run_state(&parent.run_id, &state).unwrap();
    let destination = PullDestination {
        owner_machine_id: "owner".into(),
        owner_workspace_id: "ws".into(),
        selector: "owner/ws".into(),
        execution_machine_id: "owner".into(),
    };

    let admitted = (0..RETUNED + 6)
        .filter(|index| {
            let request = pull_request(&parent.run_id, &format!("request-{index}"));
            jobs.allocate_pull_request(&destination, &request, SUBMITTED)
                .unwrap()
                .is_some()
        })
        .count();

    assert_eq!(admitted, RETUNED as usize, "the retuned limit governs");
    let occupancy = jobs.drain_leaf_occupancy().unwrap();
    assert_eq!(occupancy.occupied, RETUNED as usize);
    assert_eq!(
        occupancy.per_pipeline.get("task_claimed_local_pipeline"),
        Some(&(RETUNED as usize)),
        "one leaf definition holds every slot, with no ceiling of its own"
    );
}

/// A claimed leaf waiting for retry still owns one drain slot, while terminal
/// leaf history releases its slot and does not affect the capacity reading.
#[test]
fn retrying_claimed_leaf_occupies_capacity_and_terminal_history_does_not() {
    if !isolated("retrying_claimed_leaf_occupies_capacity_and_terminal_history_does_not") {
        return;
    }
    const CEILING: usize = 1;
    let root = TempDir::new().unwrap();
    let store = Store::open(&root.path().join("pull.db")).unwrap();
    let jobs = workspace_job_run_store(store.clone(), "ws");
    let parent = jobs
        .insert_job_run("workspace_pull_pipeline", 1, Utc::now(), None, None)
        .unwrap();
    let mut state = PipelineState::new(
        parent.run_id.clone(),
        parent.job_id.clone(),
        serde_json::json!({}),
    );
    assert!(state.set_drain_worker_limit(CEILING as u32, CEILING as u32, "cli".into(), None, None));
    jobs.write_run_state(&parent.run_id, &state).unwrap();

    let retrying = jobs
        .insert_job_run(
            "task_claimed_local_pipeline",
            1,
            Utc::now(),
            Some(serde_json::json!({"task_ids": ["ORB-TEST"]})),
            None,
        )
        .unwrap();
    let terminal = jobs
        .insert_job_run(
            "task_claimed_pr_pipeline",
            1,
            Utc::now(),
            Some(serde_json::json!({"task_ids": ["ORB-OLD"]})),
            None,
        )
        .unwrap();
    finish(jobs.as_ref(), &terminal.run_id);
    let retrying_run_id = retrying.run_id.clone();
    store
        .with_transaction(|tx| {
            let changed = tx
                .connection()
                .execute(
                    "UPDATE job_runs SET state = 'retrying' WHERE workspace_id = ?1 AND run_id = ?2",
                    ["ws", retrying_run_id.as_str()],
                )
                .unwrap();
            assert_eq!(changed, 1, "retrying fixture run exists");
            Ok(())
        })
        .unwrap();

    let occupancy = jobs.drain_leaf_occupancy().unwrap();
    assert_eq!(
        occupancy.occupied, CEILING,
        "the retrying leaf occupies capacity and the terminal leaf is excluded"
    );
    assert_eq!(
        occupancy.per_pipeline.get("task_claimed_local_pipeline"),
        Some(&1),
        "retrying claimed leaves count toward their pipeline"
    );
    assert_eq!(
        occupancy.per_pipeline.get("task_claimed_pr_pipeline"),
        None,
        "terminal leaf history does not count"
    );

    let destination = PullDestination {
        owner_machine_id: "owner".into(),
        owner_workspace_id: "ws".into(),
        selector: "owner/ws".into(),
        execution_machine_id: "owner".into(),
    };
    let request = pull_request(&parent.run_id, "retrying-leaf-at-capacity");
    assert!(
        jobs.allocate_pull_request(&destination, &request, CEILING)
            .unwrap()
            .is_none(),
        "a retrying claimed leaf prevents admission at the ceiling"
    );
}

// ---------------------------------------------------------------------------
// Coordinated admission and handoff
// ---------------------------------------------------------------------------

#[test]
fn reserved_key_publishes_a_new_bundle_before_replaying() {
    if !isolated("reserved_key_publishes_a_new_bundle_before_replaying") {
        return;
    }
    let root = TempDir::new().unwrap();
    let owner = Coordinated::open(root.path());
    for (key, digest) in [("automation", None), ("desktop", Some("payload-digest"))] {
        let params = TaskCreateParams {
            actor: "system".to_string(),
            parent_id: None,
            title: "Recover an interrupted keyed creation".to_string(),
            description: String::new(),
            acceptance_criteria: Vec::new(),
            dependencies: Vec::new(),
            relations: Vec::new(),
            tags: Vec::new(),
            required_tools: Vec::new(),
            plan: String::new(),
            execution_summary: String::new(),
            context_files: Vec::new(),
            repo_root: None,
            created_by: Some("system".to_string()),
            planned_by: None,
            implemented_by: None,
            status: TaskStatus::Proposed,
            priority: TaskPriority::Medium,
            complexity: None,
            task_type: TaskType::Chore,
            external_refs: Vec::new(),
            source_task_id: None,
            crew: None,
            crew_source: None,
            orchestrator: None,
            comments: Vec::new(),
            context_creation: Vec::new(),
        };
        let id = owner.registry.allocate_task_id(PARTITION_ID).unwrap();
        let input_digest = digest.map(str::to_string).unwrap_or_else(|| {
            format!("{:x}", Sha256::digest(serde_json::to_vec(&params).unwrap()))
        });
        // Crash injection: durable key admission succeeded, but bundle
        // publication never ran. Reservation alone is not a creation replay.
        rusqlite::Connection::open(task_registry_path(root.path()))
            .unwrap()
            .execute(
                "INSERT INTO task_action_keys VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![PARTITION_ID, key, id, input_digest],
            )
            .unwrap();
        assert!(owner.backends.task.task.get_task(&id).unwrap().is_none());
        let create = || match digest {
            Some(digest) => {
                owner
                    .backends
                    .task
                    .task
                    .create_desktop_task(params.clone(), key, digest)
            }
            None => owner
                .backends
                .task
                .task
                .create_task_idempotent(params.clone(), key),
        };
        let (created, replayed) = create().expect("recover reserved key");
        assert_eq!(created.id, id, "recovery uses the reserved task identity");
        assert!(!replayed, "first bundle publication is a new creation");
        let (existing, replayed) = create().expect("replay published bundle");
        assert!(replayed, "a published bundle is a replay");
        assert_eq!(existing, created);
    }
    assert_eq!(owner.backends.task.task.list_tasks().unwrap().len(), 2);
}

/// One owner composition over a root directory. Opening a second one on the
/// same root is what a second process (or a restart) does.
struct Coordinated {
    registry: TaskRegistryStore,
    backends: CoordinatedWorkspaceBackends,
    orbit_dir: PathBuf,
}

impl Coordinated {
    fn open(root: &Path) -> Self {
        let registry = TaskRegistryStore::open(&task_registry_path(root)).unwrap();
        let binding = bind(&registry, root, PARTITION_ID);
        let store = Store::open(&root.join("state.sqlite")).unwrap();
        let backends =
            workspace_coordinated_backends(registry.clone(), binding.partition_id, store)
                .expect("compose");
        Self {
            registry,
            backends,
            orbit_dir: binding.orbit_dir,
        }
    }

    fn open_with_fingerprint(root: &Path, partition_id: &str, fingerprint: &str) -> Self {
        let registry = TaskRegistryStore::open(&task_registry_path(root)).unwrap();
        let binding = bind_with_fingerprint(&registry, root, partition_id, fingerprint);
        let store = Store::open(&root.join("state.sqlite")).unwrap();
        let backends =
            workspace_coordinated_backends(registry.clone(), binding.partition_id, store)
                .expect("compose");
        Self {
            registry,
            backends,
            orbit_dir: binding.orbit_dir,
        }
    }

    fn create_task(&self, title: &str) -> orbit_types::task::Task {
        self.create_task_in(title, &["src/lib.rs"])
    }

    fn create_task_in(&self, title: &str, selectors: &[&str]) -> orbit_types::task::Task {
        self.backends
            .task
            .task
            .create_task(TaskCreateParams {
                actor: "codex".to_string(),
                parent_id: None,
                title: title.to_string(),
                description: "Detailed task description".to_string(),
                acceptance_criteria: vec!["First criterion".to_string()],
                dependencies: Vec::new(),
                relations: Vec::new(),
                tags: Vec::new(),
                required_tools: Vec::new(),
                plan: "1. Do the work".to_string(),
                execution_summary: String::new(),
                context_files: selectors.iter().map(|s| (*s).to_string()).collect(),
                repo_root: None,
                created_by: Some("codex".to_string()),
                planned_by: None,
                implemented_by: None,
                status: TaskStatus::Backlog,
                priority: TaskPriority::High,
                complexity: Some(TaskComplexity::Medium),
                task_type: TaskType::Feature,
                external_refs: Vec::new(),
                source_task_id: None,
                crew: None,
                crew_source: None,
                orchestrator: None,
                comments: Vec::new(),
                context_creation: Vec::new(),
            })
            .expect("create task")
    }

    fn task_status(&self, id: &str) -> TaskStatus {
        self.backends
            .task
            .task
            .get_task(id)
            .unwrap()
            .unwrap()
            .status
    }

    fn active_reservations(&self) -> Vec<ActiveTaskReservation> {
        self.backends
            .reservation
            .inspect_active_task_reservations(&self.orbit_dir.to_string_lossy(), Some(PARTITION_ID))
            .unwrap()
    }

    fn claims(&self) -> Vec<ExecutionClaim> {
        self.backends.commit_boundary.execution_claims().unwrap()
    }

    fn try_pull(&self, request: &AdmissionRequest) -> Result<AdmissionLookup, OrbitError> {
        let location = ExecutionLocation {
            machine_id: "machine-a".into(),
            machine_name: Some("display".into()),
        };
        self.backends.commit_boundary.admit_task(
            &AdmissionIdentity::trusted_remote(location),
            request,
            "test",
            self.orbit_dir.parent().unwrap(),
            &self.orbit_dir,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &|_| Ok(None),
        )
    }

    fn pull(&self, request: &AdmissionRequest) -> AdmissionReceipt {
        match self.try_pull(request).expect("pull") {
            AdmissionLookup::Found { receipt, .. } => *receipt,
            other => panic!("expected a receipt: {other:?}"),
        }
    }

    fn landing_starts(&self) -> Vec<LandingStartRequest> {
        self.backends.task.task.landing_start_requests().unwrap()
    }
}

fn owner_request(id: &str) -> AdmissionRequest {
    AdmissionRequest {
        request_id: id.into(),
        caller_version: "test".into(),
        caller_schema: orbit_store::contracts::DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
        caller_fingerprint: None,
        caller_before_pr: false,
        review_gate: false,
        run_context: AdmissionRunContext {
            run_id: "drain".into(),
            job_name: "auto".into(),
            machine_name: Some("untrusted-label".into()),
        },
        ship: AdmissionShipContract {
            mode: "pr".into(),
            base_branch: "agent-main".into(),
            landing_branch: "agent-main".into(),
            before_pr: false,
            completion: "review".into(),
            authorization_reference: None,
            review: None,
        },
        crews: None,
        os: None,
    }
}

/// Two owner compositions (two processes) replay the same request at the
/// same instant: one claim and one reservation exist, and both callers get
/// the identical receipt.
#[test]
fn simultaneous_retries_of_one_request_create_one_claim() {
    if !isolated("simultaneous_retries_of_one_request_create_one_claim") {
        return;
    }
    let root = TempDir::new().unwrap();
    let owner = Coordinated::open(root.path());
    owner.create_task("work");
    let first = Coordinated::open(root.path());
    let second = Coordinated::open(root.path());
    let barrier = Barrier::new(2);
    let request = owner_request("same");
    let (a, b) = std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            barrier.wait();
            first.pull(&request)
        });
        let b = scope.spawn(|| {
            barrier.wait();
            second.pull(&request)
        });
        (a.join().unwrap(), b.join().unwrap())
    });
    assert!(a.claim.is_some());
    assert_eq!(a, b, "a retry replays the original receipt");
    assert_eq!(owner.active_reservations().len(), 1);
    assert_eq!(owner.claims().len(), 1);
}

/// Distinct concurrent requests race over two tasks that share a file: at
/// most one claims, the other is deferred, and only one reservation exists.
#[test]
fn distinct_concurrent_requests_never_claim_overlapping_tasks() {
    if !isolated("distinct_concurrent_requests_never_claim_overlapping_tasks") {
        return;
    }
    let root = TempDir::new().unwrap();
    let owner = Coordinated::open(root.path());
    owner.create_task("one");
    owner.create_task("two");
    let other = Coordinated::open(root.path());
    let barrier = Barrier::new(2);
    let receipts = std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            barrier.wait();
            owner.pull(&owner_request("a"))
        });
        let b = scope.spawn(|| {
            barrier.wait();
            other.pull(&owner_request("b"))
        });
        [a.join().unwrap(), b.join().unwrap()]
    });
    assert_eq!(
        receipts.iter().filter(|r| r.claim.is_some()).count(),
        1,
        "overlapping footprints admit one claim: {receipts:?}"
    );
    assert_eq!(owner.active_reservations().len(), 1);
    assert_eq!(owner.claims().len(), 1);
}

const OWNER_POLICY: &str = "workspace-config:workflow.distributed_completion";
const REPOSITORY: &str = "owner/repository";
const REVIEW_CREW: &str = "reviewer";

/// One way a handoff's review evidence is spoiled before acceptance.
type Tamper = fn(&mut Delivery);

/// An admitted and bound claim with its owner-held validation logs and the
/// handoff a worker would submit for it.
struct Delivery {
    _root: TempDir,
    owner: Coordinated,
    claim: ExecutionClaim,
    handoff: TaskHandoff,
    /// The owner's `workflow.required_validation_commands`.
    required: Vec<String>,
}

impl Delivery {
    fn admit(ship: AdmissionShipContract) -> Self {
        Self::admit_in(ship, &["src/lib.rs"])
    }

    fn admit_in(ship: AdmissionShipContract, selectors: &[&str]) -> Self {
        Self::admit_requiring(ship, selectors, &["build", "test"])
    }

    /// An admitted claim whose owner requires `required`, handed off with one
    /// passing log per required command.
    fn admit_requiring(ship: AdmissionShipContract, selectors: &[&str], required: &[&str]) -> Self {
        let root = TempDir::new().unwrap();
        let owner = Coordinated::open(root.path());
        owner.create_task_in("typed handoff", selectors);
        let mut request = owner_request("first");
        request.ship = ship.clone();
        request.review_gate = ship.before_pr;
        let claim = owner.pull(&request).claim.expect("claim");
        let unbound = ClaimInvocation::trusted_worker(
            claim.task_id.clone(),
            claim.claim_id.clone(),
            claim.executed_on.machine_id.clone(),
            None,
        );
        owner
            .backends
            .commit_boundary
            .mutate_execution_claim(
                Some(&unbound),
                "bind",
                &ClaimMutation::Bind {
                    run: leaf_run(&claim),
                    ship,
                },
            )
            .expect("bind");
        let handoff = handoff_with_logs(&owner, &claim, required);
        Self {
            _root: root,
            owner,
            claim,
            handoff,
            required: required.iter().map(ToString::to_string).collect(),
        }
    }

    fn observation(&self, policy: Option<&str>) -> HandoffObservation {
        HandoffObservation {
            footprint_widening: self.handoff.footprint_widening.clone(),
            candidate: self.handoff.candidate.clone(),
            required_commands: self.required.clone(),
            owner_completion_authority: policy.map(str::to_string),
            review: self
                .handoff
                .review
                .before_pr()
                .map(|evidence| HandoffReviewObservation {
                    reviewed_base_sha: evidence.reviewed_base_sha.clone(),
                    reviewed_base_is_ancestor: true,
                    repository: REPOSITORY.into(),
                }),
        }
    }

    fn worker(&self) -> ClaimInvocation {
        ClaimInvocation::trusted_worker(
            self.claim.task_id.clone(),
            self.claim.claim_id.clone(),
            self.claim.executed_on.machine_id.clone(),
            Some(leaf_run(&self.claim)),
        )
    }

    fn operator(&self) -> ClaimInvocation {
        ClaimInvocation::trusted_operator(
            self.claim.task_id.clone(),
            self.claim.claim_id.clone(),
            "owner-operator".into(),
        )
    }

    fn accept(&self, policy: Option<&str>) -> Result<ClaimMutationResult, OrbitError> {
        self.accept_observed(self.observation(policy))
    }

    fn accept_observed(
        &self,
        observation: HandoffObservation,
    ) -> Result<ClaimMutationResult, OrbitError> {
        self.owner.backends.commit_boundary.mutate_execution_claim(
            Some(&self.worker().with_handoff_observation(observation)),
            "handoff",
            &ClaimMutation::AcceptHandoff(self.handoff.clone()),
        )
    }

    /// Persist `certificate` on the owner as the leaf's claim evidence and
    /// make the handoff carry it as its before-PR review.
    fn reviewed(&mut self, certificate: &ReviewCertificate) {
        let content = serde_json::to_vec(certificate).unwrap();
        let reference = HandoffArtifactRef {
            path: REVIEW_GATE_ARTIFACT.into(),
            sha256: format!("{:x}", Sha256::digest(&content)),
        };
        self.owner
            .backends
            .commit_boundary
            .mutate_execution_claim(
                Some(&self.worker()),
                &format!("review-certificate-{}", certificate.verdict.as_str()),
                &ClaimMutation::Evidence(ClaimEvidence {
                    artifacts: vec![TaskArtifact {
                        path: reference.path.clone(),
                        content,
                        media_type: "application/json".into(),
                        created_by: None,
                    }],
                    ..Default::default()
                }),
            )
            .expect("persist the certificate on the owner");
        self.handoff.review = HandoffReview {
            policy: ReviewTiming::BeforePr,
            disposition: HandoffReviewDisposition::BeforePr(Box::new(HandoffReviewEvidence {
                attempt_id: certificate.attempt_id.clone(),
                verdict: certificate.verdict,
                reviewed_head_sha: self.handoff.candidate.candidate.commit.clone(),
                reviewed_base_sha: certificate.base.commit.clone(),
                reviewer_commit: certificate
                    .repair_commits
                    .last()
                    .map(|repair| repair.commit.clone()),
                reviewer_crew: certificate.reviewer.crew.clone(),
                reviewer_run_id: "leaf".into(),
                certificate: reference,
                artifacts: vec![],
                host_evidence: vec![],
            })),
        };
    }

    fn review_evidence(&mut self) -> &mut HandoffReviewEvidence {
        match &mut self.handoff.review.disposition {
            HandoffReviewDisposition::BeforePr(evidence) => evidence,
            HandoffReviewDisposition::NotRequired => panic!("the handoff carries no review"),
        }
    }

    fn approve(&self) -> Result<ClaimMutationResult, OrbitError> {
        let accepted = self
            .owner
            .backends
            .commit_boundary
            .accepted_handoff(&self.claim.claim_id)?;
        self.owner.backends.commit_boundary.mutate_execution_claim(
            Some(
                &self
                    .operator()
                    .with_handoff_observation(self.observation(None)),
            ),
            "approval",
            &ClaimMutation::ApproveHandoff {
                handoff_id: accepted.handoff_id,
                candidate: self.handoff.candidate.clone(),
            },
        )
    }

    fn merge_intent(&self, policy: Option<&str>) -> Result<ClaimMutationResult, OrbitError> {
        self.owner.backends.commit_boundary.mutate_execution_claim(
            Some(
                &self
                    .operator()
                    .with_handoff_observation(self.observation(policy)),
            ),
            "merge-intent",
            &ClaimMutation::MergeIntent {
                intent_id: "sent-merge".into(),
                resolved: false,
                evidence: "pinned provider request".into(),
            },
        )
    }
}

fn leaf_run(claim: &ExecutionClaim) -> ClaimRun {
    ClaimRun {
        machine_id: claim.executed_on.machine_id.clone(),
        run_id: "leaf".into(),
    }
}

fn done_ship() -> AdmissionShipContract {
    let mut ship = owner_request("first").ship;
    ship.completion = "done".into();
    ship.authorization_reference = Some(OWNER_POLICY.into());
    ship
}

/// The worker's handoff over a pinned candidate, with one validation log per
/// command in `commands` persisted on the owner as claim evidence.
fn handoff_with_logs(
    owner: &Coordinated,
    claim: &ExecutionClaim,
    commands: &[&str],
) -> TaskHandoff {
    let mut handoff = TaskHandoff {
        schema_version: 1,
        workspace_id: PARTITION_ID.into(),
        task_id: claim.task_id.clone(),
        claim_id: claim.claim_id.clone(),
        machine_id: claim.executed_on.machine_id.clone(),
        run_id: "leaf".into(),
        candidate: HandoffCandidate {
            repository: REPOSITORY.into(),
            source_branch: "attempt/leaf".into(),
            base_branch: "agent-main".into(),
            landing_branch: "agent-main".into(),
            candidate: SourceRevision {
                commit: "a".repeat(40),
                tree: "b".repeat(40),
            },
            base: SourceRevision {
                commit: "c".repeat(40),
                tree: "d".repeat(40),
            },
            delivery: HandoffDelivery::PullRequest { number: 42 },
        },
        review: HandoffReview::not_required(),
        execution_summary: "Outcome: success\nRequired checks passed for pinned candidate".into(),
        validation: vec![],
        footprint_widening: vec![],
    };
    let artifacts = commands
        .iter()
        .map(|command| {
            let log = HandoffValidationLog {
                schema_version: 1,
                workspace_id: handoff.workspace_id.clone(),
                task_id: handoff.task_id.clone(),
                claim_id: handoff.claim_id.clone(),
                machine_id: handoff.machine_id.clone(),
                run_id: handoff.run_id.clone(),
                candidate: handoff.candidate.clone(),
                tested_head: handoff.candidate.candidate.commit.clone(),
                command: (*command).into(),
                exit_code: 0,
                output: format!("{command} captured output"),
            };
            TaskArtifact {
                path: format!("{command}.json"),
                content: serde_json::to_vec(&log).unwrap(),
                media_type: "application/json".into(),
                created_by: None,
            }
        })
        .collect::<Vec<_>>();
    handoff.validation = artifacts
        .iter()
        .map(|artifact| HandoffArtifactRef {
            path: artifact.path.clone(),
            sha256: format!("{:x}", Sha256::digest(&artifact.content)),
        })
        .collect();
    if artifacts.is_empty() {
        return handoff;
    }
    let worker = ClaimInvocation::trusted_worker(
        claim.task_id.clone(),
        claim.claim_id.clone(),
        claim.executed_on.machine_id.clone(),
        Some(leaf_run(claim)),
    );
    owner
        .backends
        .commit_boundary
        .mutate_execution_claim(
            Some(&worker),
            "validation-logs",
            &ClaimMutation::Evidence(ClaimEvidence {
                artifacts,
                ..Default::default()
            }),
        )
        .expect("persist owner logs");
    handoff
}

/// Completion authority is the owner policy the claim was admitted under,
/// still granted by the owner when the handoff arrives and when it lands.
#[test]
fn handoff_authorization_follows_the_owner_policy_at_admission() {
    if !isolated("handoff_authorization_follows_the_owner_policy_at_admission") {
        return;
    }
    // A `done` contract admitted under the policy is authorized at acceptance
    // and its landing request is pending.
    let granted = Delivery::admit(done_ship());
    granted.accept(Some(OWNER_POLICY)).expect("handoff");
    assert_eq!(
        granted.owner.task_status(&granted.claim.task_id),
        TaskStatus::Review
    );
    let starts = granted.owner.landing_starts();
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].state, LandingStartState::Pending);
    // Landing rechecks the owner's current policy.
    let withdrawn = granted
        .merge_intent(None)
        .expect_err("withdrawn policy fences landing");
    assert!(
        withdrawn.to_string().contains("policy withdrawn"),
        "{withdrawn}"
    );
    granted
        .merge_intent(Some(OWNER_POLICY))
        .expect("policy still granted");

    // The same contract, accepted while the owner grants no policy or another
    // one, waits in review for an operator.
    for policy in [None, Some("workspace-config:some.other_key")] {
        let waiting = Delivery::admit(done_ship());
        waiting
            .accept(policy)
            .expect("a valid delivery is still accepted");
        assert_eq!(
            waiting.owner.task_status(&waiting.claim.task_id),
            TaskStatus::Review
        );
        assert!(
            waiting.owner.landing_starts().is_empty(),
            "no authority for {policy:?}"
        );
        waiting.approve().expect("operator approves");
        assert_eq!(waiting.owner.landing_starts().len(), 1);
    }

    // A `review` contract is never authorized by a policy the owner grants
    // after admission.
    let review = Delivery::admit(owner_request("first").ship);
    review.accept(Some(OWNER_POLICY)).expect("handoff");
    assert!(review.owner.landing_starts().is_empty());
}

/// An owner whose `workflow.required_validation_commands` is empty requires
/// no check: it accepts a handoff carrying no validation logs, authorizes it
/// under its completion policy and lets it land. The handoff's other checks
/// still hold — a candidate the owner observes differently is refused, and an
/// owner that does require commands refuses the same log-free handoff.
#[test]
fn an_owner_requiring_no_commands_accepts_and_lands_a_handoff_without_logs() {
    if !isolated("an_owner_requiring_no_commands_accepts_and_lands_a_handoff_without_logs") {
        return;
    }
    let delivery = Delivery::admit_requiring(done_ship(), &["src/lib.rs"], &[]);
    assert!(delivery.handoff.validation.is_empty());

    let mut moved = delivery.observation(Some(OWNER_POLICY));
    moved.candidate.candidate.commit = "e".repeat(40);
    let moved = delivery
        .owner
        .backends
        .commit_boundary
        .mutate_execution_claim(
            Some(&delivery.worker().with_handoff_observation(moved)),
            "moved",
            &ClaimMutation::AcceptHandoff(delivery.handoff.clone()),
        )
        .expect_err("candidate integrity still gates a handoff with no required commands");
    assert!(moved.to_string().contains("candidate"), "{moved}");

    delivery
        .accept(Some(OWNER_POLICY))
        .expect("no required command is no gate");
    assert_eq!(
        delivery.owner.task_status(&delivery.claim.task_id),
        TaskStatus::Review
    );
    let starts = delivery.owner.landing_starts();
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].state, LandingStartState::Pending);
    delivery
        .merge_intent(Some(OWNER_POLICY))
        .expect("landing rechecks the same empty requirement");

    let mut requiring = Delivery::admit_requiring(done_ship(), &["src/lib.rs"], &[]);
    requiring.required = vec!["build".into()];
    let refused = requiring
        .accept(Some(OWNER_POLICY))
        .expect_err("a required command needs its log");
    assert!(
        refused.to_string().contains("required validation missing"),
        "{refused}"
    );
    assert_eq!(
        requiring.owner.task_status(&requiring.claim.task_id),
        TaskStatus::InProgress
    );
}

/// Validation evidence replaced in owner storage after acceptance blocks an
/// operator's approval and, once approved, the landing itself.
#[test]
fn replaced_handoff_evidence_blocks_approval_and_landing() {
    if !isolated("replaced_handoff_evidence_blocks_approval_and_landing") {
        return;
    }
    for approved in [false, true] {
        let delivery = Delivery::admit(owner_request("first").ship);
        delivery.accept(None).expect("handoff");
        if approved {
            delivery.approve().expect("approve over intact evidence");
        }
        // Owner storage replaced behind the claim, not an authorized
        // post-handoff worker write: a well-formed log for the same candidate
        // whose bytes no longer match the digest the handoff pinned.
        let log = delivery
            .owner
            .registry
            .canonical_task_bundle_path(PARTITION_ID, &delivery.claim.task_id)
            .unwrap()
            .join("artifacts/files/build.json");
        assert!(
            log.is_file(),
            "the owner holds the validation log at {}",
            log.display()
        );
        let mut replaced: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&log).unwrap()).unwrap();
        replaced["output"] = "replaced output".into();
        std::fs::write(&log, serde_json::to_vec(&replaced).unwrap()).unwrap();
        if approved {
            assert!(
                delivery.merge_intent(None).is_err(),
                "landing must refuse replaced evidence"
            );
        } else {
            assert!(
                delivery.approve().is_err(),
                "approval must refuse replaced evidence"
            );
            assert!(delivery.owner.landing_starts().is_empty());
        }
    }
}

/// Owner acceptance journals selectors, provenance history and the enlarged
/// review lock together. ORB-13990: a claimed implementer may change any path
/// the work requires, so every path an owner can track widens — another
/// crate, docs, a new top-level `tests/` file. Only metadata, environment
/// and non-normal paths are refused, leaving the claim and task unchanged.
#[test]
fn handoff_widening_accepts_any_safe_path_and_replays_once() {
    if !isolated("handoff_widening_accepts_any_safe_path_and_replays_once") {
        return;
    }
    for (path, allowed) in [
        ("crates/touched/src/split/new.rs", true),
        ("crates/other/tests/test_env.rs", true),
        ("crates/other/src/lib.rs", true),
        ("docs/new.md", true),
        ("tests/claim_refusal.rs", true),
        ("crates/touched/.env", false),
        ("crates/touched/.orbit/new", false),
        ("crates/touched/../other/src/lib.rs", false),
    ] {
        let mut delivery = Delivery::admit_in(
            owner_request("template").ship,
            &["file:crates/touched/src/lib.rs"],
        );
        delivery.handoff.footprint_widening = vec![path.into()];
        let before = delivery
            .owner
            .backends
            .task
            .task
            .get_task(&delivery.claim.task_id)
            .unwrap()
            .unwrap();
        let result = delivery.accept(None);
        if allowed {
            result.unwrap_or_else(|error| panic!("{path} widening accepted: {error}"));
            delivery.accept(None).expect("lost response replay");
            let task = delivery
                .owner
                .backends
                .task
                .task
                .get_task(&delivery.claim.task_id)
                .unwrap()
                .unwrap();
            assert_eq!(task.status, TaskStatus::Review);
            assert!(task.context_files.contains(&format!("file:{path}")));
            assert!(
                task.context_files
                    .contains(&"file:crates/touched/src/lib.rs".into())
            );
            let history = delivery
                .owner
                .backends
                .task
                .history
                .get_task_history(&delivery.claim.task_id)
                .unwrap()
                .unwrap();
            let widenings = history
                .iter()
                .filter(|e| e.event == CONTEXT_FILES_WIDENED_EVENT)
                .map(|e| ContextFilesWidening::from_note(e.note.as_deref().unwrap()).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(
                widenings.len(),
                1,
                "replay must not duplicate widening history"
            );
            assert_eq!(widenings[0].step, ContextWideningStep::Implement);
            assert_eq!(widenings[0].run_id, delivery.handoff.run_id);
            assert_eq!(widenings[0].selectors, vec![format!("file:{path}")]);
            let current = delivery.owner.claims().pop().unwrap();
            assert!(current.footprint.contains(&format!("file:{path}")));
            // Admission replay remains the immutable original receipt.
            let mut request = owner_request("first");
            request.ship = owner_request("template").ship;
            assert_eq!(
                delivery.owner.pull(&request).claim.unwrap().footprint,
                delivery.claim.footprint
            );
        } else {
            let error = result.unwrap_err().to_string();
            assert!(error.contains(path), "exact refused path: {error}");
            assert_eq!(
                delivery
                    .owner
                    .backends
                    .task
                    .task
                    .get_task(&delivery.claim.task_id)
                    .unwrap()
                    .unwrap()
                    .context_files,
                before.context_files
            );
            assert_eq!(
                delivery.owner.task_status(&delivery.claim.task_id),
                TaskStatus::InProgress
            );
        }
    }
}

/// The widening request must equal the owner's independently observed diff,
/// while a competing claim or lock on the added path no longer refuses it:
/// footprint locks are a scheduling hint, not a delivery gate.
#[test]
fn handoff_widening_accepts_locked_paths_and_refuses_observation_mismatch() {
    if !isolated("handoff_widening_accepts_locked_paths_and_refuses_observation_mismatch") {
        return;
    }
    let path = "src/new/nested.rs";
    let mut delivery = Delivery::admit(owner_request("template").ship);
    delivery.handoff.footprint_widening = vec![path.into()];
    let mut observed = delivery.observation(None);
    observed.footprint_widening.clear();
    let error = delivery
        .owner
        .backends
        .commit_boundary
        .mutate_execution_claim(
            Some(&delivery.worker().with_handoff_observation(observed)),
            "mismatch",
            &ClaimMutation::AcceptHandoff(delivery.handoff.clone()),
        )
        .unwrap_err()
        .to_string();
    assert!(
        error.contains(path),
        "request must equal independently observed diff: {error}"
    );
    assert_eq!(
        delivery.owner.task_status(&delivery.claim.task_id),
        TaskStatus::InProgress
    );

    for lock in ["claim", "reservation", "status"] {
        let mut delivery = Delivery::admit(owner_request("template").ship);
        let competitor = delivery
            .owner
            .create_task_in("competing", &["file:src/new/nested.rs"]);
        match lock {
            "claim" => {
                delivery
                    .owner
                    .pull(&owner_request("second"))
                    .claim
                    .expect("second claim");
            }
            "reservation" => {
                let reserved = delivery
                    .owner
                    .backends
                    .reservation
                    .reserve_task_reservation(
                        orbit_store::contracts::TaskReservationReserveParams {
                            workspace_orbit_dir: delivery
                                .owner
                                .orbit_dir
                                .to_string_lossy()
                                .into_owned(),
                            workspace_id: Some(PARTITION_ID.into()),
                            task_ids: vec![competitor.id.clone()],
                            requested_files: vec![format!("file:{path}")],
                            stored_files: vec![format!("file:{path}")],
                            actor: "owner".into(),
                            ttl_seconds: 600,
                            owner_run_id: None,
                            owner_metadata_json: None,
                        },
                    )
                    .unwrap();
                assert!(reserved.reserved);
            }
            _ => {
                delivery
                    .owner
                    .backends
                    .commit_boundary
                    .commit_task_transition(&orbit_store::contracts::TaskCoordinationCommitParams {
                        task_id: competitor.id.clone(),
                        actor: "owner".into(),
                        expected_status: vec![TaskStatus::Backlog],
                        status: Some(TaskStatus::InProgress),
                        ..Default::default()
                    })
                    .unwrap();
            }
        }
        delivery.handoff.footprint_widening = vec![path.into()];
        delivery
            .accept(None)
            .unwrap_or_else(|error| panic!("a live {lock} lock does not gate delivery: {error}"));
        assert_eq!(
            delivery.owner.task_status(&delivery.claim.task_id),
            TaskStatus::Review
        );
    }
}

// ---------------------------------------------------------------------------
// Before-PR review contract and evidence [ORB-13895]
// ---------------------------------------------------------------------------

fn before_pr_ship() -> AdmissionShipContract {
    let mut ship = owner_request("first").ship;
    ship.before_pr = true;
    ship.review = Some(AdmissionReviewContract {
        contract_version: REVIEW_CONTRACT_VERSION,
        crew: Some(REVIEW_CREW.into()),
        budget: ReviewBudget { minutes: 45 },
        required_validation_commands: Some(vec!["build".into(), "test".into()]),
    });
    ship
}

/// The certificate a leaf's gate would issue for `handoff`: the reviewer
/// examined an implementation and fixed its findings in the candidate's
/// last commit.
fn certificate(handoff: &TaskHandoff, verdict: ReviewVerdict) -> ReviewCertificate {
    let commit = |revision: &SourceRevision, subject: &str| CommitIdentity {
        commit: revision.commit.clone(),
        tree: revision.tree.clone(),
        author: "implementer".into(),
        committer: "implementer".into(),
        subject: subject.into(),
    };
    let reviewed = SourceRevision {
        commit: "e".repeat(40),
        tree: "f".repeat(40),
    };
    ReviewCertificate {
        schema_version: REVIEW_CONTRACT_VERSION,
        attempt_id: "attempt-1".into(),
        lineage_key: "lineage".into(),
        task_ids: vec![handoff.task_id.clone()],
        task_meaning_digest: "meaning".into(),
        repository: REPOSITORY.into(),
        base: handoff.candidate.base.clone(),
        implementation_commits: vec![commit(&reviewed, "implement")],
        reviewed_candidate: reviewed,
        final_candidate: handoff.candidate.candidate.clone(),
        repair_commits: vec![commit(&handoff.candidate.candidate, "reviewer fixes")],
        verdict,
        assurance: verdict.assurance(),
        findings: vec![],
        validation: ["build", "test"]
            .into_iter()
            .map(|command| ReviewValidation {
                id: None,
                command: command.into(),
                outcome: ValidationOutcome::Passed,
                role: ValidationRole::Required,
                note: None,
                check: None,
                control: None,
                sources: vec![],
                mutation_target: Vec::new(),
                baseline: None,
            })
            .collect(),
        required_validation_commands: Some(vec!["build".into(), "test".into()]),
        validation_complete: verdict.passed(),
        reviewer: ReviewerIdentity {
            crew: REVIEW_CREW.into(),
            provider: "provider".into(),
            model: "model".into(),
            reasoning_effort: None,
            implementer_model: None,
            same_model_as_implementer: false,
        },
        consumed: ReviewConsumption::default(),
        budget: ReviewBudget { minutes: 45 },
        escalation: None,
        retained_obligations: vec![],
        retired_validation: vec![],
        validation_scope: vec![],
        selectors_widened: vec![],
        evidence_carried: None,
        baseline_red: Vec::new(),
        host_evidence: Vec::new(),
        issued_at: Utc::now(),
    }
}

/// An owner with `review.before_pr` on pins its review contract (crew,
/// budget, contract version) into the claim it admits, but only to a PR leaf
/// that declares it runs the gate; a pull without that declaration is
/// refused. A before-PR contract without its review
/// terms is malformed. The executor's own switch never refuses [ORB-13908].
#[test]
fn a_before_pr_owner_captures_its_review_contract_on_the_claim() {
    if !isolated("a_before_pr_owner_captures_its_review_contract_on_the_claim") {
        return;
    }
    let root = TempDir::new().unwrap();
    let owner = Coordinated::open(root.path());
    owner.create_task("reviewed work");

    let mut request = owner_request("ungated");
    request.ship = before_pr_ship();
    let refused = owner
        .try_pull(&request)
        .expect_err("a leaf that runs no gate is refused");
    assert!(
        refused.to_string().contains("before_pr_unsupported"),
        "{refused}"
    );

    let mut termless = owner_request("termless");
    termless.ship = before_pr_ship();
    termless.ship.review = None;
    termless.review_gate = true;
    let malformed = owner.try_pull(&termless).expect_err("malformed contract");
    assert!(
        malformed.to_string().contains("invalid_input"),
        "{malformed}"
    );
    assert!(owner.claims().is_empty(), "nothing was admitted");

    let mut gated = owner_request("gated");
    gated.ship = before_pr_ship();
    gated.review_gate = true;
    let receipt = owner.pull(&gated);
    assert!(receipt.claim.is_some(), "a gate-running leaf is admitted");
    assert_eq!(receipt.request.ship.review, before_pr_ship().review);
    assert_eq!(owner.claims().len(), 1);

    let other = TempDir::new().unwrap();
    let unreviewed = Coordinated::open(other.path());
    unreviewed.create_task("unreviewed work");
    let mut executor_on = owner_request("executor-before-pr");
    executor_on.caller_before_pr = true;
    let receipt = unreviewed.pull(&executor_on);
    assert!(
        receipt.claim.is_some(),
        "an executor's own review.before_pr does not refuse an owner that captured it off"
    );
}

/// A before-PR claim's handoff is accepted only with passing evidence for
/// the handed-off head on a base the owner holds, reviewed by the captured
/// crew under the captured contract, and bound by the certificate the owner
/// holds; every other disposition is refused with a typed reason and leaves
/// the task in progress. A claim without the contract refuses evidence it
/// never asked for.
#[test]
fn before_pr_handoffs_are_accepted_only_with_matching_passing_evidence() {
    if !isolated("before_pr_handoffs_are_accepted_only_with_matching_passing_evidence") {
        return;
    }
    let reviewed = |verdict: ReviewVerdict| {
        let mut delivery = Delivery::admit(before_pr_ship());
        let certificate = certificate(&delivery.handoff, verdict);
        delivery.reviewed(&certificate);
        (delivery, certificate)
    };

    let (delivery, issued) = reviewed(ReviewVerdict::AcceptWithFixes);
    delivery.accept(None).expect("passing evidence is accepted");
    assert_eq!(
        delivery.owner.task_status(&delivery.claim.task_id),
        TaskStatus::Review
    );
    assert_eq!(
        delivery
            .owner
            .backends
            .commit_boundary
            .accepted_review_certificate(&delivery.claim.claim_id)
            .unwrap(),
        Some(issued),
        "the owner reads back the certificate the handoff pinned"
    );
    delivery
        .approve()
        .expect("approval rechecks the pinned review evidence");

    let refusals: [(&str, HandoffReviewRefusal, Tamper); 6] = [
        (
            "no evidence",
            HandoffReviewRefusal::ReviewEvidenceMissing,
            |d| d.handoff.review = HandoffReview::not_required(),
        ),
        ("rejected", HandoffReviewRefusal::ReviewNotPassed, |d| {
            let rejected = certificate(&d.handoff, ReviewVerdict::Reject);
            d.reviewed(&rejected);
        }),
        (
            "another head",
            HandoffReviewRefusal::ReviewedHeadMismatch,
            |d| d.review_evidence().reviewed_head_sha = "9".repeat(40),
        ),
        (
            "another crew",
            HandoffReviewRefusal::ReviewContractMismatch,
            |d| d.review_evidence().reviewer_crew = "other".into(),
        ),
        (
            "another attempt",
            HandoffReviewRefusal::ReviewCertificateMismatch,
            |d| d.review_evidence().attempt_id = "attempt-2".into(),
        ),
        (
            "another certificate digest",
            HandoffReviewRefusal::ReviewCertificateMismatch,
            |d| d.review_evidence().certificate.sha256 = "0".repeat(64),
        ),
    ];
    for (case, refusal, change) in refusals {
        let (mut delivery, _) = reviewed(ReviewVerdict::Accept);
        change(&mut delivery);
        let error = delivery.accept(None).expect_err(case).to_string();
        assert!(error.contains(refusal.as_str()), "{case}: {error}");
        assert_eq!(
            delivery.owner.task_status(&delivery.claim.task_id),
            TaskStatus::InProgress,
            "{case}"
        );
    }

    // The owner's own observation decides ancestry and repository identity.
    let (delivery, _) = reviewed(ReviewVerdict::Accept);
    let mut unrelated = delivery.observation(None);
    unrelated.review.as_mut().unwrap().reviewed_base_is_ancestor = false;
    let error = delivery.accept_observed(unrelated).unwrap_err().to_string();
    assert!(
        error.contains(HandoffReviewRefusal::ReviewedBaseNotAncestor.as_str()),
        "{error}"
    );
    let mut elsewhere = delivery.observation(None);
    elsewhere.review.as_mut().unwrap().repository = "fork/repository".into();
    let error = delivery.accept_observed(elsewhere).unwrap_err().to_string();
    assert!(
        error.contains(HandoffReviewRefusal::ReviewCertificateMismatch.as_str()),
        "{error}"
    );
    let mut unobserved = delivery.observation(None);
    unobserved.review = None;
    assert!(delivery.accept_observed(unobserved).is_err());

    let mut unasked = Delivery::admit(owner_request("first").ship);
    let certificate = certificate(&unasked.handoff, ReviewVerdict::Accept);
    unasked.reviewed(&certificate);
    let error = unasked.accept(None).unwrap_err().to_string();
    assert!(
        error.contains(HandoffReviewRefusal::ReviewEvidenceUnexpected.as_str()),
        "{error}"
    );
}

fn text_artifact(path: &str, body: &str) -> TaskArtifact {
    TaskArtifact {
        path: path.to_string(),
        content: body.as_bytes().to_vec(),
        media_type: "text/plain".to_string(),
        created_by: None,
    }
}

fn invalid_input(error: OrbitError) -> String {
    match error {
        OrbitError::InvalidInput(message) => message,
        other => panic!("expected InvalidInput before commit, got {other}"),
    }
}

/// Non-canonical claim artifact paths must not be stored raw. A trailing
/// slash or repeated separator is written under the canonical path, and two
/// spellings of one path are refused before the journal decision, so a later
/// `get_task` can still read the bundle.
#[test]
fn claim_evidence_stores_canonical_artifact_paths() {
    if !isolated("claim_evidence_stores_canonical_artifact_paths") {
        return;
    }
    let root = TempDir::new().unwrap();
    let owner = Coordinated::open(root.path());
    let task = owner.create_task("artifact paths");
    let request = owner_request("paths");
    let claim = owner.pull(&request).claim.expect("claim");
    assert_eq!(claim.task_id, task.id);
    let unbound = ClaimInvocation::trusted_worker(
        claim.task_id.clone(),
        claim.claim_id.clone(),
        claim.executed_on.machine_id.clone(),
        None,
    );
    let worker = ClaimInvocation::trusted_worker(
        claim.task_id.clone(),
        claim.claim_id.clone(),
        claim.executed_on.machine_id.clone(),
        Some(leaf_run(&claim)),
    );
    let mutate = |auth: &ClaimInvocation, id: &str, mutation: &ClaimMutation| {
        owner
            .backends
            .commit_boundary
            .mutate_execution_claim(Some(auth), id, mutation)
    };

    let duplicate = mutate(
        &unbound,
        "evidence-duplicate",
        &ClaimMutation::Evidence(ClaimEvidence {
            artifacts: vec![text_artifact("a/b", "one"), text_artifact("a//b", "two")],
            ..Default::default()
        }),
    )
    .expect_err("both spellings of one path");
    assert!(
        invalid_input(duplicate).contains("duplicate artifact path"),
        "Evidence of a/b and a//b is refused before commit"
    );
    assert!(
        owner
            .backends
            .task
            .task
            .get_task(&task.id)
            .unwrap()
            .is_some(),
        "a refused Evidence leaves the task readable"
    );
    assert_eq!(
        owner
            .backends
            .task
            .artifact
            .get_task_artifacts(&task.id)
            .unwrap()
            .unwrap(),
        Vec::<TaskArtifact>::new(),
        "a refused Evidence commits no artifact"
    );

    mutate(
        &unbound,
        "evidence-notes",
        &ClaimMutation::Evidence(ClaimEvidence {
            artifacts: vec![text_artifact("notes/", "note")],
            ..Default::default()
        }),
    )
    .expect("store notes/ under its canonical path");
    mutate(
        &unbound,
        "bind",
        &ClaimMutation::Bind {
            run: leaf_run(&claim),
            ship: request.ship,
        },
    )
    .expect("bind");
    mutate(
        &worker,
        "update-separator",
        &ClaimMutation::Update(ClaimWorkerUpdate {
            evidence: ClaimEvidence {
                artifacts: vec![text_artifact("a//b", "body")],
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .expect("store a//b under its canonical path");

    let refused_fail = mutate(
        &worker,
        "fail-duplicate",
        &ClaimMutation::Fail(ClaimEvidence {
            summary: Some("blocked".to_string()),
            artifacts: vec![text_artifact("a/b", "one"), text_artifact("a//b", "two")],
            ..Default::default()
        }),
    )
    .expect_err("Fail carrying both spellings");
    assert!(
        invalid_input(refused_fail).contains("duplicate artifact path"),
        "Fail of a/b and a//b is refused before commit"
    );
    assert_eq!(owner.task_status(&task.id), TaskStatus::InProgress);
    assert_eq!(
        owner.claims()[0].phase,
        ExecutionClaimPhase::Running,
        "a refused Fail does not settle the claim"
    );
    let stored = owner
        .backends
        .task
        .artifact
        .get_task_artifacts(&task.id)
        .unwrap()
        .unwrap();
    assert_eq!(
        stored
            .iter()
            .map(|artifact| (artifact.path.as_str(), artifact.content.as_slice()))
            .collect::<Vec<_>>(),
        vec![("a/b", b"body".as_slice()), ("notes", b"note".as_slice())],
        "Evidence and Update persist canonical paths"
    );
    let manifest = owner
        .backends
        .task
        .artifact
        .get_task_artifact_manifest(&task.id)
        .unwrap()
        .unwrap();
    for file in &manifest {
        assert_eq!(
            file.blob,
            format!("files/{}", file.path),
            "manifest blob matches the canonical file"
        );
    }

    mutate(
        &worker,
        "fail-notes",
        &ClaimMutation::Fail(ClaimEvidence {
            summary: Some("stopped".to_string()),
            artifacts: vec![text_artifact("notes/", "final")],
            ..Default::default()
        }),
    )
    .expect("Fail stores a canonical path");
    assert_eq!(owner.task_status(&task.id), TaskStatus::Blocked);
    assert!(
        owner
            .backends
            .task
            .task
            .get_task(&task.id)
            .unwrap()
            .is_some(),
        "get_task succeeds after a Fail that carried notes/"
    );
    let notes = owner
        .backends
        .task
        .artifact
        .get_task_artifact(&task.id, "notes/")
        .unwrap()
        .expect("canonical notes");
    assert_eq!(notes.path, "notes");
    assert_eq!(notes.content, b"final");
}

fn jsonl_rows<T: serde::de::DeserializeOwned>(path: &Path) -> Vec<T> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// A plain write dies after appending and before publishing `task.yaml`,
/// leaving `.pending-write.yaml`. The next writer is a claim Evidence commit
/// that changes no status: it must roll the aborted rows back first, or it
/// reuses their IDs, keeps the aborted status event as the last one, and
/// leaves a bundle that no longer reads.
#[test]
fn coordinated_commit_rolls_back_a_leftover_pending_write_first() {
    if !isolated("coordinated_commit_rolls_back_a_leftover_pending_write_first") {
        return;
    }
    let root = TempDir::new().unwrap();
    let owner = Coordinated::open(root.path());
    let task = owner.create_task("pending write");
    let claim = owner.pull(&owner_request("pending")).claim.expect("claim");
    assert_eq!(owner.task_status(&task.id), TaskStatus::InProgress);
    let bundle = owner
        .registry
        .canonical_task_bundle_path(PARTITION_ID, &task.id)
        .unwrap();
    let events_path = bundle.join(TASK_EVENTS_FILE_NAME);
    let comments_path = bundle.join(TASK_COMMENTS_FILE_NAME);
    let description_path = bundle.join(TASK_DESCRIPTION_FILE_NAME);

    // Steps 1 and 2 of the bundle write protocol, then a crash: the pending
    // record holds the pre-image, the rows and document rewrite are applied,
    // and the envelope is never published.
    let documents: BTreeMap<String, String> = [
        TASK_DESCRIPTION_FILE_NAME,
        TASK_ACCEPTANCE_FILE_NAME,
        TASK_PLAN_FILE_NAME,
        TASK_EXECUTION_SUMMARY_FILE_NAME,
    ]
    .into_iter()
    .map(|name| {
        let body = std::fs::read_to_string(bundle.join(name)).unwrap();
        (name.to_string(), body)
    })
    .collect();
    let pending = serde_json::json!({
        "schema_version": 1,
        "events_len": std::fs::metadata(&events_path).unwrap().len(),
        "comments_len": std::fs::metadata(&comments_path).map_or(0, |m| m.len()),
        "envelope_sha256": format!(
            "{:x}",
            Sha256::digest(std::fs::read(bundle.join(TASK_ENVELOPE_FILE_NAME)).unwrap())
        ),
        "documents": documents,
    });
    std::fs::write(
        bundle.join(".pending-write.yaml"),
        serde_yaml::to_string(&pending).unwrap(),
    )
    .unwrap();
    let events: Vec<TaskEventRowV2> = jsonl_rows(&events_path);
    let comments: Vec<TaskCommentRowV2> = if comments_path.exists() {
        jsonl_rows(&comments_path)
    } else {
        Vec::new()
    };
    let aborted_event = TaskEventRowV2 {
        schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
        event_id: format!("EV-{:04}", events.len() + 1),
        at: Utc::now(),
        by: "aborted-writer".into(),
        event_type: "status_changed".into(),
        note: None,
        from_status: Some(TaskStatus::InProgress),
        to_status: Some(TaskStatus::Done),
    };
    let aborted_comment = TaskCommentRowV2 {
        schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
        comment_id: format!("C-{:04}", comments.len() + 1),
        at: Utc::now(),
        by: "aborted-writer".into(),
        body: "aborted".into(),
    };
    let append = |path: &Path, row: String| {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(file, "{row}").unwrap();
    };
    append(&events_path, serde_json::to_string(&aborted_event).unwrap());
    append(
        &comments_path,
        serde_json::to_string(&aborted_comment).unwrap(),
    );
    std::fs::write(&description_path, "aborted description").unwrap();

    let unbound = ClaimInvocation::trusted_worker(
        claim.task_id.clone(),
        claim.claim_id.clone(),
        claim.executed_on.machine_id.clone(),
        None,
    );
    owner
        .backends
        .commit_boundary
        .mutate_execution_claim(
            Some(&unbound),
            "evidence-after-abort",
            &ClaimMutation::Evidence(ClaimEvidence {
                comment: Some("kept".into()),
                ..Default::default()
            }),
        )
        .expect("evidence commit");

    let read = owner
        .backends
        .task
        .task
        .get_task(&task.id)
        .expect("the bundle reads after the coordinated commit")
        .unwrap();
    assert_eq!(read.status, TaskStatus::InProgress);
    assert_eq!(read.description, task.description);
    assert!(
        !bundle.join(".pending-write.yaml").exists(),
        "the leftover pending write is settled"
    );
    let events: Vec<TaskEventRowV2> = jsonl_rows(&events_path);
    let comments: Vec<TaskCommentRowV2> = jsonl_rows(&comments_path);
    assert!(
        events.iter().all(|event| event.by != "aborted-writer")
            && comments
                .iter()
                .all(|comment| comment.by != "aborted-writer"),
        "no aborted row survives: {events:?} {comments:?}"
    );
    assert!(comments.iter().any(|comment| comment.body == "kept"));
    let event_ids: HashSet<_> = events.iter().map(|event| &event.event_id).collect();
    let comment_ids: HashSet<_> = comments.iter().map(|comment| &comment.comment_id).collect();
    assert_eq!(event_ids.len(), events.len(), "event IDs are unique");
    assert_eq!(comment_ids.len(), comments.len(), "comment IDs are unique");
}

fn review_report(verdict: &str) -> TaskArtifact {
    review_report_with(verdict, r#"{"command":"make ci-fast","outcome":"passed"}"#)
}

fn review_report_with(verdict: &str, validation: &str) -> TaskArtifact {
    TaskArtifact {
        path: REVIEW_REPORT_ARTIFACT.to_string(),
        content: format!(
            r#"{{"schema_version":1,"attempt_id":"rvw-1","verdict":"{verdict}","summary":"Checked.","validation":[{validation}],"escalation":"decide"}}"#
        )
        .into_bytes(),
        media_type: "application/json".to_string(),
        created_by: None,
    }
}

/// [ORB-14370] A claimed reviewer's live report put reaches the owner as a
/// worker update, while it can still correct the report: it is held to the
/// record-id contract and its refusal names the record. The evidence a claim
/// settles after the reviewer stopped is retained without that check (see
/// `racing_report_writers_each_retain_their_revision`).
#[test]
fn a_claimed_reviewers_live_report_put_is_held_to_record_ids() {
    if !isolated("a_claimed_reviewers_live_report_put_is_held_to_record_ids") {
        return;
    }
    let root = TempDir::new().unwrap();
    let owner = Coordinated::open(root.path());
    let task = owner.create_task("claimed review report");
    let request = owner_request("report");
    let claim = owner.pull(&request).claim.expect("claim");
    let unbound = ClaimInvocation::trusted_worker(
        claim.task_id.clone(),
        claim.claim_id.clone(),
        claim.executed_on.machine_id.clone(),
        None,
    );
    let worker = ClaimInvocation::trusted_worker(
        claim.task_id.clone(),
        claim.claim_id.clone(),
        claim.executed_on.machine_id.clone(),
        Some(leaf_run(&claim)),
    );
    let mutate = |auth: &ClaimInvocation, id: &str, mutation: &ClaimMutation| {
        owner
            .backends
            .commit_boundary
            .mutate_execution_claim(Some(auth), id, mutation)
    };
    mutate(
        &unbound,
        "bind",
        &ClaimMutation::Bind {
            run: leaf_run(&claim),
            ship: request.ship,
        },
    )
    .expect("bind");
    let put = |id: &str, artifact: TaskArtifact| {
        mutate(
            &worker,
            id,
            &ClaimMutation::Update(ClaimWorkerUpdate {
                evidence: ClaimEvidence {
                    artifacts: vec![artifact],
                    ..Default::default()
                },
                ..Default::default()
            }),
        )
    };

    let refused = put(
        "update-without-id",
        review_report_with(
            "incomplete",
            r#"{"command":"make ci-fast","outcome":"not_run","role":"required"}"#,
        ),
    )
    .expect_err("a live required record without an id");
    let message = invalid_input(refused);
    assert!(
        message.contains("`make ci-fast` -> `\"id\": \"V1\"`"),
        "the refusal names the record and an id to give it: {message}"
    );
    assert!(
        owner
            .backends
            .task
            .artifact
            .get_task_artifact(&task.id, REVIEW_REPORT_ARTIFACT)
            .unwrap()
            .is_none(),
        "a refused put stores no report"
    );

    put(
        "update-with-id",
        review_report_with(
            "incomplete",
            r#"{"id":"V1","command":"make ci-fast","outcome":"not_run","role":"required"}"#,
        ),
    )
    .expect("the corrected report");
    let dropped = put(
        "update-dropping-id",
        review_report_with(
            "accept",
            r#"{"id":"V2","command":"cargo test","outcome":"passed","role":"required"}"#,
        ),
    )
    .expect_err("a live revision dropping V1");
    let message = invalid_input(dropped);
    assert!(
        message.contains("required validation record `V1` (`make ci-fast`)"),
        "the refusal names the dropped record: {message}"
    );
    put(
        "update-carrying-id",
        review_report_with(
            "accept",
            r#"{"id":"V1","command":"TMPDIR=.orbit/tmp make ci-fast","outcome":"passed","role":"required"}"#,
        ),
    )
    .expect("the revision carrying V1 forward");
    let history = owner
        .backends
        .task
        .artifact
        .get_task_artifact(&task.id, REVIEW_REPORT_HISTORY_ARTIFACT)
        .unwrap()
        .expect("history");
    assert_eq!(
        ReviewReportHistory::parse(&history.content)
            .unwrap()
            .for_attempt("rvw-1")
            .count(),
        2,
        "only the accepted revisions are retained"
    );
}

/// Two compositions commit a claimed reviewer's report revisions at the same
/// instant (a retry racing its replacement): the commit boundary serializes
/// them, the report history retains both, and no writer may supply the
/// history itself. Settled evidence arrives after the reviewer stopped, so
/// its id-less records are retained rather than refused [ORB-14370].
#[test]
fn racing_report_writers_each_retain_their_revision() {
    if !isolated("racing_report_writers_each_retain_their_revision") {
        return;
    }
    let root = TempDir::new().unwrap();
    let owner = Coordinated::open(root.path());
    let task = owner.create_task("review report");
    let claim = owner.pull(&owner_request("report")).claim.expect("claim");
    let worker = ClaimInvocation::trusted_worker(
        claim.task_id.clone(),
        claim.claim_id.clone(),
        claim.executed_on.machine_id.clone(),
        None,
    );
    let other = Coordinated::open(root.path());
    let put = |store: &Coordinated, artifact: TaskArtifact| {
        store.backends.task.artifact.upsert_task_artifacts(
            &task.id,
            TaskArtifactUpdateParams {
                origin: None,
                actor: "codex".to_string(),
                owner_run_id: None,
                writer: None,
                upsert_artifacts: vec![artifact],
            },
        )
    };
    let evidence = |store: &Coordinated, id: &str, artifact: TaskArtifact| {
        store.backends.commit_boundary.mutate_execution_claim(
            Some(&worker),
            id,
            &ClaimMutation::Evidence(ClaimEvidence {
                artifacts: vec![artifact],
                ..Default::default()
            }),
        )
    };

    let barrier = Barrier::new(2);
    std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            barrier.wait();
            evidence(&owner, "evidence-first", review_report("incomplete"))
        });
        let second = scope.spawn(|| {
            barrier.wait();
            evidence(&other, "evidence-second", review_report("accept"))
        });
        first.join().unwrap().expect("the first revision");
        second.join().unwrap().expect("the second revision");
    });

    let history = owner
        .backends
        .task
        .artifact
        .get_task_artifact(&task.id, REVIEW_REPORT_HISTORY_ARTIFACT)
        .unwrap()
        .expect("the store wrote the report history");
    let history = ReviewReportHistory::parse(&history.content).unwrap();
    let verdicts = history
        .for_attempt("rvw-1")
        .map(|revision| revision.verdict)
        .collect::<Vec<_>>();
    assert_eq!(
        verdicts.len(),
        2,
        "both revisions are retained: {verdicts:?}"
    );
    assert!(verdicts.contains(&ReviewVerdict::Incomplete));
    assert!(verdicts.contains(&ReviewVerdict::Accept));

    let forged = text_artifact(REVIEW_REPORT_HISTORY_ARTIFACT, "{}");
    assert!(invalid_input(put(&owner, forged.clone()).unwrap_err()).contains("reserved"));
    assert!(
        invalid_input(evidence(&owner, "evidence-history", forged).unwrap_err())
            .contains("reserved")
    );
}
