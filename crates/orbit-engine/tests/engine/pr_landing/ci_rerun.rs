//! First failures through the real producer, authenticated adapter and owner.
use super::*;
use orbit_types::workflow::{CI_RERUN_RESERVED_EVENT, CiRerunPlan, CiRerunRoot};

const WORKFLOW: &str = r#"name: CI
on: [push, pull_request]
jobs:
  leaf:
    name: test
    runs-on: ubuntu-latest
    steps:
      - name: Tests
        run: echo tests
  waiter:
    name: lint
    runs-on: ubuntu-latest
    steps:
      - name: Check out producer
        uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1
        with:
          ref: ${{ github.event.pull_request.base.sha || github.sha }}
          path: .workflow
          persist-credentials: false
      - name: Wait
        working-directory: .workflow
        shell: /usr/bin/python3 -I {0}
        env:
          ORBIT_DEPENDENCY_EXECUTION: isolated-source-v1
          ORBIT_DEPENDENCY_JOB_NAME: lint
          ORBIT_DEPENDENCY_STEP: Wait
          ORBIT_DEPENDENCY_PRODUCER: scripts/ci-await-jobs.py
          ORBIT_DEPENDENCY_PRODUCER_SHA: ${{ github.event.pull_request.base.sha || github.sha }}
        run: python3 scripts/ci-await-jobs.py --job test
      - name: Own check
        if: always()
        run: echo own check
  independent:
    name: independent
    runs-on: ubuntu-latest
    steps:
      - name: Tests
        run: echo independent check
"#;

fn fixture(row: &Path) -> (Fixture, DeliveryHost, Value) {
    fixture_with_source(row, WORKFLOW)
}

fn fixture_with_source(row: &Path, workflow: &str) -> (Fixture, DeliveryHost, Value) {
    let mut fx = Fixture::new(row);
    let helper = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/ci-await-jobs.py"),
    )
    .unwrap();
    fx.commit_files_on_base(&[
        (".github/workflows/retry.yml", workflow),
        ("scripts/ci-await-jobs.py", &helper),
        ("src/stable", "stable\n"),
    ]);
    fx.git(&["rebase", BASE]);
    fx.git(&["push", "--force-with-lease", "origin", BRANCH]);
    fx.candidate = fx.head();
    fx.base_sha = fx.git(&["rev-parse", BASE]);
    let host = DeliveryHost::new(&fx.repo, TaskStatus::Review);
    open_for_completion(&fx, &host);
    let tree = fx.git(&["rev-parse", &format!("{}^{{tree}}", fx.candidate)]);
    let merge = fx.git(&[
        "commit-tree",
        &tree,
        "-p",
        &fx.base_sha,
        "-p",
        &fx.candidate,
        "-m",
        "event merge",
    ]);
    let config = json!({"head":fx.candidate,"base":fx.base_sha,"merge":merge,"workspace":fx.repo});
    fs::write(
        fx.forge.join("retry-config.json"),
        serde_json::to_vec(&config).unwrap(),
    )
    .unwrap();
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/engine/pr_landing/ci_rerun_gh.py"),
        fx.forge.join("retry-gh.py"),
    )
    .unwrap();
    let mut producer = Command::new("python3");
    producer
        .args([
            "-I",
            path_str(&fx.repo.join("scripts/ci-await-jobs.py")),
            "--job",
            "test",
            "--timeout-seconds",
            "5",
        ])
        .current_dir(&fx.repo)
        .env("GITHUB_REPOSITORY", "orbit/test")
        .env("GITHUB_RUN_ID", "700")
        .env("GITHUB_RUN_ATTEMPT", "1")
        .env("GITHUB_SHA", &merge)
        .env("GITHUB_JOB", "waiter")
        .env("ORBIT_DEPENDENCY_PRODUCER_SHA", &fx.base_sha)
        .env("ORBIT_DEPENDENCY_EXECUTION", "isolated-source-v1")
        .env(
            "GITHUB_WORKFLOW_REF",
            "orbit/test/.github/workflows/retry.yml@refs/pull/42/merge",
        )
        .env("ORBIT_DEPENDENCY_JOB_NAME", "lint")
        .env("ORBIT_DEPENDENCY_STEP", "Wait");
    let output = run_bounded_capped(&mut producer, Duration::from_secs(15), 64 * 1024).unwrap();
    assert!(
        !output.status.success(),
        "the real producer must report the dependency failure"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with("orbit-dependency-evidence:")),
        "{stdout}"
    );
    let waiter = stdout
        .lines()
        .map(|line| format!("lint\tWait\t{line}\n"))
        .collect::<String>();
    fs::write(fx.forge.join("retry-log-1001"), waiter).unwrap();
    fs::write(
        fx.forge.join("retry-log-1000"),
        "test\tTests\terror: source failure at src/stable:1:1\n",
    )
    .unwrap();
    let request = json!({"workspace_path":fx.repo,"pr":"42","head_sha":fx.candidate,"head_branch":BRANCH,"base_branch":BASE,"accepted_base":fx.base_sha,"run_id":700,"roots":[CiRerunRoot {job_id:1000,check:"CI / test".into(),classification:"flaky".into(),signature:"error: source failure at src/stable:<n>:<n>".into()}],"deadline_ms":9_999_999_999_999_u64});
    (fx, host, request)
}

