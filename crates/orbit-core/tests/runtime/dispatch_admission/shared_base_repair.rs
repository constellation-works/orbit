//! A red PR and the base repair of the failure it shares [ORB-25510].
//!
//! Replays the November 17 wait: auto-fix admitted a check repair of a PR
//! head (recording the failure it was handed), the repair widened the PR
//! task's footprint to `Cargo.lock` and could not clear the failure, so the
//! run held in run state with the task in review. The CI-failure sweep's
//! repair of that same failure on the base was then deferred behind the PR's
//! lock on every admission pass while the PR waited for that repair.
//!
//! The holder's records are written the way the auto-fix ledger writes them
//! (`apply_task_automation_update` with the typed decision), and the repair
//! is filed with the creation identity CI-failure filing records.

use chrono::Utc;
use orbit_core::application::task::TaskAddParams;
use orbit_core::{OrbitRuntime, TaskComplexity, TaskStatus};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::task::{
    CiFailureIdentity, CiFailureSource, ContextWideningStep, DecisionGate, DecisionRequest,
    DecisionRequestState, Task, TaskDecision, TaskHistoryEntry,
};
use orbit_types::workflow::{
    PR_AUTO_FIX_BASE_RED_EVENT, PR_AUTO_FIX_HEAD_FAILURE_EVENT, PR_AUTO_FIX_PUSHED_EVENT,
    PrAutoFixBaseRed, PrAutoFixFailedCheck, PrAutoFixHeadFailure,
};
use serde_json::{Value, json};
use tempfile::TempDir;

