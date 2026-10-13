//! [ORB-14668] A candidate that conflict recovery finds already on its pinned
//! base, run through `execute_job_with_resume`.
//!
//! `sync_base` is the shipped `git_rebase` action, recovered by a substitute
//! `pr_conflict_recovery` provider that resolves the conflict to the base's
//! side. When the base already holds the candidate's content, the continued
//! rebase drops every pick and the candidate settles as absorbed. When the
//! base holds different content for the same path, the resolution discarded
//! the candidate and the continuation is refused. The job's `decide` final
//! recovery and `handoff` failure activity are host stubs that record
//! whether they ran. The neighbours keep the certified absorbed checkpoint
//! but change what final recovery re-observes, and each must reach the agent
//! with its typed refusal instead of settling.

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
fn an_identical_landed_patch_settles_before_candidate_validation() {
    isolated_in(
        module_path!(),
        "an_identical_landed_patch_settles_before_candidate_validation",
        || {
            let prepared = PreparedRebase::with_commits(
                "jrun-empty-rebase",
                &[("README.md", "target\n")],
                &[
                    ("base-only.md", "base advance\n"),
                    ("README.md", "target\n"),
                ],
            );
            let host = &prepared.host;
            stub_pipeline_steps(host, &prepared);
            assert_ne!(prepared.candidate, prepared.target);
            assert_eq!(prepared.prepared["commits_ahead"], 1);

            let run = execute(host, &prepared, &pipeline(git_rebase_step()));

            assert!(run.result.is_err(), "absorption settles the failed step");
            assert_eq!(prepared.head(), prepared.target);
            assert_eq!(git(&prepared.checkout.path, &["status", "--porcelain"]), "");
            assert_eq!(run.recovery_attempts(), Vec::<bool>::new());
            assert!(!run.events.iter().any(|event| matches!(
                event,
                V2AuditEventKind::StepStarted { step_id } if step_id == "candidate_validate"
            )));
            assert_eq!(host.stub_calls_of("deliver"), Vec::<Value>::new());
            assert_eq!(host.stub_calls_of("decide"), Vec::<Value>::new());
            assert_eq!(host.stub_calls_of("handoff"), Vec::<Value>::new());
            assert_eq!(
                run.final_recovery(),
                vec![("settled".to_string(), Some("complete_no_diff".to_string()))]
            );
            let checkpoint = certified(host);
            assert_eq!(checkpoint["head_sha_before"], prepared.candidate);
            assert_eq!(checkpoint["absorbed"]["covering_commit"], prepared.target);
            let applications = host.final_recovery_applications.lock().unwrap();
            let [application] = applications.as_slice() else {
                panic!("expected one no-diff settlement, got {applications:#?}");
            };
            assert!(matches!(
                &application.decision,
                FinalRecoveryDecision::CompleteNoDiff { evidence_commit, .. }
                    if evidence_commit == &prepared.target
            ));
            // A retry reuses and verifies the certificate, rather than
            // reclassifying the empty branch or proceeding to validation.
            let mut input = prepared.common.clone();
            for (key, value) in prepared.prepared.as_object().unwrap() {
                input[key] = value.clone();
            }
            input["step_id"] = json!("sync_base");
            let retry = action(host, "git_rebase", &input).unwrap_err().to_string();
            assert!(retry.contains("[candidate_absorbed]"), "{retry}");
            assert_eq!(host.checkpoints().len(), 1);
        },
    );
}