#[test]
fn first_dependency_failure_reruns_before_owner_repair() {
    isolated(
        "ci_rerun::first_dependency_failure_reruns_before_owner_repair",
        |sandbox| {
            let (fx, host, request) = fixture(sandbox);
            let plan = run_private_vcs_operation("ci.rerun_plan", &request).unwrap();
            assert_eq!(plan["plan"]["failed_jobs"].as_array().unwrap().len(), 2);
            let mut input = fx.complete_ungated_input();
            input["auto_fix"] = json!(true);
            input["completion"] = json!("review");
            let result = complete_with_logical_clock(&host, &input);
            assert!(result.is_ok(), "{result:?}");
            assert_eq!(
                fx.forge_lines("retry-sends"),
                vec!["run rerun 700 --failed"]
            );
            assert!(
                host.history(TASK_ID)
                    .iter()
                    .any(|entry| entry.event == CI_RERUN_RESERVED_EVENT)
            );
            assert!(pr_auto_fix_attempts(&host.history(TASK_ID), "42").is_empty());
        },
    );
}

mod bounded;
mod diagnostic;

#[test]
fn dependency_adapter_refuses_incomplete_or_forged_evidence() {
    isolated(
        "ci_rerun::dependency_adapter_refuses_incomplete_or_forged_evidence",
        |sandbox| {
            for raw in [false, true] {
                for case in [
                    "incomplete",
                    "changed_helper",
                    "changed_workflow",
                    "wrong_app",
                    "wrong_root_check",
                    "wrong_root_job",
                    "printed_record",
                    "own_step",
                    "unread",
                    "wrong_repository",
                    "wrong_step_name",
                    "wrong_dependency_outcome",
                    "dispatch",
                    "wrong_branch",
                    "wrong_run",
                    "pending",
                    "wrong_execution",
                    "wrong_producer",
                    "wrong_dependency",
                    "truncated",
                    "foreign_event",
                    "wrong_pr",
                    "wrong_head",
                    "wrong_attempt",
                    "wrong_job",
                    "wrong_step",
                    "legacy",
                    "ambient_execution",
                    "missing_execution_contract",
                    "own_failure",
                    "independent",
                ] {
                    in_fresh_row(sandbox, |row| {
                        let (fx, host, mut request) = fixture(row);
                        let mut config: Value = serde_json::from_slice(
                            &fs::read(fx.forge.join("retry-config.json")).unwrap(),
                        )
                        .unwrap();
                        match case {
                            "incomplete" => config["incomplete"] = json!(true),
                            "changed_helper" => config["changed_helper"] = json!(true),
                            "changed_workflow" => config["changed_workflow"] = json!(true),
                            "wrong_app" => config["wrong_app"] = json!(true),
                            "wrong_root_check" => {
                                request["roots"][0]["check"] = json!("CI / other")
                            }
                            "wrong_root_job" => request["roots"][0]["job_id"] = json!(1005),
                            "own_step" => config["own_step"] = json!(true),
                            "unread" => {
                                fs::write(fx.forge.join("retry-log-1000"), b"").unwrap();
                            }
                            "dispatch" => {
                                config["run_override"] = json!({"event":"workflow_dispatch"})
                            }
                            "wrong_branch" => {
                                config["run_override"] = json!({"head_branch":"foreign"})
                            }
                            "wrong_run" => config["run_override"] = json!({"id":702}),
                            "pending" => {
                                config["run_override"] =
                                    json!({"status":"in_progress","conclusion":null})
                            }
                            "printed_record" => {
                                let path = fx.forge.join("retry-log-1001");
                                let old = fs::read_to_string(&path).unwrap();
                                let record = old
                                    .lines()
                                    .find_map(|line| {
                                        line.split_once("orbit-dependency-evidence:")
                                            .map(|(_, body)| body)
                                    })
                                    .unwrap();
                                fs::write(path,format!("lint\tWait\t##[group]Run echo 'orbit-dependency-evidence:{record}'\nlint\tWait\t##[endgroup]\nlint\tWait\terror: own failure\n")).unwrap();
                            }
                            "truncated" => {
                                fs::write(
                                    fx.forge.join("retry-log-1001"),
                                    format!("lint\tWait\t{}\n", "x".repeat(70 * 1024)),
                                )
                                .unwrap();
                            }
                            "foreign_event" => config["run_override"] = json!({"event":"push"}),
                            "independent" => {
                                config["candidate"] = json!(true);
                                fs::write(fx.forge.join("retry-log-1002"),"independent\tTests\terror: genuine candidate failure at src/feature.txt:1:1\n").unwrap();
                            }
                            _ => {
                                let path = fx.forge.join("retry-log-1001");
                                let log = fs::read_to_string(&path).unwrap();
                                let mut records = log
                                    .lines()
                                    .filter_map(|line| {
                                        line.split_once("orbit-dependency-evidence:").map(
                                            |(_, json)| {
                                                serde_json::from_str::<Value>(json).unwrap()
                                            },
                                        )
                                    })
                                    .collect::<Vec<_>>();
                                let record = &mut records[0];
                                match case {
                                    "wrong_pr" => {
                                        record["workflow_ref"] = json!(
                                            "orbit/test/.github/workflows/retry.yml@refs/pull/43/merge"
                                        )
                                    }
                                    "wrong_head" => record["head_sha"] = json!("a".repeat(40)),
                                    "wrong_attempt" => record["run_attempt"] = json!(2),
                                    "wrong_job" => record["source"]["job_id"] = json!(1005),
                                    "wrong_step" => record["source"]["step_number"] = json!(5),
                                    "legacy" => record["schema_version"] = json!(0),
                                    "ambient_execution" => {
                                        record["execution_contract"] = json!("ambient")
                                    }
                                    "missing_execution_contract" => {
                                        record
                                            .as_object_mut()
                                            .unwrap()
                                            .remove("execution_contract");
                                    }
                                    "wrong_execution" => {
                                        record["execution_sha"] = json!(fx.candidate)
                                    }
                                    "wrong_repository" => {
                                        record["repository"] = json!("foreign/repo")
                                    }
                                    "wrong_step_name" => {
                                        record["source"]["step_name"] = json!("Own failure")
                                    }
                                    "wrong_dependency_outcome" => {
                                        record["dependencies"][0]["conclusion"] = json!("success")
                                    }
                                    "wrong_producer" => {
                                        record["producer_sha"] = json!(fx.candidate)
                                    }
                                    "wrong_dependency" => {
                                        record["dependencies"][0]["job_id"] = json!(1005)
                                    }
                                    "own_failure" => record["kind"] = json!("own_failure"),
                                    _ => unreachable!(),
                                }
                                fs::write(
                                    path,
                                    format!("lint\tWait\torbit-dependency-evidence:{record}\n"),
                                )
                                .unwrap();
                            }
                        }
                        fs::write(
                            fx.forge.join("retry-config.json"),
                            serde_json::to_vec(&config).unwrap(),
                        )
                        .unwrap();
                        bounded::bounded_logs(&fx, raw);
                        assert!(
                            run_private_vcs_operation("ci.rerun_plan", &request).is_err(),
                            "{case}"
                        );
                        assert!(fx.forge_lines("retry-sends").is_empty(), "{case}");
                        assert!(pr_auto_fix_attempts(&host.history(TASK_ID), "42").is_empty());
                    });
                }
            }
        },
    );
}

