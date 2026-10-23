//! Real Git and provider-source proof through public completion/landing actions.
use super::*;

pub(super) const NEW: &str = "name: CI\non:\n  pull_request:\n    branches: [agent-main]\npermissions: {}\njobs:\n  check:\n    name: Protected (${{ matrix.language }})\n    strategy:\n      matrix:\n        language: [rust]\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo check\n";

const LIVE_PR_GATE: &str = "tests/engine/pr_landing/infrastructure/fixtures/live-pr-gate.yml";
const LIVE_PR_GATE_CONTEXT: &str = "Windows cargo check (x86_64-pc-windows-msvc)";
pub(super) fn setup(sandbox: &Path, source: &str) -> (Fixture, DeliveryHost) {
    setup_with_context(sandbox, source, "Protected (rust)")
}
fn setup_with_context(sandbox: &Path, source: &str, context: &str) -> (Fixture, DeliveryHost) {
    let initial = Fixture::new(sandbox);
    initial.commit_on_base(".github/workflows/ci.yml", "name: CI\non: [pull_request]\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo test\n");
    git(
        &initial.repo,
        &["merge", "--no-ff", "-qm", "Seed accepted base", BASE],
    );
    git(&initial.repo, &["push", "origin", BRANCH]);
    let fx = Fixture {
        candidate: initial.head(),
        base_sha: git(&initial.repo, &["rev-parse", BASE]),
        ..initial
    };
    let remote = fx.forge.join("orbit/test.git");
    fs::create_dir_all(remote.parent().unwrap()).unwrap();
    fs::rename(fx.forge.join("remote.git"), &remote).unwrap();
    git(
        &fx.repo,
        &["remote", "set-url", "origin", path_str(&remote)],
    );
    fx.commit_on_base(".github/workflows/ci.yml", source);
    for (name, value) in [
        ("remote", path_str(&remote)),
        ("slug", "orbit/test"),
        ("pr-head", BRANCH),
        ("pr-base", BASE),
        ("old-base", fx.base_sha.as_str()),
    ] {
        fs::write(fx.forge.join(name), value).unwrap();
    }
    let provider = sandbox.join("bin/gh");
    fs::write(
        &provider,
        fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/engine/pr_landing/fixtures/base-refresh-forge.sh"),
        )
        .unwrap()
        .replace("__FORGE__", path_str(&fx.forge))
        .replace("Protected (rust)", context),
    )
    .unwrap();
    // `handoff_land` with `max_wait_seconds: 0` reads PR status under the 15s
    // default process budget (`landing/run.rs` skips the recovery deadline, and
    // `Deadline::remaining` still clamps a passed deadline to that budget).
    // The first exec of this image is the host-wide assessment queue
    // (`test_env::prime_executable`, ORB-15293). Land it here, before that read.
    fs::set_permissions(&provider, fs::Permissions::from_mode(0o755)).unwrap();
    let mut prime = Command::new(&provider);
    prime.arg("__prime__");
    test_env::prime_executable(&mut prime);
    let host = DeliveryHost::new(&fx.repo, TaskStatus::Review);
    (fx, host)
}

#[test]
fn absent_context_refreshes_once_before_normal_owner_completion() {
    isolated(
        "absent_context::absent_context_refreshes_once_before_normal_owner_completion",
        |sandbox| {
            let (fx, host) = setup(sandbox, NEW);
            fs::write(fx.forge.join("lost-reply"), "lost").unwrap();
            fs::write(fx.forge.join("legacy-state"), "success").unwrap();
            fs::write(fx.forge.join("legacy-extra"), "control").unwrap();
            let mut input = fx.complete_input();
            input["auto_fix"] = json!(true);
            let completed = orbit_engine::test_support::with_vcs_retry_wait(
                |_| {},
                || action(&host, "pr_complete", &input),
            )
            .expect("verified current-base workflow refreshes without an agent");
            assert_eq!(completed["completed_task_ids"], json!([TASK_ID]));
            assert_eq!(
                fs::read_to_string(fx.forge.join("mutations")).unwrap(),
                "update\nmerge\n"
            );
            assert_eq!(
                host.history(TASK_ID)
                    .iter()
                    .filter(|row| row.event == "pr_base_update_reserved")
                    .count(),
                1
            );
            assert!(
                pr_auto_fix_attempts(&host.history(TASK_ID), PR_NUMBER).is_empty(),
                "no source repair runs"
            );
            assert_ne!(
                completed["merge"]["delivery_evidence"]["head_sha"],
                fx.candidate
            );
        },
    );
}

