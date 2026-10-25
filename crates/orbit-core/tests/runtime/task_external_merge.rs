//! A task whose delivery PR an operator merged outside Orbit settles to done
//! from that merge, through the owner pass and the operator entry point.
//!
//! Each test injects a fake forge holding the repository's merged PRs: the
//! most recent ones its listing returns, and older ones only a lookup by
//! number reaches. Execution claims are coordination rows written the way
//! the claim allocator writes them, so the store's own guards decide.
//!
//! A claimed delivery's handoff is accepted, authorized, dispatched, stopped
//! and revoked through the owner's own claim mutations and operator console,
//! so the claim journal decides whether a revoked handoff settles.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_core::application::review::{DistributedClaimState, ExpectedCandidate};
use orbit_core::application::task::{EXTERNAL_MERGE_SETTLED_EVENT, TaskAddParams};
use orbit_core::runtime::task_pr_forge::{ForgePullRequest, MergedPullRequest, TaskPrForge};
use orbit_core::{OrbitRuntime, TaskComplexity, TaskStatus};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_store::TaskCommitBoundary;
use orbit_store::contracts::{
    AdmissionIdentity, AdmissionLookup, AdmissionOrdering, AdmissionRequest, AdmissionRunContext,
    AdmissionShipContract, ClaimEvidence, ClaimInvocation, ClaimMutation, ClaimRun,
    DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA, ExecutionClaimPhase, ExecutionLocation, HandoffObservation,
};
use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
use orbit_types::task::ExternalRef;
use orbit_types::workflow::ReviewTiming;
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::handoff::{
    HandoffCandidate, HandoffDelivery, HandoffReview, HandoffReviewDisposition,
    LandingAttemptState, LandingStartState, TaskHandoff,
};
use serde_json::json;
use tempfile::TempDir;

const BOT: &str = "orbit-bot";
const MERGE_COMMIT: &str = "03f7af7d6a1b2c3d4e5f60718293a4b5c6d7e8f9";
/// The owner policy a `done` claim contract names, which acceptance matches
/// against the owner's observed completion authority.
const COMPLETION_POLICY: &str = "workspace-config:workflow.distributed_completion";
/// The branch a claimed handoff lands on, unlike the workspace base branch.
const LANDING: &str = "release-landing";

/// How far a claimed handoff's completion authority got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Authority {
    /// Accepted under a `review` contract: it awaits operator approval.
    Unapproved,
    /// Accepted under a `done` contract: the owner's landing job carries it.
    Authorized,
    /// Authorized, its landing attempt stopped, then revoked by an operator:
    /// the observed release-prep case.
    Revoked,
    /// Authorized and revoked while its landing attempt is still dispatched.
    RevokedWhileDispatched,
}

/// A claimed delivery the owner accepted as a typed handoff.
struct Handoff {
    task: String,
    claim_id: String,
    handoff_id: String,
}

#[derive(Default)]
struct ForgeState {
    /// What the most-recently-merged listing returns.
    recent: Vec<MergedPullRequest>,
    /// Merged PRs only a lookup by number reaches.
    older: BTreeMap<u64, MergedPullRequest>,
    /// Open and closed-unmerged PRs return no merge from the forge.
    unmerged: BTreeMap<u64, &'static str>,
    closed: Vec<u64>,
}

#[derive(Default, Clone)]
struct FakeForge(Arc<Mutex<ForgeState>>);

impl FakeForge {
    fn merged(
        number: u64,
        head: String,
        body: String,
        author: &str,
        base: &str,
    ) -> MergedPullRequest {
        MergedPullRequest {
            pull: ForgePullRequest {
                number,
                head_branch: head,
                body,
                author: author.to_string(),
            },
            base_branch: base.to_string(),
            merge_commit: MERGE_COMMIT.to_string(),
        }
    }

    /// A delivery PR of `task_id`, merged into `base` by `author`.
    fn merge_recent(&self, number: u64, task_id: &str, author: &str, base: &str) {
        self.0.lock().unwrap().recent.push(Self::merged(
            number,
            format!("orbit/{task_id}-47642870"),
            format!("Delivers {task_id}.\n\nOrbit run jrun-1"),
            author,
            base,
        ));
    }
}

impl TaskPrForge for FakeForge {
    fn open_pull_requests(&self, _repo_root: &Path) -> Result<Vec<ForgePullRequest>, OrbitError> {
        Ok(Vec::new())
    }

    fn merged_pull_requests(
        &self,
        _repo_root: &Path,
    ) -> Result<Vec<MergedPullRequest>, OrbitError> {
        Ok(self.0.lock().unwrap().recent.clone())
    }

    fn merged_pull_request(
        &self,
        _repo_root: &Path,
        number: u64,
    ) -> Result<Option<MergedPullRequest>, OrbitError> {
        let state = self.0.lock().unwrap();
        if state.unmerged.contains_key(&number) {
            return Ok(None);
        }
        Ok(state
            .recent
            .iter()
            .chain(state.older.values())
            .find(|merged| merged.pull.number == number)
            .cloned())
    }

    fn authenticated_login(&self, _repo_root: &Path) -> Result<String, OrbitError> {
        Ok(BOT.to_string())
    }

    fn close_pull_request(
        &self,
        _repo_root: &Path,
        number: u64,
        _comment: &str,
    ) -> Result<(), OrbitError> {
        self.0.lock().unwrap().closed.push(number);
        Ok(())
    }
}

