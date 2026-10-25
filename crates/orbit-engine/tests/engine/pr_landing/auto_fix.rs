//! Resumed auto-fix authority through the public job and PR boundaries.

use super::*;
use std::io::Write as _;

type JobResult = Result<orbit_engine::JobOutcome, DispatchError>;

fn cases(names: &str) -> std::str::SplitWhitespace<'_> {
    names.split_whitespace()
}

fn auto_fix_input(fx: &Fixture) -> Value {
    let mut input = fx.complete_ungated_input();
    input["auto_fix"] = json!(true);
    input["completion"] = json!("review");
    input
}

fn is_check_repair(error: &str) -> bool {
    orbit_types::workflow::is_pr_auto_fix_check_repair(error)
}

fn repair_attempts(host: &DeliveryHost) -> Vec<PrAutoFixAttempt> {
    pr_auto_fix_attempts(&host.history(TASK_ID), PR_NUMBER)
}

fn repair_pushes(host: &DeliveryHost) -> Vec<PrAutoFixPush> {
    orbit_types::workflow::pr_auto_fix_pushes(&host.history(TASK_ID), PR_NUMBER)
}

fn scripted_ran(host: &DeliveryHost, name: &str) -> bool {
    host.scripted_inputs.lock().unwrap().contains_key(name)
}

fn seed_attempt(
    host: &DeliveryHost,
    head: &str,
    blocker: PrAutoFixBlocker,
    attempt: u32,
    run_id: &str,
) {
    seed_auto_fix_history(
        host,
        PR_AUTO_FIX_ADMITTED_EVENT,
        &PrAutoFixAttempt {
            pr_number: PR_NUMBER.into(),
            head_sha: head.into(),
            blocker,
            attempt,
            budget: PR_AUTO_FIX_BUDGET,
            run_id: run_id.into(),
            crew: "fixture".into(),
            crew_source: "fixture".into(),
        },
    );
}

fn seed_spent(host: &DeliveryHost, from: u32, blocker: PrAutoFixBlocker, run_id: &str) {
    for attempt in from..=PR_AUTO_FIX_BUDGET {
        seed_attempt(host, &format!("spent-{attempt}"), blocker, attempt, run_id);
    }
}

fn reserve_base_update(host: &DeliveryHost, head: &str, base_sha: &str) {
    seed_auto_fix_history(
        host,
        orbit_types::workflow::PR_BASE_UPDATE_RESERVED_EVENT,
        &orbit_types::workflow::PrBaseUpdateReservation {
            execution: RUN_ID.into(),
            repository: "forge/remote".into(),
            pr_number: PR_NUMBER.into(),
            head_sha: head.into(),
            head_branch: BRANCH.into(),
            base_branch: BASE.into(),
            base_sha: base_sha.into(),
            deadline_ms: 1_100_000,
            evidence: json!({"cause": "inherited"}),
        },
    );
}

fn deterministic_activity(name: &str, action: &str) -> ActivityV2 {
    ActivityV2 {
        description: name.into(),
        input_schema_json: Value::Null,
        output_schema_json: Value::Null,
        fs_profile: None,
        spec: ActivityV2Spec::Deterministic(DeterministicSpec {
            action: action.into(),
            config: Value::Null,
        }),
    }
}

fn run_job(
    host: &DeliveryHost,
    mut job: orbit_types::workflow::JobV2,
    activities: &[(&str, &str)],
    run_id: &str,
    input: Value,
    resume: Option<&PipelineState>,
) -> JobResult {
    let mut catalog = V2ActivityCatalog::new();
    for &(name, action) in activities {
        catalog.insert(name.to_string(), deterministic_activity(name, action));
    }
    resolve_job_catalog_refs_for_execution(&mut job, &catalog).unwrap();
    let audit = tempfile::tempdir().unwrap();
    let (writer, _, _) = build_writer_and_sinks(audit.path(), run_id);
    execute_job_with_resume(&job, input, run_id, writer, host, resume)
}

const REPAIR_ACTS: &[(&str, &str)] = &[
    ("checkpoint", "test_checkpoint"),
    ("pr_complete", "pr_complete"),
    ("step_failure_recovery", "test_ci_repair"),
    ("pr_conflict_recovery", "unexpected_conflict"),
];

fn repair_job(input: Value) -> orbit_types::workflow::JobV2 {
    job_asset(json!([
        {"id":"commit","target":"activity:checkpoint","recovery_activity":"step_failure_recovery"},
        {"id":"complete_pr","target":"activity:pr_complete","recovery_activity":"pr_conflict_recovery","default_input":input}
    ]))
}

fn run_repair_job(host: &DeliveryHost, input: Value) -> JobResult {
    run_job(
        host,
        repair_job(input),
        REPAIR_ACTS,
        RUN_ID,
        json!({}),
        None,
    )
}

fn opened_review(row: &std::path::Path) -> (Fixture, DeliveryHost) {
    let fx = Fixture::new(row);
    let host = DeliveryHost::new(&fx.repo, TaskStatus::Review);
    open_for_completion(&fx, &host);
    (fx, host)
}

fn docs_only_pr(row: &std::path::Path) -> (Fixture, DeliveryHost) {
    let mut fx = Fixture::new(row);
    fs::create_dir_all(fx.repo.join(".git/info")).unwrap();
    fs::write(fx.repo.join(".git/info/exclude"), ".orbit/\n").unwrap();
    fx.commit_on_base("src/contract", "broken\n");
    git(&fx.repo, &["reset", "--hard", BASE]);
    fx.base_sha = fx.head();
    delete_remote_branch(&fx, BRANCH);
    fx.candidate = fx.commit("README.md", "documentation only\n");
    let host = DeliveryHost::new(&fx.repo, TaskStatus::Review);
    host.set_context_files(TASK_ID, &["file:README.md"]);
    action(
        &host,
        "pr_open",
        &fx.open_input(&fx.candidate, &fx.base_sha),
    )
    .unwrap();
    (fx, host)
}

// A successful publication checkpoint deliberately keeps H0 when completion
// fails after repairing it. Resume executes completion as R2, retaining R0's
// task/worktree ownership but inheriting only its durable ancestor authority.
fn resume_auto_fix(fx: &Fixture, host: &DeliveryHost, reviewed: bool) -> Result<Value, String> {
    let mut input = auto_fix_input(fx);
    input["published_head_sha"] = json!("{{ steps.push.output.local_sha }}");
    if reviewed {
        input["reviewed_head_sha"] = json!(fx.candidate);
    }
    host.script("test_stub_checkpoint", json!({"unexpected": true}));
    let mut resume = PipelineState::new(RUN_ID.into(), "fixture-job".into(), json!({}));
    resume.step_states = BTreeMap::from([
        (0, orbit_types::workflow::JobRunState::Success),
        (1, orbit_types::workflow::JobRunState::Success),
        (2, orbit_types::workflow::JobRunState::Failed),
    ]);
    resume.step_outputs = BTreeMap::from([
        (0, json!({"commit_sha": fx.candidate})),
        (1, json!({"local_sha": fx.candidate})),
    ]);
    let outcome = run_job(
        host,
        job_asset(json!([
            {"id": "commit", "target": "activity:fixture_checkpoint"},
            {"id": "push", "target": "activity:fixture_checkpoint"},
            {"id": "complete_pr", "target": "activity:pr_complete", "default_input": input}
        ])),
        &[
            ("fixture_checkpoint", "test_stub_checkpoint"),
            ("pr_complete", "pr_complete"),
        ],
        "jrun-resumed",
        json!({}),
        Some(&resume),
    )
    .map_err(|error| error.to_string())?;
    assert!(
        !scripted_ran(host, "test_stub_checkpoint"),
        "successful prefix is restored, never replayed"
    );
    if !outcome.success {
        return Err(outcome.message.unwrap_or_default());
    }
    Ok(outcome.pipeline["complete_pr"].clone())
}

pub(super) fn seed_auto_fix_history(
    host: &DeliveryHost,
    event: &str,
    note: &impl serde::Serialize,
) {
    host.history
        .lock()
        .unwrap()
        .entry(TASK_ID.into())
        .or_default()
        .push(TaskHistoryEntry {
            at: Utc::now(),
            by: "system".into(),
            event: event.into(),
            note: Some(serde_json::to_string(note).unwrap()),
            from_status: None,
            to_status: None,
            operation_id: None,
            decision: None,
        });
}

fn auto_fix_resume_fixture(fx: &Fixture, source: &str) -> DeliveryHost {
    let host = DeliveryHost::new(&fx.repo, TaskStatus::Review);
    action(
        &host,
        "pr_open",
        &fx.open_input(&fx.candidate, &fx.base_sha),
    )
    .unwrap();
    for (run, parent) in [
        (RUN_ID, None),
        ("jrun-middle", Some(source)),
        ("jrun-resumed", Some("jrun-middle")),
    ] {
        host.runs.lock().unwrap().insert(
            run.into(),
            serde_json::from_value(json!({
                "run_id": run, "job_id": "fixture-job", "attempt": 1, "state": "failed",
                "scheduled_at": Utc::now(), "created_at": Utc::now(),
                "retry_source_run_id": parent
            }))
            .unwrap(),
        );
    }
    seed_attempt(
        &host,
        &fx.candidate,
        PrAutoFixBlocker::FailedChecks,
        1,
        RUN_ID,
    );
    host
}

#[test]
fn auto_fix_resume_adopts_only_source_lineage_publication_chains() {
    isolated(
        "auto_fix::auto_fix_resume_adopts_only_source_lineage_publication_chains",
        |sandbox| {
            for case in cases(
                "published unrelated wrong_pr wrong_branch noncontiguous changed_head reviewed budget cross_job missing_parent cycle",
            ) {
                in_fresh_row(sandbox, |row| {
                    let fx = Fixture::new(row);
                    let host = auto_fix_resume_fixture(&fx, RUN_ID);
                    if case == "unrelated" {
                        host.runs
                            .lock()
                            .unwrap()
                            .get_mut("jrun-middle")
                            .unwrap()
                            .retry_source_run_id = None;
                    } else if case == "cross_job" {
                        host.runs.lock().unwrap().get_mut(RUN_ID).unwrap().job_id =
                            "other-job".into();
                    } else if case == "missing_parent" {
                        host.runs
                            .lock()
                            .unwrap()
                            .get_mut("jrun-middle")
                            .unwrap()
                            .retry_source_run_id = Some("missing".into());
                    } else if case == "cycle" {
                        host.runs
                            .lock()
                            .unwrap()
                            .get_mut(RUN_ID)
                            .unwrap()
                            .retry_source_run_id = Some("jrun-resumed".into());
                    }
                    let mut repair = fx.commit("repair.txt", "repair\n");
                    seed_auto_fix_history(
                        &host,
                        PR_AUTO_FIX_PUSHED_EVENT,
                        &PrAutoFixPush {
                            pr_number: if case == "wrong_pr" { "99" } else { PR_NUMBER }.into(),
                            from_head_sha: if case == "noncontiguous" {
                                fx.base_sha.clone()
                            } else {
                                fx.candidate.clone()
                            },
                            to_head_sha: repair.clone(),
                            blocker: PrAutoFixBlocker::FailedChecks,
                            run_id: RUN_ID.into(),
                        },
                    );
                    if case == "published" {
                        let next = fx.commit("repair2.txt", "next repair\n");
                        seed_auto_fix_history(
                            &host,
                            PR_AUTO_FIX_ADMITTED_EVENT,
                            &PrAutoFixAttempt {
                                pr_number: PR_NUMBER.into(),
                                head_sha: repair.clone(),
                                blocker: PrAutoFixBlocker::Conflict,
                                attempt: 2,
                                budget: PR_AUTO_FIX_BUDGET,
                                run_id: "jrun-middle".into(),
                                crew: "fixture".into(),
                                crew_source: "fixture".into(),
                            },
                        );
                        seed_auto_fix_history(
                            &host,
                            PR_AUTO_FIX_PUSHED_EVENT,
                            &PrAutoFixPush {
                                pr_number: PR_NUMBER.into(),
                                from_head_sha: repair,
                                to_head_sha: next.clone(),
                                blocker: PrAutoFixBlocker::Conflict,
                                run_id: "jrun-middle".into(),
                            },
                        );
                        repair = next;
                    }
                    if case == "wrong_branch" {
                        git(
                            &fx.repo,
                            &[
                                "push",
                                "origin",
                                &format!("{repair}:refs/heads/other-branch"),
                            ],
                        );
                        fs::write(fx.forge.join("pr-head"), "other-branch\n").unwrap();
                    }
                    if case == "changed_head" {
                        fx.commit("foreign.txt", "foreign\n");
                    }
                    // Exercise the publication ledger alone, independently of
                    // the local-repair admission consumer.
                    git(&fx.repo, &["reset", "--hard", &fx.candidate]);
                    if case == "budget" {
                        seed_spent(&host, 2, PrAutoFixBlocker::Conflict, RUN_ID);
                        fx.script_checks(&["dirty"]);
                    } else {
                        fx.script_checks(&["pending", "success"]);
                    }
                    let result = resume_auto_fix(&fx, &host, case == "reviewed");
                    if case == "published" {
                        let output = result.expect("resume must watch the source-run repair instead of refusing H1 as stale");
                        assert_eq!(output["merge"]["reason"], "auto_fix_mergeable");
                        assert_eq!(output["merge"]["head_sha"], repair);
                    } else {
                        let error = result.expect_err(case);
                        let expected = match case {
                            "reviewed" => "review_gate_stale",
                            "budget" => "auto_fix_budget_exhausted",
                            "cross_job" | "missing_parent" | "cycle" => "auto_fix",
                            _ => "delivery_evidence_stale",
                        };
                        assert!(error.contains(expected), "{case}: {error}");
                    }
                    assert!(
                        fx.merge_requests().is_empty(),
                        "repair authority never merges: {case}"
                    );
                    assert_eq!(host.status(TASK_ID), TaskStatus::Review);
                    assert_eq!(
                        repair_attempts(&host).len(),
                        if case == "budget" {
                            3
                        } else if case == "published" {
                            2
                        } else {
                            1
                        }
                    );
                });
            }
        },
    );
}

