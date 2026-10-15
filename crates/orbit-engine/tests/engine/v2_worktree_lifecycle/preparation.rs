//! Stale preparation recovery through shipped delivery inputs and real Git.
use super::*;
use orbit_engine::execute_job_with_resume;
use orbit_engine::test_support::with_prerequisite_clock;
use orbit_types::workflow::activity_job::{JobV2, V2AuditEventKind};
use std::sync::atomic::{AtomicU64, Ordering};

fn certified_input(prepared: &PreparedRebase) -> Value {
    let mut prepare = prepared.common.clone();
    prepare["authenticate_preparation"] = json!(true);
    let certificate = action(&prepared.host, "pr_prepare", &prepare).unwrap();
    let mut input = prepared.common.clone();
    for (key, value) in certificate.as_object().unwrap() {
        input[key] = value.clone();
    }
    input["preparation"] = certificate;
    input["step_id"] = json!("sync_base");
    input
}

fn a_descendant_advance_replaces_the_pin_without_editing_preparation() {
    let prepared = PreparedRebase::with_edits(
        "jrun-normal-base-race",
        |repo| commit_file(repo, "candidate.txt", "candidate\n"),
        |repo| git(repo, &["rev-parse", "HEAD"]),
    );
    let input = certified_input(&prepared);
    assert_eq!(input["sync_required"], false);
    let original = input.clone();
    let advanced = commit_file(&prepared.fixture.repo, "concurrent.txt", "concurrent\n");
    let result = action(&prepared.host, "git_rebase", &input)
        .expect("a normal descendant base advance must continue without manual preparation");
    assert_eq!(input, original, "the saved preparation is immutable");
    assert_eq!(result["base_sha"], advanced);
    assert!(is_ancestor(&prepared.checkout.path, &advanced, "HEAD"));
    assert_eq!(
        fs::read_to_string(prepared.checkout.path.join("candidate.txt")).unwrap(),
        "candidate\n"
    );
    assert_eq!(
        fs::read_to_string(prepared.checkout.path.join("concurrent.txt")).unwrap(),
        "concurrent\n"
    );
}

fn refresh_count(host: &LifecycleHost) -> usize {
    host.recovery_attempts()
        .iter()
        .filter(|(_, step, _)| step == "sync_base_preparation")
        .count()
}

fn advances(prepared: &PreparedRebase, on_initial: bool, churn: bool) {
    let repo = prepared.fixture.repo.clone();
    *prepared.host.preparation_hook.lock().unwrap() = Some(Box::new(move |checkpoint| {
        if (on_initial && checkpoint["step_id"] == "prepare_branch")
            || (churn && checkpoint["step_id"] == "sync_base_preparation")
        {
            let attempt = checkpoint["recovery_attempt"].as_u64().unwrap()
                + u64::from(checkpoint["step_id"] == "sync_base_preparation");
            commit_file(
                &repo,
                &format!("concurrent-{attempt}.txt"),
                &format!("{attempt}\n"),
            );
            git(&repo, &["push", "origin", BASE]);
            git(&repo, &["fetch", "origin", BASE]);
        }
    }));
}