struct Fixture {
    _root: TempDir,
    runtime: OrbitRuntime,
    forge: FakeForge,
    base: String,
}

impl Fixture {
    fn new() -> Self {
        let root =
            TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).expect("create tempdir");
        let global_root = root.path().join("global");
        let repo_root = root.path().join("repo");
        let workspace_root = repo_root.join(".orbit");
        std::fs::create_dir_all(&global_root).expect("create global root");
        std::fs::create_dir_all(&workspace_root).expect("create workspace root");
        std::fs::write(repo_root.join("shared.txt"), "fixture\n").expect("write shared file");
        let forge = FakeForge::default();
        let runtime = OrbitRuntime::from_roots(&global_root, &workspace_root)
            .expect("build test runtime")
            .with_task_pr_forge(Arc::new(forge.clone()));
        let base = runtime.workspace_base_branch().to_string();
        Self {
            _root: root,
            runtime,
            forge,
            base,
        }
    }

    /// A task on `shared.txt` in `status`, with `prs` recorded the way
    /// delivery records them.
    fn add_task(&self, title: &str, status: TaskStatus, prs: &[u64]) -> String {
        self.runtime
            .add_task(TaskAddParams {
                title: title.to_string(),
                description: format!("Fixture task: {title}"),
                acceptance_criteria: vec!["Fixture task is observable.".to_string()],
                plan: "Fixture plan.".to_string(),
                context_files: vec!["file:shared.txt".to_string()],
                complexity: TaskComplexity::Low,
                status: Some(status),
                external_refs: prs
                    .iter()
                    .map(|number| ExternalRef::github_pr(number.to_string()).unwrap())
                    .collect(),
                ..Default::default()
            })
            .expect("add task")
            .id
    }

    /// A running execution claim of `task_id` on `shared.txt`.
    fn claim(&self, task_id: &str, run_id: &str) {
        let claim_id = format!("claim-{run_id}");
        let claim = json!({
            "claim_id": claim_id,
            "task_id": task_id,
            "request_id": format!("req-{run_id}"),
            "executed_on": {"machine_id": "machine-1", "machine_name": null},
            "run_context": {"run_id": run_id, "job_name": "test-job", "machine_name": null},
            "footprint": ["file:shared.txt"],
            "reservation_id": format!("res-{run_id}"),
            "reservation_expires_at": (Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
            "phase": "running",
        });
        rusqlite::Connection::open(self.runtime.global_root().join("orbit.db"))
            .unwrap()
            .execute(
                "INSERT INTO task_coordination_rows(workspace_id, kind, row_id, payload_json, journal_id, created_at)
                 VALUES (?1, 'distributed-execution-claim-v1', ?2, ?3, 'fixture', ?4)",
                rusqlite::params![
                    self.runtime.workspace_id().unwrap(),
                    claim_id,
                    claim.to_string(),
                    Utc::now().to_rfc3339()
                ],
            )
            .unwrap();
    }

    /// Bind `task_id` to a running run of this process.
    fn bind_live_run(&self, task_id: &str) {
        let jobs = orbit_store::compose::workspace_job_run_store(
            self.runtime.sqlite_store().unwrap(),
            self.runtime.workspace_id().unwrap(),
        );
        let run = jobs
            .insert_job_run(
                "task_pr_pipeline",
                1,
                Utc::now(),
                Some(json!({ "task_ids": [task_id] })),
                None,
            )
            .unwrap();
        jobs.mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
            .unwrap();
        self.runtime
            .apply_task_automation_update(
                task_id,
                TaskAutomationUpdate {
                    job_run_id: Some(run.run_id),
                    ..TaskAutomationUpdate::default()
                },
            )
            .expect("bind the run");
    }

    fn status(&self, task_id: &str) -> TaskStatus {
        self.runtime.get_task(task_id).unwrap().status
    }

    fn boundary(&self) -> TaskCommitBoundary {
        TaskCommitBoundary::new(
            self.runtime.sqlite_store().unwrap(),
            TaskRegistryStore::open(&task_registry_path(&self.runtime.global_root())).unwrap(),
            self.runtime.workspace_id().unwrap(),
        )
        .unwrap()
    }

    /// Admit a real claim of a new task for a follower's leaf `leaf-<number>`
    /// under a `pr` contract landing on `landing`, completing as `done` under
    /// the owner's policy or waiting in `review`, and bind the leaf. Returns
    /// the task, its claim and the bound leaf's invocation.
    fn bound_claim(
        &self,
        number: u64,
        landing: &str,
        done: bool,
    ) -> (String, String, ClaimInvocation) {
        let task = self.add_task("Claimed delivery", TaskStatus::Backlog, &[]);
        let boundary = self.boundary();
        let ship = AdmissionShipContract {
            mode: "pr".into(),
            base_branch: self.base.clone(),
            landing_branch: landing.into(),
            before_pr: false,
            before_landing: false,
            completion: if done { "done" } else { "review" }.into(),
            authorization_reference: done.then(|| COMPLETION_POLICY.to_string()),
            review: None,
        };
        let request = AdmissionRequest {
            request_id: format!("req-{number}"),
            caller_version: "test".into(),
            caller_schema: DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
            caller_fingerprint: None,
            caller_before_pr: false,
            review_gate: true,
            run_context: AdmissionRunContext {
                run_id: format!("drain-{number}"),
                job_name: "workspace_pull_pipeline".into(),
                machine_name: None,
            },
            ship: ship.clone(),
            crews: None,
            os: None,
            browser_kit: false,
            admissible: None,
        };
        let repo = self._root.path().join("repo");
        let AdmissionLookup::Found { receipt, .. } = boundary
            .admit_task(
                &AdmissionIdentity::trusted_remote(ExecutionLocation {
                    machine_id: "follower".into(),
                    machine_name: None,
                }),
                &request,
                &AdmissionOrdering::default(),
                "test",
                &repo,
                &repo.join(".orbit"),
                &BTreeMap::new(),
                &BTreeMap::new(),
                &|_, _, _| Ok(None),
            )
            .unwrap()
        else {
            panic!("admission receipt missing");
        };
        let claim = receipt
            .claim
            .clone()
            .unwrap_or_else(|| panic!("task not admitted: {receipt:?}"));
        assert_eq!(claim.task_id, task);
        let run = ClaimRun {
            machine_id: "follower".into(),
            run_id: format!("leaf-{number}"),
        };
        let worker = ClaimInvocation::trusted_worker(
            task.clone(),
            claim.claim_id.clone(),
            "follower".into(),
            None,
        );
        self.runtime
            .mutate_execution_claim(
                Some(&worker),
                "bind",
                &ClaimMutation::Bind {
                    run: run.clone(),
                    ship,
                },
            )
            .unwrap();
        let worker = ClaimInvocation::trusted_worker(
            task.clone(),
            claim.claim_id.clone(),
            "follower".into(),
            Some(run),
        );
        (task, claim.claim_id, worker)
    }

    /// Admit and bind a real claim, then release its held PR or fail with a
    /// kept candidate. Neither path accepts a handoff or adds a task PR ref.
    fn released_pr(&self, number: u64, held: bool, landing: &str) -> String {
        let (task, claim_id, worker) = self.bound_claim(number, landing, false);
        let head = format!("orbit/{task}-claimed");
        let mut evidence = ClaimEvidence {
            summary: Some("Leaf stopped after opening its PR".into()),
            ..Default::default()
        };
        if held {
            evidence.evidence_hold = Some(
                serde_json::from_value(json!({
                    "schema_version": 1, "attempt_id": "review", "lineage_key": "lineage",
                    "run_id": format!("leaf-{number}"),
                    "candidate": {"commit": MERGE_COMMIT, "tree": MERGE_COMMIT},
                    "task_meaning_digest": "fixture", "requirements": [],
                    "published_ref": format!("refs/heads/orbit-evidence/{task}")
                }))
                .unwrap(),
            );
            evidence.held_pull_request = Some(
                serde_json::from_value(json!({
                    "number": number.to_string(), "head": head
                }))
                .unwrap(),
            );
        } else {
            evidence.failure = Some(
                serde_json::from_value(json!({
                    "class": "candidate", "reason": "Leaf failed after opening its PR",
                    "candidate": {
                        "branch": head, "head_sha": MERGE_COMMIT,
                        "pull_request": number.to_string(), "published": true
                    }
                }))
                .unwrap(),
            );
        }
        let mutation = if held {
            ClaimMutation::Release(evidence)
        } else {
            ClaimMutation::Fail(evidence)
        };
        self.runtime
            .mutate_execution_claim(Some(&worker), "release", &mutation)
            .unwrap();
        assert!(
            self.runtime
                .get_task(&task)
                .unwrap()
                .external_refs
                .is_empty()
        );
        assert!(
            self.boundary()
                .find_accepted_handoff(&claim_id)
                .unwrap()
                .is_none()
        );
        task
    }

    /// A follower's leaf hands off pull request `number`, landing on
    /// [`LANDING`], and the owner accepts it; its completion authority then
    /// goes as far as `authority` says, through the owner's own landing
    /// mutations and operator console.
    fn handed_off(&self, number: u64, authority: Authority) -> Handoff {
        let authorized = authority != Authority::Unapproved;
        let (task, claim_id, worker) = self.bound_claim(number, LANDING, authorized);
        let handoff = TaskHandoff {
            schema_version: 1,
            workspace_id: self.runtime.workspace_id().unwrap(),
            task_id: task.clone(),
            claim_id: claim_id.clone(),
            machine_id: "follower".into(),
            run_id: format!("leaf-{number}"),
            candidate: HandoffCandidate {
                repository: "owner/repository".into(),
                source_branch: format!("orbit/{task}-47642870"),
                base_branch: self.base.clone(),
                landing_branch: LANDING.into(),
                candidate: SourceRevision {
                    commit: "a".repeat(40),
                    tree: "b".repeat(40),
                },
                base: SourceRevision {
                    commit: "c".repeat(40),
                    tree: "d".repeat(40),
                },
                delivery: HandoffDelivery::PullRequest { number },
            },
            review: HandoffReview {
                policy: ReviewTiming::None,
                disposition: HandoffReviewDisposition::NotRequired,
            },
            execution_summary: "Outcome: success".into(),
            validation: vec![],
            footprint_widening: vec![],
            owed_criteria: vec![],
            regression_from: vec![],
        };
        let observation = HandoffObservation {
            footprint_widening: vec![],
            candidate: handoff.candidate.clone(),
            required_commands: vec![],
            owner_completion_authority: authorized.then(|| COMPLETION_POLICY.to_string()),
            review: None,
            known_red: vec![],
            light_delivery: vec![],
        };
        self.runtime
            .mutate_execution_claim(
                Some(&worker.with_handoff_observation(observation)),
                "handoff",
                &ClaimMutation::AcceptHandoff(handoff),
            )
            .expect("the owner accepts the handoff");
        assert_eq!(self.status(&task), TaskStatus::Review);
        let handoff_id = self
            .runtime
            .accepted_task_handoff(&claim_id)
            .unwrap()
            .handoff_id;

        let operator =
            ClaimInvocation::trusted_operator(task.clone(), claim_id.clone(), "operator".into());
        let landing = |mutation_id: &str, mutation: ClaimMutation| {
            self.runtime
                .mutate_execution_claim(Some(&operator), mutation_id, &mutation)
                .unwrap_or_else(|error| panic!("{mutation_id}: {error}"));
        };
        if matches!(
            authority,
            Authority::Revoked | Authority::RevokedWhileDispatched
        ) {
            landing(
                "dispatch",
                ClaimMutation::DispatchLanding {
                    handoff_id: handoff_id.clone(),
                    job_run_id: None,
                },
            );
        }
        if authority == Authority::Revoked {
            landing(
                "stop",
                ClaimMutation::StopLanding {
                    handoff_id: handoff_id.clone(),
                    reason: "the pull request's required checks are not green".into(),
                    repairable: false,
                    requeue: false,
                },
            );
        }
        if matches!(
            authority,
            Authority::Revoked | Authority::RevokedWhileDispatched
        ) {
            self.runtime
                .revoke_handoff_as_operator(
                    &handoff_id,
                    &ExpectedCandidate {
                        candidate_commit: "a".repeat(40),
                        base_commit: "c".repeat(40),
                    },
                    "operator",
                    "the release PR will be merged by hand",
                    "revoke",
                )
                .expect("the operator revokes completion authority");
        }
        Handoff {
            task,
            claim_id,
            handoff_id,
        }
    }

    /// The phase of `claim_id` on the owner.
    fn claim_phase(&self, claim_id: &str) -> ExecutionClaimPhase {
        self.runtime
            .inspect_execution_claims()
            .unwrap()
            .into_iter()
            .find(|state| state.claim.claim_id == claim_id)
            .unwrap_or_else(|| panic!("no claim {claim_id}"))
            .claim
            .phase
    }

    /// The owner console's completion authority for `task`'s handoff.
    fn console_authority(&self, task: &str) -> serde_json::Value {
        let console = self
            .runtime
            .distributed_claim_console_filtered(Some(task), DistributedClaimState::All, true)
            .unwrap();
        console["claims"][0]["handoff"]["authority"].clone()
    }
}

