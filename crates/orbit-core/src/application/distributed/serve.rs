//! Owner-served mutating half of the distributed drain [ORB-13625]: pull
//! admission, run binding and claim settlement.
//!
//! # Authority
//!
//! These are the entry points a follower's drain reaches over federated MCP,
//! so every authority fact is read from the trusted session the transport
//! built, exactly as the read-only half does:
//!
//! - **Access** is SSH login plus the `agent` or `operator` capability the
//!   governed-operation rows require [ORB-12564]. There is no callers file.
//! - **Attempt ownership** is the claim journal's own fence. Every mutation
//!   here reaches it as a [`ClaimInvocation`] whose machine is the session's
//!   trusted caller machine, never a machine named in tool input. A follower
//!   can only name *which* claim it is settling; the journal refuses it unless
//!   that claim was admitted to this same machine and is still in a phase
//!   that permits the write.
//! - **Observations** are the owner's own. A handoff payload names the
//!   candidate to look at; the owner reads the published pull request from the
//!   provider and resolves both commits in its own checkout before the journal
//!   compares them.
//!
//! A remote executor is refused local ship mode by the admission ladder and a
//! local-candidate handoff here: followers never run owner-local leaves, so
//! only the owner may hand off a candidate that exists solely in its checkout.

use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_store::TaskCommitBoundary;
use orbit_store::contracts::{
    AdmissionIdentity, AdmissionLookup, AdmissionReceipt, AdmissionRequest, ClaimInvocation,
    ClaimMutation, ClaimMutationResult, ClaimRun, ExecutionClaim, ExecutionClaimPhase,
    HandoffObservation, HandoffReviewObservation, JobRunQuery,
};
use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
use orbit_types::task::TaskStatus;
use orbit_types::tool::ToolSessionContext;
use orbit_types::workflow::{
    JobRunState,
    handoff::{HandoffCandidate, HandoffDelivery, TaskHandoff},
};
use serde::Serialize;
use serde_json::Value;

use crate::application::automation::source::Source;

use super::contract::{is_remote, session_machine_id, trusted_identity};
use super::{ensure_distributed_mutation_available, owner_binary_version};

/// What `orbit.task.pull` answers: the immutable receipt and, separately, the
/// claim's phase right now. A replayed receipt is historical evidence; the
/// current phase is what says whether the attempt may still execute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskPullResponse {
    pub receipt: AdmissionReceipt,
    pub claim_state: Option<ExecutionClaimPhase>,
}

/// The mutation ids a follower's retries replay under. One per claim and
/// operation, so a lost answer retried with the same claim returns the
/// recorded outcome instead of performing the write again, and a second,
/// different run can never be bound to the same claim.
fn bind_mutation_id(claim_id: &str) -> String {
    format!("pull-bind:{claim_id}")
}
fn fail_mutation_id(claim_id: &str) -> String {
    format!("pull-fail:{claim_id}")
}
fn release_mutation_id(claim_id: &str) -> String {
    format!("pull-release:{claim_id}")
}
fn handoff_mutation_id(claim_id: &str) -> String {
    format!("pull-handoff:{claim_id}")
}

fn refused(message: impl Into<String>) -> OrbitError {
    OrbitError::PolicyDenied(message.into())
}

impl crate::OrbitRuntime {
    /// Serve one pull admission for a caller this session speaks for.
    ///
    /// Refusal order follows the spec: the distributed gate and destination
    /// authority first, then the trusted caller identity, then the store's
    /// shape/version/mode/policy ladder inside [`TaskCommitBoundary::admit_task`].
    /// A new request must carry the ship contract this owner resolves now — the
    /// one the probe reported — because the receipt freezes it; a replay keeps
    /// whatever its original request carried and is compared byte-for-byte.
    pub fn serve_task_pull(
        &self,
        session: &ToolSessionContext,
        request: &AdmissionRequest,
    ) -> Result<TaskPullResponse, OrbitError> {
        ensure_distributed_mutation_available("orbit.task.pull")?;
        self.ensure_distributed_owner_workspace()?;
        let identity = self.session_admission_identity(session)?;
        let boundary = self.admission_boundary()?;
        if matches!(
            boundary.lookup_admission(&identity, &request.request_id)?,
            AdmissionLookup::NotFound
        ) {
            let owner = self.owner_ship_contract();
            if request.ship != owner {
                return Err(OrbitError::InvalidInput(format!(
                    "ship_contract_mismatch: this owner now resolves mode '{}', base '{}', landing \
                     '{}', review.before_pr {}, completion '{}'; re-read the probe before \
                     sending a new request",
                    owner.mode,
                    owner.base_branch,
                    owner.landing_branch,
                    super::contract::on_off(owner.before_pr),
                    owner.completion
                )));
            }
        }
        match self.admit_pull_request(&boundary, &identity, request)? {
            AdmissionLookup::Found {
                receipt,
                current_claim,
            } => Ok(TaskPullResponse {
                receipt: *receipt,
                claim_state: current_claim.map(|claim| claim.phase),
            }),
            AdmissionLookup::Expired => Err(OrbitError::InvalidInput("request_expired".into())),
            AdmissionLookup::NotFound => Err(OrbitError::Store(
                "admission committed no receipt for this request".into(),
            )),
        }
    }