/// Real preparation and synchronization steps copied from each shipped job.
/// The final boundary validates the exact synchronized checkout and hands the
/// resulting head/base to the publication consumer; no network service is used.
fn shipped_sync_job(name: &str, conflict_recovery: bool) -> JobV2 {
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("../orbit-core/assets");
    let raw: Value = serde_yaml::from_str(
        &fs::read_to_string(assets.join(format!("jobs/{name}.yaml"))).unwrap(),
    )
    .unwrap();
    let mut steps = vec![
        json!({"id":"worktree","spec":{"type":"deterministic","action":"worktree","config":{}}}),
    ];
    steps.push(json!({"id":"implement_one","spec":{"type":"deterministic","action":"implement_one","config":{}}}));
    for id in ["prepare_branch", "sync_base"] {
        let mut step = raw["spec"]["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|step| step["id"] == id)
            .unwrap()
            .clone();
        let object = step.as_object_mut().unwrap();
        let target = object
            .remove("target")
            .unwrap()
            .as_str()
            .unwrap()
            .strip_prefix("activity:")
            .unwrap()
            .to_string();
        object.remove("when");
        object.remove("recovery_activity");
        object.insert(
            "spec".into(),
            json!({"type":"deterministic","action":target,"config":{}}),
        );
        steps.push(step);
    }
    steps.push(json!({
        "id":"validate", "spec":{"type":"deterministic","action":"candidate_validate","config":{}},
        "default_input":{
            "job_run_id":"{{ steps.worktree.output.job_run_id }}", "completed_task_ids":"{{ input.task_ids }}",
            "workspace_path":"{{ steps.worktree.output.workspace_path }}", "base_sha":"{{ steps.sync_base.output.base_sha }}",
            "base_ref":"{{ steps.sync_base.output.base_ref }}",
        },
    }));
    steps.push(json!({
        "id":"deliver", "spec":{"type":"deterministic","action":"deliver","config":{}},
        "default_input":{"head_sha":"{{ steps.sync_base.output.head_sha }}", "base_sha":"{{ steps.sync_base.output.base_sha }}"},
    }));
    let mut job = orbit_engine::activity_job::load_job_asset(
        &json!({
            "schemaVersion":2,"kind":"Job","metadata":{"name":name},
            "spec":{"state":"enabled","kind":"workflow","steps":steps},
        })
        .to_string(),
    )
    .unwrap()
    .spec;
    if conflict_recovery {
        let recovery = super::absorbed_candidate::conflict_recovery_activity();
        let step = job
            .steps
            .iter_mut()
            .find(|step| step.id == "sync_base")
            .unwrap();
        step.recovery_activity = Some("pr_conflict_recovery".into());
        step.resolved_recovery_activity = Some(recovery);
    }
    job
}

fn run_job(
    prepared: &PreparedRebase,
    job: &JobV2,
) -> (
    Result<orbit_engine::JobOutcome, DispatchError>,
    Vec<V2AuditEventKind>,
) {
    prepared.host.stub(
        "worktree",
        Ok(json!({"workspace_path":prepared.checkout.path,"job_run_id":prepared.run_id})),
    );
    prepared.host.stub("deliver", Ok(json!({"delivered":true})));
    prepared.host.stub(
        "implement_one",
        Ok(json!({"execution_summary":"Outcome: success\nChanges:\n- Current claimed candidate."})),
    );
    let logs = TempDir::new().unwrap();
    let envelope = Arc::new(orbit_engine::V2SqliteSink::for_audit_root(
        Arc::new(orbit_store::Store::open_in_memory().unwrap()),
        "ws_preparation",
        &prepared.run_id,
        "fixture",
        None,
        logs.path(),
    ));
    let writer = Arc::new(
        V2AuditWriter::new(
            &prepared.run_id,
            "fixture",
            Arc::new(InMemorySink::new(logs.path())),
        )
        .with_envelope_sink(envelope),
    );
    let result = execute_job_with_resume(
        job,
        json!({"task_ids":["T-REBASE"],"base_branch":BASE,"base_sync":"remote"}),
        &prepared.run_id,
        writer.clone(),
        &prepared.host,
        None,
    );
    (
        result,
        writer
            .events_snapshot()
            .unwrap()
            .into_iter()
            .map(|event| event.kind)
            .collect(),
    )
}

fn shipped_pipelines_refresh_and_validate_the_current_candidate() {
    for pipeline in ["task_pr_pipeline", "task_claimed_pr_pipeline"] {
        let mut prepared =
            PreparedRebase::new(&format!("jrun-{pipeline}"), "candidate.txt", "base.txt");
        if pipeline == "task_claimed_pr_pipeline" {
            prepared.host.worker = Some(orbit_types::tool::WorkerInvocation {
                owner_machine_id: "owner".into(),
                owner_workspace_id: "workspace".into(),
                owner_destination: "owner".into(),
                task_id: "T-REBASE".into(),
                claim_id: "claim-preparation".into(),
                execution: orbit_types::task::ExecutionLocation {
                    machine_id: "follower".into(),
                    machine_name: None,
                },
                bound_run_id: prepared.run_id.clone(),
            });
        }
        git(&prepared.fixture.repo, &["push", "origin", BASE]);
        advances(&prepared, true, false);
        *prepared.host.required_commands.lock().unwrap() = vec!["test -f candidate.txt && test -f concurrent-1.txt && git rev-parse HEAD > .orbit/validated-head".into()];
        let (outcome, events) = run_job(&prepared, &shipped_sync_job(pipeline, false));
        assert!(
            outcome.as_ref().is_ok_and(|outcome| outcome.success),
            "{pipeline}: {outcome:?}"
        );
        let head = prepared.head();
        let base = git(&prepared.fixture.repo, &["rev-parse", BASE]);
        assert!(is_ancestor(&prepared.checkout.path, &base, "HEAD"));
        assert_eq!(
            fs::read_to_string(prepared.checkout.path.join("candidate.txt")).unwrap(),
            "candidate\n"
        );
        assert_eq!(
            fs::read_to_string(prepared.checkout.path.join(".orbit/validated-head"))
                .unwrap()
                .trim(),
            head
        );
        assert_eq!(
            prepared.host.stub_calls_of("deliver"),
            [json!({"head_sha":head,"base_sha":base,"run_id":prepared.run_id,"step_id":"deliver"})]
        );
        assert_eq!(refresh_count(&prepared.host), 1);
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, V2AuditEventKind::StepRecoveryAttempted { .. })),
            "normal base races never dispatch an agent"
        );
        let artifacts = prepared.host.artifacts.lock().unwrap();
        let attempts = artifacts["T-REBASE"]
            .iter()
            .filter(|a| a.path.contains("/preparation/"))
            .collect::<Vec<_>>();
        assert_eq!(
            attempts.len(),
            2,
            "original and fresh preparation remain separately identifiable"
        );
        let fresh: Value = serde_json::from_slice(&attempts[1].content).unwrap();
        assert!(
            fresh["original_error"]
                .as_str()
                .unwrap()
                .contains("moved from checkpoint")
        );
        assert_eq!(fresh["previous_base_sha"], prepared.target);
        assert_eq!(fresh["base_sha"], base);
    }
}

