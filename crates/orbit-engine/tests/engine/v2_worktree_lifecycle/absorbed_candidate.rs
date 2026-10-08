//! [ORB-14668] A candidate that conflict recovery finds already on its pinned
//! base, run through `execute_job_with_resume`.
//!
//! `sync_base` is the shipped `git_rebase` action, recovered by a substitute
//! `pr_conflict_recovery` provider that resolves the conflict to the base's
//! side, so the continued rebase drops the candidate's only pick. The job's
//! `decide` final recovery and `handoff` failure activity are host stubs that
//! record whether they ran. The neighbours keep the certified absorbed
//! checkpoint but change what final recovery re-observes, and each must reach
//! the agent with its typed refusal instead of settling.

use orbit_engine::{JobOutcome, V2SqliteSink, execute_job_with_resume};
use orbit_types::workflow::activity_job::{ActivityV2, JobV2, V2AuditEventKind};

use super::*;

/// What a job run left behind.
struct AbsorbedRun {
    result: Result<JobOutcome, DispatchError>,
    events: Vec<V2AuditEventKind>,
}

impl AbsorbedRun {
    fn post_recovery_outcomes(&self) -> Vec<(String, Option<String>)> {
        self.events
            .iter()
            .filter_map(|event| match event {
                V2AuditEventKind::StepPostRecoveryAttempt {
                    outcome,
                    error_message,
                    ..
                } => Some((outcome.clone(), error_message.clone())),
                _ => None,
            })
            .collect()
    }

    fn recovery_attempts(&self) -> Vec<bool> {
        self.events
            .iter()
            .filter_map(|event| match event {
                V2AuditEventKind::StepRecoveryAttempted {
                    recovery_succeeded, ..
                } => Some(*recovery_succeeded),
                _ => None,
            })
            .collect()
    }

    fn final_recovery(&self) -> Vec<(String, Option<String>)> {
        self.events
            .iter()
            .filter_map(|event| match event {
                V2AuditEventKind::FinalRecoveryAttempted {
                    outcome, decision, ..
                } => Some((outcome.clone(), decision.clone())),
                _ => None,
            })
            .collect()
    }
}

#[test]
fn an_absorbed_candidate_settles_as_verified_no_diff_without_an_agent() {
    isolated_in(
        module_path!(),
        "an_absorbed_candidate_settles_as_verified_no_diff_without_an_agent",
        || {
            let prepared = PreparedRebase::new("jrun-absorbed", "README.md", "README.md");
            let host = resolving_host(&prepared, "printf 'target\\n' > README.md");
            stub_pipeline_steps(&host, &prepared);

            let run = execute(&host, &prepared, &pipeline(git_rebase_step()));

            // Settled, the run still fails: it delivered nothing.
            assert!(
                run.result.is_err(),
                "a settled run does not complete: {:?}",
                run.result.as_ref().map(|outcome| &outcome.message)
            );
            // The certified continuation names the base commit carrying the
            // candidate's change.
            let checkpoints = host.checkpoints();
            let [(_, step_id, checkpoint)] = checkpoints.as_slice() else {
                panic!("expected one certified recovery, got {checkpoints:#?}");
            };
            assert_eq!(step_id, "sync_base");
            assert_eq!(checkpoint["head_sha"], prepared.target);
            assert_eq!(checkpoint["absorbed"]["covering_commit"], prepared.target);
            assert_eq!(prepared.head(), prepared.target);
            assert_eq!(git(&prepared.checkout.path, &["status", "--porcelain"]), "");

            // Recovery worked, and its retry reports the absorption rather
            // than failing as an attempt.
            assert_eq!(run.recovery_attempts(), vec![true]);
            let post = run.post_recovery_outcomes();
            let [(outcome, Some(message))] = post.as_slice() else {
                panic!("expected one post-recovery attempt, got {post:#?}");
            };
            assert_eq!(outcome, "absorbed", "{message}");
            assert!(message.contains("[candidate_absorbed]"), "{message}");

            // Final recovery settles through the host applier without
            // dispatching its agent or the failure handoff.
            assert_eq!(host.stub_calls_of("decide"), Vec::<Value>::new());
            assert_eq!(host.stub_calls_of("handoff"), Vec::<Value>::new());
            assert_eq!(host.stub_calls_of("deliver"), Vec::<Value>::new());
            assert_eq!(
                run.final_recovery(),
                vec![("settled".to_string(), Some("complete_no_diff".to_string()))]
            );
            assert_eq!(host.final_recovery_admissions.lock().unwrap().len(), 1);
            let applications = host.final_recovery_applications.lock().unwrap().clone();
            let [application] = applications.as_slice() else {
                panic!("expected one application, got {applications:#?}");
            };
            assert_eq!(application.task_id, "T-REBASE");
            assert_eq!(application.failed_step_id, "sync_base");
            let FinalRecoveryDecision::CompleteNoDiff {
                evidence_commit, ..
            } = &application.decision
            else {
                panic!("expected complete_no_diff, got {:?}", application.decision);
            };
            assert_eq!(evidence_commit, &prepared.target);
            assert_eq!(application.resume_step_index, None);
            assert_eq!(application.repair_commit, None);
            // No exemption and no completion authority: the host applier
            // decides `review` (or the claimed owner's settlement) as for any
            // other `complete_no_diff`, and the engine never moves the task.
            assert!(
                !application.completion_done,
                "the run holds no `completion: done` authority"
            );
            assert_eq!(
                host.get_task("T-REBASE").unwrap().status,
                TaskStatus::InProgress
            );
        },
    );
}

