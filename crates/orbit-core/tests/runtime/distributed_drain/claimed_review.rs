//! A claimed leaf runs the before-PR review its claim captured [ORB-13908].
//!
//! The follower's drain admits a task from an owner with `review.before_pr`
//! on. The leaf's gate steps then run on the follower under the claim's
//! worker binding, exactly as the claimed pipeline dispatches them; the
//! reviewer agent is stood in for by its worktree fix and the report it
//! persists through the same binding. Every task write the gate makes crosses
//! that binding to the owner's task, and the owner judges the handoff's
//! review evidence against its own copies.

use super::*;

use orbit_store::contracts::{HandoffReviewObservation, ReviewInvocationRecord};
use orbit_types::task::TaskStatus;
use orbit_types::tool::WorkerInvocation;
use orbit_types::workflow::handoff::HandoffReviewEvidence;
use orbit_types::workflow::{
    FindingDisposition, REVIEW_ADMISSION_KEY, REVIEW_CONTRACT_VERSION, REVIEW_GATE_ARTIFACT,
    REVIEW_MANIFEST_ARTIFACT, REVIEW_REPORT_ARTIFACT, REVIEW_REPORT_HISTORY_ARTIFACT,
    ReviewAdmission, ReviewEvidenceHold, ReviewEvidenceKind, ReviewEvidenceRequirement,
    ReviewFinding, ReviewReport, ReviewReportHistory, ReviewValidation, ReviewVerdict,
    ReviewerInvocationEvent, ValidationOutcome, ValidationRole,
};
// Used only by the Linux-gated CodeQL hold test below.
#[cfg(target_os = "linux")]
use orbit_types::workflow::REVIEW_EVIDENCE_HOLD_ARTIFACT;

/// A crew every runtime's default registry resolves.
pub(super) const REVIEW_CREW: &str = "sol";
const REPOSITORY: &str = "owner/repository";

fn before_pr_owner(crew: &str) -> String {
    format!("[review]\nbefore_pr = true\n\n[operation]\nreview_crew = \"{crew}\"\n")
}

/// Hands a bound leaf's coordination calls to the owner in process, under
/// the follower's SSH session, as the owner's MCP server receives them.
pub(super) struct ToOwner(pub(super) OrbitRuntime);

impl OwnerCoordinator for ToOwner {
    fn call(
        &self,
        name: &str,
        mut input: Value,
        mut session: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        let binding = session.worker_invocation.clone().expect("worker binding");
        input["workspace"] = json!(binding.owner_workspace_id);
        session.workspace = Some(binding.owner_workspace_id);
        session.caller_machine_id = Some(FOLLOWER.to_string());
        session.transport = Some(McpTransport::SshMcp);
        session.effective_capabilities = BTreeSet::from([McpCapability::Agent]);
        self.0.execute_owner_coordination(name, input, session)
    }
}

/// A claimed leaf on the follower, admitted from a before-PR owner and bound,
/// with its implementation committed in the follower's checkout.
pub(super) struct ReviewedLeaf {
    pub(super) pair: Pair,
    pub(super) drain: String,
    pub(super) leaf: String,
    pub(super) task: String,
    /// The follower runtime bound to the leaf's claim, as its worker runs.
    pub(super) bound: OrbitRuntime,
    pub(super) base: SourceRevision,
    pub(super) gate_input: Value,
}

impl ReviewedLeaf {
    pub(super) fn admit() -> Self {
        Self::admit_with_follower_config("")
    }

    /// [`Self::admit`] with `follower_config` as the follower's workspace
    /// `config.toml`.
    pub(super) fn admit_with_follower_config(follower_config: &str) -> Self {
        Self::admit_with_configs("", follower_config)
    }

    /// [`Self::admit_with_follower_config`] with `owner_config` appended to
    /// the before-PR owner's workspace `config.toml`.
    pub(super) fn admit_with_configs(owner_config: &str, follower_config: &str) -> Self {
        Self::admit_from(
            &format!("{}{owner_config}", before_pr_owner(REVIEW_CREW)),
            follower_config,
        )
    }

    /// A leaf admitted from an owner with `review.before_landing` on
    /// [ORB-14849].
    pub(super) fn admit_before_landing() -> Self {
        Self::admit_from(
            &format!(
                "[review]\nbefore_landing = true\n\n[operation]\nreview_crew = \"{REVIEW_CREW}\"\n"
            ),
            "",
        )
    }