#[test]
fn released_claim_pull_requests_settle_without_an_accepted_handoff_or_task_ref() {
    for held in [true, false] {
        let fx = Fixture::new();
        // The captured branch remains authoritative when workspace config differs.
        let landing = "captured-landing";
        let number = if held { 101 } else { 102 };
        let task = fx.released_pr(number, held, landing);
        fx.forge
            .merge_recent(number, &task, "follower-bot", landing);
        let settlement = if held {
            fx.runtime
                .settle_externally_merged_task(&task)
                .expect("held PR settles")
        } else {
            let mut settled = fx
                .runtime
                .settle_externally_merged_tasks()
                .expect("owner pass settles kept PR");
            assert_eq!(settled.len(), 1);
            settled.pop().unwrap()
        };
        assert_eq!(settlement.pr_number, number);
        assert_eq!(settlement.task_id, task);
        assert_eq!(fx.status(&task), TaskStatus::Done);
        let history = fx.runtime.get_task_history(&task).unwrap();
        let note = history
            .iter()
            .find(|entry| entry.event == EXTERNAL_MERGE_SETTLED_EVENT)
            .unwrap()
            .note
            .as_deref()
            .unwrap();
        assert!(
            note.contains(&format!("#{number}"))
                && note.contains(MERGE_COMMIT)
                && note.contains(landing),
            "{note}"
        );
    }
}