#[test]
fn path_filtered_website_workflow_preserves_sibling_emission_recovery() {
    isolated(
        "absent_context::path_filtered_website_workflow_preserves_sibling_emission_recovery",
        |sandbox| {
            let (fx, host) = setup(sandbox, NEW);
            let website = fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../.github/workflows/website-check.yml"),
            )
            .unwrap();
            fx.commit_on_base(".github/workflows/website-check.yml", &website);
            fs::write(fx.forge.join("website-workflow"), "registered").unwrap();
            fs::write(fx.forge.join("review-approved"), "approved").unwrap();
            let proof = orbit_engine::run_private_vcs_operation(
                "ci.emission_evidence",
                &json!({
                    "workspace_path": fx.repo,
                    "pr": PR_NUMBER,
                    "head_sha": fx.candidate,
                    "head_branch": BRANCH,
                    "base_branch": BASE,
                    "accepted_base": fx.base_sha,
                    "deadline_ms": Utc::now().timestamp_millis() + 30_000,
                }),
            )
            .expect("a path-filtered sibling must not abort authenticated emission proof");
            let sources = &proof["evidence"]["base_sources"];
            assert_eq!(
                sources[".github/workflows/ci.yml"]["contexts"],
                json!(["Protected (rust)"])
            );
            assert_eq!(
                sources[".github/workflows/website-check.yml"]["contexts"],
                json!([]),
                "path-filtered checks never contribute guaranteed emission"
            );
            let mut input = fx.complete_input();
            input["auto_fix"] = json!(true);
            let completed = action(&host, "pr_complete", &input)
                .expect("a path-filtered sibling must not abort authenticated emission proof");
            assert_eq!(completed["completed_task_ids"], json!([TASK_ID]));
            assert_eq!(
                fs::read_to_string(fx.forge.join("mutations")).unwrap(),
                "update\nmerge\n",
                "the missing sibling context permits exactly one update and merge"
            );
            assert_eq!(
                host.history(TASK_ID)
                    .iter()
                    .filter(|row| row.event == "pr_base_update_reserved")
                    .count(),
                1
            );
            assert!(pr_auto_fix_attempts(&host.history(TASK_ID), PR_NUMBER).is_empty());
        },
    );
}

#[test]
fn workflow_source_bounds_are_checked_during_emission_proof() {
    isolated(
        "absent_context::workflow_source_bounds_are_checked_during_emission_proof",
        |sandbox| {
            for (case, padding) in [("between_limits", 70 * 1024), ("over_limit", 256 * 1024)] {
                in_fresh_row(sandbox, |row| {
                    let mut source = NEW.to_owned();
                    source.push_str(&format!("\n# {}\n", "x".repeat(padding)));
                    if case == "between_limits" {
                        assert!(source.len() > 64 * 1024 && source.len() < 256 * 1024);
                    } else {
                        assert!(source.len() > 256 * 1024);
                    }

                    let (fx, host) = setup(row, &source);
                    fs::write(fx.forge.join("review-approved"), "approved").unwrap();
                    let mut input = fx.complete_input();
                    input["auto_fix"] = json!(true);
                    let result = action(&host, "pr_complete", &input);

                    if case == "between_limits" {
                        let completed = result.unwrap_or_else(|error| panic!("{case}: {error}"));
                        assert_eq!(completed["completed_task_ids"], json!([TASK_ID]));
                        assert_eq!(
                            fs::read_to_string(fx.forge.join("mutations")).unwrap(),
                            "update\nmerge\n"
                        );
                    } else {
                        let error = result.expect_err(case).to_string();
                        assert!(
                            error.contains("truncated workflow emission source"),
                            "{error}"
                        );
                        assert!(!fx.forge.join("mutations").exists());
                        assert!(pr_auto_fix_attempts(&host.history(TASK_ID), PR_NUMBER).is_empty());
                    }
                });
            }
        },
    );
}

pub(super) struct AcceptedLanding {
    host: DeliveryHost,
    pub(super) context: orbit_engine::HandoffLandingContext,
    pub(super) updates: Mutex<Vec<orbit_engine::HandoffLandingUpdate>>,
}
impl orbit_engine::test_support::NullRuntimeHost for AcceptedLanding {
    fn admit_handoff_base_update(
        &self,
        _: &str,
        request: &orbit_types::workflow::PrBaseUpdateReservation,
    ) -> Result<orbit_types::workflow::PrBaseUpdateAdmission, OrbitError> {
        RuntimeHost::admit_pr_base_update(self, &[TASK_ID.into()], request)
    }