    /// A leaf admitted from an owner whose workspace `config.toml` is
    /// `owner_config`.
    pub(super) fn admit_from(owner_config: &str, follower_config: &str) -> Self {
        let pair = Pair::with_configs(owner_config, follower_config, &[None]);
        let drain = pair.run_drain();
        let leaf = pair.launched_leaf(&drain, 1, std::process::id());
        let task = pair.claimed_task(&leaf);

        let repo = pair.follower_repo.clone();
        std::fs::write(repo.join(".gitignore"), "/.orbit/\n").unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/f0.rs"), "fn work() {}\n").unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "baseline"]);
        git(
            &repo,
            &[
                "remote",
                "add",
                "origin",
                &format!("https://github.com/{REPOSITORY}.git"),
            ],
        );
        git(&repo, &["config", "credential.interactive", "never"]);
        let base = revision(&repo, "HEAD");
        git(&repo, &["checkout", "-q", "-b", &format!("orbit/{task}")]);
        std::fs::write(repo.join("src/f0.rs"), "fn work() { todo!() }\n").unwrap();
        git(&repo, &["commit", "-q", "-am", "Implement"]);

        let record = pair.admission(&leaf);
        let claim = record.receipt.as_ref().unwrap().claim.clone().unwrap();
        let bound = pair
            .follower
            .clone()
            .with_worker_invocation(
                WorkerInvocation {
                    owner_machine_id: OWNER.into(),
                    owner_workspace_id: record.destination.owner_workspace_id.clone(),
                    owner_destination: record.destination.selector.clone(),
                    task_id: claim.task_id.clone(),
                    claim_id: claim.claim_id.clone(),
                    execution: claim.executed_on.clone(),
                    bound_run_id: leaf.clone(),
                },
                Arc::new(ToOwner(pair.wire.owner.clone())),
            )
            .unwrap();
        // What `task_claimed_pr_pipeline` passes both gate steps, with the
        // run the dispatcher injects.
        let gate_input = json!({
            "run_id": leaf,
            "job_run_id": leaf,
            "completed_task_ids": [task],
            "workspace_path": repo,
            "base": "main",
            "base_sync": "local",
            "mode": "pr",
            "skipped_no_diff_expected": false,
        });
        Self {
            pair,
            drain,
            leaf,
            task,
            bound,
            base,
            gate_input,
        }
    }

    pub(super) fn admit_review(&mut self) -> Value {
        let admission = self
            .bound
            .run_deterministic(
                "review_gate_admit",
                &json!({}),
                &self.gate_input,
                ToolContext::default(),
            )
            .expect("review admitted");
        self.gate_input["admission"] = admission.clone();
        admission
    }

    /// The reviewer's work: its fix in the worktree, when it made one, and
    /// its report, persisted through the leaf's binding as the reviewer's
    /// tool call is.
    pub(super) fn reviewer_reports(&self, attempt_id: &str, verdict: ReviewVerdict, fix: bool) {
        if fix {
            std::fs::write(
                self.pair.follower_repo.join("src/f0.rs"),
                "fn work() {}\n// reviewed\n",
            )
            .unwrap();
        }
        let report = ReviewReport {
            external_evidence: Vec::new(),
            schema_version: REVIEW_CONTRACT_VERSION,
            attempt_id: attempt_id.into(),
            verdict,
            summary: "Checked the change against the criteria.".into(),
            findings: vec![ReviewFinding {
                id: "F1".into(),
                severity: "medium".into(),
                summary: "The stub panics".into(),
                paths: vec!["src/f0.rs".into()],
                disposition: if fix {
                    FindingDisposition::Repaired
                } else {
                    FindingDisposition::Open
                },
                change: fix.then(|| "Replaced the stub".into()),
            }],
            validation: vec![ReviewValidation {
                id: Some("V1".into()),
                command: "make ci-fast".into(),
                outcome: ValidationOutcome::Passed,
                role: ValidationRole::Required,
                note: None,
                check: None,
                control: None,
                sources: Vec::new(),
                mutation_target: Vec::new(),
                deferred: Vec::new(),
                baseline: None,
            }],
            retired_validation: Vec::new(),
            escalation: (verdict == ReviewVerdict::Reject)
                .then(|| "decide whether the stub may ship".into()),
        };
        self.put_report(&report);
    }

