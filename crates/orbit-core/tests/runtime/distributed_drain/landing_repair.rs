//! A handoff whose landing stopped on its base is repaired once, without an
//! operator [ORB-14261].
//!
//! The owner drains its own backlog in local ship mode, so every step is real
//! Git and none needs a provider: the claimed-local leaf's candidate is a
//! commit in the owner's repository, and the owner lands it by fast-forwarding
//! `main`. A commit that reaches `main` between the handoff and its landing
//! stops that fast-forward. The landing job's `handoff_land` records the stop
//! as repairable, the claim waits in `repair_pending`, and the drain's next
//! pass admits a repair claim whose leaf carries the preserved candidate. That
//! leaf runs the shipped `task_claimed_local_pipeline`: `candidate_resume`
//! squash-applies the candidate onto the moved base, the implementer resolves
//! the conflict, and the shipped commit, required validation and handoff
//! steps deliver it under the same task, which then lands. A second stop
//! blocks the task with both attempts' evidence.
//!
//! The implementer is an agent and no provider runs here, so the test stands
//! in for it: it edits the leaf's checkout and supplies the summary its
//! claimed output carries, and the pipeline resumes after that step, as the
//! no-diff fixture does. Every other step is the shipped one.

use super::*;

use orbit_types::tool::WorkerInvocation;
use orbit_types::workflow::handoff::{AcceptedHandoff, LandingAttempt, LandingAttemptState};

/// The owner's one required check: no conflict marker may be handed off.
const CHECK: &str = "! grep -rqs '<<<<<<<' src";
const FILE: &str = "src/f0.rs";
const BASELINE: &str = "fn work() {}\n";
const FIRST: &str = "fn work() {\n    first();\n}\n";
const MOVED: &str = "fn work() {\n    base();\n}\n";
const REPAIRED: &str = "fn work() {\n    base();\n    first();\n}\n";

/// A base that gained a conflicting change stops the first landing. The
/// drain's next pass takes the repair, its leaf resolves the conflict on the
/// moved base, revalidates and hands off again under the same task, and that
/// candidate lands: no operator acts between the first handoff and `done`.
#[test]
fn a_landing_stopped_on_its_base_is_repaired_and_lands_under_the_same_task() {
    if !isolated(
        module_path!(),
        "a_landing_stopped_on_its_base_is_repaired_and_lands_under_the_same_task",
    ) {
        return;
    }
    let owner = OwnerLocal::new();
    let first = owner.handed_off_leaf(|checkout, resumed| {
        assert_eq!(resumed["outcome"], "fresh", "{resumed}");
        std::fs::write(checkout.join(FILE), FIRST).unwrap();
        "Outcome: success\nImplemented the first change."
    });
    let task = owner.pair.claimed_task(&first);
    let first_claim = owner.claim_of(&first);
    let first_candidate = owner.accepted_candidate(&first_claim);
    assert_eq!(owner.pair.owner_status(&task), "review");

    owner.base_moves(MOVED);
    let moved = git(&owner.pair.owner_repo, &["rev-parse", "main"]);
    let stop = owner
        .land(&task)
        .expect_err("main moved past the candidate");
    assert!(stop.to_string().contains("cannot fast-forward"), "{stop}");
    assert_eq!(owner.claim_phase(&first_claim), "repair_pending");
    assert_eq!(owner.pair.owner_status(&task), "in-progress");

    let repair = owner.handed_off_leaf(|checkout, resumed| {
        assert_eq!(resumed["outcome"], "resumed_repaired", "{resumed}");
        assert_eq!(resumed["repair"]["trigger"], "conflict", "{resumed}");
        assert_eq!(resumed["repair"]["conflicting_paths"], json!([FILE]));
        let conflicted = std::fs::read_to_string(checkout.join(FILE)).unwrap();
        assert!(
            conflicted.contains("<<<<<<<")
                && conflicted.contains("first();")
                && conflicted.contains("base();"),
            "the implementer starts from both sides: {conflicted}"
        );
        std::fs::write(checkout.join(FILE), REPAIRED).unwrap();
        "Outcome: success\nRepaired a resumed candidate onto the moved base."
    });
    assert_ne!(repair, first);
    assert_eq!(owner.pair.claimed_task(&repair), task, "the same task");
    let carried = &owner.leaf_input(&repair)["claim_repair"];
    assert_eq!(carried["repairs_claim_id"], first_claim.as_str());
    assert_eq!(carried["head_sha"], first_candidate.as_str());
    assert!(
        carried["stop_evidence"]
            .as_str()
            .is_some_and(|evidence| evidence.contains("cannot fast-forward")),
        "the repair leaf sees why the landing stopped: {carried}"
    );
    assert_eq!(
        owner.claim_phase(&first_claim),
        "revoked",
        "the repair claim supersedes the stopped one"
    );
    let repaired = owner.accepted(&owner.claim_of(&repair));
    assert_eq!(
        repaired.required_commands,
        [CHECK],
        "the owner's required check gates the repair"
    );
    assert!(
        !repaired.handoff.validation.is_empty(),
        "the repaired candidate carries its own validation evidence"
    );
    assert_eq!(
        git(
            &owner.pair.owner_repo,
            &[
                "rev-parse",
                &format!("{}^", repaired.handoff.candidate.candidate.commit)
            ]
        ),
        moved,
        "the repaired candidate is built on the moved base"
    );

    let landed = owner.land(&task).expect("the repaired candidate lands");
    assert_eq!(landed["phase"], "landed", "{landed}");
    let repair_claim = owner.claim_of(&repair);
    assert_eq!(owner.claim_phase(&repair_claim), "landed");
    assert_eq!(owner.pair.owner_status(&task), "done");
    let repo = &owner.pair.owner_repo;
    assert_eq!(
        git(repo, &["rev-parse", "main"]).trim(),
        owner.accepted_candidate(&repair_claim),
        "main is the repaired candidate"
    );
    assert_eq!(std::fs::read_to_string(repo.join(FILE)).unwrap(), REPAIRED);
}