fn direct_stale_and_invalid_preparations_refuse_without_mutation() {
    for case in [
        "direct",
        "tampered",
        "foreign-run",
        "foreign-workspace",
        "candidate",
        "branch",
        "lease",
        "dirty",
        "foreign-rebase",
        "rewound",
        "unrelated",
    ] {
        let prepared =
            PreparedRebase::new(&format!("jrun-refuse-{case}"), "candidate.txt", "base.txt");
        let mut input = certified_input(&prepared);
        commit_file(&prepared.fixture.repo, "advance.txt", "advance\n");
        match case {
            "direct" => {
                input.as_object_mut().unwrap().remove("preparation");
            }
            "tampered" => {
                input["preparation"]["preparation"]["base_sha"] = json!(prepared.base_sha)
            }
            "foreign-run" => input["preparation"]["preparation"]["run_id"] = json!("jrun-other"),
            "foreign-workspace" => {
                input["preparation"]["preparation"]["workspace_path"] = json!(prepared.fixture.repo)
            }
            "candidate" => {
                commit_file(&prepared.checkout.path, "foreign.txt", "foreign\n");
            }
            "branch" => {
                git(&prepared.checkout.path, &["checkout", "-b", "foreign"]);
            }
            "lease" => {
                git(&prepared.checkout.path, &["push", "origin", "HEAD"]);
            }
            "dirty" => fs::write(prepared.checkout.path.join("candidate.txt"), "dirty\n").unwrap(),
            "foreign-rebase" => {
                let directory = git_path(&prepared.checkout.path, "rebase-merge");
                fs::create_dir_all(&directory).unwrap();
                for (name, value) in [
                    ("orig-head", prepared.base_sha.as_str()),
                    ("onto", prepared.target.as_str()),
                    ("head-name", "refs/heads/foreign"),
                ] {
                    fs::write(directory.join(name), value).unwrap();
                }
            }
            "rewound" => {
                git(
                    &prepared.fixture.repo,
                    &[
                        "update-ref",
                        &format!("refs/heads/{BASE}"),
                        &prepared.base_sha,
                    ],
                );
            }
            "unrelated" => {
                let tree = git(&prepared.fixture.repo, &["rev-parse", "HEAD^{tree}"]);
                let unrelated = git(
                    &prepared.fixture.repo,
                    &[
                        "-c",
                        "commit.gpgsign=false",
                        "commit-tree",
                        &tree,
                        "-m",
                        "unrelated",
                    ],
                );
                git(
                    &prepared.fixture.repo,
                    &["update-ref", &format!("refs/heads/{BASE}"), &unrelated],
                );
            }
            _ => unreachable!(),
        }
        let head = prepared.head();
        let status = git(&prepared.checkout.path, &["status", "--porcelain"]);
        let error = action(&prepared.host, "git_rebase", &input).expect_err(case);
        assert!(
            !matches!(error, OrbitError::RecoverableVcsConflict(_)),
            "{case}: {error}"
        );
        assert_eq!(prepared.head(), head, "{case}");
        assert_eq!(
            git(&prepared.checkout.path, &["status", "--porcelain"]),
            status,
            "{case}"
        );
        assert_eq!(refresh_count(&prepared.host), 0, "{case}: {error}");
        if case == "direct" {
            assert!(
                error.to_string().contains("moved from checkpoint"),
                "{error}"
            );
        }
    }
}