    fn handoff_landing_context(
        &self,
        _: &str,
    ) -> Result<orbit_engine::HandoffLandingContext, OrbitError> {
        Ok(self.context.clone())
    }
    fn get_task_history(&self, id: &str) -> Result<Vec<TaskHistoryEntry>, OrbitError> {
        RuntimeHost::get_task_history(&self.host, id)
    }
    fn admit_pr_base_update(
        &self,
        tasks: &[String],
        request: &orbit_types::workflow::PrBaseUpdateReservation,
    ) -> Result<orbit_types::workflow::PrBaseUpdateAdmission, OrbitError> {
        RuntimeHost::admit_pr_base_update(&self.host, tasks, request)
    }
    fn record_handoff_landing(
        &self,
        update: &orbit_engine::HandoffLandingUpdate,
    ) -> Result<(), OrbitError> {
        self.updates.lock().unwrap().push(update.clone());
        Ok(())
    }
}
pub(super) fn accepted_landing(fx: &Fixture, host: DeliveryHost) -> AcceptedLanding {
    let revision = |commit: &str| orbit_types::workflow::automation::SourceRevision {
        commit: commit.into(),
        tree: git(&fx.repo, &["rev-parse", &format!("{commit}^{{tree}}")]),
    };
    AcceptedLanding {
        context: orbit_engine::HandoffLandingContext {
            handoff_id: "accepted-fixture".into(),
            task_id: TASK_ID.into(),
            claim_id: "fixture-claim".into(),
            workspace_path: fx.repo.clone(),
            unresolved_merge_intent: None,
            owed_hosted_checks: vec!["Protected (rust)".into()],
            automatic_repair_spent: false,
            candidate: orbit_types::workflow::handoff::HandoffCandidate {
                repository: "orbit/test".into(),
                source_branch: BRANCH.into(),
                base_branch: BASE.into(),
                landing_branch: BASE.into(),
                candidate: revision(&fx.candidate),
                base: revision(&fx.base_sha),
                delivery: HandoffDelivery::PullRequest { number: 42 },
            },
        },
        host,
        updates: Mutex::default(),
    }
}
#[test]
fn absent_context_refreshes_an_accepted_handoff_without_changing_acceptance() {
    isolated(
        "absent_context::absent_context_refreshes_an_accepted_handoff_without_changing_acceptance",
        |sandbox| {
            let (fx, host) = setup(sandbox, NEW);
            let host = accepted_landing(&fx, host);
            let accepted = host.context.candidate.clone();
            let landed = execute_deterministic_action(
                &host,
                "handoff_land",
                &json!({}),
                &json!({"handoff_id":"accepted-fixture","run_id":RUN_ID,"max_wait_seconds":30}),
                false,
                &HashMap::new(),
                None,
            )
            .unwrap();
            assert_eq!(landed["phase"], "landed");
            assert_eq!(
                fs::read_to_string(fx.forge.join("mutations")).unwrap(),
                "update\nmerge\n"
            );
            let updates = host.updates.lock().unwrap();
            assert!(
                updates
                    .iter()
                    .filter_map(|row| row.observed.as_ref())
                    .all(|candidate| *candidate == accepted),
                "acceptance and native evidence stay at the original head/base"
            );
            assert!(matches!(
                updates.last().unwrap().step,
                orbit_engine::HandoffLandingStep::Complete
            ));
        },
    );
}

#[test]
fn unsupported_or_unproven_emission_never_updates_or_repairs() {
    isolated(
        "absent_context::unsupported_or_unproven_emission_never_updates_or_repairs",
        |sandbox| {
            for case in [
                "dynamic_name",
                "dynamic_runner",
                "missing_steps",
                "invalid_job",
                "condition",
                "needs",
                "paths",
                "paths-ignore",
                "wrong_branch",
                "wrong_event",
                "old_emits",
                "foreign_app",
                "truncated",
                "auth",
                "review",
                "changed_policy",
                "changed_head",
                "rewound_base",
                "failed_fetch",
                "push",
                "no-runs",
                "source-truncated",
                "legacy-pending",
                "legacy-failed",
            ] {
                in_fresh_row(sandbox, |row| {
                    let source = match case {
                        "dynamic_name" => {
                            NEW.replace("${{ matrix.language }}", "${{ github.actor }}")
                        }
                        "dynamic_runner" => {
                            NEW.replace("runs-on: ubuntu-latest", "runs-on: ${{ github.actor }}")
                        }
                        "missing_steps" => {
                            NEW.replace("    steps:\n      - run: echo check\n", "    steps: []\n")
                        }
                        "invalid_job" => NEW.replace("  check:", "  1check:"),
                        "condition" => {
                            NEW.replace("    name:", "    if: github.actor == 'owner'\n    name:")
                        }
                        "needs" => NEW.replace("    name:", "    needs: unknown\n    name:"),
                        "paths" => {
                            NEW.replace("    branches:", "    paths: ['src/**']\n    branches:")
                        }
                        "paths-ignore" => NEW.replace(
                            "    branches:",
                            "    paths-ignore: ['docs/**']\n    branches:",
                        ),
                        "wrong_branch" => NEW.replace("branches: [agent-main]", "branches: [main]"),
                        "wrong_event" => NEW.replace("pull_request:", "push:"),
                        _ => NEW.into(),
                    };
                    let (fx, host) = setup(row, &source);
                    if matches!(case, "legacy-pending" | "legacy-failed") {
                        fs::write(
                            fx.forge.join("legacy-state"),
                            if case == "legacy-pending" {
                                "pending"
                            } else {
                                "failure"
                            },
                        )
                        .unwrap();
                        fs::write(fx.forge.join("legacy-extra"), "control").unwrap();
                    }
                    if case == "old_emits" {
                        git(
                            &fx.repo,
                            &["merge", "--no-ff", "-qm", "Already emits", BASE],
                        );
                        git(&fx.repo, &["push", "origin", BRANCH]);
                    }
                    if matches!(
                        case,
                        "foreign_app"
                            | "truncated"
                            | "auth"
                            | "review"
                            | "changed_policy"
                            | "changed_head"
                            | "push"
                            | "no-runs"
                            | "source-truncated"
                    ) {
                        fs::write(fx.forge.join(case), "control").unwrap();
                    }
                    if case == "rewound_base" {
                        git(
                            &fx.repo,
                            &[
                                "--git-dir",
                                path_str(&fx.forge.join("orbit/test.git")),
                                "update-ref",
                                &format!("refs/heads/{BASE}"),
                                &fx.base_sha,
                            ],
                        );
                    }
                    if case == "failed_fetch" {
                        git(
                            &fx.repo,
                            &["remote", "set-url", "origin", "/nonexistent/orbit/test.git"],
                        );
                    }
                    let mut input = fx.complete_input();
                    input["auto_fix"] = json!(true);
                    input["max_wait_seconds"] = json!(1);
                    if case == "old_emits" {
                        input["published_head_sha"] = json!(fx.head());
                        input["reviewed_head_sha"] = json!(fx.head());
                    }
                    let failure = action(&host, "pr_complete", &input).unwrap_err();
                    assert!(
                        !failure
                            .to_string()
                            .contains("completion_observation_unavailable:"),
                        "unproven, changed or refused evidence is never a resumable read: \
                         {case}: {failure}"
                    );
                    assert!(
                        !fx.forge.join("mutations").exists(),
                        "no update/merge: {case}"
                    );
                    assert!(
                        pr_auto_fix_attempts(&host.history(TASK_ID), PR_NUMBER).is_empty(),
                        "no source agent: {case}"
                    );
                });
            }
        },
    );
}

