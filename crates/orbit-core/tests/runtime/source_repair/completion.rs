//! An unchanged published candidate continues its native completion prefix.
use super::*;
use orbit_engine::activity_job::{load_activity_asset, load_job_asset};
use orbit_engine::{FinalRecoveryAdmissionRequest, V2AuditWriter, execute_job_with_resume};
use std::sync::Arc;

#[path = "completion/mutation.rs"]
pub(super) mod mutation;

fn fixture() -> (Fixture, PathBuf) {
    fixture_with_auto_fix(true)
}

fn fixture_with_auto_fix(auto_fix: bool) -> (Fixture, PathBuf) {
    let mut f = Fixture::new();
    let config = f.repo.join(".orbit/config.toml");
    let text = std::fs::read_to_string(&config).unwrap().replace(
        "[workflow]",
        "[workflow]\nfinal_recovery_crews = [\"fixture\"]",
    );
    std::fs::write(&config, text).unwrap();
    let global = f._root.path().join("global");
    let path = global.join("resources/jobs/task_pr_pipeline.yaml");
    let mut job: Value = serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let steps = job["spec"]["steps"].as_array_mut().unwrap();
    steps.insert(
        4,
        json!({"id":"validate","target":"activity:candidate_validate"}),
    );
    steps.insert(
        5,
        json!({"id":"review_gate_settle","target":"activity:review_gate_settle"}),
    );
    steps.insert(9, json!({"id":"promote","target":"activity:pr_promote"}));
    let complete = steps.last_mut().unwrap();
    complete["default_input"] = json!({
        "job_run_id":"{{ steps.worktree.output.job_run_id }}", "completed_task_ids":[f.task],
        "workspace_path":"{{ steps.worktree.output.workspace_path }}", "pr_number":"17",
        "completion":"done", "auto_fix":auto_fix, "head":"{{ steps.push.output.branch }}",
        "published_head_sha":"{{ steps.push.output.local_sha }}", "base":"main",
        "max_wait_seconds":60, "poll_interval_seconds":5
    });
    job["spec"]["final_recovery_activity"] = json!("completion_refused");
    std::fs::write(&path, serde_yaml::to_string(&job).unwrap()).unwrap();
    std::fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/activities/pr_promote.yaml"),
        global.join("resources/activities/pr_promote.yaml"),
    )
    .unwrap();
    std::fs::write(global.join("resources/activities/completion_refused.yaml"),
        "schemaVersion: 2\nkind: Activity\nmetadata:\n  name: completion_refused\nspec:\n  type: deterministic\n  description: A refused fast path must stop this fixture\n  action: candidate_validate\n  config: {}\n").unwrap();
    f.runtime = orbit_core::OrbitRuntime::from_roots(&global, &f.repo.join(".orbit"))
        .unwrap()
        .with_automation_execution_location(Some(orbit_types::task::ExecutionLocation {
            machine_id: "hm_fixture".into(),
            machine_name: None,
        }));
    use std::os::unix::fs::PermissionsExt;
    let hook = f._root.path().join("origin.git/hooks/post-receive");
    std::fs::write(
        &hook,
        format!(
            "#!/bin/sh\nprintf 'push\\n' >> '{}'\n",
            f._root.path().join("pushes").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let checkout = published_owner::validated_publication(&mut f, auto_fix);
    (f, checkout)
}

fn admit(f: &Fixture) {
    assert!(matches!(
        f.runtime
            .admit_final_recovery(
                &f.source.run_id,
                &FinalRecoveryAdmissionRequest {
                    task_id: f.task.clone(),
                    failed_step_id: "complete_pr".into(),
                    base_ref: Some("origin/main".into())
                }
            )
            .unwrap(),
        orbit_engine::FinalRecoveryAdmission::Admitted
    ));
}

fn tail_forge(f: &Fixture, mode: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let calls = f._root.path().join("tail-calls");
    let once = f._root.path().join("lost-read");
    let merged = f._root.path().join("merged");
    let gh = f._root.path().join("bin/gh");
    std::fs::write(&gh, format!(r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
case "$1 $2" in
 'pr view')
    if [ ! -f '{}' ]; then touch '{}'; echo 'TLS handshake timeout' >&2; exit 1; fi
    if [ -f '{}' ]; then cat '{}'; else cat '{}'; fi;;
 'repo view') echo '{{"nameWithOwner":"owner/repository"}}';;
 'api graphql') echo '{{"data":{{"repository":{{"autoMergeAllowed":false,"mergeCommitAllowed":true,"rebaseMergeAllowed":false,"squashMergeAllowed":false,"pullRequest":{{"baseRefName":"main","baseRef":{{"branchProtectionRule":null}}}}}}}}}}';;
 'api repos/'*) touch '{}'; printf '{{"merged":true,"sha":"{}"}}';;
 *) exit 99;;