/// The repaired candidate's own landing stops on its base too: there is no
/// second automatic repair. The task blocks with a comment carrying both
/// attempts' candidates and stop evidence, and no further pass takes it.
#[test]
fn a_second_base_stop_blocks_the_task_with_both_attempts_evidence() {
    if !isolated(
        module_path!(),
        "a_second_base_stop_blocks_the_task_with_both_attempts_evidence",
    ) {
        return;
    }
    let owner = OwnerLocal::new();
    let first = owner.handed_off_leaf(|checkout, _| {
        std::fs::write(checkout.join(FILE), FIRST).unwrap();
        "Outcome: success\nImplemented the first change."
    });
    let task = owner.pair.claimed_task(&first);
    let first_claim = owner.claim_of(&first);
    owner.base_moves(MOVED);
    owner
        .land(&task)
        .expect_err("main moved past the candidate");
    let repair = owner.handed_off_leaf(|checkout, _| {
        std::fs::write(checkout.join(FILE), REPAIRED).unwrap();
        "Outcome: success\nRepaired a resumed candidate onto the moved base."
    });
    let repair_claim = owner.claim_of(&repair);

    owner.base_moves("fn work() {\n    moved_again();\n}\n");
    owner
        .land(&task)
        .expect_err("main moved past the repaired candidate as well");

    assert_eq!(owner.claim_phase(&repair_claim), "failed");
    assert_eq!(owner.pair.owner_status(&task), "blocked");
    let comments = comments_of(&owner.pair.owner_task(&task));
    for evidence in [
        first_claim.as_str(),
        repair_claim.as_str(),
        owner.accepted_candidate(&first_claim).as_str(),
        owner.accepted_candidate(&repair_claim).as_str(),
        "cannot fast-forward",
    ] {
        assert!(
            comments.contains(evidence),
            "{evidence} in the blocked task's comments: {comments}"
        );
    }
    let pass = owner.pair.pass(&owner.drain);
    assert!(pass["error"].is_null(), "{pass}");
    assert_eq!(
        owner
            .pair
            .follower_jobs
            .list_job_runs(LOCAL_LEAF_JOB)
            .unwrap()
            .len(),
        2,
        "a blocked task admits no further repair: {pass}"
    );
}