#[test]
fn released_claim_pull_requests_require_a_merge_naming_the_task_into_the_captured_branch() {
    for held in [true, false] {
        for rejected in [
            "open",
            "closed",
            "wrong-base",
            "wrong-task",
            "other-claim",
            "missing-receipt",
            "mismatched-receipt",
        ] {
            let fx = Fixture::new();
            let task = fx.released_pr(103, held, "captured-landing");
            let before = fx.status(&task);
            match rejected {
                "open" | "closed" => {
                    fx.forge.0.lock().unwrap().unmerged.insert(103, rejected);
                }
                "wrong-base" => fx.forge.merge_recent(103, &task, BOT, &fx.base),
                "wrong-task" => fx
                    .forge
                    .merge_recent(103, "OTHER-1", BOT, "captured-landing"),
                "other-claim" => {
                    // The merge names an unrelated task, on the captured branch.
                    // Only this task's released claim records the PR, so the
                    // named task must not settle from another task's claim.
                    let unrelated = fx.add_task("Unrelated", TaskStatus::Blocked, &[]);
                    fx.forge
                        .merge_recent(103, &unrelated, BOT, "captured-landing");
                    assert!(
                        fx.runtime
                            .settle_externally_merged_task(&unrelated)
                            .is_err()
                    );
                    assert_eq!(fx.status(&unrelated), TaskStatus::Blocked);
                }
                "missing-receipt" => {
                    fx.forge.merge_recent(103, &task, BOT, "captured-landing");
                    rusqlite::Connection::open(fx.runtime.global_root().join("orbit.db"))
                        .unwrap().execute(
                            "DELETE FROM task_coordination_rows WHERE workspace_id = ?1 AND kind = 'distributed-admission-receipt-v1'",
                            [fx.runtime.workspace_id().unwrap()],
                        ).unwrap();
                }
                "mismatched-receipt" => {
                    fx.forge.merge_recent(103, &task, BOT, "captured-landing");
                    rusqlite::Connection::open(fx.runtime.global_root().join("orbit.db"))
                        .unwrap().execute(
                            "UPDATE task_coordination_rows SET payload_json = json_set(payload_json, '$.receipt.claim.task_id', 'OTHER-1') WHERE workspace_id = ?1 AND kind = 'distributed-admission-receipt-v1'",
                            [fx.runtime.workspace_id().unwrap()],
                        ).unwrap();
                }
                _ => unreachable!(),
            }
            assert!(
                fx.runtime.settle_externally_merged_task(&task).is_err(),
                "{held}/{rejected}"
            );
            assert!(
                fx.runtime
                    .settle_externally_merged_tasks()
                    .unwrap()
                    .is_empty(),
                "{held}/{rejected}"
            );
            assert_eq!(fx.status(&task), before, "{held}/{rejected}");
            assert!(
                !fx.runtime
                    .get_task_history(&task)
                    .unwrap()
                    .iter()
                    .any(|entry| entry.event == EXTERNAL_MERGE_SETTLED_EVENT),
                "{held}/{rejected}"
            );
        }
    }
}