fn churn_and_deadline_exhaustion_keep_original_and_fresh_evidence() {
    for deadline in [false, true] {
        let prepared = PreparedRebase::new("jrun-churn", "candidate.txt", "base.txt");
        let input = certified_input(&prepared);
        commit_file(&prepared.fixture.repo, "advance.txt", "advance\n");
        let clock = Arc::new(AtomicU64::new(1000));
        if deadline {
            let clock = clock.clone();
            let repo = prepared.fixture.repo.clone();
            *prepared.host.preparation_hook.lock().unwrap() = Some(Box::new(move |checkpoint| {
                if checkpoint["step_id"] == "sync_base_preparation" {
                    commit_file(
                        &repo,
                        "churn.txt",
                        &format!("{}\n", checkpoint["recovery_attempt"]),
                    );
                    clock.store(121_001, Ordering::SeqCst);
                }
            }));
        } else {
            advances(&prepared, false, true);
        }
        let retry_clock = clock.clone();
        let error = with_prerequisite_clock(
            move || clock.load(Ordering::SeqCst),
            |_, _| {},
            || action(&prepared.host, "git_rebase", &input),
        )
        .unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains(if deadline {
                "deadline exhausted"
            } else {
                "exhausted 2 fresh"
            }),
            "{message}"
        );
        assert!(
            message.contains("original stale preparation error")
                && message.contains(&prepared.target)
                && message.contains("attempt trace"),
            "{message}"
        );
        assert_eq!(prepared.head(), prepared.candidate);
        assert_eq!(refresh_count(&prepared.host), if deadline { 1 } else { 2 });
        let checkpoints = prepared.host.checkpoints();
        assert_eq!(
            checkpoints.len(),
            if deadline { 2 } else { 3 },
            "all attempts stay immutable"
        );
        let before = checkpoints.len();
        let retry = with_prerequisite_clock(
            move || retry_clock.load(Ordering::SeqCst),
            |_, _| {},
            || action(&prepared.host, "git_rebase", &input),
        )
        .unwrap_err();
        assert!(
            retry.to_string().contains(if deadline {
                "deadline exhausted"
            } else {
                "exhausted 2 fresh"
            }),
            "{retry}"
        );
        assert_eq!(
            prepared.host.checkpoints().len(),
            before,
            "restart cannot reset either budget"
        );
        if !deadline {
            // The existing explicit same-run reprepare seam remains usable,
            // but issuing a new preparation never replenishes refresh budget.
            *prepared.host.preparation_hook.lock().unwrap() = None;
            let fresh = certified_input(&prepared);
            let result = action(&prepared.host, "git_rebase", &fresh).unwrap();
            assert_eq!(result["head_sha"], prepared.head());
            assert_eq!(refresh_count(&prepared.host), 2);
        }
    }
}

#[test]
fn preparation_contracts() {
    isolated_in(module_path!(), "preparation_contracts", || {
        crate::git_fixture::run_scenarios(&[
            (
                "a_descendant_advance_replaces_the_pin_without_editing_preparation",
                a_descendant_advance_replaces_the_pin_without_editing_preparation,
            ),
            (
                "shipped_pipelines_refresh_and_validate_the_current_candidate",
                shipped_pipelines_refresh_and_validate_the_current_candidate,
            ),
            (
                "direct_stale_and_invalid_preparations_refuse_without_mutation",
                direct_stale_and_invalid_preparations_refuse_without_mutation,
            ),
            (
                "churn_and_deadline_exhaustion_keep_original_and_fresh_evidence",
                churn_and_deadline_exhaustion_keep_original_and_fresh_evidence,
            ),
            (
                "fresh_preparation_preserves_conflict_and_absorbed_paths",
                fresh_preparation_preserves_conflict_and_absorbed_paths,
            ),
            (
                "replacement_evidence_and_simultaneous_candidate_changes_fail_closed",
                replacement_evidence_and_simultaneous_candidate_changes_fail_closed,
            ),
            (
                "an_uncertified_rewrite_is_never_reset_during_authenticated_sync",
                an_uncertified_rewrite_is_never_reset_during_authenticated_sync,
            ),
            (
                "exhausted_preparation_never_dispatches_source_recovery",
                exhausted_preparation_never_dispatches_source_recovery,
            ),
        ]);
    });
}