/// An owner draining its own backlog in local ship mode: the drain reaches the
/// owner as its own local session, and its leaves run in worktrees of the
/// owner's repository.
struct OwnerLocal {
    pair: Pair,
    drain: String,
}

impl OwnerLocal {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let orbit = root.path().join(OWNER).join("repo/.orbit");
        std::fs::create_dir_all(&orbit).unwrap();
        std::fs::write(
            orbit.join("config.toml"),
            format!(
                "[workflow]\ndistributed_completion = \"done\"\n\
                 required_validation_commands = [\"{CHECK}\"]\n"
            ),
        )
        .unwrap();
        let (unbound, repo) = open_runtime(root.path(), OWNER);
        let task = backlog_task(&unbound, &repo, FILE, None);
        std::fs::write(repo.join(FILE), BASELINE).unwrap();
        std::fs::write(repo.join(".gitignore"), "/.orbit/\n").unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        // The operator's identity, which the engine's own merges commit as.
        git(&repo, &["config", "user.name", "Orbit Test"]);
        git(&repo, &["config", "user.email", "test@orbit.invalid"]);
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "baseline"]);

        // Accepting a handoff dispatches the owner's shipped landing job. Its
        // worker does nothing: `land` runs the job's one step in this process.
        orbit_core::test_support::install_substitute_pipeline_worker(["true".to_string()]);
        let jobs = unbound.global_root().join("resources/jobs");
        std::fs::create_dir_all(&jobs).unwrap();
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/jobs/task_landing_pipeline.yaml"),
            jobs.join("task_landing_pipeline.yaml"),
        )
        .unwrap();

        let workspace_id = unbound.workspace_id().unwrap();
        let owner = OrbitRuntime::from_roots_with_binding(
            &unbound.global_root(),
            &repo.join(".orbit"),
            orbit_core::WorkspaceRuntimeBinding {
                logical_workspace_id: workspace_id.clone(),
                task_partition_id: workspace_id.clone(),
                owner_machine_id: None,
                checkout_role: None,
                repo_root: repo.clone(),
                ship_mode: orbit_core::ShipMode::Local,
                base_branch: Some("main".to_string()),
            },
        )
        .expect("owner bound to a local-mode workspace")
        .with_automation_machine_identity(Some(OWNER.into()));
        let wire = Arc::new(Wire {
            owner: owner.clone(),
            caller: OWNER.to_string(),
            calls: Mutex::default(),
            lose: Mutex::default(),
            task_reads: Mutex::default(),
            task_reads_fail: Mutex::default(),
            task_reads_remote_error: Mutex::default(),
            unreachable: Mutex::default(),
            protocol: Mutex::default(),
            fingerprint: Mutex::default(),
            refuse_settle: Mutex::default(),
            accept_handoffs: Mutex::default(),
            local: Mutex::new(true),
        });
        let jobs = orbit_store::compose::workspace_job_run_store(
            owner.sqlite_store().unwrap(),
            workspace_id.clone(),
        );
        let pair = Pair {
            _root: Arc::new(root),
            follower: owner.clone().with_drain_owner_transport(wire.clone()),
            wire,
            owner_repo: repo.clone(),
            follower_repo: repo,
            follower_jobs: jobs,
            destination: json!({
                "owner_machine_id": OWNER,
                "owner_workspace_id": workspace_id,
                "selector": format!("{OWNER}/{workspace_id}"),
                "execution_machine_id": OWNER,
            }),
            tasks: vec![task],
        };
        let drain = pair.run_drain();
        Self { pair, drain }
    }

    /// Admit the next claim as a running leaf, run its shipped pipeline with
    /// `implement` standing in for the implementer, and let the drain's next
    /// pass settle its handoff on the owner.
    fn handed_off_leaf(&self, implement: impl FnOnce(&Path, &Value) -> &'static str) -> String {
        let leaf = self.pair.running_leaf(&self.drain, 1);
        self.run_leaf(&leaf, implement);
        self.pair
            .follower_jobs
            .finalize_job_run(&leaf, JobRunState::Success, Utc::now(), None)
            .expect("leaf finished");
        let settled = self.pair.pass(&self.drain);
        assert!(settled["error"].is_null(), "{settled}");
        assert_eq!(
            self.claim_phase(&self.claim_of(&leaf)),
            "handed_off",
            "{settled}"
        );
        leaf
    }

    fn run_leaf(&self, leaf: &str, implement: impl FnOnce(&Path, &Value) -> &'static str) {
        use orbit_engine::activity_job::{V2ActivityCatalog, load_activity_asset, load_job_asset};
        use orbit_engine::{
            V2AuditWriter, execute_job_with_resume, resolve_job_catalog_refs_for_execution,
        };

        let pair = &self.pair;
        let run = pair.follower_jobs.get_job_run(leaf).unwrap().unwrap();
        assert_eq!(run.job_id, LOCAL_LEAF_JOB, "an owner-local claim");
        let input = run.input.expect("leaf input");
        let record = pair.admission(leaf);
        let claim = record.receipt.as_ref().unwrap().claim.clone().unwrap();
        let owner = pair.wire.owner.clone();
        let bound = owner
            .clone()
            .with_worker_invocation(
                WorkerInvocation {
                    owner_machine_id: OWNER.into(),
                    owner_workspace_id: record.destination.owner_workspace_id.clone(),
                    owner_destination: record.destination.selector.clone(),
                    task_id: claim.task_id.clone(),
                    claim_id: claim.claim_id.clone(),
                    execution: claim.executed_on.clone(),
                    bound_run_id: leaf.to_string(),
                },
                Arc::new(ToSelf(owner)),
            )
            .unwrap();

        // The steps before the implementer, as the pipeline passes them; the
        // engine injects `run_id` at dispatch.
        let worktree = engine_action(
            &bound,
            "worktree_setup",
            &json!({
                "job_run_id": leaf,
                "run_id": leaf,
                "task_ids": input["task_ids"],
                "base": input["base_branch"],
                "base_sync": input["base_sync"],
                "landing_mode": input["landing_mode"],
            }),
        );
        let resumed = engine_action(
            &bound,
            "candidate_resume",
            &json!({
                "job_run_id": worktree["job_run_id"],
                "task_ids": input["task_ids"],
                "workspace_path": worktree["workspace_path"],
                "base_sha": worktree["base_sha"],
                "candidate": input["resume_candidate"],
                "claimed": true,
                "claim_repair": input["claim_repair"],
            }),
        );
        let checkout = PathBuf::from(worktree["workspace_path"].as_str().unwrap());
        let summary = implement(&checkout, &resumed);

        let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
        let mut catalog = V2ActivityCatalog::new();
        for entry in std::fs::read_dir(assets.join("activities")).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|ext| ext == "yaml") {
                let asset = load_activity_asset(&std::fs::read_to_string(path).unwrap()).unwrap();
                catalog.insert(asset.name, asset.spec);
            }
        }
        let mut job = load_job_asset(
            &std::fs::read_to_string(assets.join("jobs").join(format!("{LOCAL_LEAF_JOB}.yaml")))
                .unwrap(),
        )
        .unwrap()
        .spec;
        resolve_job_catalog_refs_for_execution(&mut job, &catalog).unwrap();
        let mut resume = PipelineState::new(leaf.into(), LOCAL_LEAF_JOB.into(), input.clone());
        resume.record_step(0, JobRunState::Success, Some(worktree), None);
        resume.record_step(1, JobRunState::Success, Some(resumed), None);
        resume.record_step(2, JobRunState::Success, Some(json!({})), None);
        resume.compound_outputs.insert(
            2,
            [(
                "implement_one".into(),
                json!({"status": "success", "execution_summary": summary}),
            )]
            .into_iter()
            .collect(),
        );
        let audit = V2AuditWriter::with_disk_sinks(
            &pair.owner_repo.join(".orbit/tmp/pipeline-audit").join(leaf),
            bound.v2_audit_store().unwrap(),
            bound.workspace_id().unwrap(),
            leaf,
            "landing-repair-fixture",
            Some(&pair.owner_repo),
        )
        .unwrap();
        let outcome =
            execute_job_with_resume(&job, input, leaf, audit, &bound, Some(&resume)).unwrap();
        assert!(outcome.success, "{outcome:#?}");
        let ClaimMutation::AcceptHandoff(_) = pair.admission(leaf).settlement.unwrap() else {
            panic!("the shipped leaf records a typed handoff")
        };
    }

    /// The landing job the owner dispatched when it accepted `task`'s open
    /// handoff: its one step, `handoff_land`, run against the owner checkout.
    fn land(&self, task: &str) -> Result<Value, OrbitError> {
        let owner = &self.pair.wire.owner;
        let LandingAttempt {
            handoff_id,
            job_run_id,
            ..
        } = owner
            .landing_attempts()
            .unwrap()
            .into_iter()
            .find(|attempt| {
                attempt.task_id == task && attempt.state == LandingAttemptState::Dispatched
            })
            .unwrap_or_else(|| panic!("no landing dispatched for {task}"));
        let job_run_id = job_run_id.expect("acceptance submitted the landing job");
        orbit_engine::execute_deterministic_action(
            owner,
            "handoff_land",
            &json!({}),
            &json!({"handoff_id": handoff_id, "task_id": task, "run_id": job_run_id}),
            false,
            &Default::default(),
            None,
        )
    }

    /// A commit reaches `main` after the handoff, rewriting the file the
    /// candidate changed.
    fn base_moves(&self, contents: &str) {
        let repo = &self.pair.owner_repo;
        std::fs::write(repo.join(FILE), contents).unwrap();
        git(repo, &["commit", "-q", "-am", "The base moves on"]);
    }

    fn claim_of(&self, leaf: &str) -> String {
        self.pair
            .admission(leaf)
            .receipt
            .and_then(|receipt| receipt.claim)
            .expect("claim")
            .claim_id
    }

    fn claim_phase(&self, claim_id: &str) -> String {
        self.pair
            .owner_claims()
            .into_iter()
            .find(|claim| claim["claim"]["claim_id"] == claim_id)
            .and_then(|claim| claim["claim"]["phase"].as_str().map(str::to_string))
            .unwrap_or_else(|| panic!("no owner claim {claim_id}"))
    }

    fn accepted(&self, claim_id: &str) -> AcceptedHandoff {
        self.pair
            .wire
            .owner
            .accepted_task_handoff(claim_id)
            .expect("accepted handoff")
    }

    /// The candidate commit the owner accepted for `claim_id`.
    fn accepted_candidate(&self, claim_id: &str) -> String {
        self.accepted(claim_id).handoff.candidate.candidate.commit
    }

    fn leaf_input(&self, leaf: &str) -> Value {
        self.pair
            .follower_jobs
            .get_job_run(leaf)
            .unwrap()
            .unwrap()
            .input
            .expect("leaf input")
    }
}

fn engine_action(host: &OrbitRuntime, action: &str, input: &Value) -> Value {
    orbit_engine::execute_deterministic_action(
        host,
        action,
        &json!({}),
        input,
        false,
        &Default::default(),
        None,
    )
    .unwrap_or_else(|error| panic!("{action}: {error}"))
}

/// Hands an owner-local leaf's coordination calls to its own owner, as the
/// owner's local session.
struct ToSelf(OrbitRuntime);

impl OwnerCoordinator for ToSelf {
    fn call(
        &self,
        name: &str,
        mut input: Value,
        mut session: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        let binding = session.worker_invocation.clone().expect("worker binding");
        input["workspace"] = json!(binding.owner_workspace_id);
        session.workspace = Some(binding.owner_workspace_id);
        session.caller_machine_id = Some(OWNER.to_string());
        session.transport = Some(McpTransport::Local);
        session.effective_capabilities = BTreeSet::from([McpCapability::Agent]);
        self.0.execute_owner_coordination(name, input, session)
    }
}