// Capture the action's causal error before a missing marker can obscure it.
fn refresh_diagnostics(
    fx: &Fixture,
    host: &DeliveryHost,
    mode: &str,
    now: u64,
    result: &Result<Value, OrbitError>,
) -> String {
    let reservations = orbit_types::workflow::pr_base_update_reservations(&host.history(TASK_ID))
        .map(|rows| {
            rows.into_iter()
                .map(|row| {
                    json!({
                        "head": row.head_sha, "base": row.base_sha, "deadline_ms": row.deadline_ms,
                        "execution": row.execution,
                    })
                })
                .collect::<Vec<_>>()
        });
    let calls = fx.forge_calls();
    let tail = calls
        .iter()
        .rev()
        .take(16)
        .rev()
        .map(|call| call.chars().take(512).collect::<String>())
        .collect::<Vec<_>>();
    format!(
        "mode={mode} now={now} action={} mutations={:?} reservation_marker={:?} reservations={reservations:?} provider_calls={} tail={tail:?} delay_entered={} delay_finished={}",
        match result {
            Ok(value) => value.to_string(),
            Err(error) => error.to_string(),
        },
        fs::read_to_string(fx.forge.join("mutations")),
        fs::read_to_string(fx.forge.join("base-reserved")),
        calls.len(),
        fx.forge.join("read-delay-entered").exists(),
        fx.forge.join("read-delay-finished").exists(),
    )
}

#[test]
fn completion_retries_transient_repository_and_merge_policy_lookups() {
    isolated(
        "absent_context::completion_retries_transient_repository_and_merge_policy_lookups",
        |sandbox| {
            for matched in ["repo view", "autoMergeAllowed"] {
                in_fresh_row(sandbox, |row| {
                    let (fx, host) = setup(row, NEW);
                    fs::write(fx.forge.join("base-applied"), "current").unwrap();
                    fs::write(fx.forge.join("fault-match"), matched).unwrap();
                    fs::write(
                        fx.forge.join("fault-stderr"),
                        "Post \"https://api.github.com/graphql\": net/http: TLS handshake timeout",
                    )
                    .unwrap();

                    let completed = action(&host, "pr_complete", &fx.complete_input())
                        .expect("a one-shot read failure must recover within completion");

                    assert!(fx.forge.join("fault-hit").exists(), "{matched}");
                    assert_eq!(
                        fx.forge_calls()
                            .iter()
                            .filter(|call| call.contains(matched))
                            .count(),
                        2,
                        "{matched}: the failed read is retried once"
                    );
                    assert_eq!(completed["merge"]["merged"], true, "{matched}");
                    assert_eq!(host.status(TASK_ID), TaskStatus::Done, "{matched}");
                    assert_eq!(host.landings().len(), 1, "{matched}");
                    assert_eq!(
                        fs::read_to_string(fx.forge.join("mutations")).unwrap(),
                        "merge\n",
                        "{matched}: one merge and no branch update"
                    );
                    assert!(
                        host.comments(TASK_ID).is_empty(),
                        "{matched}: no failure recorded"
                    );
                });
            }
        },
    );
}