fn fresh_preparation_preserves_conflict_and_absorbed_paths() {
    #[cfg(unix)]
    {
        let mut prepared = PreparedRebase::new("jrun-refresh-conflict", "README.md", "README.md");
        git(&prepared.fixture.repo, &["push", "origin", BASE]);
        let provider = prepared.fixture.root.path().join("codex");
        write_executable(
            &provider,
            &provider_script("printf 'resolved\\n' > README.md"),
        );
        prepared.host.provider = Some(provider);
        advances(&prepared, true, false);
        *prepared.host.required_commands.lock().unwrap() =
            vec!["test -f concurrent-1.txt && git rev-parse HEAD > .orbit/validated-head".into()];
        let (result, events) = run_job(&prepared, &shipped_sync_job("task_pr_pipeline", true));
        assert!(
            result.as_ref().is_ok_and(|outcome| outcome.success),
            "{result:?}; events: {events:#?}"
        );
        assert_eq!(
            fs::read_to_string(prepared.checkout.path.join("README.md")).unwrap(),
            "resolved\n"
        );
        assert_eq!(refresh_count(&prepared.host), 1);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    V2AuditEventKind::StepRecoveryAttempted {
                        recovery_succeeded: true,
                        ..
                    }
                ))
                .count(),
            1
        );
        let checkpoints = prepared.host.checkpoints();
        let (_, _, fresh) = checkpoints
            .iter()
            .find(|(_, step, _)| step == "sync_base_preparation")
            .unwrap();
        let (_, _, recovery) = checkpoints
            .iter()
            .find(|(_, step, _)| step == "sync_base")
            .unwrap();
        assert_eq!(
            recovery["target_base_sha"], fresh["base_sha"],
            "conflict recovery uses the new pin"
        );
    }
    let prepared = PreparedRebase::new("jrun-refresh-absorbed", "README.md", "README.md");
    let mut input = certified_input(&prepared);
    let covering = commit_file(&prepared.fixture.repo, "README.md", "candidate\n");
    input["final_validation"] = json!(true);
    *prepared.host.required_commands.lock().unwrap() =
        vec!["git rev-parse HEAD > .orbit/validated-head".into()];
    let error = action(&prepared.host, "git_rebase", &input).unwrap_err();
    let OrbitError::CandidateAbsorbed(candidate) = error else {
        panic!("expected certified absorption: {error}");
    };
    assert_eq!(candidate.base_sha, covering);
    assert_eq!(candidate.covering_commit, covering);
    assert_eq!(prepared.head(), covering);
    assert_eq!(
        fs::read_to_string(prepared.checkout.path.join(".orbit/validated-head"))
            .unwrap()
            .trim(),
        covering,
        "captured validation must run on the exact absorbed head before settlement"
    );
    assert_eq!(refresh_count(&prepared.host), 1);
}

