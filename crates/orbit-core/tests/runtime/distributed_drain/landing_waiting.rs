//! Waiting gates resume the same authorized landing without an operator.

use super::*;
use chrono::TimeDelta;
use orbit_types::workflow::handoff::LandingWaitGate;

struct WaitingOwner {
    owner: OwnerLocal,
    task: String,
    handoff: String,
    forge: PathBuf,
}

impl WaitingOwner {
    fn new() -> Self {
        let mut pair =
            Pair::with_owner_config("[workflow]\ndistributed_completion = \"done\"\n", &[None]);
        let workers = LandingWorkers::new();
        let jobs = pair.wire.owner.global_root().join("resources/jobs");
        std::fs::create_dir_all(&jobs).unwrap();
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/jobs/task_landing_pipeline.yaml"),
            jobs.join("task_landing_pipeline.yaml"),
        )
        .unwrap();
        let repo = &pair.owner_repo;
        git(repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join(".gitignore"), "/.orbit/\n").unwrap();
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-q", "-m", "base"]);
        let bare = pair._root.path().join("owner/repository.git");
        std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
        git(
            bare.parent().unwrap(),
            &["init", "--bare", "-q", bare.to_str().unwrap()],
        );
        git(repo, &["remote", "add", "origin", bare.to_str().unwrap()]);
        git(repo, &["push", "-q", "origin", "main"]);
        let drain = pair.run_drain();
        let leaf = pair.running_leaf(&drain, 1);
        let record = pair.admission(&leaf);
        let mut delivered = handoff(&record);
        git(
            repo,
            &["checkout", "-q", "-b", &delivered.candidate.source_branch],
        );
        std::fs::write(repo.join("src/f0.rs"), "fn changed() {}\n").unwrap();
        git(repo, &["add", "src/f0.rs"]);
        git(repo, &["commit", "-q", "-m", "candidate"]);
        git(repo, &["checkout", "-q", "main"]);
        delivered.candidate = orbit_engine::observe_candidate(
            repo,
            Some(&delivered.candidate.source_branch),
            "main",
            "main",
            HandoffDelivery::PullRequest { number: 42 },
            "owner/repository",
            orbit_engine::BaseSyncMode::Local,
        )
        .unwrap();
        let forge = pair._root.path().join("waiting-forge");
        std::fs::create_dir_all(&forge).unwrap();
        std::fs::write(
            PathBuf::from(std::env::var("HOME").unwrap()).join("waiting-forge-path"),
            forge.to_str().unwrap(),
        )
        .unwrap();
        std::fs::write(
            forge.join("candidate.json"),
            serde_json::to_vec(&delivered.candidate).unwrap(),
        )
        .unwrap();
        std::fs::write(forge.join("phase"), "pending").unwrap();
        install_waiting_forge();
        pair.wire.accept(delivered.clone()).unwrap();
        let runtime = &pair.wire.owner;
        let accepted = runtime.accepted_task_handoff(&delivered.claim_id).unwrap();
        if runtime.landing_attempts().unwrap().is_empty() {
            let context = ClaimInvocation::trusted_operator(
                delivered.task_id.clone(),
                delivered.claim_id.clone(),
                "owner-landing".into(),
            )
            .with_handoff_observation(HandoffObservation {
                candidate: delivered.candidate.clone(),
                footprint_widening: vec![],
                required_commands: vec![],
                owner_completion_authority: None,
                review: None,
                known_red: vec![],
                light_delivery: vec![],
            });
            runtime
                .mutate_execution_claim(
                    Some(&context),
                    "approve-waiting-fixture",
                    &ClaimMutation::ApproveHandoff {
                        handoff_id: accepted.handoff_id.clone(),
                        candidate: delivered.candidate.clone(),
                    },
                )
                .unwrap();
            runtime.dispatch_landing_requests().unwrap();
        }
        pair.follower_jobs = orbit_store::compose::workspace_job_run_store(
            runtime.sqlite_store().unwrap(),
            runtime.workspace_id().unwrap(),
        );
        Self {
            owner: OwnerLocal {
                pair,
                drain,
                workers,
            },
            task: delivered.task_id,
            handoff: accepted.handoff_id,
            forge,
        }
    }

    fn phase(&self, phase: &str) {
        std::fs::write(self.forge.join("phase"), phase).unwrap();
    }

    fn attempt(&self) -> LandingAttempt {
        self.owner
            .pair
            .wire
            .owner
            .landing_attempts()
            .unwrap()
            .remove(0)
    }

    fn stop(&self, gate: LandingWaitGate) -> LandingAttempt {
        if gate == LandingWaitGate::RequiredChecksPending {
            let result = orbit_engine::execute_deterministic_action(
                &self.owner.pair.wire.owner,
                "handoff_land",
                &json!({}),
                &json!({"handoff_id":self.handoff, "max_wait_seconds":0}),
                false,
                &Default::default(),
                None,
            );
            assert!(
                result.is_err(),
                "pending checks stop the public landing action"
            );
        } else {
            // The public host boundary receives this typed stop from landing's
            // base comparison. Its actual forge reads and retry are exercised below.
            self.owner.stop_landing(
                &self.task,
                orbit_engine::HandoffLandingStep::Stop { repairable: false },
                &json!({"schema_version":1,"kind":"owner_landing_refusal",
                    "reason":{"value":"base checks are still pending"},
                    "waiting_gate":gate})
                .to_string(),
            );
        }
        let attempt = self.attempt();
        assert_eq!(attempt.state, LandingAttemptState::Stopped);
        let evidence: Value = serde_json::from_str(attempt.evidence.as_deref().unwrap()).unwrap();
        assert_eq!(
            evidence["waiting_gate"],
            serde_json::to_value(gate).unwrap()
        );
        self.owner.finish_landing_worker(
            attempt.job_run_id.as_deref().unwrap(),
            JobRunState::Cancelled,
        );
        attempt
    }

    fn dispatch(
        &self,
        now: chrono::DateTime<Utc>,
    ) -> Vec<orbit_core::application::landing::LandingDispatch> {
        self.owner
            .pair
            .wire
            .owner
            .dispatch_landing_requests_at(now)
            .unwrap()
    }
}