#[test]
fn auto_fix_resume_publishes_source_admitted_local_repair_safely() {
    isolated(
        "auto_fix::auto_fix_resume_publishes_source_admitted_local_repair_safely",
        |sandbox| {
            for case in cases(
                "unpublished reviewed remote_moved validation_failed unrelated detached wrong_branch",
            ) {
                in_fresh_row(sandbox, |row| {
                    let fx = Fixture::new(row);
                    let host = auto_fix_resume_fixture(&fx, RUN_ID);
                    if case == "unrelated" {
                        host.runs
                            .lock()
                            .unwrap()
                            .get_mut("jrun-middle")
                            .unwrap()
                            .retry_source_run_id = None;
                    }
                    fs::write(fx.repo.join("repair.txt"), "repair\n").unwrap();
                    git(&fx.repo, &["add", "repair.txt"]);
                    git(&fx.repo, &["commit", "-m", "admitted repair"]);
                    let repair = fx.head();
                    host.require_commands(&[if case == "validation_failed" {
                        "exit 23"
                    } else {
                        "test -f repair.txt"
                    }]);
                    if case == "remote_moved" {
                        git(&fx.repo, &["push", "origin", BRANCH]);
                        fx.commit("foreign.txt", "foreign\n");
                        git(&fx.repo, &["reset", "--hard", &repair]);
                    } else if case == "detached" {
                        git(&fx.repo, &["checkout", "--detach"]);
                    } else if case == "wrong_branch" {
                        git(&fx.repo, &["checkout", "-b", "other-branch"]);
                    }
                    fx.script_checks(&["success"]);
                    let remote_before = fx.remote_tip(BRANCH);
                    let result = resume_auto_fix(&fx, &host, case == "reviewed");
                    match case {
                        "unpublished" => {
                            let output = result.expect("source-admitted committed repair must validate, publish and watch on resume");
                            assert_eq!(output["merge"]["head_sha"], repair);
                            assert_eq!(fx.remote_tip(BRANCH), repair);
                            let pushes = repair_pushes(&host);
                            assert_eq!(pushes.len(), 1);
                            assert_eq!(pushes[0].run_id, "jrun-resumed");
                            assert_eq!(pushes[0].from_head_sha, fx.candidate);
                        }
                        "reviewed" => {
                            let output = result.expect(
                                "resumed reviewed repair returns to review before publication",
                            );
                            assert_eq!(output["re_review_required"], true);
                            assert_eq!(output["rebased"]["head_sha"], repair);
                            assert_eq!(fx.remote_tip(BRANCH), fx.candidate);
                        }
                        "remote_moved" | "validation_failed" => {
                            let error = result.expect_err(case);
                            let expected = if case == "remote_moved" {
                                "moved away"
                            } else {
                                "23"
                            };
                            assert!(error.contains(expected), "{case}: {error}");
                            assert_eq!(
                                fx.remote_tip(BRANCH),
                                remote_before,
                                "failed repair must leave the remote unchanged"
                            );
                        }
                        _ => {
                            result.expect(
                                "without admitted branch ownership completion only watches H0",
                            );
                            assert_eq!(fx.remote_tip(BRANCH), fx.candidate);
                        }
                    }
                    assert!(
                        fx.merge_requests().is_empty(),
                        "repair authority never merges: {case}"
                    );
                    assert_eq!(host.status(TASK_ID), TaskStatus::Review);
                    assert_eq!(repair_attempts(&host).len(), 1);
                    if case != "unpublished" {
                        assert!(
                            orbit_types::workflow::pr_auto_fix_pushes(
                                &host.history(TASK_ID),
                                PR_NUMBER
                            )
                            .is_empty()
                        );
                    }
                });
            }
        },
    );
}

// CI API answers belong to the same bare-remote head read by the forge.
// Unlike a scripted green status, source_contract evaluates the published tree.
pub(super) fn ci_answer(host: &DeliveryHost, operation: &str, input: &Value) -> Option<Value> {
    let forge = host.repo.parent().unwrap().join("forge");
    let observed = || {
        let reads: usize = fs::read_to_string(forge.join("reads"))
            .unwrap_or_else(|_| "0".into())
            .trim()
            .parse()
            .unwrap();
        let script = fs::read_to_string(forge.join("checks")).unwrap();
        script
            .lines()
            .nth(reads.saturating_sub(1))
            .or_else(|| script.lines().last())
            .unwrap()
            .to_string()
    };
    let aggregate = host.scripted.lock().unwrap()["test_ci_repair"]["aggregate"]
        .as_str()
        .map(ToOwned::to_owned);
    let contract = |commit: &str| git(&host.repo, &["show", &format!("{commit}:src/contract")]);
    match operation {
        "pr.required_checks" => {
            let head = git(
                &host.repo,
                &["ls-remote", "origin", &format!("refs/heads/{BRANCH}")],
            );
            let head = head.split_whitespace().next().unwrap();
            let observed = observed();
            let status = if observed == "pending" {
                "IN_PROGRESS"
            } else {
                "COMPLETED"
            };
            let conclusion = if observed == "pending" {
                ""
            } else if observed == "success" {
                "SUCCESS"
            } else {
                "FAILURE"
            };
            let nodes = match aggregate.as_deref() {
                // [ORB-16185] The provider app's aggregate is a required check
                // run outside any workflow; its analysis job passes.
                Some(aggregate) => {
                    let failed = if contract(head) == "fixed" {
                        "SUCCESS"
                    } else {
                        "FAILURE"
                    };
                    let mut nodes = vec![
                        json!({"__typename":"CheckRun","name":"CodeQL","isRequired":true,"status":"COMPLETED","conclusion":failed,
                            "databaseId":if aggregate == "missing_identity" { Value::Null } else { json!(7001) },
                            "checkSuite":{"app":{"databaseId":57789},"workflowRun":null}}),
                        json!({"__typename":"CheckRun","name":"Analyze (rust)","isRequired":true,"status":"COMPLETED","conclusion":"SUCCESS",
                            "databaseId":7002,"checkSuite":{"app":{"databaseId":15368},"workflowRun":{"workflow":{"name":"CodeQL"}}}}),
                    ];
                    if aggregate == "mixed" {
                        nodes.push(json!({"name":"test","isRequired":true,"status":"COMPLETED","conclusion":failed}));
                    }
                    nodes
                }
                None => vec![
                    json!({"name":"test","isRequired":true,"status":status,"conclusion":conclusion}),
                ],
            };
            Some(
                json!({"pull_request":{"headRefOid":head,"commits":{"nodes":[{"commit":{
                    "oid":head,
                    "statusCheckRollup":{"contexts":{"pageInfo":{"hasNextPage":false},"nodes":nodes}}
                }}]}}}),
            )
        }
        "ci.commit_jobs" => {
            let commit = input["commit"].as_str().unwrap();
            // The base tip's own job stays green here. A shared source file is
            // still a candidate failure; a base whose job fails the same way
            // is planted through the substitute `gh`.
            let contents = if commit == git(&host.repo, &["rev-parse", BASE]) {
                "fixed".to_string()
            } else {
                contract(commit)
            };
            let run = u64::from_str_radix(&commit[..12], 16).unwrap();
            let test = json!({"name":"test","workflow":"CI","status":"completed","conclusion":if contents == "fixed" {"success"} else {"failure"},"run_id":run,"job_id":run+1,"failed_step":"source contract"});
            let jobs = match aggregate.as_deref() {
                Some(aggregate) => {
                    let mut jobs = vec![
                        json!({"name":"Analyze (rust)","workflow":"CodeQL","conclusion":"success","run_id":run+2,"job_id":run+3,"failed_step":null}),
                    ];
                    if aggregate == "mixed" {
                        jobs.push(test);
                    }
                    jobs
                }
                None => vec![test],
            };
            Some(json!({"jobs":jobs}))
        }

        "ci.job_log" => Some(
            json!({"log":"error: src/contract: expected fixed, found broken\nsource contract failed"}),
        ),
        "ci.check_annotations" => {
            let aggregate = aggregate.unwrap_or_default();
            let head = input["head_sha"].as_str().unwrap();
            assert_eq!(
                (
                    &input["name"],
                    &input["check_run_id"],
                    &input["app_id"],
                    &input["pr"]
                ),
                (
                    &json!("CodeQL"),
                    &json!(7001),
                    &json!(57789),
                    &json!(PR_NUMBER)
                ),
                "the read names the identities of the head-bound required-check read"
            );
            let path = match aggregate.as_str() {
                "workflow_path" => ".github/workflows/ci.yml",
                "escape" => "../outside/src/contract",
                _ => "src/contract",
            };
            let blob_head = if aggregate == "stale" {
                "0".repeat(40)
            } else {
                head.to_string()
            };
            Some(json!({
                "repository": "orbit/test", "repository_id": 500, "pr": 42, "head_sha": head,
                "base_ref": BASE,
                "check_run": {"id": 7001, "name": "CodeQL", "conclusion": "failure",
                    "app_id": if aggregate == "foreign_app" { 99 } else { 57789 },
                    "annotations_count": 1},
                "annotations": [{
                    "path": path,
                    "blob_href": format!("https://github.com/orbit/test/blob/{blob_head}/{path}"),
                    "start_line": 1, "end_line": 1,
                    "annotation_level": if aggregate == "no_failure" { "warning" } else { "failure" },
                    "title": "Contract violation",
                    "message": "src/contract must read fixed\nit reads broken",
                }],
            }))
        }
        _ => None,
    }
}

pub(super) fn perform_ci_repair(
    host: &DeliveryHost,
    input: &Value,
) -> Result<Value, DispatchError> {
    let mode = host.scripted.lock().unwrap()["test_ci_repair"]["mode"]
        .as_str()
        .unwrap()
        .to_string();
    host.scripted_inputs
        .lock()
        .unwrap()
        .insert("test_ci_repair".into(), input.clone());
    let error = input["error_message"].as_str().unwrap();
    assert!(is_check_repair(error), "{mode}: {error}");
    let report = error
        .split("before writing retry, write ")
        .nth(1)
        .and_then(|tail| tail.split_whitespace().next())
        .unwrap_or(".orbit/tmp/baseline-scope.json");
    let head = git(&host.repo, &["rev-parse", "HEAD"]);
    fs::write(host.repo.join("src/contract"), "fixed\n").unwrap();
    let mut paths = vec![
        json!({"path":"src/contract","reason":"fix the source contract named by the evidenced CI job"}),
    ];
    if mode == "unrelated" {
        fs::write(host.repo.join("unrelated.txt"), "unrelated work\n").unwrap();
    } else if mode == "workflow" {
        fs::create_dir_all(host.repo.join(".github/workflows")).unwrap();
        fs::write(host.repo.join(".github/workflows/ci.yml"), "weakened\n").unwrap();
        paths.push(json!({"path":".github/workflows/ci.yml","reason":"bypass check"}));
    }
    fs::create_dir_all(host.repo.join(".orbit/tmp")).unwrap();
    if mode != "missing" {
        fs::write(
            host.repo.join(report),
            serde_json::to_vec(&json!({
                "schema_version":1, "run_id":input["run_id"],
                "failed_head":if mode == "stale" { "stale" } else { &head },
                "cause":"shared-base src/contract violates the required source contract",
                "paths":paths,
            }))
            .unwrap(),
        )
        .unwrap();
    }
    if mode == "remote_moved" {
        // Another writer advances the published branch while recovery edits locally.
        let foreign = git(
            &host.repo,
            &["commit-tree", "HEAD^{tree}", "-p", &head, "-m", "foreign"],
        );
        git(
            &host.repo,
            &["push", "origin", &format!("{foreign}:refs/heads/{BRANCH}")],
        );
    }
    Ok(json!({"summary":"repair the shared-base source contract"}))
}