#[test]
fn reserved_owner_retry_never_resends_an_unknown_reply() {
    isolated(
        "ci_rerun::reserved_owner_retry_never_resends_an_unknown_reply",
        |sandbox| {
            let (fx, host, _) = fixture(sandbox);
            let mut config: Value =
                serde_json::from_slice(&fs::read(fx.forge.join("retry-config.json")).unwrap())
                    .unwrap();
            config["reply"] = json!("unknown");
            fs::write(
                fx.forge.join("retry-config.json"),
                serde_json::to_vec(&config).unwrap(),
            )
            .unwrap();
            let mut input = fx.complete_ungated_input();
            input["auto_fix"] = json!(true);
            input["completion"] = json!("review");
            input["max_wait_seconds"] = json!(10);
            for _ in 0..2 {
                assert!(complete_with_logical_clock(&host, &input).is_err());
            }
            assert_eq!(fx.forge_lines("retry-sends").len(), 1);
            assert_eq!(
                host.history(TASK_ID)
                    .iter()
                    .filter(|entry| entry.event == CI_RERUN_RESERVED_EVENT)
                    .count(),
                1
            );
            assert!(
                !host
                    .history(TASK_ID)
                    .iter()
                    .any(|entry| entry.event == "ci_check_rerun")
            );
            assert!(pr_auto_fix_attempts(&host.history(TASK_ID), "42").is_empty());
        },
    );
}