fn waiting_gates_resume_and_land_the_same_handoff() {
    for gate in [
        LandingWaitGate::BaseChecksPending,
        LandingWaitGate::RequiredChecksPending,
    ] {
        let fixture = WaitingOwner::new();
        fixture.phase(if gate == LandingWaitGate::BaseChecksPending {
            "base_pending"
        } else {
            "pending"
        });
        let stopped = fixture.stop(gate);
        let due = stopped.updated_at + TimeDelta::seconds(60);
        assert!(
            fixture.dispatch(due).is_empty(),
            "unchanged pending gate consumes no attempt"
        );
        fixture.phase(if gate == LandingWaitGate::BaseChecksPending {
            "base_green"
        } else {
            "green"
        });
        assert!(
            fixture
                .dispatch(due - TimeDelta::milliseconds(1))
                .is_empty(),
            "backoff survives a green forge"
        );
        let retried = fixture.dispatch(due);
        assert_eq!(
            retried.len(),
            1,
            "cleared {gate:?} resumes without an operator"
        );
        assert_eq!(retried[0].handoff_id, fixture.handoff);
        assert_eq!(retried[0].attempt, 2);
        assert!(
            fixture.dispatch(due).is_empty(),
            "a live successor is not duplicated"
        );
        fixture.phase("green");
        assert_eq!(
            fixture.owner.land(&fixture.task).unwrap()["phase"],
            "landed"
        );
        assert_eq!(fixture.owner.pair.owner_status(&fixture.task), "done");
        assert!(
            fixture.forge.join("merged").exists(),
            "the owner requested the merge"
        );
        assert_eq!(
            fixture
                .owner
                .pair
                .wire
                .owner
                .landing_attempt_record(&fixture.handoff, 1)
                .unwrap(),
            Some(stopped)
        );
        let history = fixture
            .owner
            .pair
            .wire
            .owner
            .get_task_history(&fixture.task)
            .unwrap();
        let note = history
            .iter()
            .find(|entry| entry.event == "landing_retry_reserved")
            .unwrap()
            .note
            .as_deref()
            .unwrap();
        assert!(
            note.contains(gate.stop_code())
                && note.contains("retry 1/3")
                && note.contains("landing_attempt=2"),
            "{note}"
        );
    }
}