use super::{
    PressureProbe, Seed, admitted, default_readiness, excluded_entry, list_backlog_tasks, release,
    runtime, seed, write_files,
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

/// How the holder's head was proven to fail as the base does.
#[derive(Clone, Copy)]
enum Proof {
    /// The incident's shape: the failure its admitted check repair was handed.
    HeadFailure,
    /// A landing poll that read the base tip already red the same way.
    KnownRedBase,
}

/// The PR task in review. Auto-fix admitted a check repair of `HEAD` (and
/// recorded the failure it handed over), and that repair's recovery widened
/// the task to `Cargo.lock`.
fn red_pr(runtime: &OrbitRuntime, proof: Proof) -> Task {
    let pr = seed(
        runtime,
        Seed::named("red pr")
            .status(TaskStatus::Review)
            .files(&["src/feature.rs"]),
    );
    match proof {
        Proof::HeadFailure => {
            let record = PrAutoFixHeadFailure {
                pr_number: "9907".into(),
                failed_head: HEAD.into(),
                base_branch: Some("agent-main".into()),
                run_id: RECOVERY_RUN.into(),
                checks: vec![PrAutoFixFailedCheck {
                    check: "CI / Check / Clippy / Test".into(),
                    step: "cargo deny".into(),
                    signature: SIGNATURE.into(),
                }],
            };
            append(
                runtime,
                &pr.id,
                PR_AUTO_FIX_HEAD_FAILURE_EVENT,
                &record.history_note(),
                Some(TaskDecision::PrAutoFixHeadFailure(record)),
            );
        }
        Proof::KnownRedBase => {
            let record = PrAutoFixBaseRed::from_base_tip_evidence(&json!({
                "failed_head": HEAD,
                "landed_base": BASE,
                "base_branch": "agent-main",
                "source": "latest_push",
                "path": "known_red_base",
                "checks": [{
                    "failed_job": {"name": "CI / Check / Clippy / Test"},
                    "base_job": {"name": "CI / Check / Clippy / Test"},
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
        }
    }
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
    runtime.get_task(&pr.id).unwrap()
}

/// A runtime whose configuration names one human crew, `dk`.
fn runtime_with_human_crew() -> (TempDir, OrbitRuntime, std::path::PathBuf) {
    crate::scenarios::require_isolated_mutable_fixture("shared_base_repair::runtime");
    let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let repo = root.path().join("repo");
    let workspace = repo.join(".orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(
        global.join("config.toml"),
        "[crews.dk]\nkind = \"human\"\ndescription = \"The operator\"\n",
    )
    .unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace)
        .expect("build runtime")
        .with_host_resource_probe(PressureProbe::calm());
    (root, runtime, repo)
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

/// The `shared_base_repair_wait` entries recorded on `task`.
fn recorded_waits(runtime: &OrbitRuntime, task: &str) -> usize {
    runtime
        .get_task_history(task)
        .unwrap()
        .iter()
        .filter(|entry| entry.event == "shared_base_repair_wait")
        .count()
}

/// Every open agent-raised decision request in the workspace.
fn open_requests(runtime: &OrbitRuntime) -> Vec<DecisionRequest> {
    runtime
        .list_decision_requests(Some(DecisionGate::AgentRequest), None, false)
        .unwrap()
        .into_iter()
        .filter(|request| request.state == DecisionRequestState::Open)
        .collect()
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
    let (_root, runtime, repo) = runtime_with_human_crew();
    write_files(&repo, &["Cargo.lock", "src/feature.rs"]);
    let pr = red_pr(&runtime, Proof::HeadFailure);
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
            "CI / Check / Clippy / Test",
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
        assert_eq!(
            recorded_waits(&runtime, &repair.id),
            1,
            "pass {pass}: one recorded wait across repeated passes"
        );
        let open = open_requests(&runtime);
        assert_eq!(
            open.len(),
            1,
            "pass {pass}: exactly one answerable request: {open:?}"
        );
        let request = &open[0];
        let question = request.summary.as_deref().unwrap_or_default();
        for needle in [repair.id.as_str(), pr.id.as_str(), "file:Cargo.lock"] {
            assert!(
                question.contains(needle),
                "the request names {needle}: {question}"
            );
        }
        let holder = runtime.get_task(&request.task_id).unwrap();
        assert_eq!(
            holder.crew.as_deref(),
            Some("dk"),
            "a human crew answers it"
        );
        assert!(
            detail.contains(&request.task_id) && detail.contains(&request.id),
            "pass {pass}: the detail names the decision to answer: {detail}"
        );
    }
    assert_eq!(recorded_waits(&runtime, &unrelated.id), 0);
    assert_eq!(recorded_waits(&runtime, &ordinary.id), 0);
    assert_eq!(recorded_waits(&runtime, &pr.id), 0);

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
    assert_eq!(recorded_waits(&runtime, &repair.id), 1);
    assert_eq!(open_requests(&runtime).len(), 1);
}

/// A landing that recorded the base tip red the same way proves the cycle
/// too. Without a human crew no request can be held: the detail says so and
/// nothing is filed or recorded, while the lock still holds.
fn a_known_red_base_proves_the_cycle_and_no_human_crew_files_nothing() {
    let (_root, runtime, repo) = runtime();
    write_files(&repo, &["Cargo.lock", "src/feature.rs"]);
    let pr = red_pr(&runtime, Proof::KnownRedBase);
    let repair = sweep_repair(&runtime, "shared repair", SIGNATURE);
    let tasks_before = runtime.list_tasks().unwrap().len();
    for _ in 0..2 {
        let output = list_backlog_tasks(&runtime, json!({}));
        assert!(!admitted(&output).contains(&repair.id), "{output}");
        let detail = excluded_entry(&output, &repair.id)["detail"]
            .as_str()
            .unwrap_or_else(|| panic!("the cycle is named: {output}"))
            .to_string();
        for needle in [
            pr.id.as_str(),
            &BASE[..12],
            "no owner decision is recorded yet",
        ] {
            assert!(detail.contains(needle), "detail names {needle}: {detail}");
        }
    }
    assert_eq!(recorded_waits(&runtime, &repair.id), 0);
    assert!(open_requests(&runtime).is_empty());
    assert_eq!(runtime.list_tasks().unwrap().len(), tasks_before);
}

/// A new PR head pushed after the head-failure record ends the proof, so the
/// repair reads as an ordinary conflict; once the PR leaves review its lock
/// frees and the repair is admitted normally.
fn the_cycle_ends_with_a_new_head_and_the_repair_admits_once_the_lock_frees() {
    let (_root, runtime, repo) = runtime();
    write_files(&repo, &["Cargo.lock", "src/feature.rs"]);
    let pr = red_pr(&runtime, Proof::HeadFailure);
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
    a_known_red_base_proves_the_cycle_and_no_human_crew_files_nothing,
    the_cycle_ends_with_a_new_head_and_the_repair_admits_once_the_lock_frees,
);