#[test]
fn docs_only_auto_fix_repairs_shared_source_and_watches_published_green_head() {
    isolated(
        "auto_fix::docs_only_auto_fix_repairs_shared_source_and_watches_published_green_head",
        |sandbox| {
            for mode in cases(
                "unrelated repair landed reviewed landed_reviewed budget review_required missing stale workflow remote_moved validation_failed",
            ) {
                in_fresh_row(sandbox, |row| {
                    let (fx, host) = docs_only_pr(row);
                    assert_eq!(
                        git(
                            &fx.repo,
                            &["diff", "--name-only", &fx.base_sha, &fx.candidate]
                        ),
                        "README.md",
                    );
                    host.script("test_ci_repair", json!({"mode":mode}));
                    host.script(
                        "test_checkpoint",
                        json!({"phase":"commit","task_id":TASK_ID,"commit_sha":fx.candidate}),
                    );
                    host.require_commands(&[if mode == "validation_failed" {
                        "exit 23"
                    } else {
                        "test \"$(cat src/contract)\" = fixed"
                    }]);
                    fx.script_checks(&["source_contract"]);
                    if matches!(mode, "landed" | "landed_reviewed") {
                        fx.commit_on_base("src/contract", "fixed\n");
                    }
                    let mut input = auto_fix_input(&fx);
                    if matches!(mode, "reviewed" | "landed_reviewed") {
                        input["reviewed_head_sha"] = json!(fx.candidate);
                        input["re_review_on_conflict"] = json!(true);
                        host.script(
                            "test_checkpoint",
                            json!({"phase":"commit","reviewed_head_sha":fx.candidate}),
                        );
                    }
                    if mode == "review_required" {
                        fx.script_checks(&["review_required"]);
                    }
                    if mode == "budget" {
                        seed_spent(&host, 1, PrAutoFixBlocker::FailedChecks, RUN_ID);
                    }
                    let outcome = run_repair_job(&host, input);
                    if matches!(mode, "repair" | "landed") {
                        let result = outcome.expect(mode);
                        assert!(result.success, "{result:?}");
                        let head = fx.head();
                        assert_ne!(head, fx.candidate);
                        assert_eq!(fx.remote_tip(BRANCH), head);
                        assert_eq!(result.pipeline["complete_pr"]["merge"]["head_sha"], head);
                        assert_eq!(
                            result.pipeline["complete_pr"]["merge"]["reason"],
                            "auto_fix_mergeable"
                        );
                        if mode == "repair" {
                            let evidence = host
                                .validation_log(TASK_ID, &format!("auto-fix/{RUN_ID}/{head}.json"));
                            assert_eq!(evidence["changed_paths"], json!(["src/contract"]));
                            assert_eq!(evidence["failed_head"], fx.candidate);
                            assert_eq!(evidence["repair_head"], head);
                            assert_eq!(host.widenings().last().unwrap().3, vec!["src/contract"]);
                        } else {
                            let evidence = host.validation_log(
                                TASK_ID,
                                &format!("auto-fix/{RUN_ID}/landed-{}.json", fx.candidate),
                            );
                            assert_eq!(evidence["failed_head"], fx.candidate);
                            assert!(
                                !host
                                    .scripted_inputs
                                    .lock()
                                    .unwrap()
                                    .contains_key("test_ci_repair"),
                                "landed repair is adopted without duplicate agent edits"
                            );
                            assert_eq!(
                                git(&fx.repo, &["diff", "--name-only", BASE, "HEAD"]),
                                "README.md"
                            );
                        }
                        let log =
                            host.validation_log(TASK_ID, &format!("validation/{RUN_ID}/0.json"));
                        assert_eq!(log["tested_head"], head);
                    } else if matches!(mode, "reviewed" | "landed_reviewed") {
                        let result = outcome.expect(mode);
                        assert!(result.success, "{result:?}");
                        assert_eq!(result.pipeline["complete_pr"]["re_review_required"], true);
                        assert_eq!(
                            fx.remote_tip(BRANCH),
                            fx.candidate,
                            "review precedes publication"
                        );
                        assert_ne!(fx.head(), fx.candidate);
                    } else {
                        let error = match outcome {
                            Ok(result) => {
                                assert!(!result.success, "{mode}: {result:?}");
                                result.message.unwrap_or_default()
                            }
                            Err(error) => error.to_string(),
                        };
                        let expected = match mode {
                            "unrelated" => "unexplained_paths",
                            "remote_moved" => "moved away",
                            "validation_failed" => "23",
                            "budget" => "auto_fix_budget_exhausted",
                            "review_required" => "REVIEW_REQUIRED",
                            _ => "CI repair scope",
                        };
                        assert!(error.contains(expected), "{mode}: {error}");
                        if matches!(mode, "budget" | "review_required") {
                            assert!(
                                !host
                                    .scripted_inputs
                                    .lock()
                                    .unwrap()
                                    .contains_key("test_ci_repair")
                            );
                        }
                        if mode != "remote_moved" {
                            assert_eq!(fx.remote_tip(BRANCH), fx.candidate, "{mode}");
                        }
                        if !matches!(mode, "remote_moved" | "validation_failed") {
                            assert_eq!(
                                fx.head(),
                                fx.candidate,
                                "scope refusal precedes commit: {mode}"
                            );
                        }
                    }
                    assert_eq!(
                        repair_attempts(&host).len(),
                        if mode == "budget" {
                            3
                        } else if matches!(mode, "review_required" | "landed" | "landed_reviewed") {
                            0
                        } else {
                            1
                        }
                    );
                    assert!(
                        fx.merge_requests().is_empty(),
                        "repair does not grant completion"
                    );
                    assert_eq!(host.status(TASK_ID), TaskStatus::Review);
                });
            }
        },
    );
}

// An admitted CI repair is published through the real PR executor first.
// Completion then receives its original H0 checkpoint after the base advances.
#[test]
fn published_ci_repair_conflict_refresh_keeps_authenticated_head_and_guards() {
    isolated(
        "auto_fix::published_ci_repair_conflict_refresh_keeps_authenticated_head_and_guards",
        |sandbox| {
            for case in cases(
                "refresh reviewed review_required real_conflict ordinary_stale budget validation_failed remote_moved remote_deleted wrong_base",
            ) {
                in_fresh_row(sandbox, |row| {
                    tracing::info!(case, "published CI repair conflict");
                    let fx = Fixture::new(row);
                    let host = auto_fix_resume_fixture(&fx, RUN_ID);
                    fs::write(fx.repo.join("repair.txt"), "CI repair\n").unwrap();
                    git(&fx.repo, &["add", "repair.txt"]);
                    git(&fx.repo, &["commit", "-m", "admitted CI repair"]);
                    let repaired = fx.head();
                    host.require_commands(&["test -f repair.txt"]);
                    let mut input = auto_fix_input(&fx);
                    let published = action(&host, "pr_complete", &input).unwrap();
                    assert_eq!(published["merge"]["head_sha"], repaired);
                    assert_eq!(fx.remote_tip(BRANCH), repaired);
                    let pushes = repair_pushes(&host);
                    assert_eq!(pushes.len(), 1);
                    assert_eq!(pushes[0].from_head_sha, fx.candidate);
                    assert_eq!(pushes[0].to_head_sha, repaired);
                    let failed_history = host.history(TASK_ID);

                    if case == "real_conflict" {
                        fx.commit_on_base(FEATURE, "base conflicts with candidate\n");
                    } else {
                        fx.advance_base();
                    }
                    let base = fx.remote_tip(BASE);
                    let mut checks = vec!["success"; fx.status_reads()];
                    checks.extend(["dirty", "success"]);
                    fx.script_checks(&checks);
                    if matches!(case, "reviewed" | "review_required") {
                        input["reviewed_head_sha"] = json!(repaired);
                        input["re_review_on_conflict"] = json!(case == "reviewed");
                    } else if case == "ordinary_stale" {
                        input["auto_fix"] = json!(false);
                    } else if case == "budget" {
                        seed_spent(&host, 2, PrAutoFixBlocker::Conflict, "sibling-run");
                    } else if case == "validation_failed" {
                        host.require_commands(&["exit 23"]);
                    } else if case == "remote_moved" {
                        // Validation is the deterministic rendezvous: another
                        // writer moves the remote after rebase's ownership read.
                        host.require_commands(&[&format!("repair_race_sha=$(git -c user.name=Other -c user.email=other@example.invalid commit-tree HEAD^{{tree}} -p {repaired} -m foreign) && git push origin \"$repair_race_sha:refs/heads/{BRANCH}\"")]);
                    } else if case == "remote_deleted" {
                        host.require_commands(&[&format!("git push origin :refs/heads/{BRANCH}")]);
                    } else if case == "wrong_base" {
                        fs::write(fx.forge.join("pr-base"), "main\n").unwrap();
                    }
                    let result = action(&host, "pr_complete", &input);
                    match case {
                        "refresh" => {
                            let output = result.expect("authenticated H1 must refresh automatically despite the original H0 checkpoint");
                            let refreshed = fx.head();
                            assert_ne!(refreshed, repaired);
                            assert_eq!(fx.remote_tip(BRANCH), refreshed);
                            assert_eq!(output["merge"]["head_sha"], refreshed);
                            assert_eq!(git(&fx.repo, &["show", "HEAD:repair.txt"]), "CI repair");
                            assert_eq!(
                                host.validation_log(
                                    TASK_ID,
                                    &format!("validation/{RUN_ID}/0.json")
                                )["tested_head"],
                                refreshed
                            );
                            let pushes = repair_pushes(&host);
                            assert_eq!(pushes.len(), 2);
                            assert_eq!(pushes[1].from_head_sha, repaired);
                            assert_eq!(pushes[1].to_head_sha, refreshed);
                            assert_eq!(pushes[1].blocker, PrAutoFixBlocker::Conflict);
                        }
                        "reviewed" => {
                            let output = result.expect(
                                "reviewed authenticated H1 must rebase for independent re-review",
                            );
                            assert_eq!(output["re_review_required"], true);
                            assert_eq!(output["rebased"]["head_sha_before"], repaired);
                            assert_eq!(output["rebased"]["remote_sha_before"], repaired);
                            assert_eq!(output["rebased"]["head_sha"], fx.head());
                            assert_ne!(fx.head(), repaired);
                            assert_eq!(
                                host.validation_log(
                                    TASK_ID,
                                    &format!("validation/{RUN_ID}/0.json")
                                )["tested_head"],
                                fx.head()
                            );
                            assert_eq!(
                                fx.remote_tip(BRANCH),
                                repaired,
                                "re-review precedes publication"
                            );
                        }
                        "real_conflict" => {
                            let conflict = completion_conflict(result);
                            assert_eq!(conflict.target_base_sha, base);
                            assert_eq!(
                                fx.remote_tip(BRANCH),
                                repaired,
                                "conflict recovery retains the owned H1 lease"
                            );
                            assert_eq!(conflict.conflicting_paths, vec![FEATURE.to_string()]);
                            let recovered_fx = Fixture {
                                candidate: repaired.clone(),
                                ..fx
                            };
                            let recovered =
                                recover_completion(&recovered_fx, &host, &conflict, "resolved\n");
                            let mut checks = vec!["success"; recovered_fx.status_reads()];
                            checks.extend(["dirty", "success"]);
                            recovered_fx.script_checks(&checks);
                            let output = action(&host, "pr_complete", &input).expect(
                                "certified conflict recovery refreshes the same authenticated head",
                            );
                            assert_eq!(output["merge"]["head_sha"], recovered);
                            assert_eq!(recovered_fx.remote_tip(BRANCH), recovered);
                            assert_eq!(
                                host.validation_log(
                                    TASK_ID,
                                    &format!("validation/{RUN_ID}/0.json")
                                )["tested_head"],
                                recovered
                            );
                            assert!(recovered_fx.merge_requests().is_empty());
                            assert_eq!(recovered_fx.remote_tip(BASE), base);
                            assert_eq!(host.status(TASK_ID), TaskStatus::Review);
                            assert_eq!(input["published_head_sha"], pushes[0].from_head_sha);
                            assert_eq!(repair_attempts(&host).len(), 2);
                            assert!(host.history(TASK_ID).starts_with(&failed_history));
                            let pushes = repair_pushes(&host);
                            assert_eq!(pushes.last().unwrap().from_head_sha, repaired);
                            assert_eq!(pushes.last().unwrap().to_head_sha, recovered);
                            return;
                        }
                        _ => {
                            let error = result.expect_err(case).to_string();
                            let expected = match case {
                                "budget" => "auto_fix_budget_exhausted",
                                "review_required" => "review_gate_stale",
                                "validation_failed" => "23",
                                "remote_moved" | "remote_deleted" => "push_lease_lost",
                                _ => "delivery_evidence_stale",
                            };
                            assert!(error.contains(expected), "{case}: {error}");
                            if case == "remote_moved" {
                                assert_ne!(
                                    fx.remote_tip(BRANCH),
                                    fx.head(),
                                    "concurrent publication survives"
                                );
                                assert_ne!(fx.remote_tip(BRANCH), repaired);
                            } else if case == "remote_deleted" {
                                assert!(
                                    git(
                                        &fx.repo,
                                        &["ls-remote", "origin", &format!("refs/heads/{BRANCH}")]
                                    )
                                    .is_empty(),
                                    "deleted branch is never recreated"
                                );
                            } else {
                                assert_eq!(
                                    fx.remote_tip(BRANCH),
                                    repaired,
                                    "refusal leaves H1 intact"
                                );
                            }
                        }
                    }
                    assert_eq!(
                        repair_attempts(&host).len(),
                        match case {
                            "budget" => 3,
                            "ordinary_stale" | "wrong_base" | "review_required" => 1,
                            _ => 2,
                        },
                        "cumulative admission budget: {case}"
                    );
                    assert_eq!(
                        input["published_head_sha"], fx.candidate,
                        "original provenance remains immutable"
                    );
                    assert!(
                        host.history(TASK_ID).starts_with(&failed_history),
                        "failed/admitted history is retained"
                    );
                    assert!(
                        fx.merge_requests().is_empty(),
                        "repair-only authority never merges"
                    );
                    assert_eq!(host.status(TASK_ID), TaskStatus::Review);
                    assert_eq!(fx.remote_tip(BASE), base, "repair does not modify the base");
                });
            }
        },
    );
}