fn waiting_retries_are_bounded_and_candidate_stops_stay_parked() {
    let fixture = WaitingOwner::new();
    for number in 1..=4 {
        fixture.phase("pending");
        let stopped = fixture.stop(LandingWaitGate::RequiredChecksPending);
        assert_eq!(stopped.attempt, number);
        fixture.phase("green");
        let due = stopped.updated_at + TimeDelta::seconds(60 * (1_i64 << (number - 1)));
        assert!(
            fixture
                .dispatch(due - TimeDelta::milliseconds(1))
                .is_empty()
        );
        let retried = fixture.dispatch(due);
        if number == 4 {
            assert!(
                retried.is_empty(),
                "three retries exhaust this handoff's budget"
            );
        } else {
            assert_eq!(retried.len(), 1);
        }
    }
    let history = fixture
        .owner
        .pair
        .wire
        .owner
        .get_task_history(&fixture.task)
        .unwrap();
    assert_eq!(
        history
            .iter()
            .filter(|entry| entry.event == "landing_retry_reserved")
            .count(),
        3
    );

    // Candidate failure, content conflict and provider review refusal retain
    // their normal public landing outcomes instead of becoming retry authority.
    for reason in [
        "candidate_failure",
        "delivery_landing_conflict",
        "review_refused",
    ] {
        let fixture = WaitingOwner::new();
        if reason == "candidate_failure" {
            fixture.owner.stop_landing(
                &fixture.task,
                orbit_engine::HandoffLandingStep::StopForRequeue,
                &json!({"schema_version":1,"kind":"owner_landing_refusal",
                    "repairable":true,"requeue":true,
                    "reason":{"value":"required check is the candidate's own failure"}})
                .to_string(),
            );
        } else {
            fixture.phase(if reason == "review_refused" {
                "review"
            } else {
                "conflict"
            });
            let result = orbit_engine::execute_deterministic_action(
                &fixture.owner.pair.wire.owner,
                "handoff_land",
                &json!({}),
                &json!({"handoff_id":fixture.handoff, "max_wait_seconds":0}),
                false,
                &Default::default(),
                None,
            );
            assert!(result.is_err(), "{reason}");
        }
        let stopped = fixture.attempt();
        assert_eq!(stopped.state, LandingAttemptState::Stopped);
        let evidence: Value = serde_json::from_str(stopped.evidence.as_deref().unwrap()).unwrap();
        assert!(
            evidence["waiting_gate"].is_null(),
            "{reason} must never grant automatic retry"
        );
        fixture.owner.finish_landing_worker(
            stopped.job_run_id.as_deref().unwrap(),
            JobRunState::Cancelled,
        );
        fixture.phase("green");
        assert!(
            fixture
                .dispatch(stopped.updated_at + TimeDelta::hours(1))
                .is_empty(),
            "{reason}"
        );
        assert_eq!(fixture.attempt(), stopped);
    }
}

struct WaitingOwnerProvider(OrbitRuntime);

impl RoutineWorkspaceProvider for WaitingOwnerProvider {
    fn discover_workspaces(&self, _: &Path) -> Result<DiscoveredWorkspaces, OrbitError> {
        let now = Utc::now();
        Ok(DiscoveredWorkspaces {
            entries: vec![(
                Workspace {
                    id: self.0.workspace_id()?,
                    name: "waiting-owner".into(),
                    owner_machine_id: Some(OWNER.into()),
                    git_remote: None,
                    ship_mode: None,
                    base_branch: "main".into(),
                    status: WorkspaceStatus::Active,
                    created_at: now,
                    updated_at: now,
                },
                self.0.clone(),
            )],
            ..Default::default()
        })
    }
}