/// [ORB-16307] Only a transport failure of an authenticated read made before
/// any mutation, within the cumulative budget, is a resumable completion
/// observation. Permanent or credential refusals, an exhausted budget and
/// durable mutation provenance keep their own failure.
#[test]
fn transient_prerequisite_read_resumes_only_before_any_mutation() {
    const TLS: &str = "Get \"https://api.github.com/graphql\": net/http: TLS handshake timeout";
    isolated(
        "absent_context::transient_prerequisite_read_resumes_only_before_any_mutation",
        |sandbox| {
            for (case, matched, stderr, resumable) in [
                ("emission_status", "/status?", TLS, true),
                (
                    "emission_gateway",
                    "/status?",
                    "gh: Service Unavailable (HTTP 503)",
                    true,
                ),
                ("emission_policy", "requiredStatusChecks", TLS, true),
                ("required_checks", "statusCheckRollup", TLS, true),
                ("merge_policy", "autoMergeAllowed", TLS, true),
                (
                    "forbidden",
                    "/status?",
                    "gh: Resource not accessible by integration (HTTP 403)",
                    false,
                ),
                (
                    "credentials",
                    "/status?",
                    "gh: Bad credentials (HTTP 401)",
                    false,
                ),
                ("not_found", "/status?", "gh: Not Found (HTTP 404)", false),
                ("expired_budget", "/status?", TLS, false),
                ("unknown_merge_outcome", "/status?", TLS, false),
                ("after_base_update", "autoMergeAllowed", TLS, false),
            ] {
                in_fresh_row(sandbox, |row| {
                    let (fx, host) = setup(row, NEW);
                    fs::write(fx.forge.join("fault-match"), matched).unwrap();
                    fs::write(fx.forge.join("fault-stderr"), stderr).unwrap();
                    if matched == "autoMergeAllowed" {
                        // Merge-policy reads retry in-process. Exhaust those
                        // attempts before checking resumable observation rules.
                        fs::write(fx.forge.join("fault-repeat"), "repeat").unwrap();
                    }
                    // Under auto-fix a blocked head's current-gates read makes the
                    // same snapshot reads before absent-context recovery, and its
                    // failure only leaves the wait unrefined. Inject the fault
                    // into recovery's own read, the second one.
                    if matches!(matched, "/status?" | "requiredStatusChecks") {
                        fs::write(fx.forge.join("fault-nth"), "2").unwrap();
                    }
                    if case == "merge_policy" {
                        // Already current and passing: the first read is the merge policy.
                        fs::write(fx.forge.join("base-applied"), "current").unwrap();
                    }
                    if case == "unknown_merge_outcome" {
                        RuntimeHost::apply_task_automation_update(
                            &host,
                            TASK_ID,
                            TaskAutomationUpdate {
                                append_history: vec![TaskHistoryEntry {
                                    at: Utc::now(),
                                    by: "system".into(),
                                    event:
                                        orbit_types::workflow::PR_COMPLETION_MUTATION_STARTED_EVENT
                                            .into(),
                                    note: Some("{}".into()),
                                    from_status: None,
                                    to_status: None,
                                    operation_id: None,
                                    decision: None,
                                }],
                                ..Default::default()
                            },
                        )
                        .unwrap();
                    }
                    let mut input = fx.complete_input();
                    input["auto_fix"] = json!(true);
                    input["max_wait_seconds"] = json!(600);
                    let hit = fx.forge.join("fault-hit");
                    let failure = orbit_engine::test_support::with_prerequisite_clock(
                        move || {
                            let now = u64::try_from(
                                std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap()
                                    .as_millis(),
                            )
                            .unwrap();
                            // The budget runs out while the failed read is outstanding.
                            if case == "expired_budget" && hit.exists() {
                                now + 601_000
                            } else {
                                now
                            }
                        },
                        |_, _| {},
                        || action(&host, "pr_complete", &input),
                    )
                    .unwrap_err()
                    .to_string();
                    assert!(
                        fx.forge.join("fault-hit").exists(),
                        "{case}: the injected read ran: {failure}"
                    );
                    if matched == "autoMergeAllowed" {
                        assert_eq!(
                            fx.forge_state("fault-seen").as_deref(),
                            Some("3"),
                            "{case}: persistent merge-policy failures stop at three attempts"
                        );
                    }
                    assert_eq!(
                        failure.contains("completion_observation_unavailable:"),
                        resumable,
                        "{case}: {failure}"
                    );
                    let mutations =
                        fs::read_to_string(fx.forge.join("mutations")).unwrap_or_default();
                    let reservations = host
                        .history(TASK_ID)
                        .iter()
                        .filter(|row| row.event == "pr_base_update_reserved")
                        .count();
                    if case == "after_base_update" {
                        assert_eq!(mutations, "update\n", "{case}");
                        assert_eq!(reservations, 1, "{case}");
                    } else {
                        assert_eq!(mutations, "", "{case}: no update or merge");
                        assert_eq!(reservations, 0, "{case}");
                    }
                    assert!(
                        pr_auto_fix_attempts(&host.history(TASK_ID), PR_NUMBER).is_empty(),
                        "{case}: no source agent"
                    );
                });
            }
        },
    );
}