    /// Bind the follower's one local leaf run to its claim, `claimed → running`.
    ///
    /// Idempotent per claim: the journal replays a repeated bind of the same
    /// run and refuses a different one, so a lost answer is safe to retry and a
    /// second leaf can never take over the attempt.
    pub fn serve_claim_bind(
        &self,
        session: &ToolSessionContext,
        claim_id: &str,
        run_id: &str,
        ship: orbit_store::contracts::AdmissionShipContract,
    ) -> Result<ClaimMutationResult, OrbitError> {
        ensure_distributed_mutation_available("orbit.drain.claim.bind")?;
        self.ensure_distributed_owner_workspace()?;
        let machine = self.session_caller_machine(session)?;
        let claim = self.current_claim(claim_id)?;
        let run_id = run_id.trim();
        if run_id.is_empty() {
            return Err(OrbitError::InvalidInput("`run_id` is required".into()));
        }
        // The invocation carries no run yet: binding is what creates that
        // association, so asserting one beforehand would fence the very
        // mutation being made.
        let context = ClaimInvocation::trusted_worker(
            claim.task_id.clone(),
            claim.claim_id.clone(),
            machine.clone(),
            None,
        );
        self.mutate_execution_claim(
            Some(&context),
            &bind_mutation_id(&claim.claim_id),
            &ClaimMutation::Bind {
                run: ClaimRun {
                    machine_id: machine,
                    run_id: run_id.to_string(),
                },
                ship,
            },
        )
    }

    /// Settle a claim with the follower's durable settlement: a typed handoff
    /// or a failure. Anything else is a lifecycle operation the executor does
    /// not own — approval, revocation and recovery stay owner-operator acts.
    ///
    /// A failure may carry the leaf's final-recovery decision [ORB-13907]; the
    /// owner applies it to its own task once the claim has failed.
    pub fn serve_claim_settle(
        &self,
        session: &ToolSessionContext,
        claim_id: &str,
        run_id: Option<&str>,
        settlement: ClaimMutation,
    ) -> Result<ClaimMutationResult, OrbitError> {
        ensure_distributed_mutation_available("orbit.drain.claim.settle")?;
        self.ensure_distributed_owner_workspace()?;
        let machine = self.session_caller_machine(session)?;
        let claim = self.current_claim(claim_id)?;
        let run = run_id
            .map(str::trim)
            .filter(|run| !run.is_empty())
            .map(|run_id| ClaimRun {
                machine_id: machine.clone(),
                run_id: run_id.to_string(),
            });
        let context = ClaimInvocation::trusted_worker(
            claim.task_id.clone(),
            claim.claim_id.clone(),
            machine,
            run,
        );
        match settlement {
            ClaimMutation::Fail(evidence) => {
                let final_recovery = evidence.final_recovery.clone();
                let result = self.mutate_execution_claim(
                    Some(&context),
                    &fail_mutation_id(&claim.claim_id),
                    &ClaimMutation::Fail(evidence),
                )?;
                if let Some(final_recovery) = final_recovery {
                    self.apply_settled_final_recovery(&claim, run_id, &result, &final_recovery);
                }
                Ok(result)
            }
            ClaimMutation::Release(evidence) => self.mutate_execution_claim(
                Some(&context),
                &release_mutation_id(&claim.claim_id),
                &ClaimMutation::Release(evidence),
            ),
            ClaimMutation::AcceptHandoff(handoff) => {
                let observation = self.observe_claim_handoff(&handoff, is_remote(session))?;
                self.accept_task_handoff(
                    &context,
                    &handoff_mutation_id(&claim.claim_id),
                    handoff,
                    observation,
                )
            }
            _ => Err(refused(
                "only a typed handoff, a failure or a release settles a claimed leaf; approval, \
                 revocation and recovery are owner-operator actions",
            )),
        }
    }