    /// The reviewer's report, persisted through the leaf's binding as the
    /// reviewer's tool call is.
    pub(super) fn put_report(&self, report: &ReviewReport) {
        let source = self
            .pair
            .follower_repo
            .join(".orbit/tmp")
            .join(REVIEW_REPORT_ARTIFACT);
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, serde_json::to_vec(report).unwrap()).unwrap();
        self.bound
            .run_tool(
                "orbit.task.artifact.put",
                json!({
                    "id": self.task,
                    "model": "codex",
                    "path": REVIEW_REPORT_ARTIFACT,
                    "source_path": source,
                }),
            )
            .expect("the reviewer's report reaches the owner");
    }

    /// The handoff `claim_handoff` builds from the settled `evidence` for
    /// the candidate `head`, judged by the owner with its own observation.
    pub(super) fn owner_accepts(
        &self,
        evidence: HandoffReviewEvidence,
        head: &SourceRevision,
    ) -> Result<(), OrbitError> {
        self.owner_accepts_review(
            HandoffReview {
                policy: ReviewTiming::BeforePr,
                disposition: HandoffReviewDisposition::BeforePr(Box::new(evidence)),
            },
            head,
        )
    }

    /// [`Self::owner_accepts`] with the handoff carrying `review` as is.
    pub(super) fn owner_accepts_review(
        &self,
        review: HandoffReview,
        head: &SourceRevision,
    ) -> Result<(), OrbitError> {
        let record = self.pair.admission(&self.leaf);
        let mut handoff = handoff(&record);
        handoff.candidate.repository = REPOSITORY.into();
        handoff.candidate.candidate = head.clone();
        handoff.candidate.base = self.base.clone();
        handoff.review = review;
        let (_, worker) = self.claim();
        let observation = HandoffObservation {
            footprint_widening: vec![],
            candidate: handoff.candidate.clone(),
            required_commands: vec![],
            owner_completion_authority: None,
            review: Some(HandoffReviewObservation {
                reviewed_base_sha: self.base.commit.clone(),
                reviewed_base_is_ancestor: true,
                repository: REPOSITORY.into(),
            }),
        };
        self.pair
            .wire
            .owner
            .accept_task_handoff(&worker, "handoff", handoff, observation)
            .map(|_| ())
    }

    /// A reviewer on a host that cannot run `command` reports everything
    /// else checked and names a Linux CodeQL run of it as the evidence owed.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn reviewer_holds_for(&self, attempt_id: &str, command: &str) {
        let report = ReviewReport {
            external_evidence: vec![ReviewEvidenceRequirement {
                kind: ReviewEvidenceKind::CodeQl,
                name: "Rust CodeQL (Linux)".into(),
                command: command.into(),
                artifact: "evidence/codeql-rust-linux.json".into(),
                os: None,
            }],
            schema_version: REVIEW_CONTRACT_VERSION,
            attempt_id: attempt_id.into(),
            verdict: ReviewVerdict::Incomplete,
            summary: "Checked the change; the Linux CodeQL run is owed.".into(),
            findings: Vec::new(),
            validation: [
                ("V1", "make ci-fast", ValidationOutcome::Passed),
                ("V2", command, ValidationOutcome::NotRun),
            ]
            .into_iter()
            .map(|(id, command, outcome)| ReviewValidation {
                id: Some(id.into()),
                command: command.into(),
                outcome,
                role: ValidationRole::Required,
                note: None,
                check: None,
                control: None,
                sources: Vec::new(),
                mutation_target: Vec::new(),
                deferred: Vec::new(),
                baseline: None,
            })
            .collect(),
            retired_validation: Vec::new(),
            escalation: Some("a Linux CodeQL run is owed".into()),
        };
        self.put_report(&report);
    }

    /// The leaf's worker ends held, as the executor ends a run whose gate
    /// settled into an evidence hold.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(super) fn leaf_holds(&self, hold: &ReviewEvidenceHold) {
        let jobs = &self.pair.follower_jobs;
        let mut state = self
            .pair
            .follower
            .read_run_state(&self.leaf)
            .unwrap()
            .unwrap_or_else(|| {
                PipelineState::new(self.leaf.clone(), LEAF_JOB.to_string(), json!({}))
            });
        state.pipeline["review_gate_settle"] =
            json!({"gate": "awaiting_evidence", "evidence_hold": hold});
        self.pair
            .follower
            .write_run_state(&self.leaf, &state)
            .unwrap();
        let now = Utc::now();
        jobs.complete_job_run_step(
            &self.leaf,
            &JobRunStepParams {
                step_index: 0,
                target_type: JobTargetType::Activity,
                target_id: "review_gate_settle".into(),
                started_at: now,
                finished_at: now,
                duration_ms: None,
                exit_code: None,
                agent_response_json: None,
                state: JobRunState::Held,
                error_code: None,
                error_message: None,
            },
        )
        .unwrap();
        jobs.finalize_job_run(&self.leaf, JobRunState::Held, now, None)
            .unwrap();
    }

    pub(super) fn settle(&self) -> Result<Value, String> {
        self.bound
            .run_deterministic(
                "review_gate_settle",
                &json!({}),
                &self.gate_input,
                ToolContext::default(),
            )
            .map_err(|error| error.to_string())
    }

    /// The claim the leaf is bound to, and the follower's invocation of it.
    pub(super) fn claim(&self) -> (String, ClaimInvocation) {
        let claim = self
            .pair
            .admission(&self.leaf)
            .receipt
            .unwrap()
            .claim
            .unwrap();
        let worker = ClaimInvocation::trusted_worker(
            claim.task_id.clone(),
            claim.claim_id.clone(),
            FOLLOWER.into(),
            Some(ClaimRun {
                machine_id: FOLLOWER.into(),
                run_id: self.leaf.clone(),
            }),
        );
        (claim.claim_id, worker)
    }

    /// The reviewer's manifest read, through the leaf's binding as its tool
    /// call is.
    pub(super) fn read_manifest(&self) -> Result<Value, String> {
        self.bound
            .run_tool(
                "orbit.task.artifact.get",
                json!({"id": self.task, "path": REVIEW_MANIFEST_ARTIFACT}),
            )
            .map_err(|error| error.to_string())
    }

    /// The reviewer, started in the leaf and not yet finished, as the
    /// follower's review ledger records it.
    fn reviewer_running(&self, attempt_id: &str, lineage_key: &str) {
        self.bound
            .review_store()
            .unwrap()
            .review_record_invocation(
                &self.pair.follower.workspace_id().unwrap(),
                &ReviewInvocationRecord {
                    lineage_key,
                    attempt_id,
                    run_id: &self.leaf,
                    event: ReviewerInvocationEvent::Started,
                    now: Utc::now(),
                },
            )
            .unwrap();
    }

    /// Let the owner's reservation for the leaf's claim run out: its window
    /// is moved, in the owner's own store, to one that closed a minute ago,
    /// and the owner's console, read at the real clock, then reports it
    /// expired while the claim stays live.
    fn elapse_reservation(&self) -> Value {
        let owner = &self.pair.wire.owner;
        let (claim_id, _) = self.claim();
        let console = |owner: &OrbitRuntime| {
            owner.distributed_claim_console().unwrap()["claims"]
                .as_array()
                .unwrap()
                .iter()
                .find(|claim| claim["claim_id"] == claim_id.as_str())
                .cloned()
                .unwrap()
        };
        let current = console(owner);
        let reservation_id = current["reservation"]["id"].as_str().unwrap();
        let window = current["reservation"]["expires_at"].as_str().unwrap();
        let expires_at = Utc::now() - chrono::Duration::minutes(1);
        let shift = chrono::DateTime::parse_from_rfc3339(window)
            .unwrap()
            .signed_duration_since(expires_at);
        let expired = expires_at.to_rfc3339();
        let workspace_id = owner.workspace_id().unwrap();
        let connection = rusqlite::Connection::open(owner.global_root().join("orbit.db")).unwrap();
        let created: String = connection
            .query_row(
                "SELECT created_at FROM task_reservations WHERE reservation_id=?1",
                rusqlite::params![reservation_id],
                |row| row.get(0),
            )
            .unwrap();
        let created =
            (chrono::DateTime::parse_from_rfc3339(&created).unwrap() - shift).to_rfc3339();
        let moved = connection
            .execute(
                "UPDATE task_reservations SET created_at=?1, expires_at=?2 WHERE reservation_id=?3",
                rusqlite::params![created, expired, reservation_id],
            )
            .unwrap();
        assert_eq!(moved, 1, "the claim's reservation row");
        // The claim and its admission keep their own copy of the window.
        let copies = connection
            .execute(
                "UPDATE task_coordination_rows SET payload_json=replace(payload_json, ?1, ?2)
                 WHERE workspace_id=?3 AND instr(payload_json, ?1) > 0",
                rusqlite::params![window, expired, workspace_id],
            )
            .unwrap();
        assert!(copies >= 1, "the claim records its reservation window");
        let elapsed = console(owner);
        assert_eq!(elapsed["reservation"]["expires_at"], expired.as_str());
        assert_eq!(elapsed["reservation"]["expired"], true, "{elapsed}");
        elapsed
    }

    /// Every artifact on the owner's task, by path, and whether the owner
    /// recorded a certificate for `attempt_id`.
    fn owner_evidence(&self, attempt_id: &str) -> (Vec<(String, Vec<u8>)>, bool) {
        let owner = &self.pair.wire.owner;
        let artifacts = owner
            .get_task_artifacts(&self.task)
            .unwrap()
            .into_iter()
            .map(|artifact| (artifact.path, artifact.content))
            .collect();
        let certificate = owner
            .review_store()
            .unwrap()
            .review_certificate(&owner.workspace_id().unwrap(), attempt_id)
            .unwrap()
            .is_some();
        (artifacts, certificate)
    }

    pub(super) fn owner_artifact(&self, path: &str) -> Option<Vec<u8>> {
        self.pair
            .wire
            .owner
            .get_task_artifact(&self.task, path)
            .unwrap()
            .map(|artifact| artifact.content)
    }
}

