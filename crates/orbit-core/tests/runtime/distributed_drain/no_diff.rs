//! Real clean-base claim execution and owner settlement, without a provider PR.
use super::claimed_review::ToOwner;
use super::*;
use orbit_types::tool::WorkerInvocation;
use orbit_types::workflow::handoff::already_landed_scope;

const CHECK: &str = "git diff --exit-code";

fn engine_action(host: &OrbitRuntime, action: &str, input: &Value) -> Result<Value, OrbitError> {
    orbit_engine::execute_deterministic_action(
        host,
        action,
        &json!({}),
        input,
        false,
        &Default::default(),
        None,
    )
}

struct CleanLeaf {
    pair: Pair,
    leaf: String,
    task: String,
    bound: OrbitRuntime,
    base: String,
    input: Value,
}

impl CleanLeaf {
    fn new(already_landed: bool, completion: &str) -> Self {
        Self::build(already_landed, completion, &[], true)
    }

    /// A clean leaf whose task carries the `no-diff-expected` tag, claimed by
    /// the follower through ordinary pull admission [ORB-14474].
    fn tagged(completion: &str) -> Self {
        Self::build(
            false,
            completion,
            &[orbit_types::task::NO_DIFF_EXPECTED_TAG],
            true,
        )
    }

    /// A tagged review whose implementer files findings and attaches its
    /// coverage, but writes no clean-tree report [ORB-14791].
    fn review_only(completion: &str) -> Self {
        Self::build(
            false,
            completion,
            &[orbit_types::task::NO_DIFF_EXPECTED_TAG],
            false,
        )
    }

    /// A tagged clean leaf whose task a `delivery-code-review` auto-task
    /// minted. Like `review_only`, it writes no clean-tree report [ORB-14791].
    fn delivery_review(completion: &str) -> Self {
        Self::build(
            false,
            completion,
            &[
                orbit_types::task::NO_DIFF_EXPECTED_TAG,
                "delivery-code-review",
                "auto-task:delivery-code-review",
            ],
            false,
        )
    }

    fn build(already_landed: bool, completion: &str, tags: &[&str], report: bool) -> Self {
        let config = format!(
            "[workflow]\ndistributed_completion = \"{completion}\"\nrequired_validation_commands = [\"{CHECK}\"]\n[review]\nbefore_pr = true\n[operation]\nreview_crew = \"sol\"\n"
        );
        let pair = Pair::with_owner_config(&config, &[None]);
        let task = pair.tasks[0].clone();
        let repo = &pair.owner_repo;
        std::fs::write(repo.join(".gitignore"), "/.orbit/\n").unwrap();
        git(repo, &["init", "-q", "-b", "main"]);
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-q", "-m", &format!("Deliver [{task}]")]);
        git(
            repo,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/owner/repository.git",
            ],
        );
        publish_origin(repo);
        let base = git(repo, &["rev-parse", "HEAD"]).trim().to_string();
        if !tags.is_empty() {
            pair.wire
                .owner
                .update_task_as_human(
                    &task,
                    orbit_core::application::task::TaskUpdateParams {
                        tags: Some(tags.iter().map(|tag| (*tag).to_string()).collect()),
                        ..Default::default()
                    },
                    "fixture operator".into(),
                )
                .unwrap();
        }
        let drain = pair.run_drain();
        let leaf = pair.launched_leaf(&drain, 1, std::process::id());
        assert_eq!(pair.claimed_task(&leaf), task);