#[test]
fn reserved_refresh_observes_lost_unapplied_and_pending_outcomes_without_another_write() {
    isolated(
        "absent_context::reserved_refresh_observes_lost_unapplied_and_pending_outcomes_without_another_write",
        |sandbox| {
            for mode in [
                "no-apply",
                "gates-pending",
                "policy-after",
                "foreign-after",
                "clean-missing",
                "unreadable-after",
                "revision-after",
                "legacy-substitute",
            ] {
                in_fresh_row(sandbox, |row| {
                    let (fx, host) = setup(row, NEW);
                    fs::write(fx.forge.join(mode), "control").unwrap();
                    if mode == "no-apply" {
                        fs::write(fx.forge.join("delay-read"), "control").unwrap();
                    }
                    let mut input = fx.complete_input();
                    input["auto_fix"] = json!(true);
                    // Reservation expiry is a logical-clock transition, not a
                    // one-second allowance for native provider/Git processes.
                    let started = 1_000;
                    let wait_seconds = 30;
                    let deadline = started + wait_seconds * 1_000;
                    input["max_wait_seconds"] = json!(wait_seconds);
                    input["poll_interval_seconds"] = json!(wait_seconds);
                    let mut original_reservation = None;
                    for (phase, now) in [
                        ("initial", started),
                        ("replay", started),
                        ("expired resume", deadline + 1),
                    ] {
                        let result = orbit_engine::test_support::with_prerequisite_clock(
                            move || now,
                            |_, _| {},
                            || {
                                orbit_engine::test_support::with_vcs_retry_wait(
                                    |_| {},
                                    || action(&host, "pr_complete", &input),
                                )
                            },
                        );
                        let diagnostics = format!(
                            "phase={phase} {}",
                            refresh_diagnostics(&fx, &host, mode, now, &result)
                        );
                        assert!(result.is_err(), "{diagnostics}");
                        assert_eq!(
                            fs::read_to_string(fx.forge.join("mutations")).unwrap_or_else(
                                |error| panic!("{diagnostics}; marker read: {error}")
                            ),
                            "update\n",
                            "one write across polls, replay and expired resume: {diagnostics}"
                        );
                        let reservations = orbit_types::workflow::pr_base_update_reservations(
                            &host.history(TASK_ID),
                        )
                        .unwrap_or_else(|error| panic!("{diagnostics}; reservation: {error}"));
                        assert_eq!(reservations.len(), 1, "{diagnostics}");
                        let reservation = &reservations[0];
                        assert_eq!(reservation.deadline_ms, deadline, "{diagnostics}");
                        if let Some(original) = &original_reservation {
                            assert_eq!(
                                reservation, original,
                                "reservation cannot renew: {diagnostics}"
                            );
                        } else {
                            original_reservation = Some(reservation.clone());
                        }
                        if phase == "expired resume" {
                            assert!(
                                result
                                    .unwrap_err()
                                    .to_string()
                                    .contains("completion deadline expired"),
                                "the original reservation deadline refuses resume: {diagnostics}"
                            );
                        }
                        if mode == "no-apply" {
                            assert!(
                                fx.forge.join("read-delay-finished").exists(),
                                "controlled slow read finishes before reservation: {diagnostics}"
                            );
                        }
                        assert!(
                            pr_auto_fix_attempts(&host.history(TASK_ID), PR_NUMBER).is_empty(),
                            "no code agent: {mode}"
                        );
                    }
                });
            }
        },
    );
}

#[test]
fn live_pr_gate_emission_contract_and_push_only_platform_workflows() {
    isolated(
        "absent_context::live_pr_gate_emission_contract_and_push_only_platform_workflows",
        |sandbox| {
            // The live-PR gate shape, frozen from the retired Windows workflow:
            // a missing check it emits is proven and repaired by one update.
            in_fresh_row(sandbox, |row| {
                let source =
                    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(LIVE_PR_GATE))
                        .unwrap();
                let (fx, host) = setup_with_context(row, &source, LIVE_PR_GATE_CONTEXT);
                let mut input = fx.complete_input();
                input["auto_fix"] = json!(true);
                action(&host, "pr_complete", &input)
                    .unwrap_or_else(|error| panic!("{LIVE_PR_GATE}: {error}"));
                assert_eq!(
                    fs::read_to_string(fx.forge.join("mutations")).unwrap(),
                    "update\nmerge\n"
                );
            });
            // The repository's own ci.yml gates its required jobs on the
            // change-scope job, and a full-mode head reports `Docs guards` as
            // skipped. Every mutation of that exact scope shape still refuses.
            let ci = || -> Value {
                serde_yaml::from_str(
                    &fs::read_to_string(
                        Path::new(env!("CARGO_MANIFEST_DIR"))
                            .join("../../.github/workflows/ci.yml"),
                    )
                    .unwrap(),
                )
                .unwrap()
            };
            let full = |row: &Path, source: &Value| {
                let (fx, host) = setup_with_context(
                    row,
                    &serde_yaml::to_string(source).unwrap(),
                    "Check / Clippy / Test",
                );
                fs::write(fx.forge.join("scope-checks"), "full").unwrap();
                let mut input = fx.complete_input();
                input["auto_fix"] = json!(true);
                (fx, action(&host, "pr_complete", &input))
            };
            in_fresh_row(sandbox, |row| {
                let (fx, result) = full(row, &ci());
                result.unwrap_or_else(|error| panic!("ci.yml: {error}"));
                assert_eq!(
                    fs::read_to_string(fx.forge.join("mutations")).unwrap(),
                    "update\nmerge\n"
                );
            });
            for case in [
                "scope_script",
                "scope_output",
                "scope_condition",
                "scope_failure",
                "scope_step",
                "always",
                "needs",
            ] {
                in_fresh_row(sandbox, |row| {
                    let mut source = ci();
                    let scope = &mut source["jobs"]["scope"];
                    match case {
                        "scope_script" => {
                            scope["steps"][1]["run"] = json!("scripts/ci-change-scope.sh --docs")
                        }
                        "scope_output" => {
                            scope["outputs"]["mode"] = json!("${{ steps.foreign.outputs.mode }}")
                        }
                        "scope_condition" => {
                            scope["if"] = json!(
                                scope["if"]
                                    .as_str()
                                    .unwrap()
                                    .replace(" }}", " && github.actor == 'owner' }}")
                            )
                        }
                        "scope_failure" => {
                            scope.as_object_mut().unwrap().remove("continue-on-error");
                        }
                        "scope_step" => {
                            scope["steps"][1]["env"]["EVENT"] = json!("push");
                        }
                        "always" => {
                            source["jobs"]["ci"]["if"] = json!(
                                source["jobs"]["ci"]["if"]
                                    .as_str()
                                    .unwrap()
                                    .replace(" }}", " && always() }}")
                            )
                        }
                        _ => {
                            source["jobs"]["ci"]["needs"]
                                .as_array_mut()
                                .unwrap()
                                .push(json!("docs"));
                        }
                    }
                    let (fx, result) = full(row, &source);
                    assert!(result.is_err(), "{case}");
                    assert!(
                        !fx.forge.join("mutations").exists(),
                        "unknown change-scope semantics cannot update: {case}"
                    );
                });
            }
            // macOS runs only on agent-main pushes, so a pull request can never
            // emit its checks: a missing one must not prompt a branch update.
            in_fresh_row(sandbox, |row| {
                let source = fs::read_to_string(
                    Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("../../.github/workflows/ci-macos.yml"),
                )
                .unwrap();
                let (fx, host) = setup_with_context(row, &source, "macOS Sandbox");
                let mut input = fx.complete_input();
                input["auto_fix"] = json!(true);
                assert!(action(&host, "pr_complete", &input).is_err());
                assert!(
                    !fx.forge.join("mutations").exists(),
                    "a push-only workflow cannot update the branch"
                );
            });
            for case in ["endpoint", "output", "condition", "conditional_step"] {
                in_fresh_row(sandbox, |row| {
                    let mut source: Value = serde_yaml::from_str(
                        &fs::read_to_string(
                            Path::new(env!("CARGO_MANIFEST_DIR")).join(LIVE_PR_GATE),
                        )
                        .unwrap(),
                    )
                    .unwrap();
                    match case {
                        "endpoint" => {
                            source["jobs"]["pr-state"]["steps"][0]["run"] = json!(
                                source["jobs"]["pr-state"]["steps"][0]["run"]
                                    .as_str()
                                    .unwrap()
                                    .replace("/pulls/", "/issues/")
                            )
                        }
                        "output" => {
                            source["jobs"]["pr-state"]["outputs"]["state"] =
                                json!("${{ steps.foreign.outputs.state }}")
                        }
                        "condition" => {
                            source["jobs"]["windows-check"]["if"] = json!("${{ always() }}")
                        }
                        _ => source["jobs"]["pr-state"]["steps"][0]["if"] = json!(false),
                    }
                    let (fx, host) = setup_with_context(
                        row,
                        &serde_yaml::to_string(&source).unwrap(),
                        LIVE_PR_GATE_CONTEXT,
                    );
                    let mut input = fx.complete_input();
                    input["auto_fix"] = json!(true);
                    assert!(action(&host, "pr_complete", &input).is_err(), "{case}");
                    assert!(
                        !fx.forge.join("mutations").exists(),
                        "unknown live-state semantics cannot update: {case}"
                    );
                });
            }
        },
    );
}