#[test]
fn a_live_leaf_carrying_a_released_claim_task_prevents_settlement() {
    let fx = Fixture::new();
    let task = fx.released_pr(104, true, &fx.base);
    fx.forge.merge_recent(104, &task, BOT, &fx.base);
    let jobs = orbit_store::compose::workspace_job_run_store(
        fx.runtime.sqlite_store().unwrap(),
        fx.runtime.workspace_id().unwrap(),
    );
    jobs.insert_job_run(
        "task_auto_pipeline",
        1,
        Utc::now(),
        Some(json!({"task_ids": [task]})),
        None,
    )
    .unwrap();
    let error = fx.runtime.settle_externally_merged_task(&task).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("live `run auto` leaf carries it"),
        "{error}"
    );
    assert!(
        fx.runtime
            .settle_externally_merged_tasks()
            .unwrap()
            .is_empty()
    );
    assert_eq!(fx.status(&task), TaskStatus::InProgress);
}

#[test]
fn external_merge_settlement_refuses_every_ineligible_status() {
    let fx = Fixture::new();
    for status in [
        TaskStatus::Proposed,
        TaskStatus::Backlog,
        TaskStatus::Someday,
        TaskStatus::Done,
        TaskStatus::Archived,
        TaskStatus::Rejected,
    ] {
        let initial = if status == TaskStatus::Archived {
            TaskStatus::Proposed
        } else {
            status
        };
        let task = fx.add_task("Ineligible status", initial, &[105]);
        if status == TaskStatus::Archived {
            fx.runtime.archive_task(&task).unwrap();
        }
        fx.forge.merge_recent(105, &task, BOT, &fx.base);
        let error = fx.runtime.settle_externally_merged_task(&task).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("only a blocked, in-progress or review task settles"),
            "{error}"
        );
        assert_eq!(fx.status(&task), status);
    }
    assert!(
        fx.runtime
            .settle_externally_merged_tasks()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_blocked_task_settles_from_its_merged_delivery_pr_while_another_task_claims_its_files() {
    let fx = Fixture::new();
    // ORB-16371: the operator merged the blocked task's delivery PR while
    // another task's run held a claim on the same files.
    let merged = fx.add_task("Merged by hand", TaskStatus::Blocked, &[4937]);
    let own_claim = fx.add_task("Claimed by its own run", TaskStatus::Blocked, &[50]);
    let other_branch = fx.add_task("Merged elsewhere", TaskStatus::Blocked, &[51]);
    let not_delivery = fx.add_task("Merged by a human", TaskStatus::Review, &[52, 53]);
    let live_run = fx.add_task("Still running", TaskStatus::InProgress, &[54]);
    let holder = fx.add_task("Claim holder", TaskStatus::InProgress, &[]);
    fx.bind_live_run(&live_run);
    fx.claim(&holder, "jrun-20261017-0625-t1");
    fx.claim(&own_claim, "jrun-own-claim");

    fx.forge.merge_recent(4937, &merged, BOT, &fx.base);
    fx.forge.merge_recent(50, &own_claim, BOT, &fx.base);
    fx.forge.merge_recent(51, &other_branch, BOT, "release");
    fx.forge.merge_recent(52, &not_delivery, "alice", &fx.base);
    fx.forge.0.lock().unwrap().recent.push(FakeForge::merged(
        53,
        format!("orbit/{not_delivery}-dddd4444"),
        format!("Delivers {not_delivery}0."),
        BOT,
        &fx.base,
    ));
    fx.forge.merge_recent(54, &live_run, BOT, &fx.base);

    // The agent-shaped manual close is still a start, and the overlapping
    // claim refuses it: the settlement must not depend on that edge.
    let refused = fx
        .runtime
        .run_tool(
            "orbit.task.update",
            json!({"id": merged, "status": "in-progress", "model": "codex"}),
        )
        .expect_err("a start beside another task's claim is refused");
    assert!(
        refused
            .to_string()
            .contains("task footprint overlaps an execution claim"),
        "{refused}"
    );

    let settled = fx
        .runtime
        .settle_externally_merged_tasks()
        .expect("owner pass");

    assert_eq!(
        settled
            .iter()
            .map(|settlement| (settlement.task_id.as_str(), settlement.pr_number))
            .collect::<Vec<_>>(),
        vec![(merged.as_str(), 4937)],
        "only the delivery PR merged into the target branch settles its task"
    );
    assert_eq!(settled[0].previous_status, TaskStatus::Blocked);
    assert_eq!(settled[0].merge_commit, MERGE_COMMIT);

    let task = fx.runtime.get_task(&merged).unwrap();
    assert_eq!(task.status, TaskStatus::Done);
    assert!(
        task.execution_summary.contains("#4937") && task.execution_summary.contains(MERGE_COMMIT),
        "the summary names the landing: {}",
        task.execution_summary
    );
    let history = fx.runtime.get_task_history(&merged).unwrap();
    let entry = history
        .iter()
        .find(|entry| entry.event == EXTERNAL_MERGE_SETTLED_EVENT)
        .expect("the settlement is recorded in history");
    assert_eq!(entry.from_status, Some(TaskStatus::Blocked));
    assert_eq!(entry.to_status, Some(TaskStatus::Done));
    let note = entry.note.as_deref().unwrap_or_default();
    assert!(
        note.contains("#4937") && note.contains(MERGE_COMMIT) && note.contains(&fx.base),
        "the landing note names the PR, branch and merge commit: {note}"
    );

    for (task_id, status, why) in [
        (&own_claim, TaskStatus::Blocked, "its own claim is live"),
        (
            &other_branch,
            TaskStatus::Blocked,
            "it merged into another branch",
        ),
        (
            &not_delivery,
            TaskStatus::Review,
            "no delivery PR of it merged",
        ),
        (&live_run, TaskStatus::InProgress, "its own run is live"),
        (&holder, TaskStatus::InProgress, "it has no PR"),
    ] {
        assert_eq!(fx.status(task_id), status, "{why}");
    }

    let again = fx
        .runtime
        .settle_externally_merged_tasks()
        .expect("owner pass");
    assert!(again.is_empty(), "a settled task does not settle twice");
}

#[test]
fn the_operator_settles_one_task_from_a_merge_older_than_the_listing() {
    let fx = Fixture::new();
    let reviewed = fx.add_task("Approved on the forge", TaskStatus::Review, &[70, 71]);
    let claimed = fx.add_task("Claimed by its own run", TaskStatus::Blocked, &[80]);
    fx.claim(&claimed, "jrun-own-claim");
    {
        let mut state = fx.forge.0.lock().unwrap();
        state.older.insert(
            71,
            FakeForge::merged(
                71,
                format!("orbit/{reviewed}-aaaa1111"),
                format!("Delivers {reviewed}."),
                BOT,
                &fx.base,
            ),
        );
        state.older.insert(
            80,
            FakeForge::merged(
                80,
                format!("orbit/{claimed}-bbbb2222"),
                format!("Delivers {claimed}."),
                BOT,
                &fx.base,
            ),
        );
    }

    assert!(
        fx.runtime
            .settle_externally_merged_tasks()
            .expect("owner pass")
            .is_empty(),
        "the owner pass reads only the recent listing"
    );
    assert_eq!(fx.status(&reviewed), TaskStatus::Review);

    let settlement = fx
        .runtime
        .settle_externally_merged_task(&reviewed)
        .expect("the operator settles the task");
    assert_eq!(settlement.pr_number, 71);
    assert_eq!(settlement.previous_status, TaskStatus::Review);
    assert_eq!(fx.status(&reviewed), TaskStatus::Done);

    let refused = fx
        .runtime
        .settle_externally_merged_task(&claimed)
        .expect_err("a task with its own live claim is not settled");
    assert!(
        refused.to_string().contains("jrun-own-claim"),
        "the refusal names the task's own claim: {refused}"
    );
    assert_eq!(fx.status(&claimed), TaskStatus::Blocked);

    let refused = fx
        .runtime
        .settle_externally_merged_task(&reviewed)
        .expect_err("a done task does not settle again");
    assert!(refused.to_string().contains("'done'"), "{refused}");
}

#[test]
fn a_revoked_handoff_and_its_claim_settle_from_its_candidate_pr_merged_outside_orbit() {
    for owner_pass in [false, true] {
        let fx = Fixture::new();
        let handoff = fx.handed_off(4930, Authority::Revoked);
        let task = handoff.task.as_str();
        assert_eq!(
            fx.claim_phase(&handoff.claim_id),
            ExecutionClaimPhase::HandedOff
        );

        // The owner console names the way out while the task is stuck.
        let authority = fx.console_authority(task);
        assert_eq!(authority["state"], "revoked");
        let command = authority["settle_command"]
            .as_str()
            .expect("a revoked pull-request handoff names its settle command");
        assert_eq!(command, format!("orbit task settle-merged {task}"));
        assert!(
            authority["summary"].as_str().unwrap().contains(command),
            "the summary the dashboard shows carries the command: {authority}"
        );

        // A human merged the handoff's own PR into its landing branch.
        fx.forge.merge_recent(4930, task, "alice", LANDING);
        let requests = fx.runtime.landing_start_requests().unwrap();
        let attempts = fx.runtime.landing_attempts().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].state, LandingStartState::Revoked);
        assert_eq!(attempts[0].state, LandingAttemptState::Stopped);

        let settlement = if owner_pass {
            let mut settled = fx
                .runtime
                .settle_externally_merged_tasks()
                .expect("owner pass");
            assert_eq!(settled.len(), 1, "{settled:?}");
            settled.pop().unwrap()
        } else {
            fx.runtime
                .settle_externally_merged_task(task)
                .expect("the operator settles the revoked handoff's task")
        };
        assert_eq!(settlement.task_id, task);
        assert_eq!(settlement.pr_number, 4930);
        assert_eq!(settlement.previous_status, TaskStatus::Review);
        assert_eq!(settlement.base_branch, LANDING);
        assert_eq!(settlement.merge_commit, MERGE_COMMIT);

        let settled = fx.runtime.get_task(task).unwrap();
        assert_eq!(settled.status, TaskStatus::Done);
        assert!(
            settled
                .external_refs
                .contains(&ExternalRef::github_pr("4930".to_string()).unwrap()),
            "the landing PR is recorded on the task: {:?}",
            settled.external_refs
        );
        let history = fx.runtime.get_task_history(task).unwrap();
        let entry = history
            .iter()
            .find(|entry| entry.event == EXTERNAL_MERGE_SETTLED_EVENT)
            .expect("the settlement is recorded in history");
        assert_eq!(entry.from_status, Some(TaskStatus::Review));
        assert_eq!(entry.to_status, Some(TaskStatus::Done));
        let note = entry.note.as_deref().unwrap_or_default();
        assert!(
            note.contains("#4930") && note.contains(MERGE_COMMIT) && note.contains(LANDING),
            "the landing note names the PR, branch and merge commit: {note}"
        );

        // The claim settled in the same write and protects nothing.
        let claims = fx.runtime.inspect_execution_claims().unwrap();
        for state in claims.iter().filter(|state| state.claim.task_id == task) {
            assert!(!state.claim.phase.is_unsettled(), "{state:?}");
            assert!(!state.claim.phase.protects_footprint(), "{state:?}");
            assert_eq!(state.unresolved_merge_intent, None);
        }
        // No authorization, landing request or attempt was added or reopened.
        assert_eq!(fx.runtime.landing_start_requests().unwrap(), requests);
        assert_eq!(fx.runtime.landing_attempts().unwrap(), attempts);

        assert!(
            fx.runtime
                .settle_externally_merged_tasks()
                .unwrap()
                .is_empty(),
            "a settled task does not settle twice"
        );
    }
}