/// The resolution keeps a candidate commit on the pin, but the base advanced
/// with that same change meanwhile: following the tip drops the pick, and the
/// candidate is certified absorbed on the tip it followed.
#[test]
fn a_candidate_the_advanced_base_absorbed_is_certified_on_that_tip() {
    isolated_in(
        module_path!(),
        "a_candidate_the_advanced_base_absorbed_is_certified_on_that_tip",
        || {
            let prepared = PreparedRebase::new("jrun-advanced", "README.md", "README.md");
            let host = resolving_host(&prepared, "printf 'resolved\\n' > README.md");
            let conflict = stopped_conflict(&prepared);
            let advanced = commit_file(&prepared.fixture.repo, "README.md", "resolved\n");

            let outcome = recover(
                &host,
                &prepared.run_id,
                recovery_input_for(&prepared, &prepared.prepared, &conflict),
            )
            .expect("the absorbed continuation is certified");

            assert!(outcome.success, "{:?}", outcome.message);
            assert_eq!(prepared.head(), advanced);
            let [(_, _, checkpoint)] = host.checkpoints().try_into().unwrap();
            assert_eq!(checkpoint["target_base_sha"], prepared.target);
            assert_eq!(checkpoint["base_sha"], advanced);
            assert_eq!(checkpoint["head_sha"], advanced);
            assert!(
                checkpoint["absorbed"]["covering_commit"].is_string(),
                "{checkpoint:#}"
            );
            let retried = prepared
                .rebase_on(&host, &prepared.prepared)
                .expect_err("an absorbed candidate fails its step")
                .to_string();
            assert!(retried.contains("[candidate_absorbed]"), "{retried}");
        },
    );
}

/// The resolution also reverted a path no base commit touched: the candidate
/// was dropped, not absorbed, and the continuation is refused as before.
#[test]
fn a_dropped_candidate_is_refused_at_the_continuation() {
    isolated_in(
        module_path!(),
        "a_dropped_candidate_is_refused_at_the_continuation",
        || {
            let mut prepared = PreparedRebase::new("jrun-dropped", "README.md", "README.md");
            let checkout = prepared.checkout.path.clone();
            fs::write(checkout.join("base.txt"), "candidate\n").unwrap();
            git(&checkout, &["add", "base.txt"]);
            git(&checkout, &["commit", "--amend", "--no-edit"]);
            prepared.candidate = prepared.head();
            prepared.prepared = action(&prepared.host, "pr_prepare", &prepared.common).unwrap();
            let host = resolving_host(
                &prepared,
                "printf 'target\\n' > README.md\nprintf 'v1\\n' > base.txt",
            );
            let conflict = stopped_conflict(&prepared);

            let error = recover(
                &host,
                &prepared.run_id,
                recovery_input_for(&prepared, &prepared.prepared, &conflict),
            )
            .expect_err("a dropped candidate is not certified");

            assert!(
                error
                    .to_string()
                    .contains("absorbed_candidate_refused: no_covering_commit"),
                "{error}"
            );
            assert!(host.checkpoints().is_empty());
        },
    );
}