#[test]
fn expired_refresh_still_settles_an_exact_existing_merge_without_another_provider_write() {
    isolated(
        "absent_context::expired_refresh_still_settles_an_exact_existing_merge_without_another_provider_write",
        |sandbox| {
            let (fx, host) = setup(sandbox, NEW);
            let host = accepted_landing(&fx, host);
            fs::write(fx.forge.join("gates-pending"), "pending").unwrap();
            fs::write(fx.forge.join("delay-read"), "control").unwrap();
            let started = 1_000;
            let wait_seconds = 30;
            let deadline = started + wait_seconds * 1_000;
            let input = json!({
                "handoff_id":"accepted-fixture","run_id":RUN_ID,
                "max_wait_seconds":wait_seconds,"poll_interval_seconds":wait_seconds,
            });
            let land = || {
                execute_deterministic_action(
                    &host,
                    "handoff_land",
                    &json!({}),
                    &input,
                    false,
                    &HashMap::new(),
                    None,
                )
            };
            let first = orbit_engine::test_support::with_prerequisite_clock(
                move || started,
                |_, _| {},
                || orbit_engine::test_support::with_vcs_retry_wait(|_| {}, land),
            );
            let diagnostics =
                refresh_diagnostics(&fx, &host.host, "gates-pending", started, &first);
            assert!(first.is_err(), "{diagnostics}");
            assert_eq!(
                fs::read_to_string(fx.forge.join("mutations"))
                    .unwrap_or_else(|error| panic!("{diagnostics}; marker read: {error}")),
                "update\n",
                "the initial action reserved and requested the update: {diagnostics}"
            );
            let reservations =
                orbit_types::workflow::pr_base_update_reservations(&host.host.history(TASK_ID))
                    .unwrap_or_else(|error| panic!("{diagnostics}; reservation: {error}"));
            assert_eq!(reservations.len(), 1, "{diagnostics}");
            assert_eq!(reservations[0].deadline_ms, deadline, "{diagnostics}");
            assert!(
                fx.forge.join("read-delay-finished").exists(),
                "controlled slow read finishes before reservation: {diagnostics}"
            );
            // The fixture provider completes the already requested external work
            // after the owner's bounded wait. No claim/receipt is edited.
            fs::remove_file(fx.forge.join("gates-pending")).unwrap();
            let head = git(
                &fx.repo,
                &[
                    "ls-remote",
                    "--heads",
                    "origin",
                    &format!("refs/heads/{BRANCH}"),
                ],
            )
            .split_whitespace()
            .next()
            .unwrap()
            .to_string();
            let merged = orbit_common::process::run_bounded_capped(
                Command::new(sandbox.join("bin/gh")).args([
                    "api",
                    "repos/{owner}/{repo}/pulls/42/merge",
                    "--method",
                    "PUT",
                    "-f",
                    &format!("sha={head}"),
                ]),
                Duration::from_secs(10),
                4096,
            )
            .unwrap();
            assert!(merged.status.success(), "{diagnostics}; merge: {merged:?}");
            let settled = orbit_engine::test_support::with_prerequisite_clock(
                move || deadline + 1,
                |_, _| {},
                land,
            )
            .unwrap();
            assert_eq!(settled["phase"], "landed");
            assert_eq!(
                fs::read_to_string(fx.forge.join("mutations")).unwrap(),
                "update\nmerge\n",
                "expiry grants readback, never another external mutation"
            );
        },
    );
}