        let follower = &pair.follower_repo;
        git(follower, &["init", "-q", "-b", "main"]);
        git(
            follower,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/owner/repository.git",
            ],
        );
        git(
            follower,
            &[
                "config",
                &format!(
                    "url.{}.insteadOf",
                    repo.with_file_name("origin.git").display()
                ),
                "https://github.com/owner/repository.git",
            ],
        );
        git(follower, &["fetch", "-q", "origin", "main"]);
        git(follower, &["reset", "-q", "--hard", "origin/main"]);
        git(
            follower,
            &["checkout", "-q", "-b", &format!("orbit/{task}")],
        );
        std::fs::write(follower.join(".orbit/config.toml"), &config).unwrap();
        let record = pair.admission(&leaf);
        let claim = record.receipt.as_ref().unwrap().claim.as_ref().unwrap();
        let bound =
            OrbitRuntime::from_roots(&pair.follower.global_root(), &follower.join(".orbit"))
                .unwrap()
                .with_automation_machine_identity(Some(FOLLOWER.into()))
                .with_coordination_write_owner(Some(OWNER.into()))
                .with_drain_owner_transport(pair.wire.clone())
                .with_worker_invocation(
                    WorkerInvocation {
                        owner_machine_id: OWNER.into(),
                        owner_workspace_id: record.destination.owner_workspace_id.clone(),
                        owner_destination: record.destination.selector.clone(),
                        task_id: task.clone(),
                        claim_id: claim.claim_id.clone(),
                        execution: claim.executed_on.clone(),
                        bound_run_id: leaf.clone(),
                    },
                    Arc::new(ToOwner(pair.wire.owner.clone())),
                )
                .unwrap();
        let bound = calm_host(bound);
        let input = json!({
            "job_run_id": leaf, "run_id": leaf, "scope": "all",
            "workspace_path": follower, "base_sha": base, "base_ref": "origin/main",
            "base": "main", "base_sync": "remote", "completed_task_ids": [task],
            "verify_already_landed": true,
            "implementation": {"execution_summary": "Outcome: success\nCurrent base satisfies the acceptance criteria; no change needed."},
        });
        let mut fixture = Self {
            pair,
            leaf,
            task,
            bound,
            base,
            input,
        };
        if !report {
            return fixture;
        }
        fixture.write_evidence(
            "implementation-validation.json",
            json!({
                "run_id": fixture.leaf, "tested_head": fixture.base,
                "command": CHECK, "exit_code": 0, "output": "",
            }),
        );
        let report = if already_landed {
            let task = RuntimeHost::get_task(&fixture.bound, &fixture.task).unwrap();
            let comments = RuntimeHost::get_task_comments(&fixture.bound, &fixture.task).unwrap();
            json!({
                "schema_version": 1, "task_id": fixture.task, "run_id": fixture.leaf,
                "tested_head": fixture.base, "covering_commit": fixture.base,
                "covering_task_id": fixture.task, "scope": already_landed_scope(&task, &comments),
                "required_commands": [CHECK], "criteria_evidence": ["The base contains the delivered scope and validation passes."],
                "validation": [{"command": CHECK, "outcome": "passed", "role": "required", "log_artifact": "implementation-validation.json"}],
            })
        } else {
            json!({
                "schema_version": 1, "task_id": fixture.task, "run_id": fixture.leaf,
                "tested_head": fixture.base, "reason": "The requested behavior is already satisfied on this clean base.",
                "validation": [{"command": CHECK, "exit_code": 0, "log_artifact": "implementation-validation.json"}],
            })
        };
        fixture.write_evidence(
            if already_landed {
                "already-landed.json"
            } else {
                "no-diff.json"
            },
            report,
        );
        fixture
    }

    fn write_evidence(&mut self, path: &str, value: Value) {
        let scratch = self.pair.follower_repo.join(".orbit/tmp");
        std::fs::create_dir_all(&scratch).unwrap();
        std::fs::write(scratch.join(path), serde_json::to_vec(&value).unwrap()).unwrap();
        let artifacts = self.input["implementation"]
            .as_object_mut()
            .unwrap()
            .entry("no_diff_artifacts")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .unwrap();
        artifacts.push(json!({"path": path, "source_path": format!(".orbit/tmp/{path}")}));
    }

    fn attach(&self, path: &str, value: Value) {
        self.bound
            .attach_claim_validation_log(path, serde_json::to_vec(&value).unwrap())
            .unwrap();
    }

    fn action(&self, action: &str, input: &Value) -> Value {
        if action == "review_gate_admit" {
            self.bound
                .run_deterministic(action, &json!({}), input, ToolContext::default())
                .unwrap()
        } else {
            engine_action(&self.bound, action, input).unwrap()
        }
    }

    fn prepare_handoff(&self) -> TaskHandoff {
        let committed = self.action("git_commit", &self.input);
        assert!(
            matches!(
                committed["decision"].as_str(),
                Some("verified_no_diff" | "verified_already_landed" | "skipped_no_diff_expected")
            ),
            "{committed}"
        );
        let mut input = self.input.clone();
        input["skipped_no_diff_expected"] = committed["skipped_no_diff_expected"].clone();
        input["already_landed_checkpoint"] = committed;
        let review = self.action("review_gate_admit", &input);
        assert_eq!(
            review["applies"], false,
            "a clean base opens no PR and runs no reviewer: {review}"
        );
        let validated = self.action("claim_validate", &input);
        assert_eq!(validated["candidate"]["delivery"]["kind"], "no_diff");
        input["no_diff_evidence"] = validated["no_diff_evidence"].clone();
        input["candidate"] = validated["candidate"].clone();
        input["validation"] = validated["validation"].clone();
        let delivered = self.action("claim_handoff", &input);
        assert_eq!(delivered["delivery"], "no_diff");
        assert_eq!(delivered["merged"], false);
        let admission = self.pair.admission(&self.leaf);
        let ClaimMutation::AcceptHandoff(handoff) = admission.settlement.unwrap() else {
            panic!("typed handoff required")
        };
        handoff
    }

    fn run_clean_pipeline(&self, name: &str) -> TaskHandoff {
        let outcome = self.execute_clean_pipeline(name).unwrap();
        assert!(outcome.success, "{outcome:#?}");
        for step in ["prepare_branch", "sync_base", "review", "push", "pr_open"] {
            assert!(outcome.pipeline.get(step).is_none(), "{outcome:#?}");
        }
        let admission = self.pair.admission(&self.leaf);
        let ClaimMutation::AcceptHandoff(handoff) = admission.settlement.unwrap() else {
            panic!("shipped clean pipeline must record a typed handoff")
        };
        handoff
    }

    fn execute_clean_pipeline(
        &self,
        name: &str,
    ) -> Result<orbit_engine::JobOutcome, orbit_engine::DispatchError> {
        use orbit_engine::activity_job::{V2ActivityCatalog, load_activity_asset, load_job_asset};
        use orbit_engine::{
            V2AuditWriter, execute_job_with_resume, resolve_job_catalog_refs_for_execution,
        };

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
            &std::fs::read_to_string(assets.join("jobs").join(format!("{name}.yaml"))).unwrap(),
        )
        .unwrap()
        .spec;
        resolve_job_catalog_refs_for_execution(&mut job, &catalog).unwrap();
        // Resume after the real deterministic commit verifier. No agent or
        // worker is dispatched; the shipped delivery steps execute unchanged.
        let input = json!({"task_ids": [self.task], "base_sync": "remote"});
        let worktree = self.input.clone();
        let implementation = self.input["implementation"].clone();
        let committed = self.action("git_commit", &self.input);
        let mut resume = PipelineState::new(self.leaf.clone(), name.into(), input.clone());
        resume.record_step(0, JobRunState::Success, Some(worktree), None);
        resume.record_step(1, JobRunState::Success, Some(json!({})), None);
        resume.compound_outputs.insert(
            1,
            [("implement_one".into(), implementation)]
                .into_iter()
                .collect(),
        );
        resume.record_step(2, JobRunState::Success, Some(committed), None);
        let audit = V2AuditWriter::with_disk_sinks(
            &self.pair.follower_repo.join(".orbit/tmp/pipeline-audit"),
            self.bound.v2_audit_store().unwrap(),
            self.bound.workspace_id().unwrap(),
            &self.leaf,
            "clean-leaf-fixture",
            Some(&self.pair.follower_repo),
        )
        .unwrap();
        execute_job_with_resume(&job, input, &self.leaf, audit, &self.bound, Some(&resume))
    }

    fn settle(&self, handoff: &TaskHandoff) -> Result<Value, OrbitError> {
        self.pair.wire.call(
            "",
            "orbit.drain.claim.settle",
            json!({
                "claim_id": handoff.claim_id, "run_id": self.leaf,
                "settlement": ClaimMutation::AcceptHandoff(handoff.clone()),
            }),
        )
    }

    fn assert_no_blocked(&self) {
        let task = self.pair.owner_task(&self.task);
        assert!(
            task["history"]
                .as_array()
                .unwrap()
                .iter()
                .all(|event| event["to_status"] != "blocked"),
            "{task:#}"
        );
        assert!(
            task["external_refs"].as_array().unwrap().is_empty(),
            "NoDiff creates no PR: {task:#}"
        );
    }
}

