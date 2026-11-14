//! [ORB-23812] A published head the forge never started a required check
//! run for is re-triggered once, then watched or held.
use super::*;
use orbit_types::workflow::DeliveryRefusal;

const RETRIGGER_EVENT: &str = "pr_checks_retrigger_requested";

/// Plain completion on the absent-context forge: the head reports only an
/// optional check and no workflow run, while branch policy requires
/// `Protected (rust)`.
fn absent_check_runs(sandbox: &Path) -> (Fixture, DeliveryHost, Value) {
    let (fx, host) = absent_context::setup(sandbox, absent_context::NEW);
    fs::write(fx.forge.join("no-runs"), "none").unwrap();
    let mut input = fx.complete_input();
    input["max_wait_seconds"] = json!(3600);
    input["poll_interval_seconds"] = json!(30);
    (fx, host, input)
}

fn retriggers(host: &DeliveryHost) -> Vec<Value> {
    host.history(TASK_ID)
        .iter()
        .filter(|entry| entry.event == RETRIGGER_EVENT)
        .filter_map(|entry| serde_json::from_str(entry.note.as_deref()?).ok())
        .collect()
}

fn remote_head(fx: &Fixture) -> String {
    let remote = fs::read_to_string(fx.forge.join("remote")).unwrap();
    git(
        &fx.repo,
        &[
            "--git-dir",
            remote.trim(),
            "rev-parse",
            &format!("refs/heads/{BRANCH}"),
        ],
    )
}

#[test]
fn a_head_without_any_required_check_run_is_retriggered_once_then_held() {
    isolated(
        "absent_check_runs::a_head_without_any_required_check_run_is_retriggered_once_then_held",
        |sandbox| {
            let (fx, host, input) = absent_check_runs(sandbox);
            let error =
                complete_with_logical_clock(&host, &input).expect_err("absent checks never merge");
            match &error {
                OrbitError::DeliveryRefused { refusal, message } => {
                    assert_eq!(*refusal, DeliveryRefusal::CheckRunsAbsent, "{message}");
                    assert!(message.contains("#42"), "{message}");
                    assert!(message.contains(&fx.candidate), "{message}");
                    assert!(message.contains("Next step"), "{message}");
                }
                other => panic!("expected the absent-check-run hold, got {other}"),
            }
            assert_eq!(fx.forge_lines("mutations"), ["close", "reopen"]);
            let records = retriggers(&host);
            assert_eq!(records.len(), 1, "{records:?}");
            assert_eq!(records[0]["pr_number"], PR_NUMBER);
            assert_eq!(records[0]["head_sha"], fx.candidate.as_str());
            assert_eq!(remote_head(&fx), fx.candidate, "the head never moves");
            assert_eq!(host.status(TASK_ID), TaskStatus::Review);
            assert!(pr_auto_fix_attempts(&host.history(TASK_ID), PR_NUMBER).is_empty());

            // A later completion attempt on the same head holds without
            // asking the forge again.
            let again = complete_with_logical_clock(&host, &input)
                .expect_err("the second attempt holds too");
            assert!(
                matches!(
                    again,
                    OrbitError::DeliveryRefused {
                        refusal: DeliveryRefusal::CheckRunsAbsent,
                        ..
                    }
                ),
                "{again}"
            );
            assert_eq!(fx.forge_lines("mutations"), ["close", "reopen"]);
            assert_eq!(retriggers(&host).len(), 1);
        },
    );
}

#[test]
fn checks_started_by_the_retrigger_are_watched_and_land_the_same_head() {
    isolated(
        "absent_check_runs::checks_started_by_the_retrigger_are_watched_and_land_the_same_head",
        |sandbox| {
            let (fx, host, input) = absent_check_runs(sandbox);
            fs::write(fx.forge.join("checks-on-reopen"), "start").unwrap();
            let completed = complete_with_logical_clock(&host, &input)
                .expect("the re-triggered checks pass and the head merges");
            assert_eq!(completed["completed_task_ids"], json!([TASK_ID]));
            assert_eq!(fx.forge_lines("mutations"), ["close", "reopen", "merge"]);
            assert_eq!(
                completed["merge"]["delivery_evidence"]["head_sha"],
                fx.candidate.as_str()
            );
            assert_eq!(retriggers(&host).len(), 1);
        },
    );
}

/// [ORB-24457] A workflow run waiting for a runner has no check runs yet.
/// That is checks still pending: completion waits out its budget and does
/// not re-trigger or hold as `delivery_check_runs_absent`. Before the fix
/// this budget recorded one re-trigger and held on the last poll.
#[test]
fn a_queued_or_in_progress_workflow_with_no_check_runs_waits_out_the_budget() {
    isolated(
        "absent_check_runs::a_queued_or_in_progress_workflow_with_no_check_runs_waits_out_the_budget",
        |sandbox| {
            for status in ["queued", "in_progress"] {
                let (fx, host, mut input) = absent_check_runs(sandbox);
                fs::remove_file(fx.forge.join("no-runs")).unwrap();
                fs::write(fx.forge.join("workflow-run-status"), status).unwrap();
                input["max_wait_seconds"] = json!(420);
                input["poll_interval_seconds"] = json!(60);
                let error = complete_with_logical_clock(&host, &input)
                    .expect_err("a workflow still waiting for a runner does not merge");
                match &error {
                    OrbitError::DeliveryRefused { refusal, message } => {
                        assert_eq!(
                            *refusal,
                            DeliveryRefusal::CompletionBudgetExpired,
                            "{status}: {message}"
                        );
                    }
                    other => panic!("{status}: expected the completion budget, got {other}"),
                }
                assert!(
                    retriggers(&host).is_empty(),
                    "{status}: a present workflow run is not re-triggered: {:?}",
                    retriggers(&host)
                );
                assert!(
                    fx.forge_lines("mutations").is_empty(),
                    "{status}: the pull request is not closed and reopened: {:?}",
                    fx.forge_lines("mutations")
                );
            }
        },
    );
}