/// Shared provider fixture for the public handoff landing regressions.
pub(crate) fn adopted_policy_cases(cases: &[&str]) {
    let parent = tempfile::tempdir().unwrap();
    for case in cases {
        in_fresh_row(parent.path(), |row| {
            let (fx, host) = setup(row, NEW);
            let mut host = accepted_landing(&fx, host);
            host.context.owed_hosted_checks = vec!["test".into()];
            let accepted = host.context.candidate.clone();
            // An external provider cleanly merges the current base into the
            // accepted head, without an Orbit emission reservation or mutation.
            let remote = fx.forge.join("orbit/test.git");
            let remote_args = ["--git-dir", path_str(&remote)];
            let base = git(
                &fx.repo,
                &[remote_args[0], remote_args[1], "rev-parse", BASE],
            );
            let tree = git(
                &fx.repo,
                &[
                    remote_args[0],
                    remote_args[1],
                    "merge-tree",
                    "--write-tree",
                    &fx.candidate,
                    &base,
                ],
            );
            let head = git(
                &fx.repo,
                &[
                    remote_args[0],
                    remote_args[1],
                    "-c",
                    "user.name=Forge",
                    "-c",
                    "user.email=forge@example.invalid",
                    "commit-tree",
                    &tree,
                    "-p",
                    &fx.candidate,
                    "-p",
                    &base,
                    "-m",
                    "External provider base update",
                ],
            );
            git(
                &fx.repo,
                &[
                    remote_args[0],
                    remote_args[1],
                    "update-ref",
                    &format!("refs/heads/{BRANCH}"),
                    &head,
                    &fx.candidate,
                ],
            );
            fs::write(fx.forge.join("base-applied"), "external").unwrap();
            match *case {
                "empty" | "unavailable" | "no-rule" => {
                    fs::write(
                        fx.forge.join(if *case == "no-rule" {
                            "policy-no-rule"
                        } else {
                            "policy-empty"
                        }),
                        "control",
                    )
                    .unwrap();
                    if *case == "unavailable" {
                        fs::write(fx.forge.join("policy-unreadable"), "control").unwrap();
                    }
                }
                _ => {
                    let state = match *case {
                        "pending" => "pending",
                        "failed" => "failure",
                        _ => "success",
                    };
                    fs::write(fx.forge.join("legacy-state"), state).unwrap();
                    if !matches!(*case, "success" | "pending" | "failed") {
                        fs::write(fx.forge.join(format!("legacy-{case}")), "control").unwrap();
                    }
                }
            }
            let result = execute_deterministic_action(
                &host,
                "handoff_land",
                &json!({}),
                &json!({"handoff_id":"accepted-fixture","run_id":RUN_ID,"max_wait_seconds":0}),
                false,
                &HashMap::new(),
                None,
            );
            let should_merge = matches!(*case, "empty" | "no-rule" | "success");
            if should_merge {
                assert_eq!(
                    result.unwrap_or_else(|error| panic!("{case}: {error}"))["phase"],
                    "landed"
                );
                assert_eq!(
                    fs::read_to_string(fx.forge.join("mutations")).unwrap(),
                    "merge\n",
                    "one ordinary merge and no owner base update: {case}"
                );
            } else {
                assert!(result.is_err(), "{case}: {result:?}");
                assert!(
                    !fx.forge.join("mutations").exists(),
                    "refused evidence never merges: {case}"
                );
            }
            let calls = fs::read_to_string(fx.forge.join("calls")).unwrap();
            assert!(
                calls.contains("requiredStatusChecks"),
                "real adapter policy read: {case}"
            );
            let updates = host.updates.lock().unwrap();
            if should_merge {
                assert_eq!(
                    updates.last().unwrap().observed.as_ref(),
                    Some(&accepted),
                    "settlement names original acceptance: {case}"
                );
            }
            assert!(
                updates
                    .iter()
                    .filter_map(|row| row.observed.as_ref())
                    .all(|candidate| *candidate == accepted),
                "original acceptance survives: {case}"
            );
            assert_eq!(
                matches!(
                    updates.last().unwrap().step,
                    orbit_engine::HandoffLandingStep::Complete
                ),
                should_merge,
                "settlement: {case}"
            );
        });
    }
}
