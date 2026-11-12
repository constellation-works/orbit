//! Shared fixture for the external-merge settlement scenarios in the
//! `runtime` integration binary: a fake forge, the owner fixture that
//! drives claims and handoffs through the owner's own mutations, the
//! sweep tick, and the git helpers for a stopped handoff's revisions.
//! Used by `task_external_merge` and its `pr_supersession` submodule.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_core::application::review::{DistributedClaimState, ExpectedCandidate};
use orbit_core::application::routines::loader::{DiscoveredWorkspaces, RoutineWorkspaceProvider};
use orbit_core::application::routines::{RoutineMachineIdentity, SweepOptions, SweepOutcome};
use orbit_core::application::task::{
    EXTERNAL_MERGE_SETTLED_EVENT, ExternalMergeSettlement, TaskAddParams,
};
use orbit_core::runtime::task_pr_forge::{
    ForgePullRequest, LeasePullRequest, MergedPullRequest, PullRequestBlockers,
    PullRequestMergeOutcome, TaskPrForge,
};
use orbit_core::{OrbitRuntime, TaskComplexity, TaskStatus};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_store::TaskCommitBoundary;
use orbit_store::contracts::{
    AdmissionIdentity, AdmissionLookup, AdmissionOrdering, AdmissionRunContext,
    AdmissionShipContract, ClaimEvidence, ClaimInvocation, ClaimMutation, ClaimRun,
    ExecutionClaimPhase, ExecutionLocation, HandoffObservation,
};
use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
use orbit_types::task::ExternalRef;
use orbit_types::workflow::ReviewTiming;
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::handoff::{
    HandoffCandidate, HandoffDelivery, HandoffReview, HandoffReviewDisposition, TaskHandoff,
};
use orbit_types::workspace::{Workspace, WorkspaceStatus};
use serde_json::json;
use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

pub(crate) const BOT: &str = "orbit-bot";
pub(crate) const MERGE_COMMIT: &str = "03f7af7d6a1b2c3d4e5f60718293a4b5c6d7e8f9";
/// The owner policy a `done` claim contract names, which acceptance matches
/// against the owner's observed completion authority.
pub(crate) const COMPLETION_POLICY: &str = "workspace-config:workflow.distributed_completion";
/// The branch a claimed handoff lands on, unlike the workspace base branch.
pub(crate) const LANDING: &str = "release-landing";

/// How far a claimed handoff's completion authority got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Authority {
    Unapproved, // `review` contract: awaits approval
    Authorized, // `done` contract: owner's landing job carries it
    Stopped,    // landing stopped, claim still holds authority
    Revoked,    // stopped, then operator-revoked
    RevokedWhileDispatched,
}

/// A claimed delivery the owner accepted as a typed handoff.
pub(crate) struct Handoff {
    pub(crate) task: String,
    pub(crate) claim_id: String,
    pub(crate) handoff_id: String,
}

/// GitHub mergeability answers a landing read can return.
#[derive(Clone, Copy)]
pub(crate) enum MergeabilityAnswer {
    Unknown,
    Conflicting,
    Mergeable,
}
pub(crate) const LAND_PR: u64 = 8801;
pub(crate) const LAND_HEAD: &str = "0dbd8e2000000000000000000000000000000000";
pub(crate) const LAND_READ_CAP: u32 = 8;

#[derive(Default)]
pub(crate) struct ForgeState {
    pub(crate) fail_merged: bool,
    pub(crate) recent: Vec<MergedPullRequest>,
    pub(crate) older: BTreeMap<u64, MergedPullRequest>,
    pub(crate) unmerged: BTreeMap<u64, &'static str>,
    pub(crate) closed: Vec<u64>,
    pub(crate) open: BTreeMap<u64, ForgePullRequest>,
    pub(crate) closure_comments: BTreeMap<u64, String>,
    pub(crate) land_pull: Option<ForgePullRequest>,
    pub(crate) land_base: String,
    pub(crate) answers: VecDeque<MergeabilityAnswer>,
    pub(crate) repeat: Option<MergeabilityAnswer>,
    pub(crate) reads: u32,
    pub(crate) merges: Vec<(u64, String)>,
    pub(crate) landed: Option<MergedPullRequest>,
}
#[derive(Default, Clone)]
pub(crate) struct FakeForge(pub(crate) Arc<Mutex<ForgeState>>);