pub(super) fn revision(repo: &Path, spec: &str) -> SourceRevision {
    SourceRevision {
        commit: git(repo, &["rev-parse", spec]).trim().to_string(),
        tree: git(repo, &["rev-parse", &format!("{spec}^{{tree}}")])
            .trim()
            .to_string(),
    }
}

/// A follower pulls from an owner with `review.before_pr` on, and its claimed
/// leaf reviews under the claim's captured contract: the reviewer's fix
/// becomes the candidate's second commit, the manifest, report, certificate
/// and verdict comment land on the owner's task, the attempt ledger stays on
/// the follower, and the owner accepts the handoff carrying the settled
/// verdict and moves the task to `review`.
#[test]
fn a_claimed_leaf_reviews_before_pr_and_the_owner_accepts_its_evidence() {
    if !isolated(
        module_path!(),
        "a_claimed_leaf_reviews_before_pr_and_the_owner_accepts_its_evidence",
    ) {
        return;
    }
    let mut leaf = ReviewedLeaf::admit();
    let pair = &leaf.pair;
    let pulls = pair.wire.calls("orbit.task.pull");
    assert_eq!(pulls[0]["review_gate"], true, "{}", pulls[0]);
    assert_eq!(pulls[0]["ship"]["review"]["crew"], REVIEW_CREW);

    // The leaf runs the owner's captured review, not the follower's.
    let input = pair
        .follower_jobs
        .get_job_run(&leaf.leaf)
        .unwrap()
        .unwrap()
        .input
        .unwrap();
    let admission = ReviewAdmission::from_run_input(&input)
        .unwrap()
        .expect("the leaf carries the claim's review admission");
    assert!(admission.gates_pr());
    assert_eq!(admission.crew.as_deref(), Some(REVIEW_CREW));
    assert_eq!(
        admission.timing_source, "claim",
        "{}",
        input[REVIEW_ADMISSION_KEY]
    );

    let admitted = leaf.admit_review();
    assert_eq!(admitted["applies"], true, "{admitted}");
    assert_eq!(admitted["reviewer"]["crew"], REVIEW_CREW);
    let attempt_id = admitted["attempt_id"].as_str().unwrap().to_string();
    assert!(
        leaf.owner_artifact(REVIEW_MANIFEST_ARTIFACT).is_some(),
        "the manifest is on the owner's task"
    );

    leaf.reviewer_reports(&attempt_id, ReviewVerdict::AcceptWithFixes, true);
    let settled = leaf.settle().expect("an accepted review passes");
    assert_eq!(settled["gate"], "passed", "{settled}");
    assert_eq!(settled["reviewer_fixed"], true, "{settled}");
    let head = revision(&leaf.pair.follower_repo, "HEAD");
    assert_eq!(settled["reviewed_head_sha"], head.commit.as_str());
    assert_eq!(
        git(&leaf.pair.follower_repo, &["log", "-1", "--format=%s"]).trim(),
        format!(
            "review: Checked the change against the criteria. [{}]",
            leaf.task
        ),
        "the reviewer's fix is the candidate's second commit"
    );

    let owner = &leaf.pair.wire.owner;
    let certificate = leaf
        .owner_artifact(REVIEW_GATE_ARTIFACT)
        .expect("the certificate is on the owner's task");
    assert!(
        comments_of(&leaf.pair.owner_task(&leaf.task)).contains(&attempt_id),
        "the verdict comment is on the owner's task"
    );
    let history = leaf
        .owner_artifact(REVIEW_REPORT_HISTORY_ARTIFACT)
        .expect("the report history is on the owner's task");
    assert_eq!(
        ReviewReportHistory::parse(&history)
            .unwrap()
            .for_attempt(&attempt_id)
            .count(),
        1,
        "the owner retained the reviewer's one report revision"
    );
    let ledger = leaf
        .bound
        .review_store()
        .unwrap()
        .review_ledger(
            &leaf.pair.follower.workspace_id().unwrap(),
            admitted["lineage_key"].as_str().unwrap(),
        )
        .unwrap()
        .expect("the attempt ledger is on the follower");
    assert_eq!(ledger.attempts.len(), 1);

    let evidence: HandoffReviewEvidence =
        serde_json::from_value(settled["handoff_evidence"].clone()).expect("handoff evidence");
    assert_eq!(
        evidence.reviewer_commit.as_deref(),
        Some(head.commit.as_str())
    );
    assert_eq!(evidence.reviewer_run_id, leaf.leaf);
    assert_eq!(
        evidence.certificate.sha256,
        orbit_common::security::release::sha256_hex(&certificate)
    );

    leaf.owner_accepts(evidence, &head)
        .expect("the owner accepts the reviewed handoff");
    assert_eq!(leaf.pair.owner_status(&leaf.task), "review");
    assert!(
        owner
            .review_store()
            .unwrap()
            .review_certificate(&owner.workspace_id().unwrap(), &attempt_id)
            .unwrap()
            .is_some(),
        "the owner records the follower's certificate"
    );
}