fn the_owner_clock_resumes_waiting_landings_but_dry_runs_do_not() {
    let fixture = WaitingOwner::new();
    let mut stopped = fixture.stop(LandingWaitGate::RequiredChecksPending);
    let runtime = &fixture.owner.pair.wire.owner;
    // Backdate only this fixture's stop instead of sleeping or creating load.
    stopped.updated_at = Utc::now() - TimeDelta::minutes(2);
    runtime.sqlite_store().unwrap().with_transaction(|tx| {
        let changed = tx.connection().execute(
            "UPDATE task_coordination_rows SET payload_json=?1 WHERE workspace_id=?2 AND kind='distributed-landing-attempt-v1' AND row_id=?3",
            rusqlite::params![serde_json::to_string(&stopped).unwrap(), runtime.workspace_id().unwrap(), fixture.handoff],
        ).unwrap();
        assert_eq!(changed, 1, "the waiting stop from the incident is aged in isolation");
        Ok(())
    }).unwrap();
    fixture.phase("green");
    let provider = WaitingOwnerProvider(runtime.clone());
    for dry_run in [true, false] {
        run_sweep_at_with_providers(
            &runtime.global_root(),
            SweepOptions {
                dry_run,
                ..Default::default()
            },
            RoutineMachineIdentity {
                machine_id: OWNER.into(),
                machine_name: "owner".into(),
            },
            &provider,
        )
        .unwrap();
        assert_eq!(fixture.attempt().attempt, if dry_run { 1 } else { 2 });
    }
    assert_eq!(
        fixture.owner.land(&fixture.task).unwrap()["phase"],
        "landed"
    );
}

/// Two owner handoffs accepted and approved for two backlog tasks, each with a
/// `Pending` landing start request. Returns the owner and each handoff's
/// `(claim_id, handoff_id)`.
fn two_approved_landings() -> (OwnerLocal, Vec<(String, String)>) {
    let mut pair = Pair::with_owner_config(
        "[workflow]\ndistributed_completion = \"done\"\n",
        &[None, None],
    );
    let workers = LandingWorkers::new();
    let jobs = pair.wire.owner.global_root().join("resources/jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/jobs/task_landing_pipeline.yaml"),
        jobs.join("task_landing_pipeline.yaml"),
    )
    .unwrap();
    let repo = pair.owner_repo.clone();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitignore"), "/.orbit/\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    let drain = pair.run_drain();
    let mut approved = Vec::new();
    for (slots, file, number) in [(1, "src/f0.rs", 42), (2, "src/f1.rs", 43)] {
        let leaf = pair.running_leaf(&drain, slots);
        let record = pair.admission(&leaf);
        let mut delivered = handoff(&record);
        let branch = delivered.candidate.source_branch.clone();
        git(&repo, &["checkout", "-q", "-b", &branch]);
        std::fs::write(repo.join(file), "fn changed() {}\n").unwrap();
        git(&repo, &["add", file]);
        git(&repo, &["commit", "-q", "-m", "candidate"]);
        git(&repo, &["checkout", "-q", "main"]);
        delivered.candidate = orbit_engine::observe_candidate(
            &repo,
            Some(&branch),
            "main",
            "main",
            HandoffDelivery::PullRequest { number },
            "owner/repository",
            orbit_engine::BaseSyncMode::Local,
        )
        .unwrap();
        pair.wire.accept(delivered.clone()).unwrap();
        let runtime = &pair.wire.owner;
        let accepted = runtime.accepted_task_handoff(&delivered.claim_id).unwrap();
        let context = ClaimInvocation::trusted_operator(
            delivered.task_id.clone(),
            delivered.claim_id.clone(),
            "owner-landing".into(),
        )
        .with_handoff_observation(HandoffObservation {
            candidate: delivered.candidate.clone(),
            footprint_widening: vec![],
            required_commands: vec![],
            owner_completion_authority: None,
            review: None,
            known_red: vec![],
            light_delivery: vec![],
        });
        runtime
            .mutate_execution_claim(
                Some(&context),
                "approve-waiting-fixture",
                &ClaimMutation::ApproveHandoff {
                    handoff_id: accepted.handoff_id.clone(),
                    candidate: delivered.candidate.clone(),
                },
            )
            .unwrap();
        approved.push((delivered.claim_id.clone(), accepted.handoff_id.clone()));
    }
    // Dispatch as the authorized-handoff path does, opening each landing job.
    pair.wire.owner.dispatch_landing_requests().unwrap();
    pair.follower_jobs = orbit_store::compose::workspace_job_run_store(
        pair.wire.owner.sqlite_store().unwrap(),
        pair.wire.owner.workspace_id().unwrap(),
    );
    (
        OwnerLocal {
            pair,
            drain,
            workers,
        },
        approved,
    )
}

