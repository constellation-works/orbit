//! A red PR and the base repair of the failure it shares [ORB-25510].
//!
//! Replays the November 17 wait: a PR task's recovery widened its footprint
//! to `Cargo.lock`, its landing then held for a base failing the same way,
//! and the CI-failure sweep's repair of that failure was deferred behind the
//! PR's lock on every admission pass while the PR waited for that repair.

use chrono::Utc;
use orbit_core::application::task::TaskAddParams;
use orbit_core::{OrbitRuntime, TaskComplexity, TaskStatus};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::task::{
    CiFailureIdentity, CiFailureSource, ContextWideningStep, Task, TaskDecision, TaskHistoryEntry,
};
use orbit_types::workflow::{
    PR_AUTO_FIX_BASE_RED_EVENT, PR_AUTO_FIX_PUSHED_EVENT, PrAutoFixBaseRed,
};
use serde_json::{Value, json};

use super::{
    Seed, admitted, default_readiness, excluded_entry, list_backlog_tasks, release, runtime, seed,
    write_files,
};

const SIGNATURE: &str = "error[vulnerability]: libs-sys: heap buffer overflow";
const HEAD: &str = "9546fa58fbe515904cc7137c64d31f23d23687ef";
const BASE: &str = "1ac571c307e5cd70a83573a9260a89970a1bdc37";
const RECOVERY_RUN: &str = "jrun-20261117-0951-c3";

/// A CI-failure sweep repair whose creation recorded `signature`.
fn sweep_repair(runtime: &OrbitRuntime, title: &str, signature: &str) -> Task {
    runtime
        .add_task(TaskAddParams {
            title: title.into(),
            description: "Repair the red base.".into(),
            acceptance_criteria: vec!["The base check passes.".into()],
            plan: "Fixture plan.".into(),
            tags: vec!["ci-failure-sweep".into()],
            context_files: vec!["file:Cargo.lock".into()],
            complexity: TaskComplexity::Low,
            task_type: Some(orbit_core::TaskType::Bug),
            status: Some(TaskStatus::Backlog),
            creation_decision: Some(TaskDecision::CiFailureIdentity(CiFailureIdentity {
                failure_key: format!("key-{title}"),
                compiler_cause_digest: None,
                open_compiler_set: None,
                signature: Some(signature.into()),
                tested_commit: BASE.into(),
                sources: vec![CiFailureSource {
                    run_id: "44086048887".into(),
                    job_id: "134048786599".into(),
                    checkout: BASE.into(),
                }],
            })),
            ..Default::default()
        })
        .expect("sweep repair")
}

/// The PR task in review: its recovery widened it to `Cargo.lock`, and its
/// landing recorded `HEAD` failing as `BASE` does.
fn red_pr(runtime: &OrbitRuntime) -> Task {
    let pr = seed(
        runtime,
        Seed::named("red pr")
            .status(TaskStatus::Review)
            .files(&["src/feature.rs"]),
    );
    let added = runtime
        .widen_task_context_files(
            &pr.id,
            RECOVERY_RUN,
            ContextWideningStep::Recovery,
            "step_failure_recovery",
            &["Cargo.lock".to_string()],
        )
        .unwrap();
    assert_eq!(added, vec!["file:Cargo.lock".to_string()]);
    let record = PrAutoFixBaseRed::from_base_tip_evidence(&json!({
        "failed_head": HEAD,
        "landed_base": BASE,
        "base_branch": "agent-main",
        "source": "latest_push",
        "path": "known_red_base",
        "checks": [{
            "failed_job": {"name": "Check / Clippy / Test"},
            "base_job": {"name": "Check / Clippy / Test"},
            "base_commit": BASE,
            "signature": SIGNATURE,
            "log_excerpt": "error[vulnerability]: libs-sys"
        }]
    }))
    .expect("known-red evidence");
    append(
        runtime,
        &pr.id,
        PR_AUTO_FIX_BASE_RED_EVENT,
        &record.history_note(),
        Some(TaskDecision::PrAutoFixBaseRed(record)),
    );
    runtime.get_task(&pr.id).unwrap()
}

fn append(
    runtime: &OrbitRuntime,
    task: &str,
    event: &str,
    note: &str,
    decision: Option<TaskDecision>,
) {
    runtime
        .apply_task_automation_update(
            task,
            TaskAutomationUpdate {
                append_history: vec![
                    TaskHistoryEntry::new(Utc::now(), "system", event)
                        .with_note(Some(note.to_owned()))
                        .with_decision(decision),
                ],
                ..TaskAutomationUpdate::default()
            },
        )
        .unwrap();
}

fn requests(runtime: &OrbitRuntime, task: &str) -> (usize, Vec<String>) {
    let recorded = runtime
        .get_task_history(task)
        .unwrap()
        .iter()
        .filter(|entry| entry.event == "shared_base_repair_wait")
        .count();
    let comments = runtime
        .get_task_comments(task)
        .unwrap()
        .into_iter()
        .map(|comment| comment.message)
        .filter(|message| message.starts_with("Operator decision needed"))
        .collect();
    (recorded, comments)
}