/// A reviewer that rejects the candidate stops the leaf before it pushes,
/// as on the owner's own route: the findings are on the owner's task, and
/// the leaf's failure settlement blocks it there.
#[test]
fn a_rejected_claimed_review_blocks_the_task_on_the_owner() {
    if !isolated(
        module_path!(),
        "a_rejected_claimed_review_blocks_the_task_on_the_owner",
    ) {
        return;
    }
    let mut leaf = ReviewedLeaf::admit();
    let admitted = leaf.admit_review();
    let attempt_id = admitted["attempt_id"].as_str().unwrap().to_string();
    leaf.reviewer_reports(&attempt_id, ReviewVerdict::Reject, false);

    let refused = leaf
        .settle()
        .expect_err("a rejected review never opens a PR");
    assert!(refused.contains("review_gate_blocked"), "{refused}");
    let task = leaf.pair.owner_task(&leaf.task);
    assert!(
        comments_of(&task).contains("The stub panics"),
        "the findings are on the owner's task: {task:#}"
    );

    leaf.pair.leaf_fails_with(&leaf.leaf, &refused);
    leaf.pair.pass(&leaf.drain);
    assert_eq!(leaf.pair.owner_status(&leaf.task), "blocked");
    assert_eq!(leaf.pair.owner_claims()[0]["claim"]["phase"], "failed");
}