/// Leave a claim's landing context invalidated while its handoff authority
/// stays unrevoked, so its landing start request remains `Pending`.
fn invalidate_landing_context(runtime: &OrbitRuntime, claim_id: &str) {
    runtime
        .sqlite_store()
        .unwrap()
        .with_transaction(|tx| {
            let workspace = runtime.workspace_id().unwrap();
            let payload: String = tx.connection().query_row(
                "SELECT payload_json FROM task_coordination_rows WHERE workspace_id=?1 AND kind='distributed-claim-lifecycle-v1' AND row_id=?2",
                rusqlite::params![workspace, claim_id],
                |row| row.get(0),
            ).unwrap();
            let mut state: Value = serde_json::from_str(&payload).unwrap();
            assert_eq!(
                state["landing_invalidated"], false,
                "the claim starts with its landing context open"
            );
            state["landing_invalidated"] = json!(true);
            let changed = tx.connection().execute(
                "UPDATE task_coordination_rows SET payload_json=?1 WHERE workspace_id=?2 AND kind='distributed-claim-lifecycle-v1' AND row_id=?3",
                rusqlite::params![state.to_string(), workspace, claim_id],
            ).unwrap();
            assert_eq!(changed, 1, "the claim lifecycle row is invalidated in isolation");
            Ok(())
        })
        .unwrap();
}

/// [ORB-23238] A Pending landing whose claim is invalidated fails its own
/// dispatch, and the owner pass still dispatches the other Pending landing.
fn an_invalidated_pending_landing_does_not_starve_later_landings() {
    let (owner, mut approved) = two_approved_landings();
    let runtime = &owner.pair.wire.owner;
    // The fixture's dispatch opened one landing job per handoff. Ending both
    // jobs leaves each handoff open without a live job, so the next pass
    // reaches the landing path for both.
    let attempts = runtime.landing_attempts().unwrap();
    assert_eq!(
        attempts.len(),
        2,
        "one landing attempt per approved handoff"
    );
    for attempt in attempts {
        owner.finish_landing_worker(
            attempt.job_run_id.as_deref().unwrap(),
            JobRunState::Cancelled,
        );
    }
    // The pass walks start requests in handoff-id order, so the invalidated
    // request is the one reached first.
    approved.sort_by(|a, b| a.1.cmp(&b.1));
    let (failing_claim, _) = &approved[0];
    let (_, valid) = &approved[1];
    invalidate_landing_context(runtime, failing_claim);
    let dispatched = runtime
        .dispatch_landing_requests_at(Utc::now())
        .expect("one failing landing request does not fail the owner pass");
    let handoffs: Vec<&str> = dispatched.iter().map(|d| d.handoff_id.as_str()).collect();
    assert_eq!(handoffs, vec![valid.as_str()], "{dispatched:?}");
    assert!(dispatched[0].submitted, "{dispatched:?}");
}