#[test]
fn a_rebase_retaining_candidate_commits_still_runs_validation() {
    isolated_in(
        module_path!(),
        "a_rebase_retaining_candidate_commits_still_runs_validation",
        || {
            use orbit_types::workflow::ValidationGateSource::{
                LocalConfig, OwnerContract, ReplicaConfig,
            };
            // Four distinct cheap commands stand in for the captured full
            // gates. They record their exact head after the real conflict
            // continuation, without generating load or measuring production.
            for (case, retained, owned, source, claimed, empty) in [
                ("retained-owner", true, true, LocalConfig, false, false),
                ("retained-claim", true, true, OwnerContract, true, false),
                ("absorbed-owner", false, true, LocalConfig, false, false),
                ("absorbed-claim", false, true, OwnerContract, true, false),
                ("failing-absorbed", false, true, LocalConfig, false, false),
                ("no-diff-expected", false, true, LocalConfig, false, false),
                ("standalone", false, false, LocalConfig, false, false),
                ("uncaptured-claim", false, true, ReplicaConfig, true, false),
                ("empty-capture", false, true, LocalConfig, false, true),
            ] {
                let mut commits = vec![("README.md", "draft\n"), ("README.md", "target\n")];
                if retained {
                    commits.push(("candidate-only.md", "new change\n"));
                }
                let prepared = PreparedRebase::with_commits(
                    &format!("jrun-{case}"),
                    &commits,
                    &[("README.md", "target\n")],
                );
                let mut host = resolving_host(&prepared, "printf 'target\\n' > README.md");
                host.gate_source = Some(source);
                if claimed {
                    host.worker = Some(orbit_types::tool::WorkerInvocation {
                        owner_machine_id: "owner".into(),
                        owner_workspace_id: "workspace".into(),
                        owner_destination: "owner".into(),
                        task_id: "T-REBASE".into(),
                        claim_id: "claim-recovery".into(),
                        execution: orbit_types::task::ExecutionLocation {
                            machine_id: "follower".into(),
                            machine_name: None,
                        },
                        bound_run_id: prepared.run_id.clone(),
                    });
                }
                stub_pipeline_steps(&host, &prepared);
                if case == "no-diff-expected" {
                    host.tasks
                        .lock()
                        .unwrap()
                        .get_mut("T-REBASE")
                        .unwrap()
                        .tags
                        .push("no-diff-expected".into());
                }
                let runs = prepared.fixture.root.path().join("required-command-runs");
                let mut commands = (0..4).map(|index| format!(
                    "test ! -d \"$(git rev-parse --git-path rebase-merge)\" && printf 'check{index} %s\\n' \"$(git rev-parse HEAD)\" >> '{}'",
                    runs.display(),
                )).collect::<Vec<_>>();
                if case == "failing-absorbed" {
                    commands[3].push_str("; orbit_required_gate_fixture_missing_command");
                }
                if !empty {
                    *host.required_commands.lock().unwrap() = commands.clone();
                }
                let mut sync = git_rebase_step();
                if owned {
                    sync["default_input"]["final_validation"] = json!(true);
                }
                let run = execute(&host, &prepared, &pipeline(sync));
                let final_head = prepared.head();
                let should_validate = retained
                    || (owned && source != ReplicaConfig && !empty && case != "no-diff-expected");
                if should_validate {
                    assert!(
                        runs.exists(),
                        "{case}: commands were not reached: {:?}; events: {:?}",
                        run.result,
                        run.events
                    );
                    assert_eq!(
                        fs::read_to_string(&runs).unwrap(),
                        (0..4)
                            .map(|index| format!("check{index} {final_head}\n"))
                            .collect::<String>(),
                        "{case}: every captured command runs once on the final head",
                    );
                    let logs = host.get_task_artifacts("T-REBASE").unwrap();
                    assert_eq!(logs.len(), 4, "{case}: every command retains evidence");
                    for (index, artifact) in logs.iter().enumerate() {
                        let log: Value = serde_json::from_slice(&artifact.content).unwrap();
                        assert_eq!(log["command"], commands[index], "{case}");
                        assert_eq!(log["tested_head"], final_head, "{case}");
                        if case == "failing-absorbed" && index == 3 {
                            assert_ne!(
                                log["exit_code"], 0,
                                "a failed required command is retained"
                            );
                        } else {
                            assert_eq!(log["exit_code"], 0, "{case}");
                        }
                    }
                } else {
                    assert!(
                        !runs.exists(),
                        "{case}: no captured host handoff; recovery keeps its checks"
                    );
                }
                if retained {
                    assert!(run.result.as_ref().unwrap().success, "{case}");
                    assert_ne!(final_head, prepared.target);
                    assert_eq!(host.stub_calls_of("deliver").len(), 1, "{case}");
                    assert!(run.final_recovery().is_empty(), "{case}");
                } else if case == "failing-absorbed" {
                    assert!(run.result.is_err());
                    assert_eq!(run.final_recovery(), vec![("skipped".into(), None)]);
                    assert!(
                        host.final_recovery_applications.lock().unwrap().iter().all(
                            |application| {
                                !matches!(
                                    application.decision,
                                    FinalRecoveryDecision::CompleteNoDiff { .. }
                                )
                            }
                        ),
                        "required-command failure cannot settle the absorbed candidate"
                    );
                } else {
                    assert!(
                        run.result.is_err(),
                        "{case}: absorption settles the failed step"
                    );
                    assert_eq!(final_head, prepared.target, "{case}");
                    assert_eq!(
                        run.final_recovery(),
                        vec![("settled".into(), Some("complete_no_diff".into()))],
                        "{case}"
                    );
                    assert!(host.stub_calls_of("deliver").is_empty(), "{case}");
                }
                assert_eq!(run.recovery_attempts(), vec![true], "{case}");
                assert_eq!(
                    git(&prepared.checkout.path, &["status", "--porcelain"]),
                    "",
                    "{case}"
                );
            }
        },
    );
}