#[test]
fn auto_fix_anchors_emitted_failure_after_printed_commands_and_before_cleanup() {
    isolated(
        "auto_fix::auto_fix_anchors_emitted_failure_after_printed_commands_and_before_cleanup",
        |sandbox| {
            let actual =
                "step.sh: line 8: scripts/ci-download-verified.sh: No such file or directory";
            let signature =
                "step.sh: line <n>: scripts/ci-download-verified.sh: no such file or directory";
            let display = "##[group]Run preflight\n  emit \"::error title=insufficient runner disk::need more space\"\n  error[E0123]: printed compiler text\n  lost communication with the server\n##[endgroup]\n";
            for case in cases(
                "raw raw_timestamp_tabs raw_timestamp_tabbed_error raw_timestamp_tabbed_compiler columns successful_step unclosed orphan truncated incomplete selected wrong_step",
            ) {
                in_fresh_row(sandbox, |row| {
                    let fx = Fixture::new(row);
                    let host = DeliveryHost::new(&fx.repo, TaskStatus::Review);
                    action(
                        &host,
                        "pr_open",
                        &fx.open_input(&fx.candidate, &fx.base_sha),
                    )
                    .unwrap();
                    host.script(
                        "ci.commit_jobs",
                        json!({"jobs":[{
                            "name":"test", "workflow":"CI", "conclusion":"failure",
                            "run_id":10, "job_id":11, "failed_step":"Install cargo-nextest"
                        }]}),
                    );
                    let (diagnostic, expected) = match case {
                        "raw_timestamp_tabbed_error" => {
                            ("error:\tactual\tinvariant", "error: actual invariant")
                        }
                        "raw_timestamp_tabbed_compiler" => (
                            "error[E0433]:\tcannot find module\t`real`",
                            "error[e0433]: cannot find module `real`",
                        ),
                        _ => (actual, signature),
                    };
                    let mut log = format!(
                        "{display}preflight passed\n{display}{diagnostic}\n##[error]Process completed with exit code 127.\n{}",
                        "post-job cleanup completed\n".repeat(1000)
                    );
                    let raw_timestamp = case.starts_with("raw_timestamp_");
                    if raw_timestamp {
                        let timestamp = "2026-10-15T02:42:56Z";
                        log = format!(
                            "{timestamp} ##[group]Run preflight\n{timestamp}   printf 'key\tvalue\tdetail\\n'\n{timestamp}   key\tvalue\t##[endgroup]\n{timestamp}   error[E0123]: printed compiler text\n{timestamp} ##[endgroup]\n{timestamp} {diagnostic}\n{timestamp} ##[error]Process completed with exit code 127."
                        );
                    }
                    if matches!(case, "columns" | "successful_step" | "wrong_step") {
                        let step = if case == "wrong_step" {
                            "successful setup"
                        } else {
                            "Install cargo-nextest"
                        };
                        log = log
                            .lines()
                            .enumerate()
                            .map(|(index, line)| {
                                let step = if case == "successful_step"
                                    && index < display.lines().count() + 1
                                {
                                    "successful setup"
                                } else {
                                    step
                                };
                                format!(
                                    "test\t{step}\t2026-10-15T02:42:56Z \x1b[31m{line}\x1b[0m\n"
                                )
                            })
                            .collect();
                    } else if case == "unclosed" {
                        log = "##[group]Run verify\n  ##[error]printed failure\n".into();
                    } else if case == "orphan" {
                        log = "  ##[error]printed failure\n##[endgroup]\n##[error]Process completed with exit code 127.\n".into();
                    }
                    let source = if raw_timestamp {
                        orbit_tools::github_cli::SOURCE_JOB_API_LOG
                    } else {
                        orbit_tools::github_cli::SOURCE_RUN_LOG
                    };
                    let mut response = json!({"log":log, "truncated":false,
                        "source_complete":true, "source":source});
                    if matches!(case, "truncated" | "selected" | "wrong_step") {
                        response["truncated"] = json!(true);
                    }
                    if case == "selected" {
                        response["log"] = json!("cleanup only");
                        response["diagnostic"] = json!(format!(
                            "{display}{actual}\n##[error]Process completed with exit code 127."
                        ));
                    } else if case == "wrong_step" || raw_timestamp {
                        response["diagnostic"] = response["log"].clone();
                    } else if case == "incomplete" {
                        response["source_complete"] = json!(false);
                    }
                    host.script("ci.job_log", response);
                    fx.script_checks(&["failure"]);
                    let input = auto_fix_input(&fx);
                    let error = action(&host, "pr_complete", &input)
                        .unwrap_err()
                        .to_string();
                    let actionable = !matches!(
                        case,
                        "unclosed" | "orphan" | "truncated" | "incomplete" | "wrong_step"
                    );
                    assert_eq!(is_check_repair(&error), actionable, "{case}: {error}");
                    if actionable {
                        assert!(
                            error.contains(&format!("with signature `{expected}`")),
                            "{case}: {error}"
                        );
                        assert!(
                            error.contains(diagnostic),
                            "{case}: repair evidence must retain the emitted failure: {error}"
                        );
                    } else {
                        assert!(
                            error.contains("no emitted diagnostic")
                                || error.contains("could not be read"),
                            "{case}: {error}"
                        );
                        assert!(repair_attempts(&host).is_empty());
                    }
                    assert!(error.len() < 12000, "repair evidence remains bounded");
                    assert!(fx.merge_requests().is_empty());
                    assert_eq!(host.status(TASK_ID), TaskStatus::Review);
                });
            }
        },
    );
}

/// Oversized evidence must reach public repair admission without granting
/// authority to partial, ambiguous or differently attributed regions.
#[test]
fn auto_fix_preserves_complete_oversized_failure_regions() {
    isolated(
        "auto_fix::auto_fix_preserves_complete_oversized_failure_regions",
        |sandbox| {
            for case in cases(
                "columns unattributed raw runner_regions source_incomplete source_missing command_incomplete selection_incomplete selection_missing source_unknown provenance_missing step_missing multiple_steps job_conflict step_conflict mixed_labels omitted_conflict ambiguous_raw tail_only",
            ) {
                writeln!(std::io::stderr(), "oversized-failure-region-case: {case}").unwrap();
                in_fresh_row(sandbox, |row| {
                    let (fx, host) = opened_review(row);
                    let label = if case == "unattributed" {
                        "UNKNOWN STEP"
                    } else {
                        "Tests"
                    };
                    let columns = format!("test\t{label}\t");
                    let prefix = if matches!(case, "raw" | "ambiguous_raw") {
                        ""
                    } else {
                        &columns
                    };
                    let padding =
                        format!("{prefix}PASS [ 0.001s] suite passing_test\n").repeat(6000);
                    let failure = if case == "runner_regions" {
                        "error[E0433]: cannot find module `real_failure`"
                    } else {
                        "FAIL [ 0.123s] suite real_failure"
                    };
                    let mut raw = format!(
                        "{prefix}##[group]Run cargo nextest run\n{prefix}##[endgroup]\n{padding}{prefix}{failure}\n{prefix}thread 'real_failure' panicked at tests/sample.rs:12:3:\n{prefix}assertion failed: operation completed\n{padding}{prefix}##[error]Process completed with exit code 100.\n"
                    );
                    if case == "omitted_conflict" {
                        raw = raw.replacen("test\tTests\tPASS", "test\tOther\tPASS", 1);
                    } else if case == "ambiguous_raw" {
                        raw.insert_str(0, "##[group]Run earlier\n##[endgroup]\n##[error]Process completed with exit code 1.\n");
                    }
                    assert!(
                        raw.len() > 262_144,
                        "exercise the full-command retention cap"
                    );
                    let mut collector =
                        orbit_tools::github_cli::StreamedLogCollector::new(65_536, 40);
                    collector.push(raw.as_bytes());
                    let log = collector.finish();
                    assert!(log.source_complete && log.truncated && log.diagnostic.is_none());
                    let regions = log.failure_regions.unwrap();
                    assert!(regions["text"].as_str().unwrap().len() <= 65_536);
                    assert_eq!(
                        regions["command_complete"],
                        !matches!(case, "omitted_conflict" | "ambiguous_raw")
                    );
                    let source = if prefix.is_empty() {
                        "job_api_log"
                    } else {
                        "run_log"
                    };
                    let mut response = json!({"log":log.text, "truncated":true,
                        "source_complete":true, "source":source, "failure_regions":regions});
                    let mut job = json!({"name":"test", "workflow":"CI", "conclusion":"failure",
                        "run_id":10, "job_id":11, "failed_step":"Tests", "failed_steps":["Tests"]});
                    match case {
                        "source_incomplete" => response["source_complete"] = json!(false),
                        "source_missing" => {
                            response.as_object_mut().unwrap().remove("source_complete");
                        }
                        "command_incomplete" => {
                            response["failure_regions"]["command_complete"] = json!(false)
                        }
                        "selection_incomplete" => {
                            response["failure_regions"]["selection_complete"] = json!(false)
                        }
                        "selection_missing" => {
                            response["failure_regions"]
                                .as_object_mut()
                                .unwrap()
                                .remove("selection_complete");
                        }
                        "source_unknown" => response["source"] = json!("unknown"),
                        "provenance_missing" => {
                            response.as_object_mut().unwrap().remove("source");
                        }
                        "step_missing" => {
                            job.as_object_mut().unwrap().remove("failed_steps");
                        }
                        "multiple_steps" => job["failed_steps"] = json!(["Tests", "Other"]),
                        "job_conflict" => {
                            let text = response["failure_regions"]["text"].as_str().unwrap();
                            response["failure_regions"]["text"] =
                                json!(text.replace("test\t", "other\t"));
                        }
                        "step_conflict" | "mixed_labels" => {
                            let text = response["failure_regions"]["text"].as_str().unwrap();
                            response["failure_regions"]["text"] = json!(text.replacen(
                                "test\tTests\t",
                                if case == "step_conflict" {
                                    "test\tOther\t"
                                } else {
                                    "test\tUNKNOWN STEP\t"
                                },
                                1
                            ));
                        }
                        "tail_only" => {
                            response.as_object_mut().unwrap().remove("failure_regions");
                        }
                        _ => {}
                    }
                    // Exercise the real private adapter and streaming read as
                    // part of public completion, not just a scripted projection.
                    if matches!(
                        case,
                        "columns" | "unattributed" | "runner_regions" | "multiple_steps"
                    ) {
                        fs::write(fx.forge.join("failed-command-log"), raw).unwrap();
                        let mut steps = vec![json!({"name":"Tests", "conclusion":"failure"})];
                        if case == "multiple_steps" {
                            steps.push(json!({"name":"Other", "conclusion":"failure"}));
                        }
                        fs::write(
                            fx.forge.join("failed-command-jobs"),
                            serde_json::to_vec(&json!({"jobs":[{
                                "id":11, "name":"test", "conclusion":"failure", "steps":steps
                            }]}))
                            .unwrap(),
                        )
                        .unwrap();
                    } else {
                        host.script("ci.job_log", response);
                        host.script("ci.commit_jobs", json!({"jobs":[job]}));
                    }
                    fx.script_checks(&["failure"]);
                    let input = auto_fix_input(&fx);
                    let error = action(&host, "pr_complete", &input)
                        .unwrap_err()
                        .to_string();
                    let actionable =
                        matches!(case, "columns" | "unattributed" | "raw" | "runner_regions");
                    assert_eq!(is_check_repair(&error), actionable, "{case}: {error}");
                    assert_eq!(
                        repair_attempts(&host).len(),
                        usize::from(actionable),
                        "{case}: invalid evidence spends no repair authority"
                    );
                    if actionable {
                        let signature = if case == "runner_regions" {
                            "error[e0433]: cannot find module `real_failure`"
                        } else {
                            "fail [ <n>.<n>s] suite real_failure"
                        };
                        assert!(
                            error.contains(&format!("with signature `{signature}`")),
                            "{case}: stable failure identity: {error}"
                        );
                        assert!(
                            error.contains(failure),
                            "{case}: preserve the middle failure: {error}"
                        );
                        assert!(
                            error.contains("assertion failed: operation completed"),
                            "{case}: preserve captured output: {error}"
                        );
                    } else {
                        assert!(error.contains("could not be read"), "{case}: {error}");
                    }
                    assert!(error.len() < 12000, "repair evidence stays bounded");
                    assert!(fx.merge_requests().is_empty());
                    assert!(!fx.forge_calls().iter().any(|call| call == "run rerun"));
                    assert_eq!(host.status(TASK_ID), TaskStatus::Review);
                    assert_eq!(fx.head(), fx.candidate);
                    assert_eq!(fx.remote_tip(BASE), fx.base_sha);
                });
                test_env::record_scenario_pass(&format!("oversized_failure_regions/{case}"));
            }
        },
    );
}