esac
"#, calls.display(), once.display(), once.display(), merged.display(), merged.with_extension("json").display(), f.answer.display(), merged.display(), f.head)).unwrap();
    if mode == "lost_merge_reply" {
        std::fs::write(&once, "read succeeds").unwrap();
        let script = std::fs::read_to_string(&gh).unwrap();
        let script = script.replace(
            "printf '{\"merged\":true,",
            "echo 'TLS handshake timeout' >&2; exit 1; printf '{\"merged\":true,",
        );
        std::fs::write(&gh, script).unwrap();
    }
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut answer: Value = serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
    answer["mergeStateStatus"] = json!("CLEAN");
    answer["isDraft"] = json!(false);
    answer["statusCheckRollup"] =
        json!([{"name":"native","status":"COMPLETED","conclusion":"SUCCESS"}]);
    std::fs::write(&f.answer, answer.to_string()).unwrap();
    answer["state"] = json!("MERGED");
    answer["mergeCommit"] = json!({"oid":f.head});
    answer["mergedAt"] = json!("2026-10-15T12:00:00Z");
    std::fs::write(merged.with_extension("json"), answer.to_string()).unwrap();
    calls
}

fn run_tail(f: &Fixture) -> Result<orbit_engine::JobOutcome, orbit_engine::DispatchError> {
    let path = f
        ._root
        .path()
        .join("global/resources/jobs/task_pr_pipeline.yaml");
    let mut job = load_job_asset(&std::fs::read_to_string(path).unwrap())
        .unwrap()
        .spec;
    orbit_engine::resolve_job_catalog_refs_for_execution(
        &mut job,
        &f.runtime.v2_activity_catalog().unwrap(),
    )
    .unwrap();
    // The recovery activity must never run: the host authenticates the prefix.
    job.resolved_final_recovery_activity = Some(
        load_activity_asset(
            &std::fs::read_to_string(
                f._root
                    .path()
                    .join("global/resources/activities/completion_refused.yaml"),
            )
            .unwrap(),
        )
        .unwrap()
        .spec,
    );
    let state = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
    let audit = V2AuditWriter::with_disk_sinks(
        &f.repo.join(".orbit/tmp/tail-audit"),
        Arc::new(orbit_store::Store::open_in_memory().unwrap()),
        "ws_fixture",
        &f.source.run_id,
        "fixture",
        Some(&f.repo),
    )
    .unwrap();
    let outcome = orbit_engine::test_support::with_vcs_retry_wait(
        |_| {},
        || {
            execute_job_with_resume(
                &job,
                f.source.input.clone().unwrap(),
                &f.source.run_id,
                audit.clone(),
                &f.runtime,
                Some(&state),
            )
        },
    );
    if matches!(&outcome, Ok(outcome) if outcome.success) {
        let started = audit
            .events_snapshot()
            .unwrap()
            .into_iter()
            .filter_map(|event| match event.kind {
                orbit_types::workflow::V2AuditEventKind::StepStarted { step_id } => Some(step_id),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            started,
            ["complete_pr", "complete_pr"],
            "only completion runs twice; implementation, commit, native validation and push never restart, including no-op attempts"
        );
    }
    outcome
}

pub(super) fn unchanged_publication_retries_only_completion() {
    for auto_fix in [false, true] {
        unchanged_publication_tail(auto_fix, false);
        if auto_fix {
            unchanged_publication_tail(auto_fix, true);
        }
    }
}

fn unchanged_publication_tail(auto_fix: bool, review: bool) {
    let (mut f, checkout) = fixture_with_auto_fix(auto_fix);
    if review {
        configure_review(&mut f, &checkout);
    }
    let calls = tail_forge(&f, "lost_read");
    let state = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
    let prefix = state.step_outputs.clone();
    let artifacts = f.runtime.get_task_artifact_manifest(&f.task).unwrap();
    let outcome = run_tail(&f).unwrap();
    assert!(outcome.success, "{outcome:?}");
    assert_eq!(git(&checkout, &["rev-parse", "HEAD"]), f.head);
    assert_eq!(
        git(
            &checkout,
            &["ls-remote", "origin", &format!("refs/heads/{}", f.branch)]
        )
        .split_whitespace()
        .next(),
        Some(f.head.as_str())
    );
    assert_eq!(
        f.runtime.get_task(&f.task).unwrap().status,
        TaskStatus::Done
    );
    let after = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
    for (index, output) in prefix {
        assert_eq!(
            after.step_output(index),
            Some(&output),
            "immutable successful prefix"
        );
    }
    for artifact in artifacts.iter().filter(|a| {
        a.path.starts_with("validation/") || a.path == orbit_types::workflow::REVIEW_GATE_ARTIFACT
    }) {
        assert!(
            f.runtime
                .get_task_artifact_manifest(&f.task)
                .unwrap()
                .iter()
                .any(|a| a.path == artifact.path && a.sha256 == artifact.sha256)
        );
    }
    assert_eq!(
        std::fs::read_to_string(f._root.path().join("pushes"))
            .unwrap()
            .lines()
            .count(),
        1,
        "completion must not push or restart CI"
    );
    let recovery = after.final_recovery.unwrap();
    assert!(matches!(
        recovery.decision,
        Some(orbit_types::workflow::FinalRecoveryDecision::Resume { .. })
    ));
    assert!(recovery.repair_commit.is_none());
    assert_eq!(after.completion_deadlines.len(), 1);
    let requests = std::fs::read_to_string(calls).unwrap();
    assert_eq!(
        requests
            .lines()
            .filter(|l| l.starts_with("api repos/"))
            .count(),
        1
    );
    assert_eq!(
        requests
            .lines()
            .filter(|l| l.starts_with("pr view"))
            .count(),
        4,
        "failed lookup, authenticated identity, fresh completion read, merged readback"
    );
    assert!(
        !requests.lines().any(|l| l.starts_with("pr create")),
        "no replacement PR or publication"
    );
}

pub(super) fn completion_reuse_refuses_drift() {
    for case in [
        "meaning",
        "policy",
        "base",
        "head",
        "same_tree",
        "lease",
        "missing_log",
        "failed_log",
        "checkpoint",
        "reviewer",
        "foreign",
        "other_worker",
        "retry_ancestor",
        "prior_recovery",
        "rebound",
        "pr",
        "merge_intent",
    ] {
        let (mut f, checkout) = fixture();
        admit(&f);
        assert!(
            f.runtime
                .authenticate_completion_tail(&f.source.run_id, &f.task, "complete_pr")
                .unwrap(),
            "control"
        );
        let before = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
        match case {
            "meaning" => {
                f.runtime
                    .update_task_with_identity(
                        &f.task,
                        orbit_core::application::task::TaskUpdateParams {
                            description: Some("changed mandate".into()),
                            ..Default::default()
                        },
                        None,
                        Some("codex".into()),
                    )
                    .unwrap();
            }
            "policy" => {
                let path = f.repo.join(".orbit/config.toml");
                let config = std::fs::read_to_string(&path).unwrap().replace(
                    "required_validation_commands = [\"true\"]",
                    "required_validation_commands = [\"false\"]",
                );
                std::fs::write(path, config).unwrap();
                f.runtime = orbit_core::OrbitRuntime::from_roots(
                    &f._root.path().join("global"),
                    &f.repo.join(".orbit"),
                )
                .unwrap()
                .with_automation_execution_location(Some(
                    orbit_types::task::ExecutionLocation {
                        machine_id: "hm_fixture".into(),
                        machine_name: None,
                    },
                ));
            }
            "base" => {
                std::fs::write(f.repo.join("new-base.txt"), "advance").unwrap();
                git(&f.repo, &["add", "new-base.txt"]);
                git(&f.repo, &["commit", "-qm", "new base"]);
                git(&f.repo, &["push", "origin", "HEAD:main"]);
            }
            "head" => {
                std::fs::write(checkout.join("source.txt"), "changed").unwrap();
            }
            "same_tree" => {
                git(
                    &checkout,
                    &["commit", "--amend", "-m", "same tree, different candidate"],
                );
            }
            "lease" => {
                git(
                    &f.repo,
                    &[
                        "push",
                        "origin",
                        &format!("{}:refs/heads/{}", f.base, f.branch),
                        "--force",
                    ],
                );
            }
            "missing_log" | "failed_log" => {
                let reference = &before.pipeline["validate"]["validation"][0];
                let path = reference["path"].as_str().unwrap();
                f.runtime
                    .attach_task_validation_log(
                        &f.task,
                        &f.source.run_id,
                        path,
                        if case == "missing_log" {
                            Vec::new()
                        } else {
                            b"{\"exit_code\":1}".to_vec()
                        },
                    )
                    .unwrap();
            }
            "checkpoint" | "reviewer" => {
                let mut state = before.clone();
                if case == "checkpoint" {
                    state.step_states.insert(4, JobRunState::Failed);
                } else {
                    state.pipeline["review_gate_settle"]["reviewer_fixed"] = json!(true);
                }
                f.runtime.write_run_state(&f.source.run_id, &state).unwrap();
            }
            "prior_recovery" => {
                let mut state = before.clone();
                state
                    .rebase_recovery_checkpoints
                    .insert("complete_pr".into(), json!({"head_sha":f.head}));
                f.runtime.write_run_state(&f.source.run_id, &state).unwrap();
            }
            "retry_ancestor" => {
                f.source.retry_source_run_id = Some("prior-attempt".into());
                published_owner::save_run(&f, &f.source);
            }
            "other_worker" => {
                f.source.pid = Some(u32::MAX);
                published_owner::save_run(&f, &f.source);
            }
            "foreign" => {
                f.runtime
                    .sqlite_store()
                    .unwrap()
                    .with_transaction(|tx| {
                        tx.connection()
                            .execute(
                                "UPDATE job_runs SET executed_on_json = ?1 WHERE run_id = ?2",
                                [
                                    json!({"machine_id":"foreign","machine_name":null}).to_string(),
                                    f.source.run_id.clone(),
                                ],
                            )
                            .unwrap();
                        Ok(())
                    })
                    .unwrap();
            }
            "rebound" => {
                f.runtime
                    .apply_task_automation_update(
                        &f.task,
                        TaskAutomationUpdate {
                            job_run_id: Some("other-run".into()),
                            ..Default::default()
                        },
                    )
                    .unwrap();
            }
            "pr" | "merge_intent" => {
                let mut answer: Value =
                    serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
                if case == "pr" {
                    answer["number"] = json!(18);
                } else {
                    answer["autoMergeRequest"] = json!({"enabledAt":"now"});
                }
                std::fs::write(&f.answer, answer.to_string()).unwrap();
            }
            _ => unreachable!(),
        }
        let expected = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
        assert!(
            f.runtime
                .authenticate_completion_tail(&f.source.run_id, &f.task, "complete_pr")
                .is_err(),
            "{case}"
        );
        assert_eq!(
            f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap(),
            expected,
            "refusal does not rewrite receipts: {case}"
        );
    }
}

pub(super) fn changed_base_gets_fresh_native_validation() {
    let (f, checkout) = fixture();
    admit(&f);
    let old = f.head.clone();
    git(&f.repo, &["checkout", "main"]);
    std::fs::write(f.repo.join("new-base.txt"), "advance").unwrap();
    git(&f.repo, &["add", "new-base.txt"]);
    git(&f.repo, &["commit", "-qm", "new base"]);
    git(&f.repo, &["push", "origin", "HEAD:main"]);
    assert!(
        f.runtime
            .authenticate_completion_tail(&f.source.run_id, &f.task, "complete_pr")
            .is_err()
    );
    git(&checkout, &["fetch", "origin", "main"]);
    git(&checkout, &["rebase", "origin/main"]);
    let head = git(&checkout, &["rev-parse", "HEAD"]);
    assert_ne!(head, old);
    let validated = f.action("candidate_validate", json!({"job_run_id":f.source.run_id,"completed_task_ids":[f.task],"workspace_path":checkout,"base_sha":git(&checkout, &["rev-parse","origin/main"])}));
    assert_eq!(validated["decision"], "passed");
    assert_eq!(validated["tested_head"], head);
    assert!(
        f.runtime
            .get_task_artifact_manifest(&f.task)
            .unwrap()
            .iter()
            .any(|a| a.path.contains("/attempts/")),
        "fresh validation preserves the earlier immutable log"
    );
}

pub(super) fn concurrent_and_lost_admission_keep_one_completion_budget() {
    let (f, _) = fixture();
    let barrier = std::sync::Barrier::new(2);
    let request = FinalRecoveryAdmissionRequest {
        task_id: f.task.clone(),
        failed_step_id: "complete_pr".into(),
        base_ref: Some("origin/main".into()),
    };
    let results = std::thread::scope(|scope| {
        let apply = || {
            barrier.wait();
            f.runtime
                .admit_final_recovery(&f.source.run_id, &request)
                .unwrap()
        };
        let first = scope.spawn(apply);
        let second = scope.spawn(apply);
        [first.join().unwrap(), second.join().unwrap()]
    });
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, orbit_engine::FinalRecoveryAdmission::Admitted))
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, orbit_engine::FinalRecoveryAdmission::Skipped { .. }))
            .count(),
        1
    );
    let deadline = f
        .runtime
        .completion_deadline(&f.source.run_id, "17", 1000)
        .unwrap();
    assert_eq!(
        f.runtime
            .completion_deadline(&f.source.run_id, "17", 2000)
            .unwrap(),
        deadline,
        "lost replies never renew the deadline"
    );
    let state = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
    assert!(
        f.runtime
            .authenticate_completion_tail(&f.source.run_id, &f.task, "complete_pr")
            .unwrap()
    );
    let application = orbit_engine::FinalRecoveryApplication {
        task_id: f.task.clone(),
        failed_step_id: "complete_pr".into(),
        decision: orbit_types::workflow::FinalRecoveryDecision::Resume {
            step_id: "complete_pr".into(),
            rationale: "authenticated observation retry".into(),
        },
        resume_step_index: Some(10),
        repair_commit: None,
        resume_context: None,
        workspace_path: f.repo.clone(),
        completion_done: true,
        infrastructure: None,
    };
    for _ in 0..2 {
        assert!(matches!(
            RuntimeHost::apply_final_recovery(&f.runtime, &f.source.run_id, &application).unwrap(),
            orbit_engine::FinalRecoveryApplied::Resume
        ));
    }
    let after = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
    assert_eq!(after.completion_deadlines, state.completion_deadlines);
    assert_eq!(
        after.step_outputs, state.step_outputs,
        "lost application reply preserves every successful receipt"
    );
    assert!(
        matches!(
            f.runtime
                .admit_final_recovery(&f.source.run_id, &request)
                .unwrap(),
            orbit_engine::FinalRecoveryAdmission::Skipped { .. }
        ),
        "replayed application cannot replenish final recovery"
    );
}