#[test]
fn a_handed_off_claim_refuses_settlement_unless_its_revoked_handoffs_own_pr_merged() {
    for (case, authority, reason) in [
        (
            "unapproved",
            Authority::Unapproved,
            "awaits completion approval",
        ),
        (
            "authorized",
            Authority::Authorized,
            "holds completion authority",
        ),
        (
            "dispatched",
            Authority::RevokedWhileDispatched,
            "an owner landing job still carries",
        ),
        ("other-pr", Authority::Revoked, "recover the claim"),
        ("other-branch", Authority::Revoked, "not its landing branch"),
        ("unmerged", Authority::Revoked, "recover the claim"),
    ] {
        let fx = Fixture::new();
        let handoff = fx.handed_off(4931, authority);
        let task = handoff.task.as_str();
        match case {
            // Another PR naming the task, by the delivery identity, merged
            // into the landing branch: only the handoff's own PR counts.
            "other-pr" => fx.forge.merge_recent(4932, task, BOT, LANDING),
            "other-branch" => fx.forge.merge_recent(4931, task, BOT, &fx.base),
            "unmerged" => {
                fx.forge.0.lock().unwrap().unmerged.insert(4931, "open");
            }
            _ => fx.forge.merge_recent(4931, task, BOT, LANDING),
        }
        let refused = fx
            .runtime
            .settle_externally_merged_task(task)
            .expect_err(case)
            .to_string();
        assert!(refused.contains(reason), "{case}: {refused}");
        assert!(
            fx.runtime
                .settle_externally_merged_tasks()
                .unwrap()
                .is_empty(),
            "{case}"
        );
        assert_eq!(fx.status(task), TaskStatus::Review, "{case}");
        assert_eq!(
            fx.claim_phase(&handoff.claim_id),
            ExecutionClaimPhase::HandedOff,
            "{case}"
        );
        if authority == Authority::Authorized {
            assert!(
                fx.console_authority(task)["settle_command"].is_null(),
                "only a revoked handoff names settle-merged"
            );
        }
    }
}