fn readiness_entry(readiness: &Value, task: &str) -> Value {
    readiness["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["task_id"] == task)
        .cloned()
        .unwrap_or_else(|| panic!("{task} missing from readiness: {readiness}"))
}

/// The cycle is named on every pass with both tasks, the selector, the
/// failure evidence and the owner, and one operator request is recorded on
/// the repair across repeated passes. The lock still holds and the PR keeps
/// its selectors. An ordinary overlap and a sweep repair of a different
/// failure keep the plain lock conflict.
fn a_red_pr_and_its_shared_base_repair_name_their_cycle_once() {
    let (_root, runtime, repo) = runtime();
    write_files(&repo, &["Cargo.lock", "src/feature.rs"]);
    let pr = red_pr(&runtime);
    let repair = sweep_repair(&runtime, "shared repair", SIGNATURE);
    let unrelated = sweep_repair(&runtime, "other failure", "error: a different failure");
    let ordinary = seed(&runtime, Seed::named("ordinary").files(&["Cargo.lock"]));

    for pass in 0..3 {
        let output = list_backlog_tasks(&runtime, json!({}));
        assert!(
            !admitted(&output).contains(&repair.id),
            "no overlapping write is granted: {output}"
        );
        let entry = excluded_entry(&output, &repair.id);
        assert_eq!(entry["reason"], "context_lock_conflict", "{entry}");
        assert_eq!(
            entry["conflicts"],
            json!([{"requested_file": "file:Cargo.lock", "locking_task_id": pr.id}]),
            "{entry}"
        );
        let detail = entry["detail"]
            .as_str()
            .unwrap_or_else(|| panic!("pass {pass}: the cycle is named: {entry}"));
        for needle in [
            repair.id.as_str(),
            pr.id.as_str(),
            "`file:Cargo.lock`",
            RECOVERY_RUN,
            SIGNATURE,
            "Check / Clippy / Test",
            &HEAD[..12],
            "owns the repair",
        ] {
            assert!(
                detail.contains(needle),
                "pass {pass}: detail names {needle}: {detail}"
            );
        }
        for plain in [&unrelated.id, &ordinary.id] {
            let entry = excluded_entry(&output, plain);
            assert_eq!(entry["reason"], "context_lock_conflict", "{entry}");
            assert!(
                entry.get("detail").is_none_or(Value::is_null),
                "an unproven overlap stays a plain conflict: {entry}"
            );
        }
        let (recorded, comments) = requests(&runtime, &repair.id);
        assert_eq!(
            (recorded, comments.len()),
            (1, 1),
            "pass {pass}: one request across repeated passes: {comments:?}"
        );
        assert!(
            comments[0].contains(&pr.id) && comments[0].contains("file:Cargo.lock"),
            "{}",
            comments[0]
        );
    }
    assert_eq!(requests(&runtime, &unrelated.id).0, 0);
    assert_eq!(requests(&runtime, &ordinary.id).0, 0);
    assert_eq!(requests(&runtime, &pr.id).0, 0);

    let after = runtime.get_task(&pr.id).unwrap();
    assert_eq!(
        after.status,
        TaskStatus::Review,
        "the PR's active work is untouched"
    );
    assert_eq!(
        after.context_files, pr.context_files,
        "no selector is dropped"
    );

    // Readiness observes the same wait with its structured evidence and
    // records nothing.
    let readiness = default_readiness(&runtime);
    let entry = readiness_entry(&readiness, &repair.id);
    assert_eq!(entry["reason"], "context_lock_conflict", "{entry}");
    let wait = &entry["shared_base_repair"];
    assert_eq!(wait["repair_task_id"], repair.id, "{entry}");
    assert_eq!(wait["holding_task_id"], pr.id, "{entry}");
    assert_eq!(wait["signature"], SIGNATURE, "{entry}");
    assert_eq!(wait["failed_head"], HEAD, "{entry}");
    assert_eq!(
        wait["selectors"][0]["widened_selector"], "file:Cargo.lock",
        "{entry}"
    );
    assert_eq!(
        wait["selectors"][0]["widened_by_run"], RECOVERY_RUN,
        "{entry}"
    );
    assert!(
        readiness_entry(&readiness, &ordinary.id)
            .get("shared_base_repair")
            .is_none()
    );
    assert_eq!(requests(&runtime, &repair.id).0, 1);
}

/// A new PR head pushed after the base-red record ends the proof, so the
/// repair reads as an ordinary conflict; once the PR leaves review its lock
/// frees and the repair is admitted normally.
fn the_cycle_ends_with_a_new_head_and_the_repair_admits_once_the_lock_frees() {
    let (_root, runtime, repo) = runtime();
    write_files(&repo, &["Cargo.lock", "src/feature.rs"]);
    let pr = red_pr(&runtime);
    let repair = sweep_repair(&runtime, "shared repair", SIGNATURE);
    let output = list_backlog_tasks(&runtime, json!({}));
    assert!(
        excluded_entry(&output, &repair.id)["detail"].is_string(),
        "{output}"
    );

    append(
        &runtime,
        &pr.id,
        PR_AUTO_FIX_PUSHED_EVENT,
        &json!({"pr_number": "9907", "from_head_sha": HEAD, "to_head_sha": "f".repeat(40)})
            .to_string(),
        None,
    );
    let output = list_backlog_tasks(&runtime, json!({}));
    let entry = excluded_entry(&output, &repair.id);
    assert!(
        entry.get("detail").is_none_or(Value::is_null),
        "a moved head proves no shared failure: {entry}"
    );

    release(&runtime, &pr.id);
    let output = list_backlog_tasks(&runtime, json!({}));
    assert!(
        admitted(&output).contains(&repair.id),
        "the freed lock admits the repair: {output}"
    );
}

crate::scenario_table!(
    shared_base_repair_scenarios,
    a_red_pr_and_its_shared_base_repair_name_their_cycle_once,
    the_cycle_ends_with_a_new_head_and_the_repair_admits_once_the_lock_frees,
);