pub(super) fn unknown_merge_reply_is_reconciled_without_republication() {
    let (f, checkout) = fixture();
    let calls = tail_forge(&f, "lost_merge_reply");
    let input = json!({"job_run_id":f.source.run_id,"run_id":f.source.run_id,
        "completed_task_ids":[f.task],"workspace_path":checkout,"pr_number":"17",
        "completion":"done","auto_fix":true,"head":f.branch,"published_head_sha":f.head,
        "base":"main","max_wait_seconds":60});
    let failed = execute_deterministic_action(
        &f.runtime,
        "pr_complete",
        &json!({}),
        &input,
        false,
        &HashMap::new(),
        None,
    )
    .unwrap_err();
    assert!(
        failed.to_string().contains("TLS handshake timeout"),
        "{failed}"
    );
    assert!(
        !failed
            .to_string()
            .contains("completion_observation_unavailable:"),
        "a lost mutation reply is never read-only authority"
    );
    let history = f.runtime.get_task_history(&f.task).unwrap();
    assert!(
        history
            .iter()
            .any(|row| row.event == orbit_types::workflow::PR_COMPLETION_MUTATION_STARTED_EVENT),
        "the unknown reply has durable write-ahead provenance"
    );
    // A later failed lookup cannot become read-only just because local flags reset.
    std::fs::rename(
        f._root.path().join("lost-read"),
        f._root.path().join("lost-read-again"),
    )
    .unwrap();
    let retry = execute_deterministic_action(
        &f.runtime,
        "pr_complete",
        &json!({}),
        &input,
        false,
        &HashMap::new(),
        None,
    )
    .unwrap_err();
    assert!(
        !retry
            .to_string()
            .contains("completion_observation_unavailable:"),
        "{retry}"
    );
    admit(&f);
    assert!(
        f.runtime
            .authenticate_completion_tail(&f.source.run_id, &f.task, "complete_pr")
            .is_err()
    );
    let result = f.action("pr_complete", input);
    assert_eq!(result["phase"], "complete");
    assert_eq!(
        f.runtime.get_task(&f.task).unwrap().status,
        TaskStatus::Done
    );
    assert_eq!(
        std::fs::read_to_string(calls)
            .unwrap()
            .lines()
            .filter(|line| line.starts_with("api repos/"))
            .count(),
        1,
        "re-observe the accepted mutation before asking again"
    );
    assert_eq!(
        std::fs::read_to_string(f._root.path().join("pushes"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert_eq!(git(&checkout, &["rev-parse", "HEAD"]), f.head);
}

pub(super) fn configured_review_requires_unchanged_host_certificate() {
    use orbit_types::workflow::REVIEW_GATE_ARTIFACT;
    let (mut f, checkout) = fixture();
    let source = configure_review(&mut f, &checkout);
    admit(&f);
    assert!(
        f.runtime
            .authenticate_completion_tail(&f.source.run_id, &f.task, "complete_pr")
            .unwrap()
    );
    // A caller's replacement artifact cannot stand in for the host certificate.
    std::fs::write(&source, "{}").unwrap();
    let refused = f
        .runtime
        .run_tool(
            "orbit.task.artifact.put",
            json!({"id":f.task,"model":"codex","path":REVIEW_GATE_ARTIFACT,"source_path":source}),
        )
        .unwrap_err();
    assert!(refused.to_string().contains("reserved"));
    let mut changed = f
        .runtime
        .read_run_state(&f.source.run_id)
        .unwrap()
        .unwrap()
        .pipeline["review_gate_settle"]
        .clone();
    changed["reviewed_head_sha"] = json!(f.base);
    published_owner::checkpoint(&f, &f.source.run_id, 5, "review_gate_settle", changed);
    assert!(
        f.runtime
            .authenticate_completion_tail(&f.source.run_id, &f.task, "complete_pr")
            .is_err()
    );
}

pub(super) fn refused_receipts_stop_before_agent_or_republication() {
    let (f, _) = fixture();
    tail_forge(&f, "lost_read");
    f.runtime
        .attach_task_validation_log(
            &f.task,
            &f.source.run_id,
            &format!("validation/{}/0.json", f.source.run_id),
            b"{}".to_vec(),
        )
        .unwrap();
    assert!(run_tail(&f).is_err());
    let state = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
    assert!(
        matches!(state.final_recovery.unwrap().decision,
        Some(orbit_types::workflow::FinalRecoveryDecision::Escalate { diagnosis, .. })
        if diagnosis.starts_with("unchanged completion reuse refused:")),
        "a stale prefix is refused by the host before the agent can request it again"
    );
    assert_eq!(
        std::fs::read_to_string(f._root.path().join("pushes"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

fn configure_review(f: &mut Fixture, checkout: &std::path::Path) -> PathBuf {
    use orbit_types::workflow::{
        REVIEW_CONTRACT_VERSION, REVIEW_REPORT_ARTIFACT, ReviewAdmission, ReviewTiming,
    };
    let config = f.repo.join(".orbit/config.toml");
    std::fs::write(
        &config,
        std::fs::read_to_string(&config)
            .unwrap()
            .replace("before_pr = false", "before_pr = true"),
    )
    .unwrap();
    f.runtime = orbit_core::OrbitRuntime::from_roots(
        &f._root.path().join("global"),
        &f.repo.join(".orbit"),
    )
    .unwrap()
    .with_automation_execution_location(Some(orbit_types::task::ExecutionLocation {
        machine_id: "hm_fixture".into(),
        machine_name: None,
    }));
    let policy = f.runtime.operation_policy();
    let admission = ReviewAdmission {
        contract_version: REVIEW_CONTRACT_VERSION,
        policy_version: policy.version,
        timing: ReviewTiming::BeforePr,
        timing_source: policy.review_before_pr.source.label().into(),
        crew: Some(policy.review_crew.value[0].clone()),
        crew_pool: Vec::new(),
        crew_source: policy.review_crew.source.label().into(),
        budget: policy.review_budget(),
        required_validation_commands: Some(
            f.runtime.workflow_required_validation_commands().to_vec(),
        ),
        baseline_commands: f.runtime.review_baseline_commands().to_vec(),
        host_evidence: policy.review_host_evidence.value.clone(),
        captured_at: Utc::now(),
    };
    f.source.input.as_mut().unwrap()["review"] = json!(admission);
    published_owner::save_run(f, &f.source);
    let input = json!({"job_run_id":f.source.run_id,"completed_task_ids":[f.task],"workspace_path":checkout,"base":"main","base_sync":"remote","mode":"pr","base_sha":f.state.pipeline["sync_base"]["base_sha"]});
    let validation = f.action("candidate_validate", input.clone());
    published_owner::checkpoint(f, &f.source.run_id, 4, "validate", validation);
    let review = f.action("review_gate_admit", input.clone());
    assert_eq!(review["applies"], true);
    let report = json!({"schema_version":REVIEW_CONTRACT_VERSION,"attempt_id":review["attempt_id"],"verdict":"accept","summary":"Checked unchanged publication","findings":[],"validation":[{"id":"V1","command":"true","outcome":"passed","role":"required"}],"escalation":null});
    let source = f.repo.join(".orbit/tmp/completion-review.json");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(&source, report.to_string()).unwrap();
    f.runtime
        .run_tool(
            "orbit.task.artifact.put",
            json!({"id":f.task,"model":"codex","path":REVIEW_REPORT_ARTIFACT,"source_path":source}),
        )
        .unwrap();
    let mut settle_input = input;
    settle_input["admission"] = review;
    let settled = f.action("review_gate_settle", settle_input);
    assert_eq!(settled["gate"], "passed");
    assert_eq!(settled["reviewer_fixed"], false);
    published_owner::checkpoint(f, &f.source.run_id, 5, "review_gate_settle", settled);
    source
}