#[test]
fn verified_no_diff_and_already_satisfied_base_complete_without_a_pr() {
    if !isolated(
        module_path!(),
        "verified_no_diff_and_already_satisfied_base_complete_without_a_pr",
    ) {
        return;
    }
    for (already_landed, job) in [
        (false, "task_claimed_pr_pipeline"),
        (true, "task_claimed_pr_pipeline"),
        (false, "task_claimed_local_pipeline"),
    ] {
        let fixture = CleanLeaf::new(already_landed, "done");
        let handoff = fixture.run_clean_pipeline(job);
        let settled = fixture
            .settle(&handoff)
            .expect("owner independently verifies the report");
        assert_eq!(settled["status"], "review", "{settled}");
        let accepted = fixture
            .pair
            .wire
            .owner
            .accepted_task_handoff(&handoff.claim_id)
            .unwrap();
        let landed = engine_action(
            &fixture.pair.wire.owner,
            "handoff_land",
            &json!({"handoff_id": accepted.handoff_id}),
        )
        .unwrap();
        assert_eq!(landed["evidence"]["external_merge"], false, "{landed}");
        assert_eq!(fixture.pair.owner_status(&fixture.task), "done");
        assert_eq!(
            git(&fixture.pair.owner_repo, &["rev-parse", "main"]).trim(),
            fixture.base
        );
        fixture.assert_no_blocked();
        let replay = fixture.settle(&handoff).unwrap();
        assert_eq!(
            replay["status"], "review",
            "journal replays acceptance without repeating completion"
        );
        assert_eq!(fixture.pair.owner_status(&fixture.task), "done");
    }
}

