// Identity allocation and coordinated admission through the store's public composition: task ids are monotonic, never wrapped past the ceiling and exhausted cleanly (including an imported maximum); job-run ids are never reminted after archive/delete; local pull admission holds one slot per request under the drain's live ceiling; the commit boundary admits at most one claim per request/task under concurrent pulls; a handoff is authorized only by its admission policy over unchanged evidence; settled claims' receipts are pruned past retention while claim/lifecycle rows stay (pruned replays are refused); claimed evidence stores canonical paths and racing report revisions are each retained; typed mutation receipts replay (text receipts still do); a pull is granted only listed, executor-runnable work (census rows land with receipts and stay live while claiming); a hard path lease defers and names itself, an advisory one holds nothing. Every test re-runs itself in a child with inherited Orbit authority cleared, a disposable `HOME`, a bounded wait.

#[path = "allocation_admission/claim_reads.rs"]
mod claim_reads;
#[path = "allocation_admission/claim_retention.rs"]
mod claim_retention;
#[path = "allocation_admission/dependencies.rs"]
mod dependencies;
#[path = "allocation_admission/handoff_compatibility.rs"]
mod handoff_compatibility;
#[path = "allocation_admission/known_red.rs"]
mod known_red;
#[path = "allocation_admission/mutation_receipts.rs"]
mod mutation_receipts;
#[path = "allocation_admission/ordering.rs"]
mod ordering;
#[path = "allocation_admission/path_leases.rs"]
mod path_leases;
#[path = "allocation_admission/reservation_grants.rs"]
mod reservation_grants;
#[path = "allocation_admission/task_index.rs"]
mod task_index;
#[path = "allocation_admission/work_fit.rs"]
mod work_fit;
/// How long one isolated test may run before it is killed and fails.
const CHILD_DEADLINE: Duration = orbit_common::test_env::CHILD_TEST_DEADLINE;
/// Run `test` alone in a child with authority cleared and a disposable `HOME`; `true` inside the child.
fn allocation_admission_isolated(test: &str) -> bool {
    const MARKER: &str = "ORBIT_TEST_STORE_ALLOCATION_CHILD";
    if std::env::var(MARKER).as_deref() == Ok(test) {
        return true;
    }
    let home = TempDir::new().unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    command
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(MARKER, test)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path());
    let output = process::run_bounded_capped(&mut command, CHILD_DEADLINE, 64 * 1024)
        .expect("run isolated allocation fixture");
    test_env::assert_child_test_passed(test, output.status, output.stdout, output.stderr);
    false
}
// Keep each test's exact-name child and deadline; only its setup guard is shared.
macro_rules! allocation_test {
    ($name:ident, $body:block) => {
        #[test]
        fn $name() {
            if allocation_admission_isolated(stringify!($name)) $body
        }
    };
}
type ClaimResult = Result<ClaimMutationResult, OrbitError>;
type JobRun = orbit_types::workflow::JobRun;
fn registry_at(root: &Path) -> TaskRegistryStore {
    TaskRegistryStore::open(&task_registry_path(root)).unwrap()
}
fn export_all(
    registry: &TaskRegistryStore,
    workspace: &str,
    dest: &Path,
) -> Result<orbit_store::workflow::task::ExportOutcome, OrbitError> {
    export_tasks(
        registry,
        workspace,
        ExportSelection::All,
        dest,
        Utc.with_ymd_and_hms(2026, 10, 3, 12, 0, 0).unwrap(),
    )
}
fn json_artifact(path: String, content: Vec<u8>) -> TaskArtifact {
    TaskArtifact {
        path,
        content,
        media_type: "application/json".into(),
        created_by: None,
    }
}
/// One pull-admission fixture: a store, its drain parent and destination.
struct PullFixture {
    _root: TempDir,
    jobs: Arc<dyn JobRunStoreBackend>,
    parent: JobRun,
    destination: PullDestination,
}
impl PullFixture {
    fn new(job: &str, limits: Option<(u32, u32)>) -> Self {
        let root = TempDir::new().unwrap();
        let jobs =
            workspace_job_run_store(Store::open(&root.path().join("pull.db")).unwrap(), "ws");
        let parent = jobs.insert_job_run(job, 1, Utc::now(), None, None).unwrap();
        let (run_id, job_id) = (parent.run_id.clone(), parent.job_id.clone());
        let mut state = PipelineState::new(run_id, job_id, serde_json::json!({}));
        if let Some((retuned, submitted)) = limits {
            assert!(state.set_drain_worker_limit(retuned, submitted, "cli".into(), None, None));
        }
        jobs.write_run_state(&parent.run_id, &state).unwrap();
        let destination = PullDestination {
            owner_machine_id: "owner".into(),
            owner_workspace_id: "ws".into(),
            selector: "owner/ws".into(),
            execution_machine_id: "owner".into(),
        };
        Self {
            _root: root,
            destination,
            jobs,
            parent,
        }
    }
    fn admit(&self, request_id: &str, ceiling: usize) -> bool {
        let request = pull_request(&self.parent.run_id, request_id);
        self.jobs
            .allocate_pull_request(&self.destination, &request, ceiling)
            .unwrap()
            .is_some()
    }
    fn occupancy(&self) -> orbit_store::contracts::DrainLeafOccupancy {
        self.jobs.drain_leaf_occupancy().unwrap()
    }
    fn mark_retrying(&self, run_id: &str) {
        let store = Store::open(&self._root.path().join("pull.db")).unwrap();
        let run_id = run_id.to_string();
        store
            .with_transaction(|tx| {
                let changed = tx.connection().execute(
                    "UPDATE job_runs SET state = 'retrying' WHERE workspace_id = ?1 AND run_id = ?2",
                    ["ws", run_id.as_str()],
                ).unwrap();
                assert_eq!(changed, 1, "retrying fixture run exists");
                Ok(())
            })
            .unwrap();
    }
    fn leaf(&self, job: &str, task: &str) -> JobRun {
        self.jobs
            .insert_job_run(
                job,
                1,
                Utc::now(),
                Some(serde_json::json!({ "task_ids": [task] })),
                None,
            )
            .unwrap()
    }
}
fn is_exhausted<T: std::fmt::Debug>(result: Result<T, OrbitError>) -> bool {
    matches!(result, Err(OrbitError::Store(ref message)) if message.contains("exhausted"))
}
// --------------------------------------------------------------------------- Task id allocation
// ---------------------------------------------------------------------------
const PARTITION_ID: &str = "orbit-test-123456";
fn bind(registry: &TaskRegistryStore, root: &Path, partition_id: &str) -> WorkspaceCheckoutBinding {
    bind_workspace(registry, root, partition_id, None)
}
fn bind_with_fingerprint(
    registry: &TaskRegistryStore,
    root: &Path,
    partition_id: &str,
    fingerprint: &str,
) -> WorkspaceCheckoutBinding {
    bind_workspace(registry, root, partition_id, Some(fingerprint))
}
fn bind_workspace(
    registry: &TaskRegistryStore,
    root: &Path,
    partition_id: &str,
    fingerprint: Option<&str>,
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
            repo_fingerprint: fingerprint.map(str::to_string),
        })
        .expect("bind workspace")
}
allocation_test!(task_ids_are_monotonic_across_workspaces_and_batches, {
    let root = TempDir::new().unwrap();
    let registry = registry_at(root.path());
    let first = bind(&registry, root.path(), "first-aaaaaa");
    let second = bind(&registry, root.path(), "second-bbbbbb");
    let next_id = |partition: &str| registry.allocate_task_id(partition).unwrap();
    let mut minted = vec![next_id(&first.partition_id), next_id(&second.partition_id)];
    minted.extend(registry.allocate_task_ids(&first.partition_id, 3).unwrap());
    minted.push(next_id(&second.partition_id));
    let want = [
        "ORB-00000",
        "ORB-00001",
        "ORB-00002",
        "ORB-00003",
        "ORB-00004",
        "ORB-00005",
    ];
    assert_eq!(
        minted, want,
        "one host-wide counter serves every workspace and batch, in order"
    );
    // Reopening the registry (a later process) continues from the counter.
    drop(registry);
    let registry = registry_at(root.path());
    let got = registry.allocate_task_id(&first.partition_id).unwrap();
    assert_eq!(got, "ORB-00006");
    // Seeding never lowers the counter.
    assert!(registry.seed_allocator_start(3).is_err());
    let got = registry.allocate_task_id(&first.partition_id).unwrap();
    assert_eq!(got, "ORB-00007");
});
// The last id is minted once; later allocations stay exhausted, never wrap. A batch crossing the ceiling is refused whole.
allocation_test!(
    task_id_allocation_exhausts_at_the_ceiling_without_wrapping,
    {
        let root = TempDir::new().unwrap();
        let registry = registry_at(root.path());
        let workspace = bind(&registry, root.path(), PARTITION_ID);
        registry.seed_allocator_start(ORB_TASK_ID_MAX - 1).unwrap();
        assert!(
            is_exhausted(registry.allocate_task_ids(&workspace.partition_id, 3)),
            "a reservation past the ceiling is refused whole"
        );
        let got = registry.allocator_next_number().unwrap();
        assert_eq!(got, ORB_TASK_ID_MAX - 1);
        let got = registry.allocate_task_id(&workspace.partition_id).unwrap();
        assert_eq!(got, format!("ORB-{}", ORB_TASK_ID_MAX - 1));
        let got = registry.allocate_task_id(&workspace.partition_id).unwrap();
        assert_eq!(got, format!("ORB-{ORB_TASK_ID_MAX}"));
        for _ in 0..2 {
            assert!(
                is_exhausted(registry.allocate_task_id(&workspace.partition_id)),
                "the allocator must stay exhausted rather than wrap"
            );
        }
        assert!(is_exhausted(registry.allocator_next_number()));
        assert!(is_exhausted(registry.seed_allocator_start(ORB_TASK_ID_MAX)));
        drop(registry);
        let registry = registry_at(root.path());
        assert!(
            is_exhausted(registry.allocate_task_id(&workspace.partition_id)),
            "exhaustion survives a reopen"
        );
    }
);
// Importing a task holding the maximum id leaves the target allocator exhausted, not wrapped.
allocation_test!(
    importing_the_maximum_task_id_exhausts_the_target_allocator,
    {
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
        let entries = || std::fs::read_dir(source_root.path()).unwrap().count();
        let entries_before = entries();
        export_all(&source.registry, PARTITION_ID, &destination_dir)
            .expect_err("publishing an archive over a directory must fail");
        assert!(destination_dir.is_dir());
        assert_eq!(std::fs::read(&archive).unwrap(), b"previous backup");
        assert_eq!(
            entries(),
            entries_before,
            "failed publication must remove its staging file"
        );
        export_all(&source.registry, PARTITION_ID, &archive).expect("export");
        assert_eq!(
            entries(),
            entries_before,
            "successful replacement must leave no staging file"
        );
        let target = registry_at(target_root.path());
        let outcome =
            import_tasks(&target, &archive, None, ImportConflictPolicy::Fail).expect("import");
        assert_eq!(outcome.tasks[0].final_id, max_id);
        assert!(target.find_task_binding(&max_id).unwrap().is_some());
        assert!(is_exhausted(target.allocator_next_number()));
        let ok = is_exhausted(target.allocate_task_id(PARTITION_ID));
        assert!(
            ok,
            "the next id after an imported maximum must be refused, not wrapped"
        );
    }
);
// Publication restore advances the allocator only past local ids (`max(previous, max_local + 1)`); foreign-only preserves it.
allocation_test!(
    publication_restore_advances_allocator_only_for_local_task_numbers,
    {
        const FINGERPRINT: &str = "ssh://source.test/orbit.git";
        let foreign_root = TempDir::new().unwrap();
        let foreign_registry = registry_at(foreign_root.path());
        foreign_registry.set_task_prefix("DANI").unwrap();
        let foreign =
            Coordinated::open_with_fingerprint(foreign_root.path(), "ws_foreign", FINGERPRINT);
        foreign.registry.seed_allocator_start(90000).unwrap();
        assert_eq!(foreign.create_task("foreign dani task").id, "DANI-90000");
        let dani_archive = foreign.root().join("dani.tar.zst");
        export_all(&foreign.registry, "ws_foreign", &dani_archive).expect("export dani");
        let source_root = TempDir::new().unwrap();
        let source =
            Coordinated::open_with_fingerprint(source_root.path(), "ws_source", FINGERPRINT);
        source.registry.seed_allocator_start(5).unwrap();
        assert_eq!(source.create_task("orb task 5").id, "ORB-00005");
        let imported = import_tasks(
            &source.registry,
            &dani_archive,
            Some("ws_source"),
            ImportConflictPolicy::Fail,
        )
        .expect("import dani into source");
        assert_eq!(imported.tasks[0].final_id, "DANI-90000");
        // Keep separate destinations and exact allocator/id checks for each row. Foreign-only restoration additionally checks the actual restored identity.
        let publication_root = TempDir::new().unwrap();
        let pub_root = publication_root.path();
        for (workspace, publisher, publication_id, authority, rows) in [
            (
                "ws_source",
                &source,
                "pub_mixed",
                "hm_owner",
                &[("below", 2, 6), ("above", 10, 10)][..],
            ),
            (
                "ws_foreign",
                &foreign,
                "pub_foreign_only",
                "hm_foreign",
                &[("foreign only", 4, 4)][..],
            ),
        ] {
            let bare = pub_root.join(format!("{publication_id}.git"));
            let remote = bare.to_str().unwrap();
            let cache = pub_root.join(format!("{publication_id}-cache"));
            std::fs::create_dir_all(&cache).unwrap();
            let mut git = crate::git_test_support::command();
            git.args(["init", "--bare", "--quiet", "-b", "main", remote]);
            assert!(
                git.status().unwrap().success(),
                "{publication_id}: initialize publication"
            );
            publish_task_snapshot(
                &publisher.registry,
                PublicationPublishRequest {
                    workspace_id: workspace.into(),
                    task_workspace_id: workspace.into(),
                    source_repository_fingerprint: FINGERPRINT.into(),
                    publication_id: publication_id.into(),
                    authority_machine_id: authority.into(),
                    local_machine_id: authority.into(),
                    caller_role: PublicationCallerRole::Owner,
                    publication_remote: remote.into(),
                    publication_branch: "main".into(),
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
            .unwrap_or_else(|error| panic!("{publication_id}: publish: {error}"));
            for &(case, previous, expected) in rows {
                let target_root = TempDir::new().unwrap();
                let target = registry_at(target_root.path());
                bind_with_fingerprint(&target, target_root.path(), "ws_target", FINGERPRINT);
                target.seed_allocator_start(previous).unwrap();
                let got = target.allocator_next_number().unwrap();
                assert_eq!(got, previous, "{case}: before restore");
                let cache = target_root.path().join("restore_cache");
                std::fs::create_dir_all(&cache).unwrap();
                let outcome = restore_publication(
                    &target,
                    PublicationRestoreRequest {
                        task_workspace_id: "ws_target".into(),
                        publication: PublicationInspectRequest {
                            workspace_id: workspace.into(),
                            source_repository_fingerprint: FINGERPRINT.into(),
                            publication_id: publication_id.into(),
                            authority_machine_id: authority.into(),
                            publication_remote: remote.into(),
                            publication_branch: "main".into(),
                            cache_dir: cache,
                            commit: None,
                        },
                        mode: PublicationRestoreMode::EmptyDestination,
                    },
                )
                .unwrap_or_else(|error| panic!("{case}: restore publication: {error}"));
                if workspace == "ws_foreign" {
                    assert_eq!(outcome.restored_task_ids, vec!["DANI-90000"], "{case}");
                } else {
                    assert_eq!(outcome.restored_task_ids.len(), 2, "{case}");
                }
                let got = target.allocator_next_number().unwrap();
                assert_eq!(
                    got, expected,
                    "{case}: restore advances only past local ids, never lowers"
                );
                let got = target.allocate_task_id("ws_target").unwrap();
                assert_eq!(
                    got,
                    format!("ORB-{expected:05}"),
                    "{case}: next minted task"
                );
            }
        }
    }
);
// Job-run ids and pull occupancy.
fn finish(jobs: &dyn JobRunStoreBackend, run_id: &str) {
    let at = Utc::now();
    let running = jobs.mark_job_run_running(run_id, at, 42).unwrap();
    assert!(running.owns_execution());
    assert!(
        jobs.finalize_job_run(run_id, JobRunState::Success, at, Some(1))
            .unwrap()
    );
}
// [ORB-13599] Archive/delete remove a run's row; its id stays reserved and is never reminted in the same minute.
allocation_test!(removed_job_run_ids_are_never_reallocated, {
    let root = TempDir::new().unwrap();
    let db = root.path().join("orbit.db");
    let open = || workspace_job_run_store(Store::open(&db).unwrap(), "ws_a");
    let mut minted = Vec::new();
    for delete in [false, true] {
        // The id stem is the wall-clock minute; retry the cycle if it crossed a minute boundary, which
        // would make the comparison vacuous.
        let (removed, next) = (0..3)
            .find_map(|_| {
                let jobs = open();
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
                let reopened = open();
                let next = reopened
                    .insert_job_run("job-a", 1, Utc::now(), None, None)
                    .unwrap();
                (removed.run_id.rsplit_once('-').unwrap().0
                    == next.run_id.rsplit_once('-').unwrap().0)
                    .then_some((removed.run_id, next.run_id))
            })
            .expect("two runs minted within one minute");
        assert_ne!(next, removed, "a removed run's id was minted again");
        minted.extend([removed, next]);
    }
    let unique = minted.iter().collect::<HashSet<_>>();
    let got = unique.len();
    assert_eq!(got, minted.len(), "every id minted once: {minted:?}");
});
fn admission_request(
    request_id: &str,
    caller_version: &str,
    run_id: &str,
    job_name: &str,
    machine_name: Option<&str>,
    ship: AdmissionShipContract,
) -> AdmissionRequest {
    AdmissionRequest {
        labels: Vec::new(),
        request_id: request_id.into(),
        caller_version: caller_version.into(),
        caller_schema: orbit_store::contracts::DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
        caller_fingerprint: None,
        caller_before_pr: false,
        review_gate: false,
        run_context: AdmissionRunContext {
            run_id: run_id.into(),
            job_name: job_name.into(),
            machine_name: machine_name.map(str::to_string),
        },
        ship,
        crews: None,
        os: None,
        browser_kit: false,
        admissible: None,
    }
}
fn ship_contract(mode: &str, branch: &str) -> AdmissionShipContract {
    AdmissionShipContract {
        mode: mode.into(),
        base_branch: branch.into(),
        landing_branch: branch.into(),
        before_pr: false,
        before_landing: false,
        completion: "review".into(),
        authorization_reference: None,
        review: None,
    }
}
fn pull_request(run_id: &str, request_id: &str) -> AdmissionRequest {
    admission_request(
        request_id,
        "1",
        run_id,
        "workspace_auto_pipeline",
        None,
        ship_contract("local", "main"),
    )
}
// Twelve connections race for pull admission under a ceiling of three: exactly three are admitted, one slot each.
allocation_test!(concurrent_pull_admission_obeys_the_shared_ceiling, {
    const RACERS: usize = 12;
    const CEILING: usize = 3;
    let fixture = PullFixture::new("workspace_auto_pipeline", None);
    let db = fixture._root.path().join("pull.db");
    let (jobs, parent, destination) = (
        fixture.jobs.clone(),
        fixture.parent.clone(),
        fixture.destination.clone(),
    );
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
    assert_eq!(fixture.occupancy().occupied, CEILING);
    let inherited = |run_id: &str| jobs.drain_leaf_occupancy_for_run(run_id).unwrap().inherited;
    assert_eq!(inherited(&parent.run_id), Some(0));
    let replacement = jobs
        .insert_job_run("workspace_auto_pipeline", 1, Utc::now(), None, None)
        .unwrap();
    assert_eq!(inherited(&replacement.run_id), Some(CEILING));
    let ok = !fixture.admit("late", CEILING);
    assert!(ok, "a full ceiling admits nothing more");
});
// The drain's live (retuned) limit is the only ceiling on pulled leaves, past any per-leaf limit.
allocation_test!(pull_admission_is_bound_only_by_the_drains_live_limit, {
    const SUBMITTED: usize = 3;
    const RETUNED: u32 = 24;
    let fixture = PullFixture::new("workspace_pull_pipeline", Some((RETUNED, SUBMITTED as u32)));
    let admitted = (0..RETUNED + 6)
        .filter(|index| fixture.admit(&format!("request-{index}"), SUBMITTED))
        .count();
    assert_eq!(admitted, RETUNED as usize, "the retuned limit governs");
    let occupancy = fixture.occupancy();
    assert_eq!(occupancy.occupied, RETUNED as usize);
    assert_eq!(
        occupancy.per_pipeline.get("task_claimed_local_pipeline"),
        Some(&(RETUNED as usize)),
        "one leaf definition holds every slot, with no ceiling of its own"
    );
});
// A retrying claimed leaf owns one drain slot; terminal leaf history releases its slot.
allocation_test!(
    retrying_claimed_leaf_occupies_capacity_and_terminal_history_does_not,
    {
        const CEILING: usize = 1;
        let fixture = PullFixture::new(
            "workspace_pull_pipeline",
            Some((CEILING as u32, CEILING as u32)),
        );
        let retrying = fixture.leaf("task_claimed_local_pipeline", "ORB-TEST");
        let terminal = fixture.leaf("task_claimed_pr_pipeline", "ORB-OLD");
        finish(fixture.jobs.as_ref(), &terminal.run_id);
        fixture.mark_retrying(&retrying.run_id);
        let occupancy = fixture.occupancy();
        assert_eq!(
            occupancy.occupied, CEILING,
            "the retrying leaf occupies capacity and the terminal leaf is excluded"
        );
        let got = occupancy.per_pipeline.get("task_claimed_local_pipeline");
        assert_eq!(
            got,
            Some(&1),
            "retrying claimed leaves count toward their pipeline"
        );
        let got = occupancy.per_pipeline.get("task_claimed_pr_pipeline");
        assert_eq!(got, None, "terminal leaf history does not count");
        let ok = !fixture.admit("retrying-leaf-at-capacity", CEILING);
        assert!(
            ok,
            "a retrying claimed leaf prevents admission at the ceiling"
        );
    }
);
/// Record a schema-valid before-landing admission result, as the leaf's
/// `landing_review_gate_admit` checkpoint does.
fn checkpoint_landing_review(jobs: &dyn JobRunStoreBackend, run_id: &str, applies: bool) {
    let mut state = PipelineState::new(
        run_id.into(),
        "task_claimed_pr_pipeline".into(),
        serde_json::json!({}),
    );
    let output = if applies {
        let admitted: orbit_types::workflow::ReviewGateAdmitted =
            serde_json::from_value(serde_json::json!({
                "decision": "admitted", "first_task_id": "ORB-REVIEW",
                "attempt_id": "attempt", "attempt_index": 1, "lineage_key": "lineage",
                "timing": "before-landing", "timing_source": "workspace",
                "reviewer": {"crew": "reviewers", "crew_source": "workspace",
                    "provider": "codex", "model": "review-model"},
                "base_sha": "0".repeat(40), "base_tree": "0".repeat(40),
                "head_sha": "1".repeat(40), "head_tree": "1".repeat(40),
                "implementation_commit_count": 1, "task_meaning_digest": "digest",
                "task_selectors": {}, "manifest_artifact": "manifest.json",
                "report_artifact": "report.md",
                "budget": orbit_types::workflow::ReviewBudget::default(),
                "remaining": {"seconds": 1800}, "started_at": Utc::now(),
            }))
            .unwrap();
        let admitted =
            orbit_types::workflow::ReviewGateAdmissionOutput::Admitted(Box::new(admitted));
        serde_json::to_value(admitted).unwrap()
    } else {
        let na =
            orbit_types::workflow::ReviewGateNotApplicable::new("review_before_landing_off", None);
        serde_json::to_value(orbit_types::workflow::ReviewGateAdmissionOutput::NotApplicable(na))
            .unwrap()
    };
    state.record_pipeline_output("landing_review_gate_admit", output);
    jobs.write_run_state(run_id, &state).unwrap();
}
// [ORB-15194] A reviewing claimed leaf keeps its slot but admits one replacement, up to the ceiling of reviewing leaves.
allocation_test!(
    reviewing_claimed_leaves_admit_replacements_up_to_the_ceiling,
    {
        const CEILING: usize = 2;
        let fixture = PullFixture::new("workspace_pull_pipeline", None);
        let jobs = fixture.jobs.clone();
        let leaf = |task: &str| fixture.leaf("task_claimed_pr_pipeline", task).run_id;
        let mut requests = 0;
        let mut admit = || {
            requests += 1;
            fixture.admit(&format!("request-{requests}"), CEILING)
        };
        let implementing = leaf("ORB-IMPL");
        checkpoint_landing_review(jobs.as_ref(), &implementing, false);
        let reviewing = leaf("ORB-REVIEW");
        checkpoint_landing_review(jobs.as_ref(), &reviewing, true);
        let occupancy = fixture.occupancy();
        assert_eq!(
            (occupancy.occupied, occupancy.reviewing),
            (CEILING, 1),
            "the reviewing leaf still occupies its slot and is reported apart"
        );
        let ok = admit();
        assert!(
            ok,
            "a full drain admits a replacement beside its reviewing leaf"
        );
        assert!(!admit(), "only one replacement per reviewing leaf");
        // The other leaf reaches its review too: one more replacement.
        checkpoint_landing_review(jobs.as_ref(), &implementing, true);
        assert!(admit(), "each reviewing leaf admits its own replacement");
        assert!(!admit());
        // A reviewing leaf past the ceiling's worth frees nothing more.
        let third = leaf("ORB-REVIEW-3");
        checkpoint_landing_review(jobs.as_ref(), &third, true);
        let occupancy = fixture.occupancy();
        let got = (occupancy.occupied, occupancy.reviewing);
        assert_eq!(
            got,
            (CEILING + 3, 3),
            "every live leaf and admission still occupies a slot"
        );
        let ok = !admit();
        assert!(
            ok,
            "at most the ceiling of reviewing leaves admit replacements"
        );
    }
);
// --------------------------------------------------------------------------- Coordinated
// admission and handoff
// ---------------------------------------------------------------------------
allocation_test!(reserved_key_publishes_a_new_bundle_before_replaying, {
    let owner = Coordinated::fixture();
    for (key, digest) in [("automation", None), ("desktop", Some("payload-digest"))] {
        let mut params = task_params("Recover an interrupted keyed creation", &[]);
        params.actor = "system".into();
        params.created_by = Some("system".into());
        params.description = String::new();
        params.acceptance_criteria = Vec::new();
        params.plan = String::new();
        params.status = TaskStatus::Proposed;
        params.priority = TaskPriority::Medium;
        params.complexity = None;
        params.task_type = TaskType::Chore;
        let id = owner.registry.allocate_task_id(PARTITION_ID).unwrap();
        let input_digest = digest
            .map(str::to_string)
            .unwrap_or_else(|| lower_hex(Sha256::digest(serde_json::to_vec(&params).unwrap())));
        // Crash injection: durable key admission succeeded, but bundle publication never ran.
        // Reservation alone is not a creation replay.
        let conn = rusqlite::Connection::open(task_registry_path(owner.root())).unwrap();
        conn.execute(
            "INSERT INTO task_action_keys VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![PARTITION_ID, key, id, input_digest],
        )
        .unwrap();
        assert!(owner.stored_task(&id).is_none());
        let tasks = &owner.backends.task.task;
        let create = || match digest {
            Some(digest) => tasks.create_desktop_task(params.clone(), key, digest),
            None => tasks.create_task_idempotent(params.clone(), key),
        };
        let (created, replayed) = create().expect("recover reserved key");
        assert_eq!(created.id, id, "recovery uses the reserved task identity");
        assert!(!replayed, "first bundle publication is a new creation");
        let (existing, replayed) = create().expect("replay published bundle");
        assert!(replayed, "a published bundle is a replay");
        assert_eq!(existing, created);
    }
    assert_eq!(owner.task_store().list_tasks().unwrap().len(), 2);
});
/// One owner composition over a root; a second open on the same root is a second process/restart.
struct Coordinated {
    registry: TaskRegistryStore,
    backends: CoordinatedWorkspaceBackends,
    orbit_dir: PathBuf,
    _root: Option<TempDir>,
}
impl Coordinated {
    fn fixture() -> Self {
        let root = TempDir::new().unwrap();
        let mut owner = Self::open(root.path());
        owner._root = Some(root);
        owner
    }
    fn open(root: &Path) -> Self {
        Self::open_bound(root, PARTITION_ID, None)
    }
    fn open_with_fingerprint(root: &Path, partition_id: &str, fingerprint: &str) -> Self {
        Self::open_bound(root, partition_id, Some(fingerprint))
    }
    fn open_bound(root: &Path, partition_id: &str, fingerprint: Option<&str>) -> Self {
        let registry = registry_at(root);
        let binding = bind_workspace(&registry, root, partition_id, fingerprint);
        let store = Store::open(&root.join("state.sqlite")).unwrap();
        let backends =
            workspace_coordinated_backends(registry.clone(), binding.partition_id, store)
                .expect("compose");
        Self {
            registry,
            backends,
            orbit_dir: binding.orbit_dir,
            _root: None,
        }
    }
    fn root(&self) -> &Path {
        self.orbit_dir.parent().unwrap().parent().unwrap()
    }
    fn reopen(&self) -> Self {
        Self::open(self.root())
    }
    fn task_store(&self) -> &Arc<dyn orbit_store::contracts::TaskStoreBackend> {
        &self.backends.task.task
    }
    fn artifact_store(&self) -> &Arc<dyn orbit_store::contracts::TaskArtifactStoreBackend> {
        &self.backends.task.artifact
    }
    fn task(&self, id: &str) -> orbit_types::task::Task {
        self.task_store().get_task(id).unwrap().unwrap()
    }
    fn artifacts(&self, id: &str) -> Vec<TaskArtifact> {
        self.artifact_store()
            .get_task_artifacts(id)
            .unwrap()
            .unwrap()
    }
    fn artifact(&self, id: &str, path: &str) -> Option<TaskArtifact> {
        self.artifact_store().get_task_artifact(id, path).unwrap()
    }
    fn mutate(&self, auth: &ClaimInvocation, id: &str, mutation: &ClaimMutation) -> ClaimResult {
        let boundary = &self.backends.commit_boundary;
        boundary.mutate_execution_claim(Some(auth), id, mutation)
    }
    fn put_evidence(
        &self,
        auth: &ClaimInvocation,
        id: &str,
        arts: Vec<TaskArtifact>,
    ) -> ClaimResult {
        let mutation = ClaimMutation::Evidence(claim_evidence(arts));
        self.mutate(auth, id, &mutation)
    }
    fn bind_claim(
        &self,
        auth: &ClaimInvocation,
        claim: &ExecutionClaim,
        ship: AdmissionShipContract,
    ) -> ClaimResult {
        let mutation = ClaimMutation::Bind {
            run: leaf_run(claim),
            ship,
        };
        self.mutate(auth, "bind", &mutation)
    }
    fn create_task(&self, title: &str) -> orbit_types::task::Task {
        self.create_task_in(title, &["src/lib.rs"])
    }
    fn create_task_in(&self, title: &str, selectors: &[&str]) -> orbit_types::task::Task {
        self.create_task_with_status(title, selectors, TaskStatus::Backlog, None)
    }
    fn create_task_with_status(
        &self,
        title: &str,
        selectors: &[&str],
        status: TaskStatus,
        parent_id: Option<String>,
    ) -> orbit_types::task::Task {
        let params = TaskCreateParams {
            parent_id,
            status,
            ..task_params(title, selectors)
        };
        self.task_store().create_task(params).expect("create task")
    }
    fn task_status(&self, id: &str) -> TaskStatus {
        self.task(id).status
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
        self.try_pull_with_ordering(request, &AdmissionOrdering::default())
    }
    fn try_pull_with_ordering(
        &self,
        request: &AdmissionRequest,
        ordering: &AdmissionOrdering,
    ) -> Result<AdmissionLookup, OrbitError> {
        self.try_pull_as("machine-a", request, ordering)
    }
    fn try_pull_as(
        &self,
        machine_id: &str,
        request: &AdmissionRequest,
        ordering: &AdmissionOrdering,
    ) -> Result<AdmissionLookup, OrbitError> {
        let location = ExecutionLocation {
            machine_id: machine_id.into(),
            machine_name: Some("display".into()),
        };
        self.backends.commit_boundary.admit_task(
            &AdmissionIdentity::trusted_remote(location),
            request,
            ordering,
            "test",
            self.orbit_dir.parent().unwrap(),
            &self.orbit_dir,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &|_, _, _| Ok(None),
        )
    }
    fn pull(&self, request: &AdmissionRequest) -> AdmissionReceipt {
        match self.try_pull(request).expect("pull") {
            AdmissionLookup::Found { receipt, .. } => *receipt,
            other => panic!("expected a receipt: {other:?}"),
        }
    }
    fn landing_starts(&self) -> Vec<LandingStartRequest> {
        self.task_store().landing_start_requests().unwrap()
    }
    fn stored_task(&self, id: &str) -> Option<orbit_types::task::Task> {
        self.task_store().get_task(id).unwrap()
    }
    fn bundle_path(&self, task_id: &str) -> PathBuf {
        self.registry
            .canonical_task_bundle_path(PARTITION_ID, task_id)
            .unwrap()
    }
    fn update_evidence(
        &self,
        auth: &ClaimInvocation,
        id: &str,
        arts: Vec<TaskArtifact>,
    ) -> ClaimResult {
        let mutation = ClaimMutation::Update(ClaimWorkerUpdate {
            evidence: claim_evidence(arts),
            ..Default::default()
        });
        self.mutate(auth, id, &mutation)
    }
    fn upsert_artifacts(
        &self,
        task_id: &str,
        artifacts: Vec<TaskArtifact>,
    ) -> Result<(), OrbitError> {
        let params = TaskArtifactUpdateParams {
            origin: None,
            actor: "codex".to_string(),
            owner_run_id: None,
            writer: None,
            upsert_artifacts: artifacts,
        };
        self.artifact_store().upsert_task_artifacts(task_id, params)
    }
    fn fail_claim(
        &self,
        auth: &ClaimInvocation,
        id: &str,
        summary: &str,
        artifacts: Vec<TaskArtifact>,
    ) -> ClaimResult {
        let mutation = ClaimMutation::Fail(ClaimEvidence {
            summary: Some(summary.to_string()),
            artifacts,
            ..Default::default()
        });
        self.mutate(auth, id, &mutation)
    }
}
fn task_params(title: &str, selectors: &[&str]) -> TaskCreateParams {
    TaskCreateParams {
        actor: "codex".into(),
        created_by: Some("codex".into()),
        acceptance_criteria: vec!["First criterion".into()],
        tags: Vec::new(),
        context_files: selectors.iter().map(|s| (*s).to_string()).collect(),
        complexity: Some(TaskComplexity::Medium),
        external_refs: Vec::new(),
        comments: Vec::new(),
        ..create_params(title, TaskStatus::Backlog)
    }
}
fn owner_request(id: &str) -> AdmissionRequest {
    admission_request(
        id,
        "test",
        "drain",
        "auto",
        Some("untrusted-label"),
        ship_contract("pr", "agent-main"),
    )
}
fn race_pulls(
    first: &Coordinated,
    a: AdmissionRequest,
    second: &Coordinated,
    b: AdmissionRequest,
) -> [AdmissionReceipt; 2] {
    let barrier = Barrier::new(2);
    std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            barrier.wait();
            first.pull(&a)
        });
        let b = scope.spawn(|| {
            barrier.wait();
            second.pull(&b)
        });
        [a.join().unwrap(), b.join().unwrap()]
    })
}
// Two compositions replay one request at once: one claim, one reservation, identical receipts.
allocation_test!(simultaneous_retries_of_one_request_create_one_claim, {
    let owner = Coordinated::fixture();
    owner.create_task("work");
    let first = owner.reopen();
    let second = owner.reopen();
    let [a, b] = race_pulls(
        &first,
        owner_request("same"),
        &second,
        owner_request("same"),
    );
    assert!(a.claim.is_some());
    assert_eq!(a, b, "a retry replays the original receipt");
    assert_eq!(owner.active_reservations().len(), 1);
    assert_eq!(owner.claims().len(), 1);
});
// Distinct concurrent requests over overlapping tasks: at most one claims, one reservation exists.
allocation_test!(
    distinct_concurrent_requests_never_claim_overlapping_tasks,
    {
        let owner = Coordinated::fixture();
        owner.create_task("one");
        owner.create_task("two");
        let other = owner.reopen();
        let receipts = race_pulls(&owner, owner_request("a"), &other, owner_request("b"));
        let got = receipts.iter().filter(|r| r.claim.is_some()).count();
        assert_eq!(
            got, 1,
            "overlapping footprints admit one claim: {receipts:?}"
        );
        assert_eq!(owner.active_reservations().len(), 1);
        assert_eq!(owner.claims().len(), 1);
    }
);
const OWNER_POLICY: &str = "workspace-config:workflow.distributed_completion";
const REPOSITORY: &str = "owner/repository";
const REVIEW_CREW: &str = "reviewer";
/// One way a handoff's review evidence is spoiled before acceptance.
type Tamper = fn(&mut Delivery);
/// An admitted, bound claim with owner-held validation logs and its worker handoff.
struct Delivery {
    owner: Coordinated,
    claim: ExecutionClaim,
    handoff: TaskHandoff,
    /// The owner's `workflow.required_validation_commands`.
    required: Vec<String>,
    /// The owner's own judgement of the handoff's known-red logs.
    known_red: Vec<KnownRedObservation>,
}
impl Delivery {
    fn admit(ship: AdmissionShipContract) -> Self {
        Self::admit_requiring(ship, &["src/lib.rs"], &["build", "test"])
    }
    /// Admit a claim whose owner requires `required`, with one passing log per command.
    fn admit_requiring(ship: AdmissionShipContract, selectors: &[&str], required: &[&str]) -> Self {
        let owner = Coordinated::fixture();
        owner.create_task_in("typed handoff", selectors);
        let mut request = owner_request("first");
        request.ship = ship.clone();
        request.review_gate = ship.before_pr;
        let claim = owner.pull(&request).claim.expect("claim");
        let unbound = claim_worker(&claim, false);
        owner.bind_claim(&unbound, &claim, ship).expect("bind");
        let handoff = handoff_with_logs(&owner, &claim, required);
        Self {
            owner,
            claim,
            handoff,
            required: required.iter().map(ToString::to_string).collect(),
            known_red: Vec::new(),
        }
    }
    fn assert_status(&self, status: TaskStatus) {
        let got = self.owner.task_status(&self.claim.task_id);
        assert_eq!(got, status, "{}", self.claim.task_id);
    }
    fn observation(&self, policy: Option<&str>) -> HandoffObservation {
        let review = self
            .handoff
            .review
            .evidence()
            .map(|evidence| HandoffReviewObservation {
                reviewed_base_sha: evidence.reviewed_base_sha.clone(),
                reviewed_base_is_ancestor: true,
                repository: REPOSITORY.into(),
            });
        HandoffObservation {
            footprint_widening: self.handoff.footprint_widening.clone(),
            candidate: self.handoff.candidate.clone(),
            required_commands: self.required.clone(),
            owner_completion_authority: policy.map(str::to_string),
            review,
            known_red: self.known_red.clone(),
            light_delivery: Vec::new(),
        }
    }
    fn worker(&self) -> ClaimInvocation {
        claim_worker(&self.claim, true)
    }
    fn operator(&self) -> ClaimInvocation {
        ClaimInvocation::trusted_operator(
            self.claim.task_id.clone(),
            self.claim.claim_id.clone(),
            "owner-operator".into(),
        )
    }
    fn accept(&self, policy: Option<&str>) -> ClaimResult {
        self.accept_observed(self.observation(policy))
    }
    fn accept_observed(&self, observation: HandoffObservation) -> ClaimResult {
        let worker = self.worker().with_handoff_observation(observation);
        let mutation = ClaimMutation::AcceptHandoff(self.handoff.clone());
        self.owner.mutate(&worker, "handoff", &mutation)
    }
    /// Persist `certificate` as claim evidence and carry it as the handoff's before-PR review.
    fn reviewed(&mut self, certificate: &ReviewCertificate) {
        let content = serde_json::to_vec(certificate).unwrap();
        let reference = HandoffArtifactRef {
            path: REVIEW_GATE_ARTIFACT.into(),
            sha256: lower_hex(Sha256::digest(&content)),
        };
        let artifacts = vec![json_artifact(reference.path.clone(), content)];
        let id = format!("review-certificate-{}", certificate.verdict.as_str());
        self.owner
            .put_evidence(&self.worker(), &id, artifacts)
            .expect("persist the certificate on the owner");
        let evidence = HandoffReviewEvidence {
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
        };
        self.handoff.review = HandoffReview {
            policy: ReviewTiming::BeforePr,
            disposition: HandoffReviewDisposition::BeforePr(Box::new(evidence)),
        };
    }
    fn review_evidence(&mut self) -> &mut HandoffReviewEvidence {
        match &mut self.handoff.review.disposition {
            HandoffReviewDisposition::BeforePr(evidence)
            | HandoffReviewDisposition::BeforeLanding(evidence) => evidence,
            HandoffReviewDisposition::NotRequired => panic!("the handoff carries no review"),
        }
    }
    fn approve(&self) -> ClaimResult {
        let boundary = &self.owner.backends.commit_boundary;
        let accepted = boundary.accepted_handoff(&self.claim.claim_id)?;
        let operator = self
            .operator()
            .with_handoff_observation(self.observation(None));
        let mutation = ClaimMutation::ApproveHandoff {
            handoff_id: accepted.handoff_id,
            candidate: self.handoff.candidate.clone(),
        };
        self.owner.mutate(&operator, "approval", &mutation)
    }
    fn merge_intent(&self, policy: Option<&str>) -> ClaimResult {
        let operator = self
            .operator()
            .with_handoff_observation(self.observation(policy));
        let mutation = ClaimMutation::MergeIntent {
            intent_id: "sent-merge".into(),
            resolved: false,
            evidence: "pinned provider request".into(),
        };
        self.owner.mutate(&operator, "merge-intent", &mutation)
    }
}
fn claim_worker(claim: &ExecutionClaim, bound: bool) -> ClaimInvocation {
    ClaimInvocation::trusted_worker(
        claim.task_id.clone(),
        claim.claim_id.clone(),
        claim.executed_on.machine_id.clone(),
        bound.then(|| leaf_run(claim)),
    )
}
fn claim_evidence(artifacts: Vec<TaskArtifact>) -> ClaimEvidence {
    ClaimEvidence {
        artifacts,
        ..Default::default()
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
    let mut handoff: TaskHandoff = serde_json::from_value(serde_json::json!({
        "schema_version": 1, "workspace_id": PARTITION_ID, "task_id": claim.task_id,
        "claim_id": claim.claim_id, "machine_id": claim.executed_on.machine_id,
        "run_id": "leaf",
        "candidate": {
            "repository": REPOSITORY, "source_branch": "attempt/leaf",
            "base_branch": "agent-main", "landing_branch": "agent-main",
            "candidate": {"commit": "a".repeat(40), "tree": "b".repeat(40)},
            "base": {"commit": "c".repeat(40), "tree": "d".repeat(40)},
            "delivery": {"kind": "pull_request", "number": 42},
        },
        "review": HandoffReview::not_required(),
        "execution_summary": "Outcome: success\nRequired checks passed for pinned candidate",
        "validation": [],
    }))
    .unwrap();
    let artifacts = commands
        .iter()
        .map(|command| {
            let log = serde_json::json!({
                "schema_version": 1, "workspace_id": handoff.workspace_id,
                "task_id": handoff.task_id, "claim_id": handoff.claim_id,
                "machine_id": handoff.machine_id, "run_id": handoff.run_id,
                "candidate": handoff.candidate,
                "tested_head": handoff.candidate.candidate.commit,
                "command": command, "exit_code": 0,
                "output": format!("{command} captured output"),
            });
            json_artifact(format!("{command}.json"), serde_json::to_vec(&log).unwrap())
        })
        .collect::<Vec<_>>();
    handoff.validation = artifacts
        .iter()
        .map(|artifact| HandoffArtifactRef {
            path: artifact.path.clone(),
            sha256: lower_hex(Sha256::digest(&artifact.content)),
        })
        .collect();
    if artifacts.is_empty() {
        return handoff;
    }
    let worker = claim_worker(claim, true);
    owner
        .put_evidence(&worker, "validation-logs", artifacts)
        .expect("persist owner logs");
    handoff
}
// Completion authority is the admission-time owner policy, rechecked at handoff and at landing.
allocation_test!(
    handoff_authorization_follows_the_owner_policy_at_admission,
    {
        // A `done` contract admitted under the policy is authorized at acceptance and its landing
        // request is pending.
        let granted = Delivery::admit(done_ship());
        granted.accept(Some(OWNER_POLICY)).expect("handoff");
        granted.assert_status(TaskStatus::Review);
        let starts = granted.owner.landing_starts();
        assert_eq!(starts.len(), 1);
        assert_eq!(starts[0].state, LandingStartState::Pending);
        // Landing rechecks the owner's current policy.
        let withdrawn = granted
            .merge_intent(None)
            .expect_err("withdrawn policy fences landing");
        let ok = withdrawn.to_string().contains("policy withdrawn");
        assert!(ok, "{withdrawn}");
        granted
            .merge_intent(Some(OWNER_POLICY))
            .expect("policy still granted");
        // The same contract, accepted while the owner grants no policy or another one, waits in review
        // for an operator.
        for policy in [None, Some("workspace-config:some.other_key")] {
            let waiting = Delivery::admit(done_ship());
            waiting
                .accept(policy)
                .expect("a valid delivery is still accepted");
            waiting.assert_status(TaskStatus::Review);
            let ok = waiting.owner.landing_starts().is_empty();
            assert!(ok, "no authority for {policy:?}");
            waiting.approve().expect("operator approves");
            assert_eq!(waiting.owner.landing_starts().len(), 1);
        }
        // A `review` contract is never authorized by a policy the owner grants after admission.
        let review = Delivery::admit(owner_request("first").ship);
        review.accept(Some(OWNER_POLICY)).expect("handoff");
        assert!(review.owner.landing_starts().is_empty());
    }
);
// An owner requiring no commands accepts and lands a log-free handoff; candidate integrity and a requiring owner still refuse it.
allocation_test!(
    an_owner_requiring_no_commands_accepts_and_lands_a_handoff_without_logs,
    {
        let delivery = Delivery::admit_requiring(done_ship(), &["src/lib.rs"], &[]);
        assert!(delivery.handoff.validation.is_empty());
        let mut moved = delivery.observation(Some(OWNER_POLICY));
        moved.candidate.candidate.commit = "e".repeat(40);
        let moved = delivery
            .owner
            .mutate(
                &delivery.worker().with_handoff_observation(moved),
                "moved",
                &ClaimMutation::AcceptHandoff(delivery.handoff.clone()),
            )
            .expect_err("candidate integrity still gates a handoff with no required commands");
        assert!(moved.to_string().contains("candidate"), "{moved}");
        delivery
            .accept(Some(OWNER_POLICY))
            .expect("no required command is no gate");
        delivery.assert_status(TaskStatus::Review);
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
        let ok = refused.to_string().contains("required validation missing");
        assert!(ok, "{refused}");
        requiring.assert_status(TaskStatus::InProgress);
    }
);
// Replaced handoff evidence blocks approval and, once approved, landing.
allocation_test!(replaced_handoff_evidence_blocks_approval_and_landing, {
    for approved in [false, true] {
        let delivery = Delivery::admit(owner_request("first").ship);
        delivery.accept(None).expect("handoff");
        if approved {
            delivery.approve().expect("approve over intact evidence");
        }
        // Owner storage replaced behind the claim, not an authorized post-handoff worker write: a well-
        // formed log for the same candidate whose bytes no longer match the digest the handoff pinned.
        let log = delivery
            .owner
            .bundle_path(&delivery.claim.task_id)
            .join("artifacts/files/build.json");
        let ok = log.is_file();
        assert!(
            ok,
            "the owner holds the validation log at {}",
            log.display()
        );
        let mut replaced: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&log).unwrap()).unwrap();
        replaced["output"] = "replaced output".into();
        std::fs::write(&log, serde_json::to_vec(&replaced).unwrap()).unwrap();
        if approved {
            let ok = delivery.merge_intent(None).is_err();
            assert!(ok, "landing must refuse replaced evidence");
        } else {
            let ok = delivery.approve().is_err();
            assert!(ok, "approval must refuse replaced evidence");
            assert!(delivery.owner.landing_starts().is_empty());
        }
    }
});
// [ORB-13990] Widening accepts any safe tracked path (other crates, docs, new tests); only metadata, environment and non-normal paths are refused.
allocation_test!(handoff_widening_accepts_any_safe_path_and_replays_once, {
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
        let ship = owner_request("template").ship;
        let mut delivery = Delivery::admit_requiring(
            ship,
            &["file:crates/touched/src/lib.rs"],
            &["build", "test"],
        );
        delivery.handoff.footprint_widening = vec![path.into()];
        let before = delivery.owner.task(&delivery.claim.task_id);
        let result = delivery.accept(None);
        if allowed {
            result.unwrap_or_else(|error| panic!("{path} widening accepted: {error}"));
            delivery.accept(None).expect("lost response replay");
            let task = delivery.owner.task(&delivery.claim.task_id);
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
                .map(|e| match &e.decision {
                    Some(TaskDecision::ContextFilesWidened(widening)) => {
                        assert_eq!(
                            e.note.as_deref(),
                            Some(serde_json::to_string(widening).unwrap().as_str()),
                            "the note is unchanged beside the decision"
                        );
                        widening.clone()
                    }
                    other => panic!("the widening is a typed decision: {other:?}"),
                })
                .collect::<Vec<_>>();
            let got = widenings.len();
            assert_eq!(got, 1, "replay must not duplicate widening history");
            assert_eq!(widenings[0].step, ContextWideningStep::Implement);
            assert_eq!(widenings[0].run_id, delivery.handoff.run_id);
            assert_eq!(widenings[0].selectors, vec![format!("file:{path}")]);
            let current = delivery.owner.claims().pop().unwrap();
            assert!(current.footprint.contains(&format!("file:{path}")));
            // Admission replay remains the immutable original receipt.
            let mut request = owner_request("first");
            request.ship = owner_request("template").ship;
            let got = delivery.owner.pull(&request).claim.unwrap().footprint;
            assert_eq!(got, delivery.claim.footprint);
        } else {
            let error = result.unwrap_err().to_string();
            assert!(error.contains(path), "exact refused path: {error}");
            let got = delivery.owner.task(&delivery.claim.task_id).context_files;
            assert_eq!(got, before.context_files);
            delivery.assert_status(TaskStatus::InProgress);
        }
    }
});
// Widening must equal the owner's observed diff; competing claims/locks no longer refuse it (scheduling hint, not a gate).
allocation_test!(
    handoff_widening_accepts_locked_paths_and_refuses_observation_mismatch,
    {
        let path = "src/new/nested.rs";
        let mut delivery = Delivery::admit(owner_request("template").ship);
        delivery.handoff.footprint_widening = vec![path.into()];
        let mut observed = delivery.observation(None);
        observed.footprint_widening.clear();
        let error = delivery
            .owner
            .mutate(
                &delivery.worker().with_handoff_observation(observed),
                "mismatch",
                &ClaimMutation::AcceptHandoff(delivery.handoff.clone()),
            )
            .unwrap_err()
            .to_string();
        let ok = error.contains(path);
        assert!(
            ok,
            "request must equal independently observed diff: {error}"
        );
        delivery.assert_status(TaskStatus::InProgress);
        for lock in ["claim", "reservation", "status"] {
            let mut delivery = Delivery::admit(owner_request("template").ship);
            let owner = &delivery.owner;
            let competitor = owner.create_task_in("competing", &["file:src/new/nested.rs"]);
            match lock {
                "claim" => {
                    owner
                        .pull(&owner_request("second"))
                        .claim
                        .expect("second claim");
                }
                "reservation" => {
                    let file = format!("file:{path}");
                    let reserved = owner
                        .backends
                        .reservation
                        .reserve_task_reservation(
                            orbit_store::contracts::TaskReservationReserveParams {
                                workspace_orbit_dir: owner.orbit_dir.to_string_lossy().into_owned(),
                                workspace_id: Some(PARTITION_ID.into()),
                                task_ids: vec![competitor.id.clone()],
                                requested_files: vec![file.clone()],
                                stored_files: vec![file],
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
                    owner
                        .backends
                        .commit_boundary
                        .commit_task_transition(
                            &orbit_store::contracts::TaskCoordinationCommitParams {
                                task_id: competitor.id.clone(),
                                actor: "owner".into(),
                                expected_status: vec![TaskStatus::Backlog],
                                status: Some(TaskStatus::InProgress),
                                ..Default::default()
                            },
                        )
                        .unwrap();
                }
            }
            delivery.handoff.footprint_widening = vec![path.into()];
            delivery.accept(None).unwrap_or_else(|error| {
                panic!("a live {lock} lock does not gate delivery: {error}")
            });
            delivery.assert_status(TaskStatus::Review);
        }
    }
);
// --------------------------------------------------------------------------- Before-PR review
// contract and evidence [ORB-13895]
// ---------------------------------------------------------------------------
fn before_pr_ship() -> AdmissionShipContract {
    let mut ship = owner_request("first").ship;
    ship.before_pr = true;
    ship.review = Some(AdmissionReviewContract {
        contract_version: REVIEW_CONTRACT_VERSION,
        crew: Some(REVIEW_CREW.into()),
        budget: ReviewBudget { minutes: 45 },
        required_validation_commands: Some(vec!["build".into(), "test".into()]),
        baseline_commands: Vec::new(),
        host_evidence: Vec::new(),
    });
    ship
}
/// The certificate a leaf's gate would issue for `handoff` (reviewed, with findings fixed in the last commit).
fn certificate(handoff: &TaskHandoff, verdict: ReviewVerdict) -> ReviewCertificate {
    let commit = |revision: &SourceRevision, subject: &str| {
        serde_json::json!({
            "commit": revision.commit, "tree": revision.tree,
            "author": "implementer", "committer": "implementer", "subject": subject,
        })
    };
    let reviewed = SourceRevision {
        commit: "e".repeat(40),
        tree: "f".repeat(40),
    };
    serde_json::from_value(serde_json::json!({
        "schema_version": REVIEW_CONTRACT_VERSION, "attempt_id": "attempt-1",
        "lineage_key": "lineage", "task_ids": [handoff.task_id],
        "task_meaning_digest": "meaning", "repository": REPOSITORY,
        "base": handoff.candidate.base,
        "reviewed_candidate": reviewed,
        "final_candidate": handoff.candidate.candidate,
        "implementation_commits": [commit(&reviewed, "implement")],
        "repair_commits": [commit(&handoff.candidate.candidate, "reviewer fixes")],
        "verdict": verdict, "assurance": verdict.assurance(), "findings": [],
        "validation": (["build", "test"].iter().map(|command| serde_json::json!({
            "command": command, "outcome": "passed", "role": "required",
        })).collect::<Vec<_>>()),
        "required_validation_commands": ["build", "test"],
        "validation_complete": verdict.passed(),
        "reviewer": {"crew": REVIEW_CREW, "provider": "provider", "model": "model",
            "same_model_as_implementer": false},
        "consumed": {"seconds": 0}, "budget": {"minutes": 45},
        "issued_at": Utc::now(),
    }))
    .unwrap()
}
// A before-PR owner pins its review contract on the claim, only to a gate-declaring PR leaf; termless contracts are malformed [ORB-13908].
allocation_test!(
    a_before_pr_owner_captures_its_review_contract_on_the_claim,
    {
        let owner = Coordinated::fixture();
        owner.create_task("reviewed work");
        let mut request = owner_request("ungated");
        request.ship = before_pr_ship();
        let refused = owner
            .try_pull(&request)
            .expect_err("a leaf that runs no gate is refused");
        let ok = refused.to_string().contains("before_pr_unsupported");
        assert!(ok, "{refused}");
        let mut termless = owner_request("termless");
        termless.ship = before_pr_ship();
        termless.ship.review = None;
        termless.review_gate = true;
        let malformed = owner.try_pull(&termless).expect_err("malformed contract");
        let ok = malformed.to_string().contains("invalid_input");
        assert!(ok, "{malformed}");
        assert!(owner.claims().is_empty(), "nothing was admitted");
        let mut gated = owner_request("gated");
        gated.ship = before_pr_ship();
        gated.review_gate = true;
        let receipt = owner.pull(&gated);
        assert!(receipt.claim.is_some(), "a gate-running leaf is admitted");
        assert_eq!(receipt.request.ship.review, before_pr_ship().review);
        assert_eq!(owner.claims().len(), 1);
        let unreviewed = Coordinated::fixture();
        unreviewed.create_task("unreviewed work");
        let mut executor_on = owner_request("executor-before-pr");
        executor_on.caller_before_pr = true;
        let receipt = unreviewed.pull(&executor_on);
        assert!(
            receipt.claim.is_some(),
            "an executor's own review.before_pr does not refuse an owner that captured it off"
        );
    }
);
// Before-PR handoffs need passing evidence for the handed-off head on an owner-held base, by the captured crew/contract, bound by the held certificate; other dispositions are typed refusals.
allocation_test!(
    before_pr_handoffs_are_accepted_only_with_matching_passing_evidence,
    {
        let reviewed = |verdict: ReviewVerdict| {
            let mut delivery = Delivery::admit(before_pr_ship());
            let certificate = certificate(&delivery.handoff, verdict);
            delivery.reviewed(&certificate);
            (delivery, certificate)
        };
        let (delivery, issued) = reviewed(ReviewVerdict::AcceptWithFixes);
        delivery.accept(None).expect("passing evidence is accepted");
        delivery.assert_status(TaskStatus::Review);
        let boundary = &delivery.owner.backends.commit_boundary;
        assert_eq!(
            boundary
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
            let got = delivery.owner.task_status(&delivery.claim.task_id);
            assert_eq!(got, TaskStatus::InProgress, "{case}");
        }
        // [ORB-14684] The certificate carries the owner's captured `review.baseline_commands`: a leaf
        // that drops the list, and with it the rule that a listed check's failure is never a diagnostic,
        // is refused; the captured list is accepted.
        let mut listed = before_pr_ship();
        listed.review.as_mut().unwrap().baseline_commands = vec!["heavy suite".into()];
        let mut dropped = Delivery::admit(listed.clone());
        let unlisted = certificate(&dropped.handoff, ReviewVerdict::Accept);
        dropped.reviewed(&unlisted);
        let error = dropped.accept(None).unwrap_err().to_string();
        assert!(
            error.contains(HandoffReviewRefusal::ReviewContractMismatch.as_str())
                && error.contains("baseline commands"),
            "{error}"
        );
        let mut kept = Delivery::admit(listed);
        let mut captured = certificate(&kept.handoff, ReviewVerdict::Accept);
        captured.baseline_commands = vec!["heavy suite".into()];
        kept.reviewed(&captured);
        kept.accept(None)
            .expect("a certificate carrying the captured list is accepted");
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
);
fn text_artifact(path: &str, body: &str) -> TaskArtifact {
    TaskArtifact {
        path: path.to_string(),
        content: body.as_bytes().to_vec(),
        media_type: "text/plain".to_string(),
        created_by: None,
    }
}
#[cfg(unix)]
allocation_test!(
    validation_artifact_directories_are_private_under_shared_umask,
    {
        use std::os::unix::fs::PermissionsExt;
        // SAFETY: the fixture runs alone in an isolated child, so the process-wide umask cannot affect
        // another test or the launching process.
        unsafe { libc::umask(0o002) };
        let owner = Coordinated::fixture();
        let task = owner.create_task("private validation artifacts");
        owner
            .upsert_artifacts(
                &task.id,
                vec![text_artifact("ordinary.json", "ordinary artifact")],
            )
            .expect("ordinary artifact writer");
        let claim = owner
            .pull(&owner_request("private-artifacts"))
            .claim
            .expect("claim");
        let worker = claim_worker(&claim, false);
        let artifact_path = format!("validation/{}/0.json", claim.claim_id);
        let content = "{\"exit_code\":0,\"output\":\"passed\"}";
        owner
            .put_evidence(
                &worker,
                "validation-log",
                vec![text_artifact(&artifact_path, content)],
            )
            .expect("claim validation evidence writer");
        let files = owner.bundle_path(&task.id).join("artifacts/files");
        for path in [
            &files,
            &files.join("validation"),
            &files.join("validation").join(&claim.claim_id),
        ] {
            let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode,
                0o700,
                "owner-only validation dir under umask 0002: {}",
                path.display()
            );
        }
        let got = std::fs::read(files.join(&artifact_path)).unwrap();
        assert_eq!(got, content.as_bytes());
        let stored = owner.artifacts(&task.id);
        let found = stored
            .iter()
            .any(|a| a.path == artifact_path && a.content == content.as_bytes());
        assert!(
            found,
            "the validation log remains readable through the artifact-store boundary"
        );
    }
);
fn invalid_input(error: OrbitError) -> String {
    match error {
        OrbitError::InvalidInput(message) => message,
        other => panic!("expected InvalidInput before commit, got {other}"),
    }
}
// Claim evidence stores canonical artifact paths; two spellings of one path are refused before commit.
allocation_test!(claim_evidence_stores_canonical_artifact_paths, {
    let owner = Coordinated::fixture();
    let task = owner.create_task("artifact paths");
    let request = owner_request("paths");
    let claim = owner.pull(&request).claim.expect("claim");
    assert_eq!(claim.task_id, task.id);
    let unbound = claim_worker(&claim, false);
    let worker = claim_worker(&claim, true);
    let artifacts = vec![text_artifact("a/b", "one"), text_artifact("a//b", "two")];
    let result = owner.put_evidence(&unbound, "evidence-duplicate", artifacts);
    let duplicate = result.expect_err("both spellings of one path");
    let ok = invalid_input(duplicate).contains("duplicate artifact path");
    assert!(ok, "Evidence of a/b and a//b is refused before commit");
    let ok = owner.stored_task(&task.id).is_some();
    assert!(ok, "a refused Evidence leaves the task readable");
    let got = owner.artifacts(&task.id);
    assert_eq!(
        got,
        Vec::<TaskArtifact>::new(),
        "a refused Evidence commits no artifact"
    );
    let artifacts = vec![text_artifact("notes/", "note")];
    let result = owner.put_evidence(&unbound, "evidence-notes", artifacts);
    result.expect("store notes/ under its canonical path");
    owner
        .bind_claim(&unbound, &claim, request.ship)
        .expect("bind");
    let artifacts = vec![text_artifact("a//b", "body")];
    let result = owner.update_evidence(&worker, "update-separator", artifacts);
    result.expect("store a//b under its canonical path");
    let artifacts = vec![text_artifact("a/b", "one"), text_artifact("a//b", "two")];
    let result = owner.fail_claim(&worker, "fail-duplicate", "blocked", artifacts);
    let refused_fail = result.expect_err("Fail carrying both spellings");
    assert!(
        invalid_input(refused_fail).contains("duplicate artifact path"),
        "Fail of a/b and a//b is refused before commit"
    );
    assert_eq!(owner.task_status(&task.id), TaskStatus::InProgress);
    let got = owner.claims()[0].phase;
    assert_eq!(
        got,
        ExecutionClaimPhase::Running,
        "a refused Fail does not settle the claim"
    );
    let stored = owner.artifacts(&task.id);
    assert_eq!(
        stored
            .iter()
            .map(|artifact| (artifact.path.as_str(), artifact.content.as_slice()))
            .collect::<Vec<_>>(),
        vec![("a/b", b"body".as_slice()), ("notes", b"note".as_slice())],
        "Evidence and Update persist canonical paths"
    );
    let manifest = owner
        .artifact_store()
        .get_task_artifact_manifest(&task.id)
        .unwrap()
        .unwrap();
    for file in &manifest {
        let got = file.blob.clone();
        assert_eq!(
            got,
            format!("files/{}", file.path),
            "manifest blob matches the canonical file"
        );
    }
    let artifacts = vec![text_artifact("notes/", "final")];
    let result = owner.fail_claim(&worker, "fail-notes", "stopped", artifacts);
    result.expect("Fail stores a canonical path");
    assert_eq!(owner.task_status(&task.id), TaskStatus::Blocked);
    let ok = owner.stored_task(&task.id).is_some();
    assert!(ok, "get_task succeeds after a Fail that carried notes/");
    let notes = owner.artifact(&task.id, "notes/").expect("canonical notes");
    assert_eq!(notes.path, "notes");
    assert_eq!(notes.content, b"final");
});
fn jsonl_rows<T: serde::de::DeserializeOwned>(path: &Path) -> Vec<T> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
// A leftover `.pending-write.yaml` is rolled back by the next claim Evidence commit before it writes, so no aborted row or ID survives.
allocation_test!(
    coordinated_commit_rolls_back_a_leftover_pending_write_first,
    {
        let owner = Coordinated::fixture();
        let task = owner.create_task("pending write");
        let claim = owner.pull(&owner_request("pending")).claim.expect("claim");
        assert_eq!(owner.task_status(&task.id), TaskStatus::InProgress);
        let bundle = owner.bundle_path(&task.id);
        let events_path = bundle.join(TASK_EVENTS_FILE_NAME);
        let comments_path = bundle.join(TASK_COMMENTS_FILE_NAME);
        let description_path = bundle.join(TASK_DESCRIPTION_FILE_NAME);
        // Steps 1 and 2 of the bundle write protocol, then a crash: the pending record holds the pre-
        // image, the rows and document rewrite are applied, and the envelope is never published.
        let mut documents = BTreeMap::new();
        for name in [
            TASK_DESCRIPTION_FILE_NAME,
            TASK_ACCEPTANCE_FILE_NAME,
            TASK_PLAN_FILE_NAME,
            TASK_EXECUTION_SUMMARY_FILE_NAME,
        ] {
            documents.insert(
                name.to_string(),
                std::fs::read_to_string(bundle.join(name)).unwrap(),
            );
        }
        let pending = serde_json::json!({
            "schema_version": 1,
            "events_len": std::fs::metadata(&events_path).unwrap().len(),
            "comments_len": std::fs::metadata(&comments_path).map_or(0, |m| m.len()),
            "envelope_sha256": lower_hex(Sha256::digest(std::fs::read(bundle.join(TASK_ENVELOPE_FILE_NAME)).unwrap())),
            "documents": documents,
        });
        let pending_yaml = serde_yaml::to_string(&pending).unwrap();
        std::fs::write(bundle.join(".pending-write.yaml"), pending_yaml).unwrap();
        let events: Vec<TaskEventRowV2> = jsonl_rows(&events_path);
        let comments: Vec<TaskCommentRowV2> = if comments_path.exists() {
            jsonl_rows(&comments_path)
        } else {
            Vec::new()
        };
        let aborted_event: TaskEventRowV2 = serde_json::from_value(serde_json::json!({
            "schema_version": TASK_ARTIFACT_SCHEMA_VERSION,
            "event_id": format!("EV-{:04}", events.len() + 1), "at": Utc::now(),
            "by": "aborted-writer", "type": "status_changed",
            "from_status": "in_progress", "to_status": "done",
        }))
        .unwrap();
        let aborted_comment: TaskCommentRowV2 = serde_json::from_value(serde_json::json!({
            "schema_version": TASK_ARTIFACT_SCHEMA_VERSION,
            "comment_id": format!("C-{:04}", comments.len() + 1), "at": Utc::now(),
            "by": "aborted-writer", "body": "aborted",
        }))
        .unwrap();
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
        let unbound = claim_worker(&claim, false);
        let evidence = ClaimMutation::Evidence(ClaimEvidence {
            comment: Some("kept".into()),
            ..Default::default()
        });
        owner
            .mutate(&unbound, "evidence-after-abort", &evidence)
            .expect("evidence commit");
        let read = owner
            .stored_task(&task.id)
            .expect("the bundle reads after the coordinated commit");
        assert_eq!(read.status, TaskStatus::InProgress);
        assert_eq!(read.description, task.description);
        let ok = !bundle.join(".pending-write.yaml").exists();
        assert!(ok, "the leftover pending write is settled");
        let events: Vec<TaskEventRowV2> = jsonl_rows(&events_path);
        let comments: Vec<TaskCommentRowV2> = jsonl_rows(&comments_path);
        let ok = events.iter().all(|event| event.by != "aborted-writer")
            && comments
                .iter()
                .all(|comment| comment.by != "aborted-writer");
        assert!(ok, "no aborted row survives: {events:?} {comments:?}");
        assert!(comments.iter().any(|comment| comment.body == "kept"));
        let event_ids: HashSet<_> = events.iter().map(|event| &event.event_id).collect();
        let comment_ids: HashSet<_> = comments.iter().map(|comment| &comment.comment_id).collect();
        assert_eq!(event_ids.len(), events.len(), "event IDs are unique");
        assert_eq!(comment_ids.len(), comments.len(), "comment IDs are unique");
    }
);
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
// [ORB-14370] A claimed reviewer's live report put is held to record ids and refusals name the record.
allocation_test!(a_claimed_reviewers_live_report_put_is_held_to_record_ids, {
    let owner = Coordinated::fixture();
    let task = owner.create_task("claimed review report");
    let request = owner_request("report");
    let claim = owner.pull(&request).claim.expect("claim");
    let unbound = claim_worker(&claim, false);
    let worker = claim_worker(&claim, true);
    owner
        .bind_claim(&unbound, &claim, request.ship)
        .expect("bind");
    let put = |id: &str, artifact: TaskArtifact| owner.update_evidence(&worker, id, vec![artifact]);
    for (case, verdict, validation, refusal) in [
        (
            "update-without-id",
            "incomplete",
            r#"{"command":"make ci-fast","outcome":"not_run","role":"required"}"#,
            Some("`make ci-fast` -> `\"id\": \"V1\"`"),
        ),
        (
            "update-with-id",
            "incomplete",
            r#"{"id":"V1","command":"make ci-fast","outcome":"not_run","role":"required"}"#,
            None,
        ),
        (
            "update-dropping-id",
            "accept",
            r#"{"id":"V2","command":"cargo test","outcome":"passed","role":"required"}"#,
            Some("required validation record `V1` (`make ci-fast`)"),
        ),
        (
            "update-carrying-id",
            "accept",
            r#"{"id":"V1","command":"TMPDIR=.orbit/tmp make ci-fast","outcome":"passed","role":"required"}"#,
            None,
        ),
    ] {
        let result = put(case, review_report_with(verdict, validation));
        if let Some(reason) = refusal {
            let message = invalid_input(result.expect_err(case));
            let ok = message.contains(reason);
            assert!(ok, "{case}: refusal names the record: {message}");
            if case == "update-without-id" {
                let ok = owner.artifact(&task.id, REVIEW_REPORT_ARTIFACT).is_none();
                assert!(ok, "{case}: a refused put stores no report");
            }
        } else {
            result.unwrap_or_else(|error| panic!("{case}: accepted revision: {error}"));
        }
    }
    let history = owner
        .artifact(&task.id, REVIEW_REPORT_HISTORY_ARTIFACT)
        .expect("history");
    assert_eq!(
        ReviewReportHistory::parse(&history.content)
            .unwrap()
            .for_attempt("rvw-1")
            .count(),
        2,
        "only the accepted revisions are retained"
    );
});
// Racing report revisions are both retained in history; no writer may supply the history itself [ORB-14370].
allocation_test!(racing_report_writers_each_retain_their_revision, {
    let owner = Coordinated::fixture();
    let task = owner.create_task("review report");
    let claim = owner.pull(&owner_request("report")).claim.expect("claim");
    let worker = claim_worker(&claim, false);
    let other = owner.reopen();
    let put = |store: &Coordinated, artifact: TaskArtifact| {
        store.upsert_artifacts(&task.id, vec![artifact])
    };
    let evidence = |store: &Coordinated, id: &str, artifact: TaskArtifact| {
        store.put_evidence(&worker, id, vec![artifact])
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
        .artifact(&task.id, REVIEW_REPORT_HISTORY_ARTIFACT)
        .expect("history");
    let history = ReviewReportHistory::parse(&history.content).unwrap();
    let verdicts: Vec<_> = history
        .for_attempt("rvw-1")
        .map(|revision| revision.verdict)
        .collect();
    let got = verdicts.len();
    assert_eq!(got, 2, "both revisions are retained: {verdicts:?}");
    assert!(verdicts.contains(&ReviewVerdict::Incomplete));
    assert!(verdicts.contains(&ReviewVerdict::Accept));
    let forged = text_artifact(REVIEW_REPORT_HISTORY_ARTIFACT, "{}");
    assert!(invalid_input(put(&owner, forged.clone()).unwrap_err()).contains("reserved"));
    assert!(
        invalid_input(evidence(&owner, "evidence-history", forged).unwrap_err())
            .contains("reserved")
    );
});