/// An unrelated optional failure must not end the authenticated required wait.
#[test]
fn auto_fix_watches_required_running_beside_optional_failure() {
    isolated(
        "auto_fix::auto_fix_watches_required_running_beside_optional_failure",
        |sandbox| {
            let (fx, host) = opened_review(sandbox);
            host.script("ci.prerequisite_evidence", json!({"evidence":null}));
            fx.script_checks(&["mixed_pending", "mixed_pending", "mixed_success"]);
            let mut input = auto_fix_input(&fx);
            input["max_wait_seconds"] = json!(15);
            let clock = std::rc::Rc::new(std::cell::Cell::new(1_000_u64));
            let reads = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let now = clock.clone();
            let elapsed = clock.clone();
            let captured = reads.clone();
            let completed = orbit_engine::test_support::with_prerequisite_clock(
                move || now.get(),
                move |label, timeout| captured.borrow_mut().push((label.to_string(), timeout)),
                || {
                    orbit_engine::test_support::with_vcs_retry_wait(
                        move |duration| elapsed.set(elapsed.get() + duration.as_millis() as u64),
                        || action(&host, "pr_complete", &input),
                    )
                },
            )
            .expect("optional failure must not stop the current required running watcher");
            assert_eq!(
                *reads.borrow(),
                [
                    ("PR status".into(), 15_000),
                    ("PR required checks".into(), 15_000),
                    ("PR status".into(), 10_000),
                    ("PR required checks".into(), 10_000),
                    ("PR status".into(), 5_000)
                ],
                "all observations consume the original absolute completion deadline"
            );
            assert_eq!(completed["merge"]["reason"], "auto_fix_mergeable");
            assert_eq!(completed["merge"]["head_sha"], fx.candidate);
            assert_eq!(completed["merge"]["waited_seconds"], 10);
            assert_eq!(fx.status_reads(), 3, "watch until required success");
            assert!(repair_attempts(&host).is_empty());
            assert!(
                fx.merge_requests().is_empty(),
                "watch authority never merges"
            );
            assert_eq!(fx.head(), fx.candidate, "no source repair");
            assert_eq!(fx.remote_tip(BRANCH), fx.candidate);
            assert_eq!(fx.remote_tip(BASE), fx.base_sha);
            assert_eq!(host.status(TASK_ID), TaskStatus::Review);
            assert!(!fx.forge_calls().iter().any(|call| call == "run rerun"));
        },
    );
}

#[test]
fn auto_fix_mixed_failure_wait_preserves_recovery_and_refusals() {
    isolated(
        "auto_fix::auto_fix_mixed_failure_wait_preserves_recovery_and_refusals",
        |sandbox| {
            for case in cases(
                "required_failure optional_pending administrative review foreign_head foreign_commit missing_head unknown_required unclassified truncated malformed auth timeout changed_head changed_base",
            ) {
                in_fresh_row(sandbox, |row| {
                    let (fx, host) = opened_review(row);
                    host.script("ci.prerequisite_evidence", json!({"evidence":null}));
                    fx.script_checks(&["mixed_pending", "mixed_success"]);
                    let mut input = auto_fix_input(&fx);
                    let mut policy = json!({"headRefOid":fx.candidate,"commits":{"nodes":[{"commit":{
                        "oid":fx.candidate,"statusCheckRollup":{"contexts":{
                            "pageInfo":{"hasNextPage":false},"nodes":[
                                {"name":"test","isRequired":true,"status":"IN_PROGRESS","conclusion":""},
                                {"name":"coverage","isRequired":false,"status":"COMPLETED","conclusion":"FAILURE"}
                            ]}
                        }
                    }}]}});
                    let mut expected = match case {
                        "optional_pending" | "administrative" => "only optional checks failed",
                        "review" => "review is required (REVIEW_REQUIRED)",
                        "foreign_head" | "foreign_commit" | "missing_head" => "current head",
                        "unknown_required" => "unreadable",
                        "unclassified" => "did not say whether",
                        "truncated" => "truncated",
                        "malformed" => "no pull request",
                        "auth" => "HTTP 401",
                        _ => "auto_fix_stopped",
                    };
                    match case {
                        "required_failure" => {
                            fx.script_checks(&["mixed_failure"]);
                            let nodes = &mut policy["commits"]["nodes"][0]["commit"]["statusCheckRollup"]
                                ["contexts"]["nodes"];
                            nodes[0]["status"] = json!("COMPLETED");
                            nodes[0]["conclusion"] = json!("FAILURE");

                            host.script("ci.commit_jobs", json!({"jobs":[{"name":"test","conclusion":"failure","run_id":10,"job_id":11,"failed_step":"compile"}]}));
                            host.script(
                                "ci.job_log",
                                json!({"log":"error[E0433]: cannot find module `broken`"}),
                            );
                        }
                        "optional_pending" => {
                            policy["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"]
                                ["nodes"][0]["isRequired"] = json!(false)
                        }
                        "administrative" => {
                            let check = &mut policy["commits"]["nodes"][0]["commit"]["statusCheckRollup"]
                                ["contexts"]["nodes"][0];
                            check["status"] = json!("COMPLETED");
                            check["conclusion"] = json!("SUCCESS");
                        }
                        "review" => {
                            fs::write(fx.forge.join("review-decision"), "REVIEW_REQUIRED").unwrap()
                        }
                        "foreign_head" => policy["headRefOid"] = json!("foreign"),
                        "foreign_commit" => {
                            policy["commits"]["nodes"][0]["commit"]["oid"] = json!("foreign")
                        }
                        "missing_head" => {
                            policy.as_object_mut().unwrap().remove("headRefOid");
                        }
                        "unknown_required" => {
                            policy["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"]
                                ["nodes"][0]["status"] = json!("UNKNOWN_STATE")
                        }
                        "unclassified" => {
                            policy["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"]["nodes"][0].as_object_mut().unwrap().remove("isRequired");
                        }
                        "truncated" => {
                            policy["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"]
                                ["pageInfo"]["hasNextPage"] = json!(true)
                        }
                        "malformed" => policy = Value::Null,
                        "auth" => fs::write(fx.forge.join("required-auth"), "").unwrap(),
                        "timeout" => {
                            fx.script_checks(&["mixed_pending"]);
                            input["max_wait_seconds"] = json!(10);
                            expected = "timed out after 10s";
                        }
                        "changed_head" => {
                            fs::write(fx.forge.join("pr-head"), "other-branch").unwrap();
                            expected = "delivery_evidence_stale";
                        }
                        "changed_base" => {
                            fs::write(fx.forge.join("pr-base"), "main").unwrap();
                            expected = "delivery_evidence_stale";
                        }
                        _ => unreachable!(),
                    }
                    fs::write(
                        fx.forge.join("required-policy"),
                        serde_json::to_vec(&json!({"data":{"repository":{"pullRequest":policy}}}))
                            .unwrap(),
                    )
                    .unwrap();
                    let error = action(&host, "pr_complete", &input)
                        .expect_err(case)
                        .to_string();
                    if case == "required_failure" {
                        assert!(
                            is_check_repair(&error),
                            "eligible required failure: {error}"
                        );
                    } else {
                        assert!(error.contains(expected), "{case}: {error}");
                    }
                    assert_eq!(
                        repair_attempts(&host).len(),
                        usize::from(case == "required_failure"),
                        "{case}: only required failure admits recovery"
                    );
                    assert_eq!(
                        fx.status_reads(),
                        if case == "timeout" { 2 } else { 1 },
                        "{case}: no broad pending-first wait"
                    );
                    assert!(fx.merge_requests().is_empty(), "{case}: no merge authority");
                    assert!(
                        !fx.forge_calls().iter().any(|call| call == "run rerun"),
                        "{case}: no infrastructure rerun"
                    );
                    assert_eq!(fx.head(), fx.candidate);
                    assert_eq!(fx.remote_tip(BRANCH), fx.candidate);
                    assert_eq!(fx.remote_tip(BASE), fx.base_sha);
                    assert_eq!(host.status(TASK_ID), TaskStatus::Review);
                });
            }
        },
    );
}