impl FakeForge {
    pub(crate) fn merged(
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
                author: author.into(),
            },
            base_branch: base.into(),
            head_sha: "0dbd8e2000000000000000000000000000000000".into(),
            merge_commit: MERGE_COMMIT.into(),
        }
    }
    pub(crate) fn script_land(
        &self,
        task: &str,
        base: &str,
        answers: &[MergeabilityAnswer],
        repeat: MergeabilityAnswer,
    ) {
        let mut state = self.0.lock().unwrap();
        state.land_pull = Some(ForgePullRequest {
            number: LAND_PR,
            head_branch: format!("orbit/{task}-47642870"),
            body: format!("Delivers {task}.\n\nOrbit run jrun-1"),
            author: BOT.into(),
        });
        state.land_base = base.into();
        state.answers = VecDeque::from(answers.to_vec());
        state.repeat = Some(repeat);
    }
    pub(crate) fn reads(&self) -> u32 {
        self.0.lock().unwrap().reads
    }
    pub(crate) fn merges(&self) -> Vec<(u64, String)> {
        self.0.lock().unwrap().merges.clone()
    }
    pub(crate) fn blockers(pull: ForgePullRequest, conflicting: bool) -> PullRequestBlockers {
        PullRequestBlockers {
            pull,
            head_sha: LAND_HEAD.into(),
            conflicting,
            failed_required_checks: Vec::new(),
            pending_required_checks: Vec::new(),
            passed_required_checks: vec!["Test".into()],
        }
    }
    /// A delivery PR of `task_id`, merged into `base` by `author`.
    pub(crate) fn merge_recent(&self, number: u64, task_id: &str, author: &str, base: &str) {
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
    fn open_pull_requests(&self, _: &Path) -> Result<Vec<ForgePullRequest>, OrbitError> {
        let state = self.0.lock().unwrap();
        let mut pulls: Vec<_> = state.open.values().cloned().collect();
        if state.landed.is_none() {
            pulls.extend(state.land_pull.clone());
        }
        Ok(pulls)
    }
    fn lease_pull_request(
        &self,
        _: &Path,
        binding: &str,
    ) -> Result<Option<LeasePullRequest>, OrbitError> {
        let state = self.0.lock().unwrap();
        Ok(Some(LeasePullRequest {
            number: binding.trim_start_matches('#').parse().unwrap(),
            base_branch: state.land_base.clone(),
            merged: state.landed.is_some(),
        }))
    }
    fn pull_request_blockers(
        &self,
        _: &Path,
        number: u64,
    ) -> Result<Option<PullRequestBlockers>, OrbitError> {
        let mut state = self.0.lock().unwrap();
        let Some(pull) = state.land_pull.clone() else {
            return Ok(None);
        };
        if state.landed.is_some() || pull.number != number {
            return Ok(None);
        }
        state.reads += 1;
        assert!(
            state.reads <= LAND_READ_CAP,
            "mergeability reads ran past the landing budget"
        );
        match state
            .answers
            .pop_front()
            .unwrap_or_else(|| state.repeat.unwrap())
        {
            MergeabilityAnswer::Unknown => {
                Err(OrbitError::PullRequestMergeabilityUnknown { number })
            }
            MergeabilityAnswer::Conflicting => Ok(Some(Self::blockers(pull, true))),
            MergeabilityAnswer::Mergeable => Ok(Some(Self::blockers(pull, false))),
        }
    }
    fn merge_pull_request(
        &self,
        _: &Path,
        number: u64,
        head_sha: &str,
    ) -> Result<PullRequestMergeOutcome, OrbitError> {
        let mut state = self.0.lock().unwrap();
        state.merges.push((number, head_sha.into()));
        let pull = state.land_pull.clone().unwrap();
        state.landed = Some(MergedPullRequest {
            pull,
            base_branch: state.land_base.clone(),
            head_sha: head_sha.into(),
            merge_commit: MERGE_COMMIT.into(),
        });
        Ok(PullRequestMergeOutcome::Merged {
            merge_commit: MERGE_COMMIT.into(),
        })
    }
    fn merged_pull_requests(&self, _: &Path) -> Result<Vec<MergedPullRequest>, OrbitError> {
        let state = self.0.lock().unwrap();
        if state.fail_merged {
            return Err(OrbitError::Execution("forge unavailable".into()));
        }
        let mut merged = state.recent.clone();
        merged.extend(state.landed.clone());
        Ok(merged)
    }
    fn merged_pull_request(
        &self,
        _: &Path,
        number: u64,
    ) -> Result<Option<MergedPullRequest>, OrbitError> {
        let state = self.0.lock().unwrap();
        if state.fail_merged {
            return Err(OrbitError::Execution("forge unavailable".into()));
        }
        if state.unmerged.contains_key(&number) {
            return Ok(None);
        }
        Ok(state
            .recent
            .iter()
            .chain(state.older.values())
            .chain(state.landed.iter())
            .find(|m| m.pull.number == number)
            .cloned())
    }
    fn authenticated_login(&self, _: &Path) -> Result<String, OrbitError> {
        Ok(BOT.into())
    }
    fn close_pull_request(&self, _: &Path, number: u64, comment: &str) -> Result<(), OrbitError> {
        let mut state = self.0.lock().unwrap();
        state.open.remove(&number);
        state.closed.push(number);
        state.closure_comments.insert(number, comment.into());
        Ok(())
    }
}