fn replacement_evidence_and_simultaneous_candidate_changes_fail_closed() {
    for case in [
        "tamper-refresh",
        "missing-refresh",
        "candidate-after-refresh",
        "branch-after-refresh",
        "owner-revoked",
    ] {
        let prepared = PreparedRebase::new("jrun-refresh-race", "candidate.txt", "base.txt");
        let input = certified_input(&prepared);
        commit_file(&prepared.fixture.repo, "advance.txt", "advance\n");
        let checkout = prepared.checkout.path.clone();
        let repo = prepared.fixture.repo.clone();
        if case == "owner-revoked" {
            *prepared.host.recovery_owner_refusal.lock().unwrap() =
                Some("live ownership revoked".into());
        } else {
            *prepared.host.preparation_hook.lock().unwrap() = Some(Box::new(move |checkpoint| {
                if checkpoint["step_id"] != "sync_base_preparation" {
                    return;
                }
                match case {
                    "candidate-after-refresh" => {
                        commit_file(&checkout, "foreign.txt", "foreign\n");
                    }
                    "branch-after-refresh" => {
                        git(&checkout, &["checkout", "-b", "foreign"]);
                    }
                    _ => {
                        commit_file(
                            &repo,
                            "churn.txt",
                            &format!("{}\n", checkpoint["recovery_attempt"]),
                        );
                    }
                }
            }));
        }
        let first = action(&prepared.host, "git_rebase", &input).unwrap_err();
        if case == "tamper-refresh" || case == "missing-refresh" {
            let mut state = prepared.host.run_states.lock().unwrap();
            let checkpoints = &mut state
                .get_mut(&prepared.run_id)
                .unwrap()
                .rebase_recovery_checkpoints;
            if case == "tamper-refresh" {
                checkpoints.get_mut("sync_base_preparation").unwrap()["base_sha"] =
                    json!(prepared.target);
            } else {
                checkpoints.remove("sync_base_preparation");
            }
            drop(state);
            let before = prepared.head();
            let retry = action(&prepared.host, "git_rebase", &input).unwrap_err();
            assert!(retry.to_string().contains("refresh"), "{case}: {retry}");
            assert_eq!(prepared.head(), before);
        } else {
            assert!(
                !matches!(first, OrbitError::RecoverableVcsConflict(_)),
                "{case}: {first}"
            );
            if case == "candidate-after-refresh" {
                assert!(prepared.checkout.path.join("foreign.txt").exists());
            } else if case == "branch-after-refresh" {
                assert_eq!(
                    git(&prepared.checkout.path, &["branch", "--show-current"]),
                    "foreign"
                );
            } else {
                assert_eq!(refresh_count(&prepared.host), 0);
            }
        }
    }
}

fn an_uncertified_rewrite_is_never_reset_during_authenticated_sync() {
    let prepared = PreparedRebase::new("jrun-foreign-rewrite", "candidate.txt", "base.txt");
    let input = certified_input(&prepared);
    git(&prepared.checkout.path, &["rebase", &prepared.target]);
    let foreign = commit_file(&prepared.checkout.path, "foreign.txt", "foreign\n");
    prepared.host.leaf_writes_recovery(
        &prepared.run_id,
        "sync_base",
        json!({
            "run_id":prepared.run_id, "step_id":"sync_base", "head_sha":foreign,
            "workspace_path":prepared.checkout.path, "rewritten":true,
        }),
    );
    let error = action(&prepared.host, "git_rebase", &input).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("without an authenticated rewrite"),
        "{error}"
    );
    assert_eq!(
        prepared.head(),
        foreign,
        "untrusted evidence never permits resetting foreign work"
    );
    assert!(prepared.checkout.path.join("foreign.txt").exists());
    assert_eq!(refresh_count(&prepared.host), 0);
}

fn exhausted_preparation_never_dispatches_source_recovery() {
    let prepared = PreparedRebase::new("jrun-churn-no-agent", "candidate.txt", "base.txt");
    git(&prepared.fixture.repo, &["push", "origin", BASE]);
    advances(&prepared, true, true);
    let mut job = shipped_sync_job("task_pr_pipeline", true);
    job.final_recovery_activity = Some("decide".into());
    job.resolved_final_recovery_activity =
        Some(super::absorbed_candidate::deterministic_activity("decide"));
    prepared.host.stub("decide", Ok(json!({"unexpected":true})));
    let (result, events) = run_job(&prepared, &job);
    assert!(result.is_err(), "churn must exhaust: {result:?}");
    assert_eq!(refresh_count(&prepared.host), 2);
    assert_eq!(prepared.head(), prepared.candidate);
    assert!(
        prepared.host.stub_calls_of("decide").is_empty(),
        "exhaustion never dispatches source repair"
    );
    assert!(events.iter().any(|event| matches!(event, V2AuditEventKind::FinalRecoveryAttempted { outcome, detail: Some(reason), .. }
        if outcome == "skipped" && reason.contains("host-owned preparation"))), "{events:#?}");
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, V2AuditEventKind::StepRecoveryAttempted { .. })),
        "a base race is not conflict recovery"
    );
}