/// The full-code-review chore shape: a `no-diff-expected` task the follower
/// claims, whose leaf files findings on the owner through the claimed-owner
/// broker and hands off `NoDiff`, with no PR [ORB-14474].
#[test]
fn a_follower_claims_no_diff_expected_work_files_findings_and_completes_without_a_pr() {
    if !isolated(
        module_path!(),
        "a_follower_claims_no_diff_expected_work_files_findings_and_completes_without_a_pr",
    ) {
        return;
    }
    let fixture = CleanLeaf::tagged("done");
    let owner_task = fixture.pair.owner_task(&fixture.task);
    assert!(
        owner_task["tags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tag| tag == orbit_types::task::NO_DIFF_EXPECTED_TAG),
        "{owner_task:#}"
    );

    let filed = fixture
        .bound
        .run_tool(
            "orbit.task.add",
            json!({
                "title": "Finding from the review chore",
                "description": "Found while reviewing the crate.",
                "complexity": "low", "model": "codex",
                "relations": [{"type": "spawned_from", "target": fixture.task}],
            }),
        )
        .expect("a claimed no-diff leaf files a finding on the owner");
    let id = filed["id"].as_str().expect("the new task's id");
    let prefix = |id: &str| id.split('-').next().unwrap().to_string();
    assert_eq!(prefix(id), prefix(&fixture.task), "owner prefix: {filed}");
    let stored = fixture
        .pair
        .wire
        .owner
        .run_tool("orbit.task.show", json!({"id": id}))
        .expect("the owner holds the filed task");
    assert_eq!(stored["id"], id, "{stored}");

    let handoff = fixture.run_clean_pipeline("task_claimed_pr_pipeline");
    fixture
        .settle(&handoff)
        .expect("owner independently verifies the report");
    let accepted = fixture
        .pair
        .wire
        .owner
        .accepted_task_handoff(&handoff.claim_id)
        .unwrap();
    engine_action(
        &fixture.pair.wire.owner,
        "handoff_land",
        &json!({"handoff_id": accepted.handoff_id}),
    )
    .unwrap();
    assert_eq!(fixture.pair.owner_status(&fixture.task), "done");
    fixture.assert_no_blocked();
}

/// The delivery-code-review shape that blocked on a follower: a tagged task
/// whose implementer attaches its coverage and writes no clean-tree report.
/// The leaf skips its clean commit as the owner does and hands off `NoDiff`;
/// the owner settles it to the status its own `promote_no_diff` and
/// `complete_no_diff` reach, with no commit failure or already-landed proof
/// [ORB-14791].
#[test]
fn a_claimed_no_diff_expected_review_without_a_report_settles_like_the_owner() {
    if !isolated(
        module_path!(),
        "a_claimed_no_diff_expected_review_without_a_report_settles_like_the_owner",
    ) {
        return;
    }
    for (completion, job, status) in [
        ("done", "task_claimed_pr_pipeline", "done"),
        ("done", "task_claimed_local_pipeline", "done"),
        ("review", "task_claimed_pr_pipeline", "review"),
    ] {
        let fixture = CleanLeaf::review_only(completion);
        fixture.attach("automation-coverage.json", json!({"findings": []}));
        let committed = fixture.action("git_commit", &fixture.input);
        assert_eq!(
            committed["decision"], "skipped_no_diff_expected",
            "{committed}"
        );
        assert_eq!(committed["base_sha"], fixture.base, "{committed}");

        let handoff = fixture.run_clean_pipeline(job);
        let settled = fixture
            .settle(&handoff)
            .expect("owner settles a tagged clean base");
        assert_eq!(settled["status"], "review", "{job}: {settled}");
        if completion == "done" {
            let accepted = fixture
                .pair
                .wire
                .owner
                .accepted_task_handoff(&handoff.claim_id)
                .unwrap();
            let landed = engine_action(
                &fixture.pair.wire.owner,
                "handoff_land",
                &json!({"handoff_id": accepted.handoff_id}),
            )
            .unwrap();
            assert_eq!(landed["evidence"]["external_merge"], false, "{landed}");
        }
        assert_eq!(
            fixture.pair.owner_status(&fixture.task),
            status,
            "{job} with {completion} completion"
        );
        let artifacts = RuntimeHost::get_task_artifacts(&fixture.pair.wire.owner, &fixture.task)
            .unwrap()
            .into_iter()
            .map(|artifact| artifact.path)
            .collect::<Vec<_>>();
        assert!(
            artifacts
                .iter()
                .any(|path| path == "automation-coverage.json"),
            "the coverage evidence reaches the owner: {artifacts:?}"
        );
        assert!(
            !artifacts
                .iter()
                .any(|path| path == "already-landed.json" || path == "no-diff.json"),
            "a review writes no clean-tree report: {artifacts:?}"
        );
        fixture.assert_no_blocked();
    }
}

/// A claimed tagged task hands off `NoDiff` and has no PR route for a change,
/// so a pending diff or a commit of its own is refused by name, before the
/// index is touched [ORB-14791].
#[test]
fn a_claimed_no_diff_expected_task_that_changed_the_worktree_is_refused() {
    if !isolated(
        module_path!(),
        "a_claimed_no_diff_expected_task_that_changed_the_worktree_is_refused",
    ) {
        return;
    }
    for committed in [false, true] {
        let fixture = CleanLeaf::review_only("done");
        let follower = &fixture.pair.follower_repo;
        std::fs::write(follower.join("src/f0.rs"), "fn reviewed() {}\n").unwrap();
        if committed {
            git(follower, &["commit", "-q", "-am", "Review edits code"]);
        }
        let error = engine_action(&fixture.bound, "git_commit", &fixture.input).unwrap_err();
        assert!(
            error.to_string().contains("no_diff_expected_changed"),
            "committed={committed}: {error}"
        );
        assert!(
            git(follower, &["diff", "--cached", "--name-only"]).is_empty(),
            "the refusal stages nothing"
        );
        if !committed {
            assert_eq!(git(follower, &["rev-parse", "HEAD"]).trim(), fixture.base);
        }
        assert_eq!(fixture.pair.owner_status(&fixture.task), "in-progress");
        fixture.assert_no_blocked();
    }
}

/// The tag is the only authority a review's skip carries, and the owner's copy
/// of the task holds it: a skip checkpoint pinned for an untagged task is
/// refused at settlement, without blocking the task [ORB-14791].
#[test]
fn owner_refuses_a_no_diff_expected_skip_for_an_untagged_task() {
    if !isolated(
        module_path!(),
        "owner_refuses_a_no_diff_expected_skip_for_an_untagged_task",
    ) {
        return;
    }
    let fixture = CleanLeaf::new(false, "done");
    let mut handoff = fixture.prepare_handoff();
    let skip = json!({
        "phase": "commit", "decision": "skipped_no_diff_expected", "committed": false,
        "skipped_no_diff_expected": true, "task_id": fixture.task,
        "job_run_id": fixture.leaf, "base_sha": fixture.base,
    });
    let path = "no-diff-handoff/forged-skip.json";
    fixture.attach(path, skip.clone());
    handoff.candidate.delivery = HandoffDelivery::NoDiff {
        evidence: HandoffArtifactRef {
            path: path.into(),
            sha256: sha256_hex(&serde_json::to_vec(&skip).unwrap()),
        },
    };
    let error = fixture.settle(&handoff).unwrap_err();
    assert!(
        error.to_string().contains("no-diff-expected"),
        "the refusal names the missing tag: {error}"
    );
    assert_eq!(fixture.pair.owner_status(&fixture.task), "in-progress");
    assert!(
        fixture
            .pair
            .wire
            .owner
            .accepted_task_handoff(&handoff.claim_id)
            .is_err()
    );
    fixture.assert_no_blocked();
}

/// A follower's claimed `delivery-code-review` files each finding on the
/// owner with `regression_from` the task that introduced it, which the owner
/// checks is one of its own tasks, and the owner records the claim and run
/// that filed it. A finding the owner still refuses reaches it as the review
/// task's `unfiled-findings.json`, which the handoff attaches [ORB-14792].
#[test]
fn a_claimed_delivery_review_files_regression_findings_and_hands_off_the_rest() {
    if !isolated(
        module_path!(),
        "a_claimed_delivery_review_files_regression_findings_and_hands_off_the_rest",
    ) {
        return;
    }
    let mut fixture = CleanLeaf::delivery_review("done");
    let owner = fixture.pair.wire.owner.clone();
    let culprit = owner
        .run_tool(
            "orbit.task.add",
            json!({
                "title": "The delivery that introduced the defect",
                "description": "Landed in the reviewed batch.",
                "complexity": "low", "model": "codex",
                "workspace": fixture.pair.owner_repo.to_string_lossy(),
            }),
        )
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let relations = |culprit: &str| {
        json!([{"type": "spawned_from", "target": fixture.task},
               {"type": "regression_from", "target": culprit}])
    };
    let file = |culprit: &str| {
        fixture.bound.run_tool(
            "orbit.task.add",
            json!({
                "title": "Finding from the delivery review",
                "description": "The culprit delivery broke the invariant.",
                "complexity": "low", "type": "bug", "model": "codex",
                "tags": ["delivery-code-review"], "relations": relations(culprit),
            }),
        )
    };

    let filed = file(&culprit).expect("a claimed review files a regression finding");
    let id = filed["id"].as_str().expect("the finding's id");
    assert_eq!(
        fixture.pair.owner_task(id)["relations"],
        relations(&culprit)
    );
    let binding = fixture.bound.worker_invocation().unwrap().clone();
    assert!(
        owner
            .get_task_comments(id)
            .unwrap()
            .iter()
            .any(|comment| comment.message.contains(&binding.claim_id)
                && comment.message.contains(&fixture.leaf)),
        "the finding records the claim and run that filed it"
    );

    let before = owner.list_tasks().unwrap().len();
    let prefix = culprit.split('-').next().unwrap();
    let missing = file(&format!("{prefix}-999999")).unwrap_err().to_string();
    assert!(
        missing.contains("not a task in the claimed task's workspace"),
        "a culprit outside the owner's workspace is refused: {missing}"
    );
    assert_eq!(owner.list_tasks().unwrap().len(), before);

    let unfiled = json!([{
        "title": "A finding the owner refused",
        "description": "The owner refused it during the run.",
        "relations": relations(&culprit),
    }]);
    fixture.input["implementation"]["unfiled_findings"] = unfiled.clone();
    let handoff = fixture.run_clean_pipeline("task_claimed_pr_pipeline");
    let attached = owner
        .run_tool(
            "orbit.task.artifact.get",
            json!({"id": fixture.task, "path": "unfiled-findings.json"}),
        )
        .expect("the handoff attaches the unfiled findings to the review task");
    let record: Value = serde_json::from_str(attached["content"].as_str().unwrap()).unwrap();
    assert_eq!(record["findings"], unfiled, "{record}");
    assert_eq!(record["claim_id"], binding.claim_id);
    assert_eq!(record["run_id"], fixture.leaf);
    assert!(
        handoff.execution_summary.contains("unfiled-findings.json"),
        "the owner's summary points at the attachment: {}",
        handoff.execution_summary
    );
    fixture
        .settle(&handoff)
        .expect("the owner accepts the review's handoff");
}

/// A candidate that was published with plain-string `unfiled_findings` (the
/// implement step now refuses them before delivery) still hands off: each
/// string becomes a finding object that keeps its full text, the entry and
/// the owner's summary say it was normalized, and object entries pass through
/// untouched. A malformed entry that is not a string stays refused, and so
/// does a malformed field other than the findings [ORB-14927].
#[test]
fn a_published_candidates_string_findings_are_normalized_at_handoff() {
    if !isolated(
        module_path!(),
        "a_published_candidates_string_findings_are_normalized_at_handoff",
    ) {
        return;
    }
    let mut fixture = CleanLeaf::delivery_review("done");
    let owner = fixture.pair.wire.owner.clone();
    let long = format!("{} and on", "A very long first sentence".repeat(8));
    let object = json!({
        "title": "Already structured",
        "description": "Kept as returned.",
        "relations": [{"type": "spawned_from", "target": fixture.task}],
    });
    fixture.input["implementation"]["unfiled_findings"] = json!([
        "The owner refused this. It needs a follow-up on the retry path.",
        object,
        long,
    ]);
    let handoff = fixture.run_clean_pipeline("task_claimed_pr_pipeline");
    let attached = owner
        .run_tool(
            "orbit.task.artifact.get",
            json!({"id": fixture.task, "path": "unfiled-findings.json"}),
        )
        .expect("the handoff attaches the normalized findings");
    let record: Value = serde_json::from_str(attached["content"].as_str().unwrap()).unwrap();
    let findings = record["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 3, "{record}");
    assert_eq!(findings[0]["title"], "The owner refused this.");
    assert_eq!(
        findings[0]["description"],
        "The owner refused this. It needs a follow-up on the retry path.",
        "the full text survives"
    );
    assert_eq!(findings[0]["normalized_from"], "string");
    assert_eq!(findings[1], object, "an object entry is kept verbatim");
    assert_eq!(findings[2]["description"], long);
    assert!(findings[2]["title"].as_str().unwrap().len() <= 120);
    assert_eq!(record["normalized_string_entries"], 2);
    assert!(
        handoff.execution_summary.contains("normalized_from"),
        "the owner's summary records the repair: {}",
        handoff.execution_summary
    );
    fixture
        .settle(&handoff)
        .expect("the owner accepts the normalized handoff");

    for value in [json!([42]), json!(["   "]), json!("not an array")] {
        let mut fixture = CleanLeaf::delivery_review("done");
        fixture.input["implementation"]["unfiled_findings"] = value.clone();
        let handed_off = fixture
            .execute_clean_pipeline("task_claimed_pr_pipeline")
            .is_ok_and(|outcome| outcome.success);
        assert!(!handed_off, "{value} is still refused at handoff");
        assert!(
            fixture.pair.admission(&fixture.leaf).settlement.is_none(),
            "{value} records no handoff"
        );
    }
}

#[test]
fn no_diff_retains_owner_completion_authority() {
    if !isolated(module_path!(), "no_diff_retains_owner_completion_authority") {
        return;
    }
    let fixture = CleanLeaf::new(false, "review");
    let handoff = fixture.prepare_handoff();
    fixture.settle(&handoff).unwrap();
    assert_eq!(fixture.pair.owner_status(&fixture.task), "review");
    assert!(
        fixture
            .pair
            .wire
            .owner
            .landing_start_requests()
            .unwrap()
            .is_empty()
    );
    fixture.assert_no_blocked();
}

#[test]
fn owner_refuses_changed_no_diff_evidence_base_or_missing_required_checks_without_blocking() {
    if !isolated(
        module_path!(),
        "owner_refuses_changed_no_diff_evidence_base_or_missing_required_checks_without_blocking",
    ) {
        return;
    }
    for changed in ["report", "log", "base", "required_check"] {
        let fixture = CleanLeaf::new(false, "done");
        let mut handoff = fixture.prepare_handoff();
        match changed {
            "report" => {
                let HandoffDelivery::NoDiff { evidence } = &handoff.candidate.delivery else {
                    panic!("NoDiff")
                };
                fixture.attach(&evidence.path, json!({"decision": "verified_no_diff"}));
            }
            "log" => fixture.attach(
                "implementation-validation.json",
                json!({
                    "run_id": fixture.leaf, "tested_head": fixture.base, "command": CHECK,
                    "exit_code": 0, "output": "changed after checkpoint",
                }),
            ),
            "base" => {
                std::fs::write(fixture.pair.owner_repo.join("src/f0.rs"), "fn newer() {}\n")
                    .unwrap();
                git(
                    &fixture.pair.owner_repo,
                    &["commit", "-q", "-am", "Base advances"],
                );
                publish_origin(&fixture.pair.owner_repo);
            }
            "required_check" => handoff.validation.clear(),
            _ => unreachable!(),
        }
        let error = fixture.settle(&handoff).expect_err(changed);
        assert!(
            matches!(
                error,
                OrbitError::PolicyDenied(_)
                    | OrbitError::InvalidInput(_)
                    | OrbitError::Execution(_)
            ),
            "{changed}: {error}"
        );
        assert_eq!(fixture.pair.owner_status(&fixture.task), "in-progress");
        assert!(
            fixture
                .pair
                .wire
                .owner
                .accepted_task_handoff(&handoff.claim_id)
                .is_err()
        );
        fixture.assert_no_blocked();
    }
}

#[test]
fn claimed_no_diff_requires_a_verifier_checkpoint_and_passing_logs() {
    if !isolated(
        module_path!(),
        "claimed_no_diff_requires_a_verifier_checkpoint_and_passing_logs",
    ) {
        return;
    }
    let fixture = CleanLeaf::new(false, "done");
    let mut input = fixture.input.clone();
    input["skipped_no_diff_expected"] = json!(true);
    let error = engine_action(&fixture.bound, "claim_validate", &input).unwrap_err();
    assert!(
        error.to_string().contains("skip flag or tag alone"),
        "{error}"
    );
    let mut escaped = fixture.input.clone();
    escaped["implementation"]["no_diff_artifacts"][0]["source_path"] = json!("src/f0.rs");
    let error = engine_action(&fixture.bound, "git_commit", &escaped).unwrap_err();
    assert!(
        error.to_string().contains("outside workspace_root"),
        "scratch import cannot read other checkout files: {error}"
    );
    std::fs::write(
        fixture
            .pair
            .follower_repo
            .join(".orbit/tmp/implementation-validation.json"),
        serde_json::to_vec(&json!({
            "run_id": fixture.leaf, "tested_head": fixture.base, "command": CHECK,
            "exit_code": 1, "output": "failed",
        }))
        .unwrap(),
    )
    .unwrap();
    let error = engine_action(&fixture.bound, "git_commit", &fixture.input).unwrap_err();
    assert!(error.to_string().contains("validation log"), "{error}");
    assert_eq!(fixture.pair.owner_status(&fixture.task), "in-progress");
    fixture.assert_no_blocked();
}
