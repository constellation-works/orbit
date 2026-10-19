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
use std::io::Write;

#[path = "landing_worker.rs"]
mod worker;
use worker::LandingWorkers;

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
fn a_landing_stopped_on_its_base_is_repaired_and_lands_under_the_same_task() {
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
fn a_second_base_stop_blocks_the_task_with_both_attempts_evidence() {
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

/// An owner pull-request refusal stores its diagnostic on the landing attempt.
/// The task status note, the repair leaf's stop text and the blocked comment
/// carry the diagnostic's human reason. The engine's handoff landing records
/// this same evidence string; its test host does not persist a task, so the
/// note and comment are observed on the owner's record of that stop.
fn an_owner_landing_refusal_shows_the_human_reason_not_the_diagnostic() {
    const FIRST_REASON: &str =
        "pull request #42 conflicts with its base; the owner does not rebase unvalidated code";
    const SECOND_REASON: &str =
        "pull request #42 conflicts with its base again after the one automatic repair";
    const FIRST_SENTINEL: &str = "first-observation-sentinel";
    const SECOND_SENTINEL: &str = "second-observation-sentinel";

    let owner = OwnerLocal::new();
    let first = owner.handed_off_leaf(|checkout, _| {
        std::fs::write(checkout.join(FILE), FIRST).unwrap();
        "Outcome: success\nImplemented the first change."
    });
    let task = owner.pair.claimed_task(&first);
    let first_claim = owner.claim_of(&first);
    let first_evidence = owner_landing_refusal(FIRST_REASON, FIRST_SENTINEL);
    let first_handoff = owner.stop_with_refusal(&task, &first_evidence);

    assert_eq!(owner.claim_phase(&first_claim), "repair_pending");
    assert_eq!(owner.pair.owner_status(&task), "in-progress");
    assert_eq!(
        attempt_evidence(&owner, &first_handoff).as_deref(),
        Some(first_evidence.as_str())
    );
    let notes = landing_stopped_notes(&owner.pair.owner_task(&task));
    assert!(
        notes.iter().any(|note| {
            note.contains(FIRST_REASON)
                && note.contains("automatic repair")
                && !note.contains(FIRST_SENTINEL)
                && !note.contains("owner_landing_refusal")
        }),
        "the status note carries the human reason: {notes:?}"
    );

    let repair = owner.handed_off_leaf(|checkout, _| {
        std::fs::write(checkout.join(FILE), REPAIRED).unwrap();
        "Outcome: success\nRepaired a resumed candidate onto the current base."
    });
    let carried = owner.leaf_input(&repair);
    let carried_stop = carried["claim_repair"]["stop_evidence"]
        .as_str()
        .unwrap_or("");
    assert!(
        carried_stop.contains(FIRST_REASON)
            && !carried_stop.contains(FIRST_SENTINEL)
            && !carried_stop.contains("owner_landing_refusal"),
        "the repair leaf sees the human reason: {carried_stop}"
    );

    let repair_claim = owner.claim_of(&repair);
    let second_evidence = owner_landing_refusal(SECOND_REASON, SECOND_SENTINEL);
    let second_handoff = owner.stop_with_refusal(&task, &second_evidence);
    assert_eq!(owner.claim_phase(&repair_claim), "failed");
    assert_eq!(owner.pair.owner_status(&task), "blocked");
    assert_eq!(
        attempt_evidence(&owner, &second_handoff).as_deref(),
        Some(second_evidence.as_str())
    );
    assert_eq!(
        attempt_evidence(&owner, &first_handoff).as_deref(),
        Some(first_evidence.as_str()),
        "the first attempt keeps the diagnostic after the repair stops"
    );
    let notes = landing_stopped_notes(&owner.pair.owner_task(&task));
    assert!(
        notes.iter().any(|note| note == SECOND_REASON),
        "the blocked status note is the human reason: {notes:?}"
    );
    assert!(
        notes.iter().all(|note| {
            !note.contains(FIRST_SENTINEL)
                && !note.contains(SECOND_SENTINEL)
                && !note.contains("owner_landing_refusal")
        }),
        "no status note carries the diagnostic: {notes:?}"
    );
    let comments = comments_of(&owner.pair.owner_task(&task));
    for evidence in [
        first_claim.as_str(),
        repair_claim.as_str(),
        FIRST_REASON,
        SECOND_REASON,
    ] {
        assert!(
            comments.contains(evidence),
            "{evidence} in the blocked task's comments: {comments}"
        );
    }
    assert!(
        !comments.contains(FIRST_SENTINEL)
            && !comments.contains(SECOND_SENTINEL)
            && !comments.contains("owner_landing_refusal"),
        "the blocked comment does not carry the diagnostic: {comments}"
    );
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

/// [ORB-16890] A handoff whose pull request fails a required check the
/// candidate caused, once its one automatic repair is spent, does not sit in
/// review: the owner ends the claim and returns the task to the backlog with
/// the candidate and the stop. Its next run repairs that candidate, seeing the
/// failed check's signature, and its own failure of that check returns the
/// task again until the landing repair budget is spent, when the task blocks
/// with every counted stop's evidence.
fn a_spent_repair_whose_candidate_fails_its_check_returns_to_the_backlog_until_blocked() {
    use orbit_engine::HandoffLandingStep;
    const SIGNATURE: &str = "error[E0425]: cannot find value `delivered` in this scope";
    let reason = |stop: usize| {
        format!(
            "required check `CI / clippy` failed on pull request #42, classified `candidate`: \
             {SIGNATURE} (stop {stop})"
        )
    };
    let spent = |owner: &OwnerLocal, task: &str| {
        RuntimeHost::handoff_landing_context(
            &owner.pair.wire.owner,
            &owner.dispatched_handoff(task),
        )
        .expect("landing context")
        .automatic_repair_spent
    };
    let requeues = |owner: &OwnerLocal, task: &str| {
        owner.pair.owner_task(task)["history"]
            .as_array()
            .expect("task history")
            .iter()
            .filter(|event| event["event"] == "landing_repair_requeued")
            .count()
    };

    let owner = OwnerLocal::new();
    let first = owner.handed_off_leaf(|checkout, _| {
        std::fs::write(checkout.join(FILE), FIRST).unwrap();
        "Outcome: success\nImplemented the first change."
    });
    let task = owner.pair.claimed_task(&first);
    assert!(!spent(&owner, &task), "the first landing has its repair");
    owner.stop_with_refusal(&task, &owner_landing_refusal(&reason(1), "first-sentinel"));
    let repair = owner.handed_off_leaf(|checkout, _| {
        std::fs::write(checkout.join(FILE), REPAIRED).unwrap();
        "Outcome: success\nRepaired the candidate's failing check."
    });
    assert!(spent(&owner, &task), "the repair claim spent the repair");

    let mut stopped = owner.claim_of(&repair);
    let mut leaves = 2;
    for stop in 2..=3 {
        let kept = owner.accepted_candidate(&stopped);
        owner.stop_landing(
            &task,
            HandoffLandingStep::StopForRequeue,
            &owner_landing_refusal(&reason(stop), "requeue-sentinel"),
        );
        assert_eq!(owner.claim_phase(&stopped), "revoked", "stop {stop}");
        assert_eq!(
            owner.pair.owner_status(&task),
            "backlog",
            "a spent repair's candidate failure returns the task: stop {stop}"
        );
        assert_eq!(requeues(&owner, &task), stop - 1, "stop {stop}");

        let next = owner.handed_off_leaf(|checkout, resumed| {
            assert_eq!(resumed["outcome"], "resumed_repaired", "{resumed}");
            assert_eq!(resumed["repair"]["trigger"], "landing", "{resumed}");
            let output = resumed["repair"]["output"].as_str().unwrap_or("");
            assert!(
                output.contains(SIGNATURE) && !output.contains("requeue-sentinel"),
                "the run sees the failed check and its signature: {output}"
            );
            assert_eq!(
                std::fs::read_to_string(checkout.join(FILE)).unwrap(),
                REPAIRED,
                "the run starts from the kept candidate, not afresh"
            );
            "Outcome: success\nRepaired the kept candidate's failing check."
        });
        leaves += 1;
        assert_eq!(owner.pair.claimed_task(&next), task, "the same task");
        let carried = &owner.leaf_input(&next)["resume_candidate"];
        assert_eq!(carried["head_sha"], kept.as_str(), "{carried}");
        assert!(
            carried["landing_stop"]
                .as_str()
                .is_some_and(|stop| stop.contains(SIGNATURE)),
            "the kept candidate carries the stop: {carried}"
        );
        assert!(spent(&owner, &task), "a requeued run has spent the repair");
        stopped = owner.claim_of(&next);
    }

    owner.stop_landing(
        &task,
        HandoffLandingStep::StopForRequeue,
        &owner_landing_refusal(&reason(4), "budget-sentinel"),
    );
    assert_eq!(owner.claim_phase(&stopped), "failed");
    assert_eq!(owner.pair.owner_status(&task), "blocked");
    let comments = comments_of(&owner.pair.owner_task(&task));
    let reasons = [reason(2), reason(3), reason(4)];
    let blocked_candidate = owner.accepted_candidate(&stopped);
    for evidence in [stopped.as_str(), blocked_candidate.as_str()]
        .into_iter()
        .chain(reasons.iter().map(String::as_str))
    {
        assert!(
            comments.contains(evidence),
            "{evidence} in the blocked task's comments: {comments}"
        );
    }
    assert!(
        !comments.contains("sentinel"),
        "the blocked comment carries no diagnostic: {comments}"
    );
    let pass = owner.pair.pass(&owner.drain);
    assert!(pass["error"].is_null(), "{pass}");
    assert_eq!(
        owner
            .pair
            .follower_jobs
            .list_job_runs(LOCAL_LEAF_JOB)
            .unwrap()
            .len(),
        leaves,
        "a blocked task admits no further run: {pass}"
    );
}

/// A repair claim is admitted only where the task could start: a `ui` task's
/// pending repair waits, named, on a host whose kit is no longer verified and
/// keeps its claim, and the next pass on a host with a verified kit takes it.
fn a_ui_repair_waits_for_a_verified_browser_kit() {
    let mut owner = OwnerLocal::new();
    let task = owner.pair.tasks[0].clone();
    owner
        .pair
        .wire
        .owner
        .run_tool(
            "orbit.task.update",
            json!({"id": task, "tags": ["ui"], "model": "codex"}),
        )
        .expect("tag the task ui");
    let host = |owner: &mut OwnerLocal, home: &Path| {
        owner.pair.follower = owner.pair.follower.clone().with_browser_kit_home(home);
    };
    let verified = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    super::super::host_ui_routing::write_kit(verified.path(), true);
    let unverified = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    super::super::host_ui_routing::write_kit(unverified.path(), false);

    host(&mut owner, verified.path());
    let first = owner.handed_off_leaf(|checkout, _| {
        std::fs::write(checkout.join(FILE), FIRST).unwrap();
        "Outcome: success\nImplemented the first change."
    });
    assert_eq!(owner.pair.claimed_task(&first), task);
    let first_claim = owner.claim_of(&first);
    owner.base_moves(MOVED);
    owner
        .land(&task)
        .expect_err("main moved past the candidate");
    assert_eq!(owner.claim_phase(&first_claim), "repair_pending");

    host(&mut owner, unverified.path());
    let idle = owner.pair.pass(&owner.drain);
    assert_eq!(idle["admitted"], 0, "{idle}");
    assert_eq!(owner.claim_phase(&first_claim), "repair_pending");
    let receipt = owner
        .pair
        .follower_jobs
        .local_pull_admissions()
        .unwrap()
        .into_iter()
        .rev()
        .filter(|record| record.phase == LocalPullPhase::Idle)
        .find_map(|record| record.receipt)
        .expect("the owner answered idle");
    assert!(
        receipt
            .browser_kit_unavailable
            .iter()
            .any(|wait| wait.task_id == task
                && wait
                    .reason
                    .starts_with(&orbit_types::task::browser_kit_wait())),
        "{receipt:#?}"
    );

    host(&mut owner, verified.path());
    let repair = owner.handed_off_leaf(|checkout, resumed| {
        assert_eq!(resumed["outcome"], "resumed_repaired", "{resumed}");
        std::fs::write(checkout.join(FILE), REPAIRED).unwrap();
        "Outcome: success\nRepaired a resumed candidate onto the moved base."
    });
    assert_eq!(owner.pair.claimed_task(&repair), task, "the same task");
    assert_eq!(
        owner.leaf_input(&repair)["claim_repair"]["repairs_claim_id"],
        first_claim.as_str()
    );
}

/// An owner draining its own backlog in local ship mode: the drain reaches the
/// owner as its own local session, and its leaves run in worktrees of the
/// owner's repository.
struct OwnerLocal {
    pair: Pair,
    drain: String,
    workers: LandingWorkers,
}

impl OwnerLocal {
    fn new() -> Self {
        Self::requiring(CHECK, "")
    }

    /// [`Self::new`] with `check` as the owner's one required command and
    /// `config` after its `[workflow]` table.
    fn requiring(check: &str, config: &str) -> Self {
        let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let orbit = root.path().join(OWNER).join("repo/.orbit");
        std::fs::create_dir_all(&orbit).unwrap();
        std::fs::write(
            orbit.join("config.toml"),
            format!(
                "[workflow]\ndistributed_completion = \"done\"\n\
                 required_validation_commands = [\"{check}\"]\n{config}"
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

        // The real supervisor owns/reaps a child held behind a handshake.
        // `land` runs the shipped step in process while that child stays alive.
        let workers = LandingWorkers::new();
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
        let owner = calm_host(owner);
        // No provider runs here, but the drain's window preflight still
        // refuses a host whose crews have no launchable provider.
        launchable_providers(&owner);
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
            gateless: Mutex::default(),
            refuse_settle: Mutex::default(),
            accept_handoffs: Mutex::default(),
            local: Mutex::new(true),
            prefix_hosts: Mutex::default(),
            by_id_reads: Mutex::default(),
            before_settle: Mutex::default(),
            before_pull: Mutex::default(),
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
        Self {
            pair,
            drain,
            workers,
        }
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
        let bound = self.bound(leaf);

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

    /// The owner bound as `leaf`'s worker, exactly as the drain's launcher
    /// binds it: the claim, task, hosts and run its durable admission records.
    fn bound(&self, leaf: &str) -> OrbitRuntime {
        let record = self.pair.admission(leaf);
        let claim = record.receipt.as_ref().unwrap().claim.clone().unwrap();
        self.bound_as(WorkerInvocation {
            owner_machine_id: OWNER.into(),
            owner_workspace_id: record.destination.owner_workspace_id.clone(),
            owner_destination: record.destination.selector.clone(),
            task_id: claim.task_id.clone(),
            claim_id: claim.claim_id.clone(),
            execution: claim.executed_on.clone(),
            bound_run_id: leaf.to_string(),
        })
    }

    /// The drain's runtime bound to `invocation`, whatever claim it names,
    /// coordinating with its own owner as the launcher's bound clone does.
    fn bound_as(&self, invocation: WorkerInvocation) -> OrbitRuntime {
        self.pair
            .follower
            .clone()
            .with_worker_invocation(invocation, Arc::new(ToSelf(self.pair.wire.owner.clone())))
            .unwrap()
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
        self.own_landing_worker(&job_run_id);
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

    fn own_landing_worker(&self, run: &str) {
        let pid = self.workers.own(run).unwrap();
        let jobs = &self.pair.follower_jobs;
        let recorded = jobs.get_job_run(run).unwrap().unwrap();
        if recorded.pid.is_none() && !recorded.state.is_terminal() {
            assert!(jobs.claim_pending_job_run_owner(run, pid).unwrap());
        }
    }

    fn finish_landing_worker(&self, run: &str, terminal: JobRunState) {
        self.own_landing_worker(run);
        let runtime = &self.pair.wire.owner;
        match terminal {
            JobRunState::Cancelled => {
                let result = runtime
                    .cancel_job_run_with_options_and_policy(
                        run,
                        "operator",
                        "fixture",
                        Some("stop the owned landing"),
                        false,
                        false,
                    )
                    .unwrap();
                assert_eq!(result.final_state, "cancelled", "{result:?}");
            }
            JobRunState::Failed => {
                self.pair
                    .follower_jobs
                    .mark_job_run_running(run, Utc::now(), self.workers.pid(run).unwrap())
                    .unwrap();
            }
            _ => panic!("unsupported fixture outcome {terminal}"),
        }
        // Reproduce the observer-first ordering deterministically: it observes
        // and reaps the released child before the public retry reads evidence.
        self.workers.release(run).unwrap();
        // A reconciled read can itself interrupt a dead running owner. Wait on
        // the store until the real startup observer has settled its exit first.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !self
            .pair
            .follower_jobs
            .get_job_run(run)
            .unwrap()
            .unwrap()
            .state
            .is_terminal()
        {
            assert!(
                std::time::Instant::now() < deadline,
                "landing observer settlement"
            );
            std::thread::park_timeout(Duration::from_millis(10));
        }
        let observed = runtime
            .wait_pipeline_runs(&[run.to_string()], 30, 1, None)
            .unwrap();
        assert_eq!(
            observed.results[0].status,
            terminal.to_string(),
            "{observed:?}"
        );
        assert_eq!(
            self.pair
                .follower_jobs
                .get_job_run(run)
                .unwrap()
                .unwrap()
                .state,
            terminal
        );
        let _ = writeln!(
            std::io::stderr(),
            "owned landing settled: run={run} terminal={terminal} reaped=true"
        );
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

    /// Record the diagnostic string an owner pull-request refusal stores,
    /// against the landing attempt acceptance dispatched.
    fn stop_with_refusal(&self, task: &str, evidence: &str) -> String {
        self.stop_landing(
            task,
            orbit_engine::HandoffLandingStep::Stop { repairable: true },
            evidence,
        )
    }

    /// The handoff of `task`'s dispatched landing attempt.
    fn dispatched_handoff(&self, task: &str) -> String {
        self.pair
            .wire
            .owner
            .landing_attempts()
            .unwrap()
            .into_iter()
            .find(|attempt| {
                attempt.task_id == task && attempt.state == LandingAttemptState::Dispatched
            })
            .unwrap_or_else(|| panic!("no landing dispatched for {task}"))
            .handoff_id
    }

    /// Record `step` with `evidence` against the landing attempt acceptance
    /// dispatched, as the owner's landing activity records it.
    fn stop_landing(
        &self,
        task: &str,
        step: orbit_engine::HandoffLandingStep,
        evidence: &str,
    ) -> String {
        let runtime = &self.pair.wire.owner;
        let attempt = runtime
            .landing_attempts()
            .unwrap()
            .into_iter()
            .find(|attempt| {
                attempt.task_id == task && attempt.state == LandingAttemptState::Dispatched
            })
            .unwrap_or_else(|| panic!("no landing dispatched for {task}"));
        let handoff_id = attempt.handoff_id.clone();
        self.own_landing_worker(attempt.job_run_id.as_deref().expect("landing job"));
        RuntimeHost::record_handoff_landing(
            runtime,
            &orbit_engine::HandoffLandingUpdate {
                handoff_id: handoff_id.clone(),
                step,
                observed: None,
                evidence: evidence.to_string(),
            },
        )
        .unwrap_or_else(|error| panic!("record owner landing refusal: {error}"));
        handoff_id
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

impl Drop for OwnerLocal {
    fn drop(&mut self) {
        let cleanup = || -> Result<(), OrbitError> {
            let runtime = &self.pair.wire.owner;
            for attempt in runtime.landing_attempts()? {
                for number in 1..=attempt.attempt {
                    let Some(run) = self.pair.follower_jobs.automation_job_for_key(&format!(
                        "landing:{}:{number}",
                        attempt.handoff_id
                    ))?
                    else {
                        continue;
                    };
                    if self.workers.is_persisted_only(&run) {
                        continue;
                    }
                    // A refusal fixture may deliberately delete the run, but
                    // only after its actual worker has been settled and reaped.
                    self.workers
                        .own(&run)
                        .map_err(|e| OrbitError::Io(e.to_string()))?;
                    if self
                        .pair
                        .follower_jobs
                        .get_job_run(&run)?
                        .is_some_and(|r| !r.state.is_terminal())
                    {
                        runtime.cancel_job_run_with_options_and_policy(
                            &run,
                            "operator",
                            "fixture",
                            Some("fixture teardown"),
                            false,
                            false,
                        )?;
                    }
                    self.workers
                        .release(&run)
                        .map_err(|e| OrbitError::Io(e.to_string()))?;
                    let _ = writeln!(
                        std::io::stderr(),
                        "landing cleanup: run={run} reaped=true root_present={}",
                        self.pair._root.path().exists()
                    );
                }
            }
            Ok(())
        };
        if let Err(error) = cleanup() {
            // Preserve the root if an ownership/reap failure prevents cleanup.
            // Never remove a directory a supervisor may still need.
            std::mem::forget(self.pair._root.clone());
            let _ = writeln!(
                std::io::stderr(),
                "landing fixture cleanup failed; root retained: {error}"
            );
            if !std::thread::panicking() {
                panic!("{error}");
            }
        }
    }
}

/// The evidence string `stop_pr` records for an owner landing refusal.
fn owner_landing_refusal(reason: &str, sentinel: &str) -> String {
    json!({
        "schema_version": 1,
        "kind": "owner_landing_refusal",
        "reason": {
            "value": reason,
            "original_chars": reason.chars().count(),
            "truncated": false
        },
        "repairable": true,
        "owner_owed": {
            "state": "collected",
            "original_count": 40,
            "truncated": true,
            "items": [{
                "value": sentinel,
                "original_chars": sentinel.chars().count(),
                "truncated": false
            }]
        },
        "observation": {"required_checks": {"state": "not_collected"}, "sentinel": sentinel},
        "workflow_prerequisites": {"state": "not_collected"},
        "emission_proof": {"state": "not_collected"}
    })
    .to_string()
}

fn attempt_evidence(owner: &OwnerLocal, handoff_id: &str) -> Option<String> {
    owner
        .pair
        .wire
        .owner
        .landing_attempt_record(handoff_id, 1)
        .unwrap()
        .expect("landing attempt")
        .evidence
}

fn landing_stopped_notes(task: &Value) -> Vec<String> {
    task["history"]
        .as_array()
        .expect("task history")
        .iter()
        .filter(|event| event["event"] == "landing_stopped")
        .map(|event| event["note"].as_str().unwrap_or("").to_string())
        .collect()
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

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    a_landing_stopped_on_its_base_is_repaired_and_lands_under_the_same_task_scenarios,
    a_landing_stopped_on_its_base_is_repaired_and_lands_under_the_same_task,
    a_second_base_stop_blocks_the_task_with_both_attempts_evidence,
    an_owner_landing_refusal_shows_the_human_reason_not_the_diagnostic,
    a_spent_repair_whose_candidate_fails_its_check_returns_to_the_backlog_until_blocked,
    a_ui_repair_waits_for_a_verified_browser_kit
);

#[path = "landing_retry.rs"]
mod retry;

#[path = "requeued_claim.rs"]
mod requeued;