#[test]
fn the_owner_store_settles_a_revoked_handoff_and_its_task_in_one_write_or_neither() {
    fn settle(
        fx: &Fixture,
        handoff: &Handoff,
        number: u64,
        branch: &str,
        mutation_id: &str,
    ) -> Result<orbit_store::contracts::ClaimMutationResult, OrbitError> {
        let operator = ClaimInvocation::trusted_operator(
            handoff.task.clone(),
            handoff.claim_id.clone(),
            "operator".into(),
        );
        fx.runtime.mutate_execution_claim(
            Some(&operator),
            mutation_id,
            &ClaimMutation::SettleExternalMerge {
                handoff_id: handoff.handoff_id.clone(),
                pull_request: number,
                base_branch: branch.into(),
                merge_commit: MERGE_COMMIT.into(),
                note: format!("settled from pull request #{number}"),
            },
        )
    }

    // Each claim holds the fixture's one file, so each needs its own owner.
    let authorized_fx = Fixture::new();
    let authorized = authorized_fx.handed_off(4933, Authority::Authorized);
    let fx = Fixture::new();
    let revoked = fx.handed_off(4934, Authority::Revoked);
    for (owner, handoff, number, branch, why) in [
        (
            &authorized_fx,
            &authorized,
            4933,
            LANDING,
            "its authority is not revoked",
        ),
        (
            &fx,
            &revoked,
            4933,
            LANDING,
            "another handoff's pull request",
        ),
        (&fx, &revoked, 4934, "main", "a merge into another branch"),
    ] {
        assert!(
            settle(
                owner,
                handoff,
                number,
                branch,
                &format!("refused-{number}-{branch}")
            )
            .is_err(),
            "{why}"
        );
        assert_eq!(owner.status(&handoff.task), TaskStatus::Review, "{why}");
        assert_eq!(
            owner.claim_phase(&handoff.claim_id),
            ExecutionClaimPhase::HandedOff,
            "a refused write leaves the claim handed off: {why}"
        );
    }

    let result = settle(&fx, &revoked, 4934, LANDING, "settle").expect("the exact settlement");
    assert_eq!(result.phase, ExecutionClaimPhase::Revoked);
    assert_eq!(result.status, TaskStatus::Done);
    assert_eq!(fx.status(&revoked.task), TaskStatus::Done);
    assert_eq!(
        fx.claim_phase(&revoked.claim_id),
        ExecutionClaimPhase::Revoked
    );
}