#[test]
fn an_absorbed_candidate_settles_as_verified_no_diff_without_an_agent() {
    isolated_in(
        module_path!(),
        "an_absorbed_candidate_settles_as_verified_no_diff_without_an_agent",
        || {
            let prepared = absorbed_rebase("jrun-absorbed");
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
            let legacy = legacy_absorbed_message(&prepared, &prepared.target);
            assert_eq!(
                message, &legacy,
                "the persisted post-recovery diagnostic stays byte-identical"
            );
            let error = run.result.as_ref().unwrap_err();
            let DispatchError::CandidateAbsorbed { candidate, .. } = error else {
                panic!("post-recovery wrapping must retain absorption: {error:?}");
            };
            assert_eq!(candidate.head, prepared.checkout.branch);
            assert_eq!(candidate.base_sha, prepared.target);
            assert_eq!(candidate.covering_commit, prepared.target);
            assert_eq!(
                candidate.base_ref,
                prepared.prepared["base_ref"].as_str().unwrap()
            );
            assert_eq!(error.failure_code(), "pipeline_step_failed");
            let initial = format!(
                "recoverable VCS conflict during 'git_rebase': original base '{}', target base '{}'; rebase of '{}' onto checkpoint '{}' stopped with conflicts; conflicting paths: README.md",
                prepared.base_sha, prepared.target, prepared.checkout.branch, prepared.target
            );
            let expected = format!(
                "job executor: post-recovery attempt absorbed: {legacy}; original error before recovery: {initial}"
            );
            assert_eq!(error.to_string(), expected);
            let persisted = run
                .events
                .iter()
                .find_map(|event| match event {
                    V2AuditEventKind::StepFinished {
                        step_id,
                        error_message,
                        ..
                    } if step_id == "sync_base" => error_message.as_deref(),
                    _ => None,
                })
                .expect("persisted step diagnostic");
            assert_eq!(persisted, expected);

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

/// The base changed the candidate's lines to different content, and the
/// resolution took the base's side: the conflicting commit touched the
/// candidate's path, but the base does not hold the candidate's change. The
/// continuation is refused with its typed reason and the final-recovery agent
/// decides, rather than the task settling as delivered by that commit.
#[test]
fn a_discarded_candidate_reaches_the_final_recovery_agent() {
    isolated_in(
        module_path!(),
        "a_discarded_candidate_reaches_the_final_recovery_agent",
        || {
            let prepared = PreparedRebase::new("jrun-discarded", "README.md", "README.md");
            let host = resolving_host(&prepared, "printf 'target\\n' > README.md");
            stub_pipeline_steps(&host, &prepared);
            host.stub(
                "decide",
                Ok(json!({
                    "decision": "escalate",
                    "diagnosis": "the resolution discarded the candidate",
                    "human_action": "restore the candidate's change",
                })),
            );

            let run = execute(&host, &prepared, &pipeline(git_rebase_step()));

            assert!(
                run.result.is_err(),
                "a discarded candidate does not complete"
            );
            assert!(host.checkpoints().is_empty(), "nothing is certified");
            assert_eq!(run.recovery_attempts(), vec![false]);

            let decided = host.stub_calls_of("decide");
            let [input] = decided.as_slice() else {
                panic!("the final recovery agent runs once, got {decided:#?}");
            };
            let attempts = input["step_recovery_attempts"]
                .as_array()
                .expect("step recovery evidence");
            assert!(
                attempts.iter().any(|attempt| attempt["phase"] == "recovery"
                    && attempt["outcome"] == "failed"
                    && attempt["error_message"]
                        .as_str()
                        .is_some_and(|message| message
                            .contains("absorbed_candidate_refused: no_covering_commit"))),
                "the agent reads the typed refusal: {input:#}"
            );
            assert!(
                host.final_recovery_applications.lock().unwrap().iter().all(
                    |application| !matches!(
                        application.decision,
                        FinalRecoveryDecision::CompleteNoDiff { .. }
                    )
                ),
                "nothing settles as no-diff"
            );
            assert_eq!(
                run.final_recovery(),
                vec![("escalated".to_string(), Some("escalate".to_string()))]
            );
            assert_eq!(host.stub_calls_of("handoff").len(), 1);
        },
    );
}

/// The resolution keeps the candidate's commit on the pin, but the base
/// advanced with that same content meanwhile: following the tip drops the
/// pick, and the candidate is certified absorbed by the advanced commit.
#[test]
fn a_candidate_the_advanced_base_absorbed_is_certified_on_that_tip() {
    isolated_in(
        module_path!(),
        "a_candidate_the_advanced_base_absorbed_is_certified_on_that_tip",
        || {
            let prepared = PreparedRebase::new("jrun-advanced", "README.md", "README.md");
            let host = resolving_host(&prepared, "printf 'candidate\\n' > README.md");
            let conflict = stopped_conflict(&prepared);
            let advanced = commit_file(&prepared.fixture.repo, "README.md", "candidate\n");

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
            assert_eq!(checkpoint["absorbed"]["covering_commit"], advanced);
            let retried = prepared
                .rebase_on(&host, &prepared.prepared)
                .expect_err("an absorbed candidate fails its step");
            let OrbitError::CandidateAbsorbed(candidate) = &retried else {
                panic!("expected typed absorption, got {retried:?}");
            };
            assert_eq!(candidate.base_sha, advanced);
            assert_eq!(candidate.covering_commit, advanced);
            assert_eq!(
                retried.to_string(),
                legacy_absorbed_message(&prepared, &advanced)
                    .strip_prefix("deterministic action `git_rebase` failed: ")
                    .unwrap()
            );
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
                let prepared = absorbed_rebase("jrun-neighbour");
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
                let error = prepared
                    .rebase_on(&host, &prepared.prepared)
                    .expect_err("an absorbed candidate fails its step");
                let message = format!("deterministic action `sync_base_absorbed` failed: {error}");
                let OrbitError::CandidateAbsorbed(candidate) = error else {
                    panic!("{case}: expected typed absorption, got {error:?}");
                };
                stub_pipeline_steps(&host, &prepared);
                host.stubs.lock().unwrap().insert(
                    "sync_base_absorbed".into(),
                    Err(DispatchError::CandidateAbsorbed { candidate, message }),
                );
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

/// A candidate the base genuinely absorbed: its first commit conflicts with
/// the base's, and its second ends on the base's content. Resolving the
/// conflict to the base's side leaves the candidate's final content in place.
fn absorbed_rebase(run_id: &str) -> PreparedRebase {
    PreparedRebase::with_commits(
        run_id,
        &[("README.md", "draft\n"), ("README.md", "target\n")],
        &[("README.md", "target\n")],
    )
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

/// `worktree → prepare_branch → sync_base → candidate_validate → deliver`, with `sync_base`
/// recovered by `pr_conflict_recovery`, `decide` as final recovery and
/// `handoff` as the failure activity.
fn pipeline(sync_base: Value) -> JobV2 {
    let stub = |id: &str| json!({ "id": id, "spec": { "type": "deterministic", "action": id, "config": {} } });
    let validate = json!({
        "id": "candidate_validate",
        "spec": { "type": "deterministic", "action": "candidate_validate", "config": {} },
        "default_input": {
            "job_run_id": "{{ steps.worktree.output.job_run_id }}",
            "completed_task_ids": "{{ input.task_ids }}",
            "workspace_path": "{{ steps.worktree.output.workspace_path }}",
            "base_sha": "{{ steps.sync_base.output.base_sha }}",
        },
    });
    let asset = json!({
        "schemaVersion": 2,
        "kind": "Job",
        "metadata": { "name": "absorbed_candidate_fixture" },
        "spec": {
            "state": "enabled",
            "kind": "workflow",
            "steps": [stub("worktree"), stub("prepare_branch"), sync_base, validate, stub("deliver")],
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
            "step_id": "sync_base",
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
        self.stubs.lock().unwrap().insert(
            action.to_string(),
            reply.map_err(|message| DispatchError::action_failed(action, message)),
        );
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

/// The pre-typing diagnostic is a persisted compatibility contract.
fn legacy_absorbed_message(prepared: &PreparedRebase, base_sha: &str) -> String {
    format!(
        "deterministic action `git_rebase` failed: execution failed: [candidate_absorbed] git_rebase: conflict recovery left '{}' on base '{}' with no candidate commit; covering commit {} on '{}' already carries the candidate's change, so verified no-diff settlement completes the task",
        prepared.checkout.branch,
        base_sha,
        base_sha,
        prepared.prepared["base_ref"].as_str().unwrap()
    )
}

#[test]
fn a_quoted_absorption_marker_never_settles_a_certified_candidate() {
    isolated_in(
        module_path!(),
        "a_quoted_absorption_marker_never_settles_a_certified_candidate",
        || {
            let prepared = absorbed_rebase("jrun-quoted-absorbed");
            let host = resolving_host(&prepared, "printf 'target\\n' > README.md");
            let conflict = stopped_conflict(&prepared);
            recover(
                &host,
                &prepared.run_id,
                recovery_input_for(&prepared, &prepared.prepared, &conflict),
            )
            .unwrap();
            assert!(!host.checkpoints().is_empty());
            stub_pipeline_steps(&host, &prepared);
            host.stub(
                "sync_base_absorbed",
                Err("tool quoted [candidate_absorbed]".into()),
            );
            host.stub("decide", Ok(json!({"decision":"escalate", "diagnosis":"ordinary tool failure", "human_action":"inspect tool"})));
            let mut job = pipeline(absorbed_step());
            job.steps[2].recovery_activity = None;
            job.steps[2].resolved_recovery_activity = None;
            let run = execute(&host, &prepared, &job);
            assert!(run.result.is_err());
            let calls = host.stub_calls_of("decide");
            let [input] = calls.as_slice() else {
                panic!("the final recovery agent must run: {calls:?}");
            };
            assert!(
                input.get("absorbed_refusal").is_none(),
                "quoted text must not trigger absorption verification"
            );
            assert_eq!(
                run.final_recovery(),
                vec![("escalated".into(), Some("escalate".into()))]
            );
            assert_eq!(host.stub_calls_of("handoff").len(), 1);
        },
    );
}