/// Each neighbour of an absorbed candidate keeps the certified checkpoint but
/// changes what final recovery re-observes. None settles: the agent runs and
/// reads the typed reason.
#[test]
fn absorbed_neighbours_fail_closed_with_a_typed_reason() {
    isolated_in(
        module_path!(),
        "absorbed_neighbours_fail_closed_with_a_typed_reason",
        || {
            type Change = fn(&PreparedRebase, &LifecycleHost);
            let cases: [(&str, Change, &str); 11] = [
                (
                    "a branch moved off its pinned base",
                    |prepared, _| {
                        git(
                            &prepared.checkout.path,
                            &["reset", "--hard", &prepared.base_sha],
                        );
                    },
                    "not_on_pinned_base",
                ),
                (
                    "a dirty checkout",
                    |prepared, _| {
                        fs::write(prepared.checkout.path.join("README.md"), "edited\n").unwrap();
                    },
                    "dirty_worktree",
                ),
                (
                    "an untracked file",
                    |prepared, _| {
                        fs::write(prepared.checkout.path.join("stray.txt"), "stray\n").unwrap();
                    },
                    "dirty_worktree",
                ),
                (
                    "rebase state left in the checkout",
                    |prepared, _| {
                        fs::create_dir_all(git_path(&prepared.checkout.path, "rebase-merge"))
                            .unwrap();
                    },
                    "rebase_in_progress",
                ),
                (
                    "another branch checked out",
                    |prepared, _| {
                        git(&prepared.checkout.path, &["checkout", "-b", "elsewhere"]);
                    },
                    "wrong_branch",
                ),
                (
                    "a checkpoint for another attempt",
                    |prepared, host| {
                        let mut stale = certified(host);
                        stale["recovery_attempt"] = json!(2);
                        host.leaf_writes_recovery(&prepared.run_id, "sync_base", stale);
                    },
                    "stale_checkpoint",
                ),
                (
                    "a certified checkpoint for another run",
                    |prepared, host| {
                        let mut foreign = certified(host);
                        foreign["run_id"] = json!("jrun-other");
                        host.checkpoint_rebase_recovery(&prepared.run_id, "sync_base", &foreign)
                            .unwrap();
                    },
                    "stale_checkpoint",
                ),
                (
                    "a certified checkpoint for another step",
                    |prepared, host| {
                        let mut foreign = certified(host);
                        foreign["step_id"] = json!("complete_pr");
                        host.checkpoint_rebase_recovery(&prepared.run_id, "sync_base", &foreign)
                            .unwrap();
                    },
                    "stale_checkpoint",
                ),
                (
                    "a certified checkpoint for another task",
                    |prepared, host| {
                        let mut foreign = certified(host);
                        foreign["task_ids"] = json!(["T-OTHER"]);
                        host.checkpoint_rebase_recovery(&prepared.run_id, "sync_base", &foreign)
                            .unwrap();
                    },
                    "stale_checkpoint",
                ),
                (
                    "a covering commit the landing branch no longer reaches",
                    |prepared, _| {
                        git(
                            &prepared.fixture.repo,
                            &["reset", "--hard", &prepared.base_sha],
                        );
                    },
                    "covering_unreachable",
                ),
                (
                    "a task whose criteria changed since recovery",
                    |_, host| host.set_acceptance_criteria("T-REBASE", &["a different outcome"]),
                    "task_scope_changed",
                ),
            ];
            for (case, change, reason) in cases {
                let prepared = PreparedRebase::new("jrun-neighbour", "README.md", "README.md");
                let host = resolving_host(&prepared, "printf 'target\\n' > README.md");
                let conflict = stopped_conflict(&prepared);
                let outcome = recover(
                    &host,
                    &prepared.run_id,
                    recovery_input_for(&prepared, &prepared.prepared, &conflict),
                )
                .expect("the absorbed continuation is certified");
                assert!(outcome.success, "{case}: {:?}", outcome.message);
                // The retry's settlement claim, as git_rebase made it before
                // the change.
                let absorbed = prepared
                    .rebase_on(&host, &prepared.prepared)
                    .expect_err("an absorbed candidate fails its step")
                    .to_string();
                assert!(
                    absorbed.contains("[candidate_absorbed]"),
                    "{case}: {absorbed}"
                );
                stub_pipeline_steps(&host, &prepared);
                host.stub("sync_base_absorbed", Err(absorbed));
                host.stub(
                    "decide",
                    Ok(json!({
                        "decision": "escalate",
                        "diagnosis": "the absorbed candidate was refused",
                        "human_action": "inspect the checkout",
                    })),
                );

                change(&prepared, &host);
                let run = execute(&host, &prepared, &pipeline(absorbed_step()));

                let decided = host.stub_calls_of("decide");
                let [input] = decided.as_slice() else {
                    panic!("{case}: the final recovery agent runs once, got {decided:#?}");
                };
                assert_eq!(
                    input["absorbed_refusal"]["reason"], reason,
                    "{case}: {input:#}"
                );
                assert!(
                    host.final_recovery_applications.lock().unwrap().iter().all(
                        |application| !matches!(
                            application.decision,
                            FinalRecoveryDecision::CompleteNoDiff { .. }
                        )
                    ),
                    "{case}: nothing settles as no-diff"
                );
                assert_eq!(
                    run.final_recovery(),
                    vec![("escalated".to_string(), Some("escalate".to_string()))],
                    "{case}"
                );
                assert_eq!(host.stub_calls_of("handoff").len(), 1, "{case}");
            }
        },
    );
}