struct ClaimedRetry<'a> {
    inner: &'a DeliveryHost,
    context: orbit_engine::HandoffLandingContext,
    steps: Mutex<Vec<orbit_engine::HandoffLandingStep>>,
}
impl orbit_engine::test_support::NullRuntimeHost for ClaimedRetry<'_> {
    fn repo_root(&self) -> Res<String> {
        Ok(self.inner.repo.to_string_lossy().into_owned())
    }
    fn handoff_landing_context(&self, _: &str) -> Res<orbit_engine::HandoffLandingContext> {
        Ok(self.context.clone())
    }
    fn record_handoff_landing(&self, update: &orbit_engine::HandoffLandingUpdate) -> Res<()> {
        locked(&self.steps).push(update.step.clone());
        Ok(())
    }
    fn record_handoff_landing_wait(&self, _: &str, _: &str) -> Res<()> {
        Ok(())
    }
    fn record_handoff_landing_history(&self, _: &str, event: &str, note: &str) -> Res<()> {
        locked(&self.inner.history)
            .entry(TASK_ID.into())
            .or_default()
            .push(TaskHistoryEntry {
                at: Utc::now(),
                by: "system".into(),
                event: event.into(),
                note: Some(note.into()),
                from_status: None,
                to_status: None,
                operation_id: None,
                decision: None,
            });
        Ok(())
    }
    fn get_task_history(&self, task: &str) -> Res<Vec<TaskHistoryEntry>> {
        Ok(self.inner.history(task))
    }
    fn admit_ci_rerun(
        &self,
        tasks: &[String],
        handoff: Option<&str>,
        plan: &CiRerunPlan,
    ) -> Res<orbit_types::workflow::CiRerunAdmission> {
        RuntimeHost::admit_ci_rerun(self.inner, tasks, handoff, plan)
    }
    fn run_private_vcs_operation(&self, operation: &str, input: Value) -> Res<Value> {
        if operation == "pr.merge" {
            assert_eq!(
                input["reviewed_head_sha"],
                self.context.candidate.candidate.commit
            );
            fs::write(
                self.inner.repo.parent().unwrap().join("forge/retry-merged"),
                b"merged",
            )
            .unwrap();
            return Ok(json!({"landed_commit":self.context.candidate.candidate.commit}));
        }
        RuntimeHost::run_private_vcs_operation(self.inner, operation, input)
    }
}

fn claimed<'a>(fx: &Fixture, host: &'a DeliveryHost) -> ClaimedRetry<'a> {
    let mut candidate = orbit_engine::observe_candidate(
        &fx.repo,
        Some(BRANCH),
        BASE,
        BASE,
        HandoffDelivery::PullRequest { number: 42 },
        "ws_owner",
        BaseSyncMode::Local,
    )
    .unwrap();
    candidate.repository = "orbit/test".into();
    ClaimedRetry {
        inner: host,
        context: orbit_engine::HandoffLandingContext {
            handoff_id: "handoff-retry".into(),
            task_id: TASK_ID.into(),
            claim_id: "claim-retry".into(),
            candidate,
            unresolved_merge_intent: None,
            workspace_path: fx.repo.clone(),
            owed_hosted_checks: vec![],
            automatic_repair_spent: false,
        },
        steps: Mutex::default(),
    }
}