pub(crate) struct Fixture {
    pub(crate) _root: TempDir,
    pub(crate) runtime: OrbitRuntime,
    pub(crate) forge: FakeForge,
    pub(crate) base: String,
}

impl Fixture {
    pub(crate) fn new() -> Self {
        Self::with_config("")
    }

    pub(crate) fn with_config(config: &str) -> Self {
        crate::scenarios::require_isolated_mutable_fixture("task_external_merge::Fixture::new");
        let root =
            TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).expect("create tempdir");
        let global_root = root.path().join("global");
        let repo_root = root.path().join("repo");
        let workspace_root = repo_root.join(".orbit");
        std::fs::create_dir_all(&global_root).expect("create global root");
        std::fs::create_dir_all(&workspace_root).expect("create workspace root");
        std::fs::write(repo_root.join("shared.txt"), "fixture\n").expect("write shared file");
        std::fs::write(workspace_root.join("config.toml"), config).expect("write config");
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
    pub(crate) fn add_task(&self, title: &str, status: TaskStatus, prs: &[u64]) -> String {
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
    pub(crate) fn claim(&self, task_id: &str, run_id: &str) {
        self.claim_with_phase(task_id, run_id, "running");
    }

    /// A live execution claim in `phase` on `shared.txt`.
    pub(crate) fn claim_with_phase(&self, task_id: &str, run_id: &str, phase: &str) {
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
            "phase": phase,
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
                    Utc::now().to_rfc3339() ], ) .unwrap();
    }

    /// Bind `task_id` to a running run of this process.
    pub(crate) fn bind_live_run(&self, task_id: &str) {
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

    pub(crate) fn status(&self, task_id: &str) -> TaskStatus {
        self.runtime.get_task(task_id).unwrap().status
    }

    pub(crate) fn boundary(&self) -> TaskCommitBoundary {
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
    pub(crate) fn bound_claim(
        &self,
        number: u64,
        landing: &str,
        done: bool,
    ) -> (String, String, ClaimInvocation) {
        self.bound_claim_with_mode(number, landing, done, "pr")
    }

    pub(crate) fn bound_claim_with_mode(
        &self,
        number: u64,
        landing: &str,
        done: bool,
        mode: &str,
    ) -> (String, String, ClaimInvocation) {
        let task = self.add_task("Claimed delivery", TaskStatus::Backlog, &[]);
        let boundary = self.boundary();
        let ship = AdmissionShipContract {
            mode: mode.into(),
            base_branch: self.base.clone(),
            landing_branch: landing.into(),
            before_pr: false,
            before_landing: false,
            completion: if done { "done" } else { "review" }.into(),
            authorization_reference: done.then(|| COMPLETION_POLICY.to_string()),
            review: None,
        };
        let run_context = AdmissionRunContext {
            run_id: format!("drain-{number}"),
            job_name: "workspace_pull_pipeline".into(),
            machine_name: None,
        };
        let mut request = crate::admission_test_support::admission_request(
            format!("req-{number}"),
            run_context,
            ship.clone(),
        );
        request.caller_version = "test".into();
        request.review_gate = true;
        let repo = self._root.path().join("repo");
        let location = ExecutionLocation {
            machine_id: "follower".into(),
            machine_name: None,
        };
        let identity = if mode == "local" {
            AdmissionIdentity::trusted_local(location)
        } else {
            AdmissionIdentity::trusted_remote(location)
        };
        let AdmissionLookup::Found { receipt, .. } = boundary
            .admit_task(
                &identity,
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
    pub(crate) fn released_pr(&self, number: u64, held: bool, landing: &str) -> String {
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

    pub(crate) fn handoff_obj(
        &self,
        task: &str,
        claim_id: &str,
        number: u64,
        revisions: (SourceRevision, SourceRevision),
        delivery: HandoffDelivery,
        summary: &str,
    ) -> TaskHandoff {
        TaskHandoff {
            schema_version: 1,
            workspace_id: self.runtime.workspace_id().unwrap(),
            task_id: task.into(),
            claim_id: claim_id.into(),
            machine_id: "follower".into(),
            run_id: format!("leaf-{number}"),
            candidate: HandoffCandidate {
                repository: "owner/repository".into(),
                source_branch: format!("orbit/{task}-47642870"),
                base_branch: self.base.clone(),
                landing_branch: LANDING.into(),
                candidate: revisions.0,
                base: revisions.1,
                delivery,
            },
            review: HandoffReview {
                selective_receipt: None,
                policy: ReviewTiming::None,
                disposition: HandoffReviewDisposition::NotRequired,
            },
            execution_summary: summary.into(),
            validation: vec![],
            footprint_widening: vec![],
            owed_criteria: vec![],
            regression_from: vec![],
            visual: None,
        }
    }
    pub(crate) fn handed_off(&self, number: u64, authority: Authority) -> Handoff {
        self.handed_off_with_delivery(number, authority, HandoffDelivery::PullRequest { number })
    }

    /// A follower's leaf hands off `delivery`, then follows `authority`.
    pub(crate) fn handed_off_with_delivery(
        &self,
        number: u64,
        authority: Authority,
        delivery: HandoffDelivery,
    ) -> Handoff {
        let authorized = authority != Authority::Unapproved;
        let mode = if matches!(&delivery, HandoffDelivery::LocalCandidate) {
            "local"
        } else {
            "pr"
        };
        let (task, claim_id, worker) =
            self.bound_claim_with_mode(number, LANDING, authorized, mode);
        let (candidate_revision, base_revision) = if authority == Authority::Stopped {
            let repo = self.runtime.paths().repo_root.clone();
            settlement_git(&repo, &["init", "-q", "-b", &self.base]);
            settlement_git(&repo, &["add", "shared.txt"]);
            settlement_git(&repo, &["commit", "-qm", "base"]);
            let base = settlement_revision(&repo, "HEAD");
            if self.base != LANDING {
                settlement_git(&repo, &["branch", LANDING]);
            }
            settlement_git(
                &repo,
                &["checkout", "-qb", &format!("orbit/{task}-47642870")],
            );
            std::fs::write(repo.join("delivery.txt"), "candidate\n").unwrap();
            settlement_git(&repo, &["add", "delivery.txt"]);
            settlement_git(&repo, &["commit", "-qm", "candidate"]);
            let candidate = settlement_revision(&repo, "HEAD");
            settlement_git(&repo, &["checkout", "-q", &self.base]);
            (candidate, base)
        } else {
            (rev("a", "b"), rev("c", "d"))
        };
        let handoff = self.handoff_obj(
            &task,
            &claim_id,
            number,
            (candidate_revision, base_revision),
            delivery,
            "Outcome: success",
        );
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
            Authority::Revoked | Authority::RevokedWhileDispatched | Authority::Stopped
        ) {
            landing(
                "dispatch",
                ClaimMutation::DispatchLanding {
                    handoff_id: handoff_id.clone(),
                    job_run_id: None,
                },
            );
        }
        if matches!(authority, Authority::Revoked | Authority::Stopped) {
            landing(
                "stop",
                ClaimMutation::StopLanding {
                    handoff_id: handoff_id.clone(),
                    reason: json!({
                        "schema_version": 1,
                        "kind": "owner_landing_refusal",
                        "reason": {"value": "the pull request's required checks are not green"},
                        "observation": {"classification": {"value": "blocked"}},
                    })
                    .to_string(),
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
                    "operator-revoke",
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
    pub(crate) fn claim_phase(&self, claim_id: &str) -> ExecutionClaimPhase {
        self.runtime
            .inspect_execution_claims()
            .unwrap()
            .into_iter()
            .find(|state| state.claim.claim_id == claim_id)
            .unwrap_or_else(|| panic!("no claim {claim_id}"))
            .claim
            .phase
    }

    // -- shared scenario helpers --
    pub(crate) fn merge(&self, n: u64, task: &str, author: &str, base: &str) {
        self.forge.merge_recent(n, task, author, base);
    }
    pub(crate) fn push_merged(&self, n: u64, head: String, body: String, author: &str, base: &str) {
        self.forge
            .0
            .lock()
            .unwrap()
            .recent
            .push(FakeForge::merged(n, head, body, author, base));
    }
    pub(crate) fn mark_unmerged(&self, n: u64, kind: &'static str) {
        self.forge.0.lock().unwrap().unmerged.insert(n, kind);
    }
    pub(crate) fn set_forge_down(&self, down: bool) {
        self.forge.0.lock().unwrap().fail_merged = down;
    }
    pub(crate) fn settle_one(&self, task: &str) -> ExternalMergeSettlement {
        self.runtime
            .settle_externally_merged_task(task)
            .expect("task settles")
    }
    pub(crate) fn settle_err(&self, task: &str, case: &str) -> String {
        self.runtime
            .settle_externally_merged_task(task)
            .expect_err(case)
            .to_string()
    }
    pub(crate) fn settle_all(&self) -> Vec<ExternalMergeSettlement> {
        self.runtime
            .settle_externally_merged_tasks()
            .expect("owner pass")
    }
    pub(crate) fn assert_status(&self, task: &str, want: TaskStatus, case: &str) {
        assert_eq!(self.status(task), want, "{case}");
    }
    pub(crate) fn assert_phase(&self, claim: &str, want: ExecutionClaimPhase, case: &str) {
        assert_eq!(self.claim_phase(claim), want, "{case}");
    }
    pub(crate) fn assert_done(&self, task: &str, case: &str) {
        self.assert_status(task, TaskStatus::Done, case);
    }
    pub(crate) fn note_has(&self, task: &str, parts: &[&str], case: &str) {
        let note = self.settled_note(task);
        for part in parts {
            assert!(note.contains(part), "{case}: missing {part}: {note}");
        }
    }
    pub(crate) fn history_to(&self, task: &str, to: TaskStatus, case: &str) {
        assert_eq!(self.settled_entry(task).to_status, Some(to), "{case}");
    }
    pub(crate) fn history_is(&self, task: &str, from: TaskStatus, to: TaskStatus, case: &str) {
        let entry = self.settled_entry(task);
        assert_eq!(
            (entry.from_status, entry.to_status),
            (Some(from), Some(to)),
            "{case}"
        );
    }
    pub(crate) fn settled_entry(&self, task: &str) -> orbit_types::task::TaskHistoryEntry {
        self.runtime
            .get_task_history(task)
            .unwrap()
            .into_iter()
            .find(|e| e.event == EXTERNAL_MERGE_SETTLED_EVENT)
            .expect("settlement recorded in history")
    }
    pub(crate) fn settled_note(&self, task: &str) -> String {
        self.settled_entry(task).note.unwrap_or_default()
    }
    pub(crate) fn assert_no_settled(&self, task: &str, case: &str) {
        assert!(
            !self
                .runtime
                .get_task_history(task)
                .unwrap()
                .iter()
                .any(|e| e.event == EXTERNAL_MERGE_SETTLED_EVENT),
            "{case}"
        );
    }
    pub(crate) fn older_merge(&self, n: u64, task: &str) {
        let base = self.base.clone();
        self.forge.0.lock().unwrap().older.insert(
            n,
            FakeForge::merged(
                n,
                format!("orbit/{task}-aaaa1111"),
                format!("Delivers {task}."),
                BOT,
                &base,
            ),
        );
    }
    /// The owner console's completion authority for `task`'s handoff.
    pub(crate) fn console_authority(&self, task: &str) -> serde_json::Value {
        let console = self
            .runtime
            .distributed_claim_console_filtered(Some(task), DistributedClaimState::All, true)
            .unwrap();
        console["claims"][0]["handoff"]["authority"].clone()
    }
}

pub(crate) struct TickProvider {
    pub(crate) runtime: OrbitRuntime,
    pub(crate) replica: bool,
}

impl RoutineWorkspaceProvider for TickProvider {
    fn discover_workspaces(&self, _: &Path) -> Result<DiscoveredWorkspaces, OrbitError> {
        let workspace = Workspace {
            id: self.runtime.workspace_id()?,
            name: "tick-workspace".into(),
            owner_machine_id: None,
            git_remote: None,
            ship_mode: None,
            base_branch: "main".into(),
            status: WorkspaceStatus::Active,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let pair = (workspace, self.runtime.clone());
        if self.replica {
            Ok(DiscoveredWorkspaces {
                replicas: vec![pair],
                ..DiscoveredWorkspaces::default()
            })
        } else {
            Ok(DiscoveredWorkspaces {
                entries: vec![pair],
                ..DiscoveredWorkspaces::default()
            })
        }
    }
}

pub(crate) fn tick(fx: &Fixture, dry_run: bool, replica: bool) -> SweepOutcome {
    orbit_core::test_support::run_sweep_at(
        &fx.runtime.global_root(),
        SweepOptions {
            dry_run,
            ..SweepOptions::default()
        },
        RoutineMachineIdentity {
            machine_id: "tick-machine".into(),
            machine_name: "tick-host".into(),
        },
        &TickProvider {
            runtime: fx.runtime.clone(),
            replica,
        },
        Utc::now(),
    )
    .expect("the tick runs")
}

pub(crate) fn rev(commit: &str, tree: &str) -> SourceRevision {
    SourceRevision {
        commit: commit.repeat(40),
        tree: tree.repeat(40),
    }
}
pub(crate) fn settlement_git(repo: &Path, args: &[&str]) -> String {
    let output = crate::git_test_support::command()
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

pub(crate) fn settlement_revision(repo: &Path, reference: &str) -> SourceRevision {
    SourceRevision {
        commit: settlement_git(repo, &["rev-parse", reference]),
        tree: settlement_git(repo, &["rev-parse", &format!("{reference}^{{tree}}")]),
    }
}

pub(crate) fn tree_of(repo: &Path, rev: &str) -> String {
    settlement_git(repo, &["rev-parse", &format!("{rev}^{{tree}}")])
}
pub(crate) fn commit_tree(repo: &Path, tree: &str, parents: &[&str], msg: &str) -> String {
    let mut args = vec!["commit-tree", tree];
    for parent in parents {
        args.extend(["-p", parent]);
    }
    args.extend(["-m", msg]);
    settlement_git(repo, &args)
}
pub(crate) fn update_ref(repo: &Path, name: &str, value: &str) {
    settlement_git(repo, &["update-ref", name, value]);
}

pub(crate) fn mergeability_land(
    answers: &[MergeabilityAnswer],
    repeat: MergeabilityAnswer,
) -> (Fixture, FakeForge, String) {
    let fx = Fixture::new();
    let task = fx.add_task("Mergeability land", TaskStatus::Review, &[LAND_PR]);
    fx.forge
        .script_land(&task, &fx.base.clone(), answers, repeat);
    let forge = fx.forge.clone();
    (fx, forge, task)
}

pub(crate) fn virtual_mergeability_wait() -> orbit_common::test_env::ScopedEnv {
    orbit_common::test_env::scoped([("ORBIT_TEST_MERGEABILITY_WAIT", Some("virtual"))])
}