/// The newest checkpoint the host certified.
fn certified(host: &LifecycleHost) -> Value {
    host.checkpoints()
        .last()
        .expect("a certified recovery")
        .2
        .clone()
}

/// `prepared`'s host, with a conflict-recovery provider that runs `body`.
fn resolving_host(prepared: &PreparedRebase, body: &str) -> LifecycleHost {
    let provider = prepared.fixture.root.path().join("codex");
    write_executable(&provider, &provider_script(body));
    prepared.host.with_provider(&provider)
}

/// The steps around `sync_base` answer as the shipped ones did for
/// `prepared`; the rest record that they ran.
fn stub_pipeline_steps(host: &LifecycleHost, prepared: &PreparedRebase) {
    host.stub(
        "worktree",
        Ok(json!({
            "workspace_path": prepared.checkout.path,
            "job_run_id": prepared.run_id,
            "base_ref": prepared.prepared["base_ref"],
            "base_sha": prepared.base_sha,
        })),
    );
    host.stub("prepare_branch", Ok(prepared.prepared.clone()));
    for action in ["deliver", "handoff", "decide"] {
        host.stub(action, Ok(json!({ "action": action })));
    }
}

/// `worktree → prepare_branch → sync_base → deliver`, with `sync_base`
/// recovered by `pr_conflict_recovery`, `decide` as final recovery and
/// `handoff` as the failure activity.
fn pipeline(sync_base: Value) -> JobV2 {
    let stub = |id: &str| json!({ "id": id, "spec": { "type": "deterministic", "action": id, "config": {} } });
    let asset = json!({
        "schemaVersion": 2,
        "kind": "Job",
        "metadata": { "name": "absorbed_candidate_fixture" },
        "spec": {
            "state": "enabled",
            "kind": "workflow",
            "steps": [stub("worktree"), stub("prepare_branch"), sync_base, stub("deliver")],
        },
    });
    let mut job = orbit_engine::activity_job::load_job_asset(&asset.to_string())
        .unwrap()
        .spec;
    job.steps[2].recovery_activity = Some("pr_conflict_recovery".to_string());
    job.steps[2].resolved_recovery_activity = Some(conflict_recovery_activity());
    job.failure_activity = Some("handoff".to_string());
    job.resolved_failure_activity = Some(deterministic_activity("handoff"));
    job.final_recovery_activity = Some("decide".to_string());
    job.resolved_final_recovery_activity = Some(deterministic_activity("decide"));
    job
}