    /// The owner's own reading of the candidate a claim is settling.
    ///
    /// Read from the owner checkout — and, for a published delivery, from the
    /// provider — with the shared observation rules, so the worker's handoff
    /// payload contributes nothing but the identity to look *at*. The claim
    /// journal then compares this observation against the submitted candidate.
    ///
    /// `remote` refuses a local candidate: it exists only in the executor's
    /// checkout, which the owner cannot read, and followers never run local
    /// mode. NoDiff verifies a clean-tree checkpoint against the owner's live
    /// base; the executor's branch need not be published.
    pub(crate) fn observe_claim_handoff(
        &self,
        handoff: &TaskHandoff,
        remote: bool,
    ) -> Result<HandoffObservation, OrbitError> {
        let claim = self.current_claim(&handoff.claim_id)?;
        let AdmissionLookup::Found { receipt, .. } = self.admission_boundary()?.lookup_admission(
            &AdmissionIdentity::trusted_local(claim.executed_on.clone()),
            &claim.request_id,
        )?
        else {
            return Err(refused("original claim receipt unavailable"));
        };
        let candidate = match handoff.candidate.delivery {
            HandoffDelivery::NoDiff { .. } => orbit_engine::observe_no_diff_candidate(
                self,
                &self.paths().repo_root,
                handoff,
                if receipt.request.ship.mode == "pr" {
                    "remote"
                } else {
                    "local"
                },
            )?,
            HandoffDelivery::LocalCandidate if remote => {
                return Err(refused(
                    "a follower cannot hand off a local candidate: followers never execute \
                     owner-local leaves, and the owner cannot observe a candidate that exists \
                     only in the executor's checkout",
                ));
            }
            HandoffDelivery::LocalCandidate => orbit_engine::observe_candidate(
                &self.paths().repo_root,
                Some(&handoff.candidate.source_branch),
                &handoff.candidate.base_branch,
                &handoff.candidate.landing_branch,
                HandoffDelivery::LocalCandidate,
                &handoff.workspace_id,
                // An owner-local candidate has no origin to fetch and must
                // keep reading the local base it was synchronized onto.
                "local",
            )?,
            // [ORB-12500] The owner reads the published pull request itself:
            // the provider names the delivery, and the candidate and base
            // objects are resolved in this checkout.
            HandoffDelivery::PullRequest { .. } => orbit_engine::observe_published_candidate(
                self,
                &self.paths().repo_root,
                &handoff.candidate,
            )?,
            HandoffDelivery::AlreadyLanded { .. } => {
                return Err(refused(
                    "already-landed delivery carries its own typed report through the no-diff \
                     verifier; a claimed leaf does not hand one off",
                ));
            }
        };
        let original = receipt
            .claim
            .as_ref()
            .ok_or_else(|| refused("original claim footprint unavailable"))?;
        let (new_paths, footprint_widening) = orbit_engine::validate_claim_new_paths(
            &self.paths().repo_root,
            &original.footprint,
            &candidate.base.commit,
            &candidate.candidate.commit,
        )?;
        for path in &new_paths {
            let decision = self.policy_engine().check(
                "implementer",
                orbit_types::policy::FsOperation::Modify,
                path.clone(),
            )?;
            if !decision.allowed {
                return Err(refused(format!(
                    "footprint widening refused protected path: {path}"
                )));
            }
        }
        let review = self.observe_handoff_review(handoff, &candidate)?;
        // An empty list is no required check: the handoff is accepted with no
        // validation logs, the way the owner's own delivery runs none.
        Ok(HandoffObservation {
            footprint_widening,
            candidate,
            required_commands: self.workflow_required_validation_commands().to_vec(),
            owner_completion_authority: self.owner_completion_authority(),
            review,
        })
    }

    /// The owner's reading of the facts a before-PR certificate stands on
    /// [ORB-13895]: whether the reviewed base is in the history of the base
    /// the owner observed the candidate on, and the repository identity its
    /// coverage matches certificates against. `None` for a handoff carrying
    /// no before-PR evidence; the claim journal judges the rest.
    fn observe_handoff_review(
        &self,
        handoff: &TaskHandoff,
        candidate: &HandoffCandidate,
    ) -> Result<Option<HandoffReviewObservation>, OrbitError> {
        let Some(evidence) = handoff.review.before_pr() else {
            return Ok(None);
        };
        let repo_root = &self.paths().repo_root;
        let repository = Source::new(repo_root)
            .repository()
            .map_err(orbit_automation::automation_error_to_orbit)?;
        Ok(Some(HandoffReviewObservation {
            reviewed_base_sha: evidence.reviewed_base_sha.clone(),
            reviewed_base_is_ancestor: orbit_engine::review_gate::contains_commit(
                repo_root,
                &evidence.reviewed_base_sha,
                &candidate.base.commit,
            )?,
            repository,
        }))
    }