/// [ORB-16185] A required provider-app aggregate fails on the exact head
/// with source annotations while its analysis job passes and no workflow job
/// answers it. Its authenticated annotations admit the one same-PR repair,
/// retained before admission, which is validated, published and watched to a
/// green head; the mixed row proves the aggregate is not hidden by a failed
/// workflow job beside it. Annotations that are foreign, stale, unbound to
/// the policy read's identity, non-source or outside the checkout refuse the
/// repair and leave the candidate and its budget untouched.
#[test]
fn auto_fix_repairs_required_aggregate_from_authenticated_source_annotations() {
    isolated(
        "auto_fix::auto_fix_repairs_required_aggregate_from_authenticated_source_annotations",
        |sandbox| {
            for aggregate in cases(
                "valid mixed foreign_app missing_identity stale workflow_path escape no_failure",
            ) {
                in_fresh_row(sandbox, |row| {
                    let (fx, host) = docs_only_pr(row);
                    host.script(
                        "test_ci_repair",
                        json!({"mode":"repair","aggregate":aggregate}),
                    );
                    host.script(
                        "test_checkpoint",
                        json!({"phase":"commit","task_id":TASK_ID,"commit_sha":fx.candidate}),
                    );
                    host.require_commands(&["test \"$(cat src/contract)\" = fixed"]);
                    fx.script_checks(&[if aggregate == "mixed" {
                        "source_aggregate_mixed"
                    } else {
                        "source_aggregate"
                    }]);
                    let input = auto_fix_input(&fx);
                    let outcome = run_repair_job(&host, input);
                    let repair = host
                        .scripted_inputs
                        .lock()
                        .unwrap()
                        .get("test_ci_repair")
                        .cloned();
                    let attempts = repair_attempts(&host);
                    if matches!(aggregate, "valid" | "mixed") {
                        let result = outcome.expect(aggregate);
                        assert!(result.success, "{aggregate}: {result:?}");
                        let head = fx.head();
                        assert_ne!(head, fx.candidate);
                        assert_eq!(
                            fx.remote_tip(BRANCH),
                            head,
                            "the same PR carries the repair"
                        );
                        assert_eq!(
                            result.pipeline["complete_pr"]["merge"]["reason"],
                            "auto_fix_mergeable"
                        );
                        let handed = repair.expect("one repair runs")["error_message"]
                            .as_str()
                            .unwrap()
                            .to_string();
                        assert!(
                            handed.contains("required check 'CodeQL' (check run 7001 of app 57789")
                                && handed.contains("- src/contract:1 [Contract violation]: src/contract must read fixed it reads broken"),
                            "the repair receives the authenticated source diagnostics: {handed}"
                        );
                        assert_eq!(
                            handed.contains("'CI / test' failed in step `source contract`"),
                            aggregate == "mixed",
                            "a failed workflow job is evidenced beside the aggregate: {handed}"
                        );
                        let retained = host.validation_log(
                            TASK_ID,
                            &format!("auto-fix/{RUN_ID}/check-annotations-{}.json", fx.candidate),
                        );
                        assert_eq!(retained["failed_head"], fx.candidate);
                        assert_eq!(retained["checks"][0]["check_run"]["id"], 7001);
                        assert_eq!(retained["checks"][0]["head_sha"], fx.candidate);
                        let validated =
                            host.validation_log(TASK_ID, &format!("validation/{RUN_ID}/0.json"));
                        assert_eq!(
                            validated["tested_head"], head,
                            "fresh validation of the repair"
                        );
                        assert_eq!(attempts.len(), 1, "one repair from the shared budget");
                        assert_eq!(attempts[0].head_sha, fx.candidate);
                    } else {
                        let error = match outcome {
                            Ok(result) => {
                                assert!(!result.success, "{aggregate}: {result:?}");
                                result.message.unwrap_or_default()
                            }
                            Err(error) => error.to_string(),
                        };
                        let expected = match aggregate {
                            "foreign_app" => "not bound to this pull request",
                            "missing_identity" => "no check run or app identity",
                            "stale" => "does not link that file at head",
                            "workflow_path" | "escape" => "names no repository source path",
                            _ => "no failure annotation naming a source location",
                        };
                        assert!(
                            error.contains("auto_fix_stopped") && error.contains(expected),
                            "{aggregate}: {error}"
                        );
                        assert!(repair.is_none(), "{aggregate}: no repair agent runs");
                        assert!(attempts.is_empty(), "{aggregate}: no budget is spent");
                        assert_eq!(fx.remote_tip(BRANCH), fx.candidate, "{aggregate}");
                        assert_eq!(fx.head(), fx.candidate, "{aggregate}");
                    }
                    assert!(
                        fx.merge_requests().is_empty(),
                        "repair does not grant completion"
                    );
                    assert_eq!(host.status(TASK_ID), TaskStatus::Review);
                });
            }
        },
    );
}

/// One commit's workflow run, as `gh api` serves `ci.commit_jobs` and
/// `gh run view --log-failed` serves `ci.job_log`. Absent unless this test
/// plants it, so other rows keep the substitute's ordinary refusal.
fn plant_commit_ci(fx: &Fixture, sha: &str, run_id: u64, conclusion: &str, log: &str) {
    plant_paired_commit_ci(fx, sha, run_id, &[("test", conclusion)], log);
}