/// The shipped `git_rebase` action, wired as the shipped pipelines wire it.
fn git_rebase_step() -> Value {
    let prepared = |field: &str| format!("{{{{ steps.prepare_branch.output.{field} }}}}");
    json!({
        "id": "sync_base",
        "spec": { "type": "deterministic", "action": "git_rebase", "config": {} },
        "default_input": {
            "job_run_id": "{{ steps.worktree.output.job_run_id }}",
            "completed_task_ids": "{{ input.task_ids }}",
            "workspace_path": "{{ steps.worktree.output.workspace_path }}",
            "base_sync": "local",
            "head": prepared("head"),
            "head_sha": prepared("head_sha"),
            "base": prepared("base"),
            "base_ref": prepared("base_ref"),
            "base_sha": prepared("base_sha"),
            "remote_sha": prepared("remote_sha"),
            "commits_behind": prepared("commits_behind"),
            "sync_required": prepared("sync_required"),
        },
    })
}

/// A `sync_base` that fails as `git_rebase` did once it verified the
/// absorption, so final recovery re-observes whatever changed since.
fn absorbed_step() -> Value {
    json!({
        "id": "sync_base",
        "spec": { "type": "deterministic", "action": "sync_base_absorbed", "config": {} },
    })
}

fn conflict_recovery_activity() -> ActivityV2 {
    let asset = json!({
        "schemaVersion": 2,
        "kind": "Activity",
        "metadata": { "name": "pr_conflict_recovery" },
        "spec": {
            "type": "agent_loop",
            "description": "Resolve the stopped rebase.",
            "instruction": "Resolve the stopped rebase.",
            "provider": "codex",
            "max_iterations": 1,
            "wall_clock_timeout_seconds": 30,
            "require_completion_envelope": true,
            "on_denial": "terminate",
        },
    });
    orbit_engine::activity_job::load_activity_asset(&asset.to_string())
        .expect("conflict recovery fixture loads")
        .spec
}

fn deterministic_activity(name: &str) -> ActivityV2 {
    let asset = json!({
        "schemaVersion": 2,
        "kind": "Activity",
        "metadata": { "name": name },
        "spec": { "type": "deterministic", "description": name, "action": name, "config": {} },
    });
    orbit_engine::activity_job::load_activity_asset(&asset.to_string())
        .expect("fixture activity loads")
        .spec
}

/// Run `job` as `prepared`'s run, keeping its audit events.
fn execute(host: &LifecycleHost, prepared: &PreparedRebase, job: &JobV2) -> AbsorbedRun {
    let audit_root = TempDir::new().unwrap();
    let store = Arc::new(orbit_store::Store::open_in_memory().expect("open sqlite sink"));
    let envelope = Arc::new(V2SqliteSink::for_audit_root(
        store,
        "ws_absorbed",
        &prepared.run_id,
        "test-agent",
        None,
        audit_root.path(),
    ));
    let writer = Arc::new(
        V2AuditWriter::new(
            &prepared.run_id,
            "test-agent",
            Arc::new(InMemorySink::new(audit_root.path().join("blobs"))),
        )
        .with_envelope_sink(envelope),
    );
    let result = execute_job_with_resume(
        job,
        json!({ "task_ids": ["T-REBASE"] }),
        &prepared.run_id,
        writer.clone(),
        host,
        None,
    );
    AbsorbedRun {
        result,
        events: writer
            .events_snapshot()
            .expect("audit events")
            .into_iter()
            .map(|event| event.kind)
            .collect(),
    }
}

impl LifecycleHost {
    fn stub(&self, action: &str, reply: Result<Value, String>) {
        self.stubs.lock().unwrap().insert(action.to_string(), reply);
    }

    fn stub_calls_of(&self, action: &str) -> Vec<Value> {
        self.stub_calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(name, _)| name == action)
            .map(|(_, input)| input.clone())
            .collect()
    }
}