    /// One admission on this owner's commit boundary. Shared by the routed
    /// tool above and the owner-local drain adapter, so both reach the same
    /// transaction with the same owner version and repository roots.
    pub(crate) fn admit_pull_request(
        &self,
        boundary: &TaskCommitBoundary,
        identity: &AdmissionIdentity,
        request: &AdmissionRequest,
    ) -> Result<AdmissionLookup, OrbitError> {
        let mut admission_holds = self
            .live_local_delivery_runs()?
            .into_iter()
            .map(|(task, run)| {
                (
                    task,
                    format!("live local delivery run {run} is carrying it"),
                )
            })
            .collect::<BTreeMap<_, _>>();
        for (task, runs) in
            crate::application::automation::preparation::active_task_pilot_preparations(self)?
        {
            admission_holds.entry(task).or_insert_with(|| {
                format!(
                    "active task-pilot preparation holds it: {}",
                    runs.into_iter().collect::<Vec<_>>().join(", ")
                )
            });
        }
        boundary.admit_task(
            identity,
            request,
            owner_binary_version(),
            &self.paths().repo_root,
            &self.data_root(),
            &admission_holds,
            &self.baseline_held_tasks()?,
        )
    }

    /// Each `backlog` task a red base still holds, mapped to why
    /// [ORB-14258]. Read before the admission lock: the check consults Git
    /// (and may refresh the base from `origin`), and a hold that lifts a
    /// moment late only defers the task to the next request.
    fn baseline_held_tasks(&self) -> Result<BTreeMap<String, String>, OrbitError> {
        let mut held = BTreeMap::new();
        for task in
            self.list_tasks_filtered(Some(TaskStatus::Backlog), None, None, None, None, None)?
        {
            match self.standing_baseline_hold(&task) {
                Ok(Some(why)) => {
                    held.insert(task.id.clone(), why);
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(task_id = %task.id, "could not read baseline red hold: {error}");
                }
            }
        }
        Ok(held)
    }

    /// Each task a live run on this owner holds the delivery slot of, mapped
    /// to that run [ORB-13918].
    ///
    /// A local drain admits a task by dispatching a wrapper and gate that
    /// carry it in `input.task_ids`; the task stays `backlog`, and reserves
    /// nothing, until the gate gets its context locks. Those runs are the only
    /// record of the admission, so pull admission reads them — the same
    /// `spec.task_delivery` holders `orbit run ship` refuses a duplicate
    /// against — rather than handing the task to a follower as well.
    fn live_local_delivery_runs(&self) -> Result<BTreeMap<String, String>, OrbitError> {
        let delivery_jobs = self.task_delivery_job_ids()?;
        let jobs = self.stores().jobs();
        let mut runs = jobs.list_job_runs_filtered(&JobRunQuery {
            active_only: true,
            include_steps: false,
            ..JobRunQuery::default()
        })?;
        // `active_only` intentionally means pending/running across the store
        // API. Retrying is also a live run state, so include it separately
        // while the runner sleeps between attempts.
        runs.extend(jobs.list_job_runs_filtered(&JobRunQuery {
            state: Some(JobRunState::Retrying),
            include_steps: false,
            ..JobRunQuery::default()
        })?);
        let mut carried = BTreeMap::new();
        for run in runs
            .into_iter()
            .filter(|run| delivery_jobs.contains(&run.job_id))
        {
            let task_ids = run
                .input
                .as_ref()
                .and_then(|input| input.get("task_ids"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str);
            for task_id in task_ids {
                carried
                    .entry(task_id.to_string())
                    .or_insert_with(|| run.run_id.clone());
            }
        }
        Ok(carried)
    }

    pub(crate) fn admission_boundary(&self) -> Result<TaskCommitBoundary, OrbitError> {
        TaskCommitBoundary::new(
            self.sqlite_store()?,
            TaskRegistryStore::open(&task_registry_path(&self.global_root()))?,
            self.workspace_id()?,
        )
    }

    /// The machine this session speaks for. Required: a claim is fenced on it,
    /// so a session the transport could not attribute gets no admission.
    fn session_caller_machine(&self, session: &ToolSessionContext) -> Result<String, OrbitError> {
        session_machine_id(session).ok_or_else(|| {
            OrbitError::InvalidInput(
                "no trusted caller machine on this session; a follower reaches \
                 the owner through federated SSH, which names its machine"
                    .into(),
            )
        })
    }

    fn session_admission_identity(
        &self,
        session: &ToolSessionContext,
    ) -> Result<AdmissionIdentity, OrbitError> {
        let machine = self.session_caller_machine(session)?;
        Ok(trusted_identity(&machine, session))
    }

    /// The claim a follower names, read from the journal. Absence is the
    /// stale-attempt answer: a revoked or superseded claim is still listed, so
    /// an id that resolves to nothing was never this owner's.
    fn current_claim(&self, claim_id: &str) -> Result<ExecutionClaim, OrbitError> {
        let claim_id = claim_id.trim();
        if claim_id.is_empty() {
            return Err(OrbitError::InvalidInput("`claim_id` is required".into()));
        }
        self.inspect_execution_claims()?
            .into_iter()
            .map(|inspection| inspection.claim)
            .find(|claim| claim.claim_id == claim_id)
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "stale_claim: claim '{claim_id}' is not held by this owner workspace"
                ))
            })
    }
}