/// One push run whose jobs are `(name, conclusion)`; an empty conclusion is
/// a job still running. One log serves every job in the run. Callers that
/// compare diagnostics plant the same text on each commit whose failed job
/// is read.
fn plant_paired_commit_ci(fx: &Fixture, sha: &str, run_id: u64, jobs: &[(&str, &str)], log: &str) {
    let dir = fx.forge.join("commit-ci").join(sha);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("run_id"), run_id.to_string()).unwrap();
    fs::write(
        dir.join("runs.json"),
        serde_json::to_vec(&json!({
            "total_count": 1,
            "workflow_runs": [{
                "id": run_id,
                "name": "CI",
                "workflow_id": 1,
                "event": "push",
                "head_sha": sha,
                "head_branch": BASE
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        dir.join("jobs.json"),
        serde_json::to_vec(&jobs_listing(run_id, jobs)).unwrap(),
    )
    .unwrap();
    fs::write(dir.join("log"), log).unwrap();
}

fn jobs_listing(run_id: u64, jobs: &[(&str, &str)]) -> Value {
    let listed = jobs
        .iter()
        .enumerate()
        .map(|(index, (name, conclusion))| {
            let id = run_id + 1 + u64::try_from(index).unwrap();
            let open = conclusion.is_empty();
            json!({
                "id": id,
                "name": name,
                "status": if open { "in_progress" } else { "completed" },
                "conclusion": if open { Value::Null } else { json!(conclusion) },
                "steps": [{
                    "name": "compile",
                    "conclusion": if *conclusion == "failure" { "failure" } else { "success" }
                }]
            })
        })
        .collect::<Vec<_>>();
    json!({"total_count": jobs.len(), "jobs": listed})
}

/// From PR status read `poll` on, the planted run on `sha` lists `jobs`, or
/// refuses the listing when `jobs` is `None`.
fn stage_commit_jobs(fx: &Fixture, sha: &str, poll: usize, jobs: Option<&[(&str, &str)]>) {
    let dir = fx.forge.join("commit-ci").join(sha);
    let run_id: u64 = fs::read_to_string(dir.join("run_id"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let answer = match jobs {
        Some(jobs) => serde_json::to_vec(&jobs_listing(run_id, jobs)).unwrap(),
        None => b"unreadable".to_vec(),
    };
    fs::write(dir.join(format!("jobs-from-{poll}.json")), answer).unwrap();
}

/// Land one more commit on the base, as another delivery would.
fn land_on_base(fx: &Fixture, label: &str) -> String {
    fx.git(&["checkout", BASE]);
    fs::write(fx.repo.join("base.txt"), format!("{label}\n")).unwrap();
    fx.git(&["add", "base.txt"]);
    fx.git(&["commit", "-m", label]);
    fx.git(&["push", "origin", BASE]);
    fx.git(&["checkout", BRANCH]);
    fx.local_tip(BASE)
}

/// Two required failures on one pull request. `test` passed on the fork point
/// and on the current tip, so it is the candidate's. `lint` is the sibling:
/// known-red on the tip, also candidate-owned, or green on the tip with no
/// fork-point job.
fn paired_required_base_case(row: &std::path::Path, case: &str) {
    let (fx, host) = opened_review(row);
    let shared = "error[E0433]: cannot find module `broken_base`\n";
    let own = "error[E0433]: cannot find module `different_failure`\n";
    let known_red = case.starts_with("known_red_");
    plant_paired_commit_ci(
        &fx,
        &fx.candidate,
        11,
        &[("test", "failure"), ("lint", "failure")],
        if known_red { shared } else { own },
    );
    let fork_point = fx.local_tip(BASE);
    // The sibling has no successful fork-point job unless every failure is
    // candidate-owned. Known-red does not consult that job. The green-tip
    // sibling must not become candidate-owned because of it.
    let fork_jobs: &[(&str, &str)] = if case.starts_with("candidate_owned_pair") {
        &[("test", "success"), ("lint", "success")]
    } else {
        &[("test", "success")]
    };
    plant_paired_commit_ci(&fx, &fork_point, 51, fork_jobs, "");
    fx.advance_base();
    let tip = fx.local_tip(BASE);
    let tip_jobs: &[(&str, &str)] = if known_red {
        &[("test", "success"), ("lint", "failure")]
    } else {
        &[("test", "success"), ("lint", "success")]
    };
    plant_paired_commit_ci(&fx, &tip, 31, tip_jobs, if known_red { shared } else { "" });
    if case.ends_with("_reserved") {
        reserve_base_update(&host, &fx.candidate, &tip);
    }
    let rollup = if matches!(
        case,
        "known_red_before_candidate" | "candidate_beside_green_tip"
    ) {
        "paired_failure_reversed"
    } else {
        "paired_failure"
    };
    fx.script_checks(&[rollup]);
    let mut input = auto_fix_input(&fx);
    input["max_wait_seconds"] = json!(15);
    input["poll_interval_seconds"] = json!(5);
    let result = super::complete_with_logical_clock(&host, &input);
    let history = host.history(TASK_ID);
    let attempts = pr_auto_fix_attempts(&history, PR_NUMBER);
    let base_red = history
        .iter()
        .filter(|entry| entry.event == "pr_auto_fix_base_red")
        .count();
    let refreshed = history
        .iter()
        .any(|entry| entry.event == "pr_auto_fix_base_refresh");
    if known_red {
        let error = result
            .expect_err("known-red sibling admits no repair")
            .to_string();
        assert!(error.contains("base_red"), "{case}: {error}");
        assert!(
            attempts.is_empty(),
            "{case}: known-red base spends no repair attempt"
        );
        assert_eq!(base_red, 1, "{case}: one pr_auto_fix_base_red record");
        assert!(!refreshed, "{case}: known-red base records no refresh");
        assert_eq!(fx.remote_tip(BRANCH), fx.candidate);
    } else {
        let error = result
            .expect_err("candidate-owned checks take one repair")
            .to_string();
        assert!(is_check_repair(&error), "{case}: {error}");
        assert_eq!(attempts.len(), 1, "{case}: one ordinary repair");
        assert_eq!(base_red, 0, "{case}: no known-red record");
        assert!(
            !refreshed,
            "{case}: candidate-owned checks record no refresh"
        );
        assert_eq!(fx.remote_tip(BRANCH), fx.candidate);
    }
    assert!(
        fx.merge_requests().is_empty(),
        "{case}: auto-fix alone never merges"
    );
}

/// A failed PR cut before a base fix must refresh before repair admission,
/// even without historical CI on its fork point. Current-base failures with
/// the same diagnostic hold instead; a distinct diagnostic still repairs. A
/// failure whose same job passed on the fork point is the candidate's: a
/// green tip does not spend the base update on it, and a base update already
/// reserved for the pull request does not stop its repair. A candidate-owned
/// check beside a known-red sibling records that red base and admits no
/// repair, in either rollup order. Two candidate-owned checks, and a
/// candidate-owned check beside a green tip with no successful fork-point
/// job, still take one ordinary repair and record no base refresh.
#[test]
#[allow(
    clippy::print_stderr,
    reason = "isolated regression rows report their identity and elapsed time"
)]
fn auto_fix_checks_current_base_before_spending_a_repair() {
    isolated(
        "auto_fix::auto_fix_checks_current_base_before_spending_a_repair",
        |sandbox| {
            for case in cases(
                "fixed_base red_tip candidate already_current red_tip_older_green red_tip_foreign_green non_push_green incomplete_green fixed_base_older_red fixed_base_exhausted_repairs fixed_base_reserved candidate_green_tip candidate_green_tip_reserved known_red_beside_candidate known_red_before_candidate candidate_owned_pair candidate_owned_pair_reserved candidate_beside_green_tip auto_fix_does_not_repair_while_the_moved_base_ci_is_open",
            ) {
                in_fresh_row(sandbox, |row| {
                    eprintln!("scenario: {case}");
                    let row_started = std::time::Instant::now();
                    if case == "auto_fix_does_not_repair_while_the_moved_base_ci_is_open" {
                        open_moved_base_ci(row);
                        eprintln!(
                            "scenario: {case} row elapsed {:.3} s",
                            row_started.elapsed().as_secs_f64()
                        );
                        return;
                    }
                    if matches!(
                        case,
                        "known_red_beside_candidate"
                            | "known_red_before_candidate"
                            | "candidate_owned_pair"
                            | "candidate_owned_pair_reserved"
                            | "candidate_beside_green_tip"
                    ) {
                        paired_required_base_case(row, case);
                        eprintln!(
                            "scenario: {case} row elapsed {:.3} s",
                            row_started.elapsed().as_secs_f64()
                        );
                        return;
                    }
                    let (fx, host) = opened_review(row);
                    let shared = "error[E0433]: cannot find module `broken_base`\n";
                    let own = "error[E0433]: cannot find module `different_failure`\n";
                    let candidate_only = case.starts_with("candidate_green_tip");
                    plant_commit_ci(
                        &fx,
                        &fx.candidate,
                        11,
                        "failure",
                        if candidate_only { own } else { shared },
                    );
                    if candidate_only {
                        // The fork point's own push run passed the same job,
                        // so the head's failure is the candidate's change.
                        let fork_point = fx.local_tip(BASE);
                        plant_commit_ci(&fx, &fork_point, 51, "success", "");
                    }
                    if case != "already_current" {
                        fx.advance_base();
                    }
                    let tip = fx.local_tip(BASE);
                    plant_commit_ci(
                        &fx,
                        &tip,
                        31,
                        if case.starts_with("red_tip") || case == "candidate" {
                            "failure"
                        } else {
                            "success"
                        },
                        if case == "candidate" { own } else { shared },
                    );
                    let runs_path = fx.forge.join("commit-ci").join(&tip).join("runs.json");
                    let mut runs: Value =
                        serde_json::from_slice(&fs::read(&runs_path).unwrap()).unwrap();
                    match case {
                        "red_tip_older_green"
                        | "fixed_base_older_red"
                        | "red_tip_foreign_green" => {
                            // No jobs exist for this distractor: consulting it
                            // instead of the latest base push cannot pass.
                            let mut distractor = runs["workflow_runs"][0].clone();
                            distractor["id"] = json!(if case == "red_tip_foreign_green" {
                                41
                            } else {
                                21
                            });
                            if case == "red_tip_foreign_green" {
                                distractor["event"] = json!("pull_request");
                            }
                            runs["workflow_runs"]
                                .as_array_mut()
                                .unwrap()
                                .push(distractor);
                            runs["total_count"] = json!(2);
                        }
                        "non_push_green" => {
                            runs["workflow_runs"][0]["event"] = json!("pull_request")
                        }
                        "incomplete_green" => runs["total_count"] = json!(101),
                        _ => {}
                    }
                    fs::write(runs_path, serde_json::to_vec(&runs).unwrap()).unwrap();
                    if case == "fixed_base_exhausted_repairs" {
                        seed_spent(&host, 1, PrAutoFixBlocker::FailedChecks, RUN_ID);
                    }
                    if case.ends_with("_reserved") {
                        reserve_base_update(&host, &fx.candidate, &tip);
                    }
                    fx.script_checks(
                        if case.starts_with("fixed_base") && case != "fixed_base_reserved" {
                            &["failure", "success"]
                        } else {
                            &["failure"]
                        },
                    );
                    let mut input = auto_fix_input(&fx);
                    input["max_wait_seconds"] = json!(15);
                    input["poll_interval_seconds"] = json!(5);
                    let result = super::complete_with_logical_clock(&host, &input);
                    let attempts = repair_attempts(&host);
                    match case {
                        "fixed_base" | "fixed_base_older_red" | "fixed_base_exhausted_repairs" => {
                            let output = result.expect("refresh stale branch without repair");
                            assert!(
                                attempts.len()
                                    == if case == "fixed_base_exhausted_repairs" {
                                        PR_AUTO_FIX_BUDGET as usize
                                    } else {
                                        0
                                    },
                                "a base refresh spends no repair attempt"
                            );
                            let refreshed = fx.remote_tip(BRANCH);
                            assert_ne!(refreshed, fx.candidate);
                            assert_eq!(git(&fx.repo, &["merge-base", &tip, &refreshed]), tip);
                            assert_eq!(output["merge"]["head_sha"], refreshed);
                            assert!(
                                host.history(TASK_ID)
                                    .iter()
                                    .any(|entry| entry.event == "pr_auto_fix_base_refresh")
                            );
                        }
                        "red_tip" | "red_tip_older_green" | "red_tip_foreign_green" => {
                            let error = result.expect_err("current base is known red").to_string();
                            assert!(error.contains("base_red"), "{error}");
                            assert!(
                                attempts.is_empty(),
                                "known-red base spends no repair attempt"
                            );
                            assert_eq!(
                                host.history(TASK_ID)
                                    .iter()
                                    .filter(|entry| entry.event == "pr_auto_fix_base_red")
                                    .count(),
                                1,
                                "repeated known-red polls keep a single decision record"
                            );
                            assert_eq!(fx.remote_tip(BRANCH), fx.candidate);
                        }
                        "fixed_base_reserved" => {
                            let error = result.expect_err("one cumulative base update").to_string();
                            assert!(error.contains("base update budget is spent"), "{error}");
                            assert!(attempts.is_empty());
                            assert_eq!(fx.remote_tip(BRANCH), fx.candidate);
                        }
                        _ => {
                            let error = result.expect_err("candidate needs repair").to_string();
                            assert!(is_check_repair(&error), "{case}: {error}");
                            assert_eq!(attempts.len(), 1, "{case}: ordinary repair admission");
                            assert_eq!(fx.remote_tip(BRANCH), fx.candidate);
                            assert!(
                                !host
                                    .history(TASK_ID)
                                    .iter()
                                    .any(|entry| entry.event == "pr_auto_fix_base_refresh"),
                                "{case}: a candidate failure spends no base refresh"
                            );
                        }
                    }
                    assert!(
                        fx.merge_requests().is_empty(),
                        "auto-fix alone never merges"
                    );
                    eprintln!(
                        "scenario: {case} row elapsed {:.3} s",
                        row_started.elapsed().as_secs_f64()
                    );
                });
            }
        },
    );
}

/// The advanced base's same job, still running: GitHub leaves `conclusion`
/// null until the job finishes. No log is planted, because an open job has
/// none and a hold must not read one.
fn plant_open_commit_ci(fx: &Fixture, sha: &str, run_id: u64) {
    let dir = fx.forge.join("commit-ci").join(sha);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("run_id"), run_id.to_string()).unwrap();
    fs::write(
        dir.join("runs.json"),
        format!(r#"{{"workflow_runs":[{{"id":{run_id},"name":"CI"}}]}}"#),
    )
    .unwrap();
    fs::write(
        dir.join("jobs.json"),
        format!(
            r#"{{"jobs":[{{"id":{},"name":"test","status":"in_progress","conclusion":null,"steps":[]}}]}}"#,
            run_id + 1
        ),
    )
    .unwrap();
}

/// An unmoved base whose own job fails the same way spends no repair attempt
/// and stops base-red at the completion deadline, and an optional check that
/// also fails beside it, with no job of its own, does not end that hold. A
/// failure the base does not fail that way is still admitted from the same
/// budget. A provider-side head need not be fetched to recognize matching
/// current-base diagnostics from authenticated CI jobs.
#[test]
fn auto_fix_waits_for_an_unmoved_red_base_and_repairs_a_candidate_failure() {
    isolated(
        "auto_fix::auto_fix_waits_for_an_unmoved_red_base_and_repairs_a_candidate_failure",
        |sandbox| {
            for case in cases("red_base red_base_beside_optional candidate unfetched_head") {
                in_fresh_row(sandbox, |row| {
                    let (fx, host) = opened_review(row);
                    let shared = "error[E0433]: cannot find module `broken_base`\n\
                        ##[error]Process completed with exit code 101.\n";
                    let candidate_only = "error[E0433]: cannot find module `candidate_only`\n\
                        ##[error]Process completed with exit code 101.\n";
                    // A provider-side branch update: the PR head exists only
                    // on the remote, so no local git command can read it.
                    let head = if case == "unfetched_head" {
                        let remote = fx.forge.join("remote.git");
                        let remote = path_str(&remote);
                        let tree = format!("{}^{{tree}}", fx.candidate);
                        let update = git(
                            &fx.repo,
                            &[
                                // The bare remote has no configured identity.
                                "-c",
                                "user.name=Provider",
                                "-c",
                                "user.email=provider@example.invalid",
                                "--git-dir",
                                remote,
                                "commit-tree",
                                &tree,
                                "-p",
                                &fx.candidate,
                                "-m",
                                "provider update",
                            ],
                        );
                        git(
                            &fx.repo,
                            &[
                                "--git-dir",
                                remote,
                                "update-ref",
                                &format!("refs/heads/{BRANCH}"),
                                &update,
                            ],
                        );
                        update
                    } else {
                        fx.candidate.clone()
                    };
                    plant_commit_ci(
                        &fx,
                        &head,
                        11,
                        "failure",
                        if case == "candidate" {
                            candidate_only
                        } else {
                            shared
                        },
                    );
                    plant_commit_ci(
                        &fx,
                        &fx.base_sha,
                        21,
                        if case == "candidate" {
                            "success"
                        } else {
                            "failure"
                        },
                        shared,
                    );
                    // The optional `coverage` failure rides in the same rollup
                    // as the required `test` failure and has no job to read.
                    let rollup = if case == "red_base_beside_optional" {
                        "mixed_failure"
                    } else {
                        "failure"
                    };
                    fx.script_checks(&[rollup]);
                    let mut input = fx.complete_ungated_input();
                    if case == "unfetched_head" {
                        // No published pin, so the provider's head is the one
                        // the watcher repairs.
                        input.as_object_mut().unwrap().remove("published_head_sha");
                    }
                    input["auto_fix"] = json!(true);
                    input["completion"] = json!("review");
                    input["max_wait_seconds"] = json!(15);
                    input["poll_interval_seconds"] = json!(5);
                    let reads_before = fx.status_reads();
                    let error = action(&host, "pr_complete", &input)
                        .expect_err(case)
                        .to_string();
                    let attempts = repair_attempts(&host);
                    let polls = fx.status_reads() - reads_before;
                    if case != "candidate" {
                        assert!(
                            error.contains("auto_fix_stopped")
                                && error.contains("base_red")
                                && error.contains(&fx.base_sha)
                                && !error.contains("timed out"),
                            "{case}: {error}"
                        );
                        assert!(
                            !is_check_repair(&error),
                            "{case}: no repair handoff: {error}"
                        );
                        assert!(attempts.is_empty(), "{case}: no attempt is spent");
                        assert!(
                            polls >= 2,
                            "{case}: waits for the base before the deadline ({polls} polls)"
                        );
                    } else {
                        assert!(is_check_repair(&error), "{case}: {error}");
                        let evidence = "candidate_only";
                        assert!(
                            error.contains(&format!(
                                "error[e0433]: cannot find module `{evidence}`"
                            )),
                            "{case}: the head signature is the repair evidence: {error}"
                        );
                        assert_eq!(attempts.len(), 1, "{case}: one repair is admitted");
                        assert_eq!(attempts[0].budget, PR_AUTO_FIX_BUDGET);
                        assert_eq!(attempts[0].head_sha, head, "{case}");
                        assert_eq!(polls, 1, "{case}: a repairable failure does not wait");
                    }
                    assert!(fx.merge_requests().is_empty(), "{case}: no merge");
                    assert_eq!(fx.head(), fx.candidate, "{case}");
                    assert_eq!(fx.remote_tip(BASE), fx.base_sha, "{case}");
                    assert_eq!(host.status(TASK_ID), TaskStatus::Review);
                });
            }
        },
    );
}

/// A base that moved to a commit whose same job has not concluded is still
/// the inherited failure. That poll admits no repair: the attempt waits until
/// the tip job finishes, when a success can refresh and a failure can be
/// judged on its own. Labelled row of
/// `auto_fix_checks_current_base_before_spending_a_repair`.
fn open_moved_base_ci(row: &std::path::Path) {
    let case = "auto_fix_does_not_repair_while_the_moved_base_ci_is_open";
    let (fx, host) = opened_review(row);
    let shared = "error[E0433]: cannot find module `broken_base`\n\
                    ##[error]Process completed with exit code 101.\n";
    plant_commit_ci(&fx, &fx.candidate, 11, "failure", shared);
    plant_commit_ci(&fx, &fx.base_sha, 21, "failure", shared);
    fx.advance_base();
    let tip = fx.local_tip(BASE);
    assert_ne!(tip, fx.base_sha, "the base commit moved");
    assert_eq!(fx.remote_tip(BASE), tip);
    plant_open_commit_ci(&fx, &tip, 31);
    fx.script_checks(&["failure"]);
    let mut input = auto_fix_input(&fx);
    input["max_wait_seconds"] = json!(15);
    input["poll_interval_seconds"] = json!(5);
    let reads_before = fx.status_reads();
    // The prerequisite clock and the poll wait advance together, so
    // the hold reaches its deadline without a wall-clock sleep.
    let error = super::complete_with_logical_clock(&host, &input)
        .expect_err(case)
        .to_string();
    let attempts = repair_attempts(&host);
    assert!(
        error.contains("auto_fix_stopped")
            && error.contains("base_red")
            && error.contains(&tip)
            && error.contains("has not concluded"),
        "{case}: {error}"
    );
    assert!(
        !is_check_repair(&error),
        "{case}: no repair handoff: {error}"
    );
    assert!(
        attempts.is_empty(),
        "{case}: no repair attempt is spent: {attempts:?}"
    );
    assert!(
        fx.status_reads() - reads_before >= 2,
        "{case}: the open tip is held across polls, not repaired on the first"
    );
    assert!(fx.merge_requests().is_empty(), "{case}: no merge");
    assert_eq!(fx.head(), fx.candidate, "{case}");
    assert_eq!(fx.remote_tip(BASE), tip, "{case}");
    assert_eq!(host.status(TASK_ID), TaskStatus::Review, "{case}");
}

/// [ORB-18370] A required failure the base already fails the same way is
/// never handed to a repair agent while the moved base cannot prove it
/// fixed. A tip whose CI skipped the job (a docs-only commit) is read back
/// along the base's first-parent history: a green commit refreshes the PR,
/// a same-signature red one holds as known-red, and nothing within the bound
/// holds. A pending tip holds, keeps holding through a poll whose tip or
/// fork-point jobs cannot be read (the 2026-10-25 admissions), and refreshes
/// once its jobs pass. None of these spends a repair attempt.
#[test]
#[allow(
    clippy::print_stderr,
    reason = "isolated regression rows report their identity and elapsed time"
)]
fn auto_fix_holds_an_inherited_failure_until_the_base_proves_it() {
    isolated(
        "auto_fix::auto_fix_holds_an_inherited_failure_until_the_base_proves_it",
        |sandbox| {
            for case in cases(
                "skipped_tip_green_parent skipped_tip_red_parent skipped_tip_unproven pending_tip_unreadable_tip pending_tip_unreadable_fork_point",
            ) {
                in_fresh_row(sandbox, |row| {
                    eprintln!("scenario: {case}");
                    let row_started = std::time::Instant::now();
                    inherited_failure_case(row, case);
                    eprintln!(
                        "scenario: {case} row elapsed {:.3} s",
                        row_started.elapsed().as_secs_f64()
                    );
                });
            }
        },
    );
}

fn inherited_failure_case(row: &std::path::Path, case: &str) {
    let (fx, host) = opened_review(row);
    let shared = "error[E0433]: cannot find module `broken_base`\n";
    let pair = case.starts_with("pending_tip");
    let failing: &[(&str, &str)] = if pair {
        &[("test", "failure"), ("lint", "failure")]
    } else {
        &[("test", "failure")]
    };
    plant_paired_commit_ci(&fx, &fx.candidate, 11, failing, shared);
    // The fork point fails the same way: the head's failure is inherited.
    let fork_point = fx.local_tip(BASE);
    plant_paired_commit_ci(&fx, &fork_point, 21, failing, shared);
    let mut green = None;
    let tip = match case {
        "skipped_tip_green_parent" => {
            let fixed = land_on_base(&fx, "fix the base");
            plant_commit_ci(&fx, &fixed, 31, "success", "");
            green = Some(fixed);
            let tip = land_on_base(&fx, "docs only");
            plant_commit_ci(&fx, &tip, 41, "skipped", "");
            tip
        }
        "skipped_tip_red_parent" => {
            let tip = land_on_base(&fx, "docs only");
            plant_commit_ci(&fx, &tip, 41, "skipped", "");
            tip
        }
        "skipped_tip_unproven" => {
            // More commits with no run of the job than the read covers.
            for commit in 0..8 {
                land_on_base(&fx, &format!("no ci {commit}"));
            }
            let tip = land_on_base(&fx, "docs only");
            plant_commit_ci(&fx, &tip, 41, "skipped", "");
            tip
        }
        _ => {
            let tip = land_on_base(&fx, "fix the base");
            plant_paired_commit_ci(&fx, &tip, 31, &[("test", ""), ("lint", "")], "");
            if case == "pending_tip_unreadable_tip" {
                stage_commit_jobs(&fx, &tip, 2, None);
            } else {
                stage_commit_jobs(&fx, &fork_point, 2, None);
                stage_commit_jobs(&fx, &fork_point, 3, Some(failing));
            }
            stage_commit_jobs(
                &fx,
                &tip,
                3,
                Some(&[("test", "success"), ("lint", "success")]),
            );
            green = Some(tip.clone());
            tip
        }
    };
    let rollup = if pair { "paired_failure" } else { "failure" };
    // The refreshed head passes on the status read after the refresh: the
    // first poll for a skipped tip, the third for a pending one.
    match (&green, pair) {
        (Some(_), true) => fx.script_checks(&[rollup, rollup, rollup, "success"]),
        (Some(_), false) => fx.script_checks(&[rollup, "success"]),
        (None, _) => fx.script_checks(&[rollup]),
    }
    let mut input = auto_fix_input(&fx);
    input["max_wait_seconds"] = json!(30);
    input["poll_interval_seconds"] = json!(5);
    let reads_before = fx.status_reads();
    let result = super::complete_with_logical_clock(&host, &input);
    let polls = fx.status_reads() - reads_before;
    let history = host.history(TASK_ID);
    let attempts = repair_attempts(&host);
    assert!(
        attempts.is_empty(),
        "{case}: an inherited failure spends no repair attempt: {attempts:?} {result:?}"
    );
    let base_red = history
        .iter()
        .filter(|entry| entry.event == "pr_auto_fix_base_red")
        .filter_map(|entry| serde_json::from_str::<Value>(entry.note.as_deref()?).ok())
        .collect::<Vec<_>>();
    let refresh = history
        .iter()
        .find(|entry| entry.event == "pr_auto_fix_base_refresh")
        .and_then(|entry| serde_json::from_str::<Value>(entry.note.as_deref()?).ok());
    if let Some(green) = green {
        let output = result.unwrap_or_else(|error| panic!("{case}: refreshed: {error}"));
        let refresh = refresh.unwrap_or_else(|| panic!("{case}: a base refresh is recorded"));
        let refreshed = fx.remote_tip(BRANCH);
        assert_ne!(refreshed, fx.candidate, "{case}");
        assert_eq!(output["merge"]["head_sha"], refreshed, "{case}");
        assert_eq!(
            git(&fx.repo, &["merge-base", &tip, &refreshed]),
            tip,
            "{case}: the refresh rebases onto the reserved tip"
        );
        for check in refresh["checks"].as_array().unwrap() {
            assert_eq!(
                check["base_commit"], green,
                "{case}: the refresh names the newest commit whose job passed: {refresh}"
            );
        }
        if pair {
            assert!(
                base_red.len() >= 2 && polls >= 4,
                "{case}: the pending tip held across polls ({polls}) before it passed: {base_red:?}"
            );
        }
    } else {
        let error = result
            .expect_err("an unproven base admits no repair")
            .to_string();
        assert!(
            error.contains("auto_fix_stopped") && error.contains("base_red"),
            "{case}: {error}"
        );
        assert!(!is_check_repair(&error), "{case}: {error}");
        assert!(refresh.is_none(), "{case}: no green base, no refresh");
        if case == "skipped_tip_red_parent" {
            assert!(
                base_red.iter().any(|note| note["path"] == "known_red_base"
                    && note["checks"][0]["base_commit"] == fork_point.as_str()),
                "{case}: the newest red commit decides: {base_red:?}"
            );
        } else {
            assert!(
                error.contains("did not run to a result") && polls >= 2,
                "{case}: held across polls ({polls}): {error}"
            );
        }
        assert_eq!(fx.remote_tip(BRANCH), fx.candidate, "{case}");
    }
    assert!(fx.merge_requests().is_empty(), "{case}: no merge");
}

/// A second required failure must reach both repair admission and the job's
/// independent final-recovery observation, even when the earlier error is stale.
#[test]
fn auto_fix_settles_staggered_required_failures_before_repair_and_final_recovery() {
    isolated(
        "auto_fix::auto_fix_settles_staggered_required_failures_before_repair_and_final_recovery",
        |sandbox| {
            for case in ["settled", "timeout", "policy_ahead"] {
                in_fresh_row(sandbox, |row| {
                    let (fx, host) = opened_review(row);
                    plant_paired_commit_ci(
                        &fx,
                        &fx.candidate,
                        11,
                        &[("test", "failure"), ("lint", "failure")],
                        "unused",
                    );
                    let logs = fx.forge.join("commit-ci").join(&fx.candidate);
                    fs::write(logs.join("log-12"), "error: candidate compile failure\n").unwrap();
                    fs::write(logs.join("log-13"), "error: candidate lint failure\n").unwrap();
                    plant_commit_ci(&fx, &fx.base_sha, 21, "success", "passed");
                    fx.script_checks(if case == "settled" {
                        &["paired_pending", "paired_pending", "paired_failure"]
                    } else {
                        &["paired_pending"]
                    });
                    if case == "policy_ahead" {
                        // The sibling concludes between the rollup and policy
                        // reads; its newer failure must join repair evidence.
                        fs::write(fx.forge.join("required-policy"), serde_json::to_vec(&json!({
                            "data":{"repository":{"pullRequest":{"headRefOid":fx.candidate,
                                "commits":{"nodes":[{"commit":{"oid":fx.candidate,
                                    "statusCheckRollup":{"contexts":{"pageInfo":{"hasNextPage":false},"nodes":[
                                        {"name":"test","isRequired":true,"status":"COMPLETED","conclusion":"FAILURE"},
                                        {"name":"lint","isRequired":true,"status":"COMPLETED","conclusion":"FAILURE"}
                                    ]}}
                                }}]}}
                            }}
                        })).unwrap()).unwrap();
                    }
                    let mut input = auto_fix_input(&fx);
                    input["max_wait_seconds"] = json!(15);
                    let error = super::complete_with_logical_clock(&host, &input)
                        .expect_err("failed checks cannot complete")
                        .to_string();
                    let attempts = repair_attempts(&host);
                    assert_eq!(
                        fx.status_reads(),
                        if case == "policy_ahead" { 1 } else { 3 },
                        "repair requires settled policy: {error}"
                    );
                    if case != "timeout" {
                        assert_eq!(attempts.len(), 1);
                        for diagnostic in ["candidate compile failure", "candidate lint failure"] {
                            assert!(
                                error.contains(diagnostic),
                                "both signatures reach repair: {error}"
                            );
                        }
                        for name in ["CI / test", "CI / lint"] {
                            assert!(error.contains(name), "both jobs reach repair: {error}");
                        }
                    } else {
                        assert!(attempts.is_empty(), "timeout spends no repair attempt");
                        assert!(error.contains("timed out"), "{error}");
                    }

                    // Final recovery is a fresh public job-boundary read, rather
                    // than a copy of the duplicate-admission error, which has no job logs.
                    host.script(
                        "test_checkpoint",
                        json!({"workspace_path":fx.repo,"base_sha":fx.base_sha}),
                    );
                    host.script(
                        "test_final_decision",
                        json!({"decision":"escalate",
                        "diagnosis":"inspect both failures", "human_action":"repair"}),
                    );
                    let mut job = job_asset(json!([
                        {"id":"setup","target":"activity:checkpoint"},
                        {"id":"complete_pr","target":"activity:pr_complete","default_input":input}
                    ]));
                    job.final_recovery_activity = Some("decide".into());
                    let outcome = run_job(
                        &host,
                        job,
                        &[
                            ("checkpoint", "test_checkpoint"),
                            ("pr_complete", "pr_complete"),
                            ("decide", "test_final_decision"),
                        ],
                        RUN_ID,
                        json!({"task_ids": [TASK_ID]}),
                        None,
                    );
                    assert!(!outcome.is_ok_and(|outcome| outcome.success));
                    let recovery = host
                        .scripted_inputs
                        .lock()
                        .unwrap()
                        .get("test_final_decision")
                        .cloned();
                    if case != "timeout" {
                        let recovery =
                            recovery.expect("settled checks authorize a complete diagnosis");
                        let activity =
                            load_activity_asset(
                                &fs::read_to_string(workspace_root().join(
                                    "crates/orbit-core/assets/activities/final_recovery.yaml",
                                ))
                                .unwrap(),
                            )
                            .unwrap();
                        let schema =
                            jsonschema::JSONSchema::compile(&activity.spec.input_schema_json)
                                .unwrap();
                        // The deterministic stand-in adds its dispatch step id;
                        // an agent activity receives the original envelope.
                        let mut agent_input = recovery.clone();
                        agent_input.as_object_mut().unwrap().remove("step_id");
                        if let Err(errors) = schema.validate(&agent_input) {
                            panic!(
                                "canonical recovery input: {:?}",
                                errors.map(|error| error.to_string()).collect::<Vec<_>>()
                            );
                        }
                        let evidence = &recovery["required_check_failures"];
                        assert_eq!(evidence["head_sha"], fx.candidate);
                        let failures = evidence["failed_checks"].as_array().unwrap();
                        assert_eq!(failures.len(), 2);
                        for (name, signature) in [
                            ("test", "candidate compile failure"),
                            ("lint", "candidate lint failure"),
                        ] {
                            let failure =
                                failures.iter().find(|entry| entry["name"] == name).unwrap();
                            assert!(failure["evidence"].as_str().unwrap().contains(signature));
                        }
                    } else {
                        assert!(
                            recovery.is_none(),
                            "partial checks cannot authorize a base-only diagnosis"
                        );
                    }
                    assert!(fx.merge_requests().is_empty());
                    assert_eq!(fx.head(), fx.candidate);
                });
            }
        },
    );
}