/// A claimed reviewer that cannot run the Linux CodeQL check holds the review
/// for it. The leaf publishes the held candidate to its branch on `origin`
/// and settles by releasing the claim with the hold, so the owner's task
/// stays in progress awaiting the evidence instead of blocking, and is not
/// pulled again. The Linux owner then runs the check at the held commit,
/// fetched from `origin`, and receipt queues the task for a fresh review.
#[cfg(target_os = "linux")]
#[test]
fn a_claimed_review_held_for_linux_codeql_is_fulfilled_by_the_owner() {
    if !isolated(
        module_path!(),
        "a_claimed_review_held_for_linux_codeql_is_fulfilled_by_the_owner",
    ) {
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let command = "scripts/codeql-rust-local.sh --ram 16384 codeql/rust-queries:codeql-suites/rust-security-extended.qls";
    let mut leaf = ReviewedLeaf::admit();
    let root = leaf.pair._root.path().to_path_buf();
    let origin = root.join("origin.git");
    git(&root, &["init", "--bare", "-q", origin.to_str().unwrap()]);
    let follower_repo = leaf.pair.follower_repo.clone();
    git(
        &follower_repo,
        &["remote", "set-url", "origin", origin.to_str().unwrap()],
    );
    let script = follower_repo.join("scripts/codeql-rust-local.sh");
    std::fs::create_dir_all(script.parent().unwrap()).unwrap();
    std::fs::write(
        &script,
        "#!/usr/bin/env bash\nset -euo pipefail\n\
         head=\"$(git rev-parse HEAD)\"\n\
         echo \"ORBIT_CODEQL_STUB_RUN: $head\"\n\
         run_dir=\"$(mktemp -d \"$ORBIT_SCRATCH_DIR/codeql-rust-local.XXXXXX\")\"\n\
         echo \"codeql-rust-local: run directory: $run_dir\" >&2\n\
         printf '{\"runs\":[{\"results\":[]}]}' >\"$run_dir/results.sarif\"\n\
         echo \"codeql-rust-local: analysis completed\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    git(&follower_repo, &["add", "scripts"]);
    git(
        &follower_repo,
        &["commit", "-q", "-m", "Add the CodeQL entry point"],
    );

    let admitted = leaf.admit_review();
    let attempt_id = admitted["attempt_id"].as_str().unwrap().to_string();
    leaf.reviewer_holds_for(&attempt_id, command);
    leaf.settle()
        .expect_err("a held review opens no pull request");
    let hold: ReviewEvidenceHold = serde_json::from_slice(
        &leaf
            .owner_artifact(REVIEW_EVIDENCE_HOLD_ARTIFACT)
            .expect("the hold is on the owner's task"),
    )
    .unwrap();
    assert_eq!(hold.run_id, leaf.leaf);
    assert_eq!(
        git(
            &origin,
            &[
                "rev-parse",
                &format!("refs/heads/orbit-evidence/orbit/{}", leaf.task)
            ]
        )
        .trim(),
        hold.candidate.commit,
        "the held candidate is published for the owner to fetch"
    );

    leaf.leaf_holds(&hold);
    let pass = leaf.pair.pass(&leaf.drain);
    assert_eq!(pass["consecutive_failures"], 0, "{pass}");
    assert_eq!(
        pass["admitted"], 0,
        "the held task is not pulled back: {pass}"
    );
    let settles = leaf.pair.wire.calls("orbit.drain.claim.settle");
    assert_eq!(settles.len(), 1, "{settles:?}");
    assert_eq!(
        settles[0]["settlement"]["Release"]["evidence_hold"]["attempt_id"],
        hold.attempt_id.as_str(),
        "{settles:?}"
    );
    assert_eq!(leaf.pair.owner_status(&leaf.task), "in-progress");
    let owner = &leaf.pair.wire.owner;
    let latest = owner
        .get_task_history(&leaf.task)
        .unwrap()
        .into_iter()
        .rev()
        .find(|entry| entry.to_status.is_some() || entry.event.starts_with("review_"))
        .unwrap();
    assert_eq!(latest.event, "review_awaiting_evidence");

    // The owner: a Linux checkout whose `origin` is the one the leaf pushed to.
    let owner_repo = leaf.pair.owner_repo.clone();
    git(&owner_repo, &["init", "-q", "-b", "main"]);
    git(
        &owner_repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    let resources = owner.paths().global_dir.join("resources");
    std::fs::create_dir_all(resources.join("activities")).unwrap();
    std::fs::create_dir_all(resources.join("jobs")).unwrap();
    std::fs::write(
        resources.join("activities/fulfil_review_evidence.yaml"),
        include_str!("../../../assets/activities/fulfil_review_evidence.yaml"),
    )
    .unwrap();
    // Lower only the default disk gate; the owner still requires a working
    // Bubblewrap namespace before it runs the candidate's stub script.
    std::fs::write(
        resources.join("jobs/review_evidence_fulfilment_pipeline.yaml"),
        include_str!("../../../assets/jobs/review_evidence_fulfilment_pipeline.yaml")
            .replace("min_free_mib: 30720", "min_free_mib: 1"),
    )
    .unwrap();
    let released = root.join("released");
    crate::worker_fixture::install(&released, "created");
    let tick = owner
        .run_review_evidence_fulfilment_tick(Utc::now())
        .unwrap();
    let probe = orbit_exec::probe_bwrap();
    if !probe.available {
        orbit_exec::report_bwrap_deferral(
            "owner CodeQL fulfilment of a claimed hold",
            &probe.detail,
        );
        assert!(tick.dispatched.is_empty(), "{tick:?}");
        assert!(
            tick.skipped
                .as_deref()
                .is_some_and(|reason| reason.contains("Bubblewrap")),
            "the owner defers until Bubblewrap namespaces work: {tick:?}"
        );
        assert_eq!(leaf.pair.owner_status(&leaf.task), "in-progress");
        assert_eq!(
            leaf.pair
                .wire
                .owner
                .get_task_history(&leaf.task)
                .unwrap()
                .last()
                .unwrap()
                .event,
            "review_awaiting_evidence"
        );
        return;
    }
    assert_eq!(tick.dispatched.len(), 1, "{tick:?}");
    let executed = owner.execute_pipeline_run_worker(&tick.dispatched[0].1);
    std::fs::write(&released, "").unwrap();
    executed.unwrap();

    let evidence = hold
        .requirements
        .first()
        .expect("the held CodeQL requirement");
    let log_path = format!(
        "{}.log.json",
        evidence
            .artifact
            .strip_suffix(".json")
            .expect("the evidence artifact is JSON")
    );
    let log = owner
        .get_task_artifact(&leaf.task, &log_path)
        .unwrap()
        .expect("the owner's run log is attached");
    let log: serde_json::Value = serde_json::from_slice(&log.content).unwrap();
    assert_eq!(log["tested_head"], hold.candidate.commit);
    assert!(
        log["stdout"].as_str().is_some_and(|stdout| stdout
            .lines()
            .any(|line| { line == format!("ORBIT_CODEQL_STUB_RUN: {}", hold.candidate.commit) })),
        "the owner ran the check once at the fetched held commit: {log}"
    );
    assert_eq!(leaf.pair.owner_status(&leaf.task), "backlog");
    assert_eq!(
        owner
            .get_task_history(&leaf.task)
            .unwrap()
            .last()
            .unwrap()
            .event,
        "review_evidence_received"
    );
}

/// A follower that cannot resolve the owner's captured review crew requests
/// no claim: its pass stops at the probe with the reason.
#[test]
fn a_follower_without_the_review_crew_claims_nothing() {
    if !isolated(
        module_path!(),
        "a_follower_without_the_review_crew_claims_nothing",
    ) {
        return;
    }
    let pair = Pair::with_owner_config(&before_pr_owner("ghost"), &[None]);
    let drain = pair.run_drain();
    let pass = pair.pass(&drain);
    let refusal = pass["refusal"].as_str().unwrap_or_default();
    assert!(
        refusal.contains("before_pr_reviewer_unavailable") && refusal.contains("ghost"),
        "{pass}"
    );
    assert!(pair.wire.calls("orbit.task.pull").is_empty(), "{pass}");
    assert_eq!(pair.owner_status(&pair.tasks[0]), "backlog");
}

/// [ORB-14221] The owner answers a claimed reviewer's manifest read only while
/// the claim could still take that worker's report: once the owner released,
/// failed or revoked the claim, or a later pull superseded it, the read and
/// the report are refused as `stale_claim` and nothing on the owner changes,
/// though the follower's ledger still records the reviewer running. An
/// elapsed reservation alone ends nothing: the live claim still reads and
/// writes until the owner recovers it. The leaf's binding reaches the owner
/// exactly as its run's broker forwards a bridged call.
#[test]
fn a_claimed_reviewers_manifest_read_needs_the_owners_active_claim() {
    if !isolated(
        module_path!(),
        "a_claimed_reviewers_manifest_read_needs_the_owners_active_claim",
    ) {
        return;
    }
    type Settle = fn(&ReviewedLeaf);
    let release: Settle = |leaf| {
        let (_, worker) = leaf.claim();
        leaf.pair
            .wire
            .owner
            .mutate_execution_claim(
                Some(&worker),
                "release",
                &ClaimMutation::Release(ClaimEvidence {
                    summary: Some("The executor gave the claim back.".into()),
                    ..ClaimEvidence::default()
                }),
            )
            .expect("the executor releases its claim");
    };
    let fail: Settle = |leaf| {
        let (_, worker) = leaf.claim();
        leaf.pair
            .wire
            .owner
            .mutate_execution_claim(
                Some(&worker),
                "fail",
                &ClaimMutation::Fail(ClaimEvidence {
                    summary: Some("The leaf failed.".into()),
                    ..ClaimEvidence::default()
                }),
            )
            .expect("the executor fails its claim");
    };
    let revoke: Settle = |leaf| {
        let (claim_id, _) = leaf.claim();
        leaf.pair
            .wire
            .owner
            .recover_claim_as_operator(
                &claim_id,
                "running",
                TaskStatus::Backlog,
                "operator",
                "The operator took the claim back.",
                "recover",
            )
            .expect("the operator revokes the claim");
    };
    let recover_expired: Settle = |leaf| {
        let (claim_id, _) = leaf.claim();
        leaf.pair
            .wire
            .owner
            .recover_claim_as_operator(
                &claim_id,
                "running",
                TaskStatus::Backlog,
                "operator",
                "The claim outlived its reservation.",
                "recover",
            )
            .expect("the operator recovers the expired claim");
    };
    for (case, settle) in [
        ("released", release),
        ("failed", fail),
        ("revoked", revoke),
        ("expired", recover_expired),
    ] {
        let mut leaf = ReviewedLeaf::admit();
        let admitted = leaf.admit_review();
        let attempt_id = admitted["attempt_id"].as_str().unwrap().to_string();
        let lineage_key = admitted["lineage_key"].as_str().unwrap().to_string();
        leaf.reviewer_running(&attempt_id, &lineage_key);
        let pinned = leaf.owner_artifact(REVIEW_MANIFEST_ARTIFACT).unwrap();
        let read = leaf.read_manifest().expect("the active claim reads");
        assert_eq!(
            read["content"].as_str().map(str::as_bytes),
            Some(pinned.as_slice()),
            "{case}: {read}"
        );

        let source = leaf.pair.follower_repo.join(".orbit/tmp/report.json");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(
            &source,
            serde_json::to_vec(&json!({
                "schema_version": REVIEW_CONTRACT_VERSION, "attempt_id": attempt_id,
                "verdict": "incomplete", "summary": "Fixture report.", "findings": [],
                "validation": [], "escalation": "fixture only",
            }))
            .unwrap(),
        )
        .unwrap();
        let put = json!({
            "id": leaf.task, "path": REVIEW_REPORT_ARTIFACT, "source_path": source,
        });
        leaf.bound
            .run_tool("orbit.task.artifact.put", put.clone())
            .expect("the active claim writes its report");
        leaf.bound
            .run_tool("orbit.task.artifact.put", put.clone())
            .expect("the active claim can reconcile its report replay");

        if case == "expired" {
            // The reservation window has really closed, yet the owner never
            // ended the claim: it is still live, its footprint still
            // protected, and the reviewer, inside its own deadline, still
            // reads and reconciles its report.
            let elapsed = leaf.elapse_reservation();
            assert_eq!(elapsed["phase"], "running", "{elapsed}");
            assert_eq!(elapsed["footprint_protected"], true, "{elapsed}");
            let read = leaf
                .read_manifest()
                .expect("an elapsed reservation leaves the claim reading");
            assert_eq!(
                read["content"].as_str().map(str::as_bytes),
                Some(pinned.as_slice()),
                "{read}"
            );
            leaf.bound
                .run_tool("orbit.task.artifact.put", put.clone())
                .expect("an elapsed reservation leaves the claim writing");
        }

        settle(&leaf);
        let before = leaf.owner_evidence(&attempt_id);
        let refused = leaf
            .read_manifest()
            .expect_err("a settled claim reads nothing");
        assert!(refused.contains("stale_claim"), "{case}: {refused}");
        let put = leaf
            .bound
            .run_tool("orbit.task.artifact.put", put)
            .expect_err("a settled claim writes nothing")
            .to_string();
        assert!(put.contains("stale_claim"), "{case}: {put}");
        assert_eq!(
            leaf.owner_evidence(&attempt_id),
            before,
            "{case}: the refusals change nothing on the owner"
        );
        assert!(!before.1, "{case}: no certificate");
        let ledger = leaf
            .bound
            .review_store()
            .unwrap()
            .review_ledger(&leaf.pair.follower.workspace_id().unwrap(), &lineage_key)
            .unwrap()
            .unwrap();
        assert!(
            ledger.attempts[0]
                .reviewer_running
                .as_ref()
                .is_some_and(|running| running.run_id == leaf.leaf && Utc::now() < running.deadline),
            "{case}: the follower still records the reviewer running"
        );

        if matches!(case, "revoked" | "expired") {
            // The task went back to the backlog and a later pull claims it
            // again: the new claim's leaf reads, the superseded one does not.
            let next = leaf.pair.queued_leaf(&leaf.drain, 2);
            let record = leaf.pair.admission(&next);
            let claim = record.receipt.as_ref().unwrap().claim.clone().unwrap();
            assert_eq!(claim.task_id, leaf.task, "the same task is claimed again");
            let current = leaf
                .pair
                .follower
                .clone()
                .with_worker_invocation(
                    WorkerInvocation {
                        owner_machine_id: OWNER.into(),
                        owner_workspace_id: record.destination.owner_workspace_id.clone(),
                        owner_destination: record.destination.selector.clone(),
                        task_id: claim.task_id.clone(),
                        claim_id: claim.claim_id.clone(),
                        execution: claim.executed_on.clone(),
                        bound_run_id: next.clone(),
                    },
                    Arc::new(ToOwner(leaf.pair.wire.owner.clone())),
                )
                .unwrap();
            current
                .run_tool(
                    "orbit.task.artifact.get",
                    json!({"id": leaf.task, "path": REVIEW_MANIFEST_ARTIFACT}),
                )
                .expect("the current claim reads");
            let superseded = leaf
                .read_manifest()
                .expect_err("a superseded claim reads nothing");
            assert!(superseded.contains("stale_claim"), "{superseded}");
        }
    }
}