#[cfg(unix)]
fn install_waiting_forge() {
    use std::os::unix::fs::PermissionsExt;
    let bin = PathBuf::from(std::env::var("HOME").unwrap()).join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let gh = bin.join("gh");
    std::fs::write(&gh, r#"#!/usr/bin/env python3
import json, os, pathlib, sys
f = pathlib.Path((pathlib.Path(os.environ['HOME']) / 'waiting-forge-path').read_text())
c = json.loads((f / 'candidate.json').read_text())
p = (f / 'phase').read_text()
a = sys.argv[1:]
head = c['candidate']['commit']
base = c['base']['commit']
check = {'__typename':'CheckRun','name':'Test','status':'COMPLETED' if p in ['green','base_green'] else 'IN_PROGRESS','conclusion':'SUCCESS','isRequired':True,'checkSuite':{'workflowRun':{'workflow':{'name':'CI'}}}}
if p.startswith('base_'):
    check['status'] = 'COMPLETED'
    check['conclusion'] = 'FAILURE'
pr = {'number':42,'state':'MERGED' if (f / 'merged').exists() else 'OPEN','isDraft':False,'mergedAt':'2026-11-08T00:00:00Z' if (f / 'merged').exists() else None,'mergeStateStatus':'CLEAN' if p == 'green' else 'BLOCKED','reviewDecision':'APPROVED','headRefName':c['source_branch'],'headRefOid':head,'baseRefName':c['base_branch'],'mergeCommit':{'oid':head} if (f / 'merged').exists() else None,'url':'https://github.com/owner/repository/pull/42','statusCheckRollup':[check]}
if p == 'review':
    pr['reviewDecision'] = 'REVIEW_REQUIRED'
if p == 'conflict':
    pr['mergeStateStatus'] = 'DIRTY'
if a[:2] == ['pr','view']:
    out = pr
elif any(x.endswith('/pulls/42/merge') for x in a):
    (f / 'merged').write_text('owner requested merge')
    out = {'merged':True,'sha':head,'message':'merged'}
elif a[:2] == ['pr','merge']:
    (f / 'merged').write_text('owner requested merge')
    sys.exit(0)
elif a[:2] == ['repo','view']:
    out = {'nameWithOwner':'owner/repository'}
elif 'graphql' in a:
    query = ' '.join(a)
    if 'autoMergeAllowed' in query:
        out = {'data':{'repository':{'autoMergeAllowed':False,'mergeCommitAllowed':True,'rebaseMergeAllowed':False,'squashMergeAllowed':False,'pullRequest':{'baseRefName':c['base_branch'],'baseRef':{'branchProtectionRule':{'requiresLinearHistory':False}}}}}}
    else:
        out = {'data':{'repository':{'pullRequest':{'headRefOid':head,'commits':{'nodes':[{'commit':{'oid':head,'statusCheckRollup':{'contexts':{'pageInfo':{'hasNextPage':False},'nodes':[check]}},'checkSuites':{'pageInfo':{'hasNextPage':False},'nodes':[]}}}]}}}}}
elif any('/actions/runs/1/jobs' in x for x in a):
    out = {'total_count':1,'jobs':[{'id':2,'name':'Test','status':'completed' if p == 'base_green' else 'in_progress','conclusion':'success' if p == 'base_green' else None,'steps':[]}]}
elif any('/actions/runs' in x for x in a):
    out = {'total_count':1,'workflow_runs':[{'id':1,'workflow_id':3,'name':'CI','head_sha':base,'head_branch':c['base_branch'],'event':'push','status':'completed' if p == 'base_green' else 'in_progress','conclusion':'success' if p == 'base_green' else None}]}
else:
    raise RuntimeError(a)
print(json.dumps(out))
"#).unwrap();
    std::fs::set_permissions(gh, std::fs::Permissions::from_mode(0o755)).unwrap();
}

crate::scenario_table!(
    automatic_waiting_landing_scenarios,
    waiting_gates_resume_and_land_the_same_handoff,
    waiting_retries_are_bounded_and_candidate_stops_stay_parked,
    the_owner_clock_resumes_waiting_landings_but_dry_runs_do_not,
    an_invalidated_pending_landing_does_not_starve_later_landings
);