#[test]
fn first_dependency_failure_reruns_before_claimed_landing_repair() {
    isolated(
        "ci_rerun::first_dependency_failure_reruns_before_claimed_landing_repair",
        |sandbox| {
            let (fx, host, _) = fixture(sandbox);
            let claimed = claimed(&fx, &host);
            let result = with_completion_clock(|| {
                execute_deterministic_action(
                    &claimed,
                    "handoff_land",
                    &json!({}),
                    &json!({"handoff_id":"handoff-retry","task_id":TASK_ID,"run_id":RUN_ID,"max_wait_seconds":30,"poll_interval_seconds":5}),
                    false,
                    &HashMap::new(),
                    None,
                )
            });
            assert!(result.is_ok(), "{result:?}; {:?}", locked(&claimed.steps));
            assert_eq!(
                fx.forge_lines("retry-sends"),
                vec!["run rerun 700 --failed"]
            );
            assert!(
                host.history(TASK_ID)
                    .iter()
                    .any(|entry| entry.event == CI_RERUN_RESERVED_EVENT)
            );
            assert!(pr_auto_fix_attempts(&host.history(TASK_ID), "42").is_empty());
        },
    );
}

#[test]
fn owner_retry_preserves_read_only_and_independent_candidate_routes() {
    isolated(
        "ci_rerun::owner_retry_preserves_read_only_and_independent_candidate_routes",
        |sandbox| {
            for case in [
                "read_only",
                "base_unproven",
                "independent",
                "policy_changed",
                "conflicting_reservation",
                "unsupported_reservation",
                "lost_applied",
                "repeat",
                "different_signature",
            ] {
                in_fresh_row(sandbox, |row| {
                    let (fx, host, _) = fixture(row);
                    let path = fx.forge.join("retry-config.json");
                    let mut config: Value =
                        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                    if case == "base_unproven" {
                        config["base_unproven"] = json!(true);
                        fs::write(
                            fx.forge.join("retry-log-1001"),
                            "lint\tWait\tci-await: test concluded failure\n",
                        )
                        .unwrap();
                    } else if case == "independent" {
                        config["candidate"] = json!(true);
                        fs::write(fx.forge.join("retry-log-1002"),"independent\tTests\terror: genuine candidate failure at src/feature.txt:1:1\n").unwrap();
                    } else if case == "policy_changed" {
                        fs::write(fx.forge.join("retry-change-policy"), b"change").unwrap();
                    } else if matches!(case, "conflicting_reservation" | "unsupported_reservation")
                    {
                        fs::write(
                            fx.forge.join("retry-ambiguous-ledger"),
                            if case == "unsupported_reservation" {
                                "unsupported"
                            } else {
                                "conflicting"
                            },
                        )
                        .unwrap();
                    } else if case == "lost_applied" {
                        config["reply"] = json!("lost");
                    } else if matches!(case, "repeat" | "different_signature") {
                        config["repeat"] = json!(true);
                        let root = if case == "repeat" {
                            "source failure"
                        } else {
                            "different failure"
                        };
                        fs::write(
                            fx.forge.join("retry-log-1010"),
                            format!("test\tTests\terror: {root} at src/stable:1:1\n"),
                        )
                        .unwrap();
                        fs::copy(
                            fx.forge.join("retry-log-1001"),
                            fx.forge.join("retry-log-1011"),
                        )
                        .unwrap();
                    }
                    fs::write(path, serde_json::to_vec(&config).unwrap()).unwrap();
                    let mut input = fx.complete_ungated_input();
                    input["auto_fix"] = json!(case != "read_only");
                    input["completion"] = json!("review");
                    input["max_wait_seconds"] = json!(10);
                    let result = complete_with_logical_clock(&host, &input);
                    let attempts = pr_auto_fix_attempts(&host.history(TASK_ID), "42");
                    if matches!(case, "independent" | "repeat") {
                        let error = result.expect_err(case);
                        assert!(
                            orbit_types::workflow::is_pr_auto_fix_check_repair(&error.to_string()),
                            "{case}: {error}"
                        );
                        assert_eq!(attempts.len(), 1, "{case}");
                    } else if case == "lost_applied" {
                        assert!(result.is_ok(), "{result:?}");
                        assert!(attempts.is_empty());
                    } else {
                        assert!(result.is_err(), "{case}");
                        assert!(attempts.is_empty(), "{case}: no candidate attribution");
                    }
                    assert_eq!(
                        fx.forge_lines("retry-sends").len(),
                        usize::from(matches!(
                            case,
                            "lost_applied" | "repeat" | "different_signature"
                        )),
                        "{case}"
                    );
                    assert!(
                        fx.merge_requests().is_empty(),
                        "watching never grants merge authority"
                    );
                });
            }
        },
    );
}