/// A merged delivery whose body is a repository template with no task
/// placeholder still settles, because of the generated identity comment.
/// The same body from another author, on another branch, without the token,
/// or naming only a longer id does not.
#[test]
fn a_template_body_without_a_task_placeholder_still_settles_an_external_merge() {
    let fx = Fixture::new();
    let task = fx.add_task("Fix the widget", TaskStatus::Review, &[61]);
    let bare = fx.add_task("Template text only", TaskStatus::Review, &[62]);
    let human = fx.add_task("Merged by a human", TaskStatus::Review, &[63]);
    let other_base = fx.add_task("Merged elsewhere", TaskStatus::Review, &[64]);
    let other_head = fx.add_task("Another branch", TaskStatus::Review, &[65]);
    let lookalike = fx.add_task("Lookalike id", TaskStatus::Review, &[66]);

    let identity = |id: &str| format!("<!-- orbit-delivery-task: {id} -->");
    let body = format!("## Summary\n\nFix the widget\n\n{}", identity(&task));
    let mut recent = fx.forge.0.lock().unwrap();
    recent.recent.push(FakeForge::merged(
        61,
        format!("orbit/{task}-0123abcd"),
        body.clone(),
        BOT,
        &fx.base,
    ));
    recent.recent.push(FakeForge::merged(
        62,
        format!("orbit/{bare}-0123abcd"),
        "## Summary\n\nFix the widget".to_string(),
        BOT,
        &fx.base,
    ));
    recent.recent.push(FakeForge::merged(
        63,
        format!("orbit/{human}-0123abcd"),
        format!("## Summary\n\nFix the widget\n\n{}", identity(&human)),
        "alice",
        &fx.base,
    ));
    recent.recent.push(FakeForge::merged(
        64,
        format!("orbit/{other_base}-0123abcd"),
        format!("## Summary\n\nFix the widget\n\n{}", identity(&other_base)),
        BOT,
        "release",
    ));
    recent.recent.push(FakeForge::merged(
        65,
        format!("orbit/{other_head}0-0123abcd"),
        format!("## Summary\n\nFix the widget\n\n{}", identity(&other_head)),
        BOT,
        &fx.base,
    ));
    recent.recent.push(FakeForge::merged(
        66,
        format!("orbit/{lookalike}-0123abcd"),
        format!("## Summary\n\nFix the widget\n\n<!-- orbit-delivery-task: {lookalike}0 -->"),
        BOT,
        &fx.base,
    ));
    drop(recent);

    let settlement = fx
        .runtime
        .settle_externally_merged_task(&task)
        .expect("the template body names the task");
    assert_eq!(settlement.pr_number, 61);
    assert_eq!(settlement.task_id, task);
    assert_eq!(fx.status(&task), TaskStatus::Done);

    for (id, why) in [
        (&bare, "the template text has no task token"),
        (&human, "a human author is not the delivery identity"),
        (&other_base, "it merged into another branch"),
        (&other_head, "the head branch names a longer id"),
        (&lookalike, "the body names only a longer id"),
    ] {
        assert!(
            fx.runtime.settle_externally_merged_task(id).is_err(),
            "{why}"
        );
        assert_eq!(fx.status(id), TaskStatus::Review, "{why}");
    }
}
