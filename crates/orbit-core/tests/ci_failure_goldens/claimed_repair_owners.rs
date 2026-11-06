//! A follower claim on a matching repair owner must not abort a CI sweep: the
//! observation waits for settlement and independent failures still file.

use super::*;

const LOG_PATH: &str = r"crates\orbit-core\src\application\distributed\entry.rs";
const LOG_MESSAGE: &str = "cannot find `git_sandbox` in `runtime`";
const INDEPENDENT: &str = "error: independent landing regression";
const REPAIRED_DETAIL: &str = "error: repair-covered regression at this checkout";

fn receipts(runtime: &OrbitRuntime, owner: &str, prefix: &str) -> Vec<String> {
    runtime
        .get_task_artifacts(owner)
        .unwrap()
        .into_iter()
        .map(|artifact| artifact.path)
        .filter(|path| path.starts_with(prefix))
        .collect()
}

fn observation_comments(runtime: &OrbitRuntime, owner: &str) -> Vec<String> {
    runtime
        .get_task_comments(owner)
        .unwrap()
        .into_iter()
        .map(|comment| comment.message)
        .filter(|message| message.starts_with("CI compiler observation:"))
        .collect()
}

fn deferred_run_ids(output: &Value, owner: &str) -> Vec<Value> {
    assert_eq!(
        output["audit"]["deferred_attribution"], output["deferred_attribution"],
        "{output}"
    );
    output["deferred_attribution"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["task_id"] == owner && entry["reason"] == "claimed")
        .map(|entry| entry["run_id"].clone())
        .collect()
}

/// A compiler owner claimed by a follower, as filed by an earlier sweep.
fn claimed_compiler_owner(
    runtime: &OrbitRuntime,
    commit: &str,
) -> (String, orbit_store::contracts::ExecutionClaim) {
    let log = compiler_log("E0433", LOG_MESSAGE, LOG_PATH, 277, 25);
    let first = file(runtime, vec![failure(&log, 0, commit)]);
    let owner = first["filed"][0]["task_id"].as_str().unwrap().to_string();
    runtime
        .update_task_as_human(
            &owner,
            TaskUpdateParams {
                plan: Some("Reproduce the compiler error and repair its source.".into()),
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .unwrap();
    let claim = protecting_branch_claim(runtime, &owner);
    (owner, claim)
}

/// The sweep failure of 2026-11-05: an open compiler owner under a follower
/// claim. Several observations of one cluster all land on settlement, and
/// neither a repeated sweep nor a later one adds a receipt or a comment.
#[test]
fn compiler_observation_on_a_claimed_owner_waits_for_settlement() {
    if !isolated(
        "claimed_repair_owners::compiler_observation_on_a_claimed_owner_waits_for_settlement",
    ) {
        return;
    }
    let (_root, runtime, commits) = operator_fixture("");
    let (owner, claim) = claimed_compiler_owner(&runtime, &commits[0]);
    let moved = compiler_log("E0433", LOG_MESSAGE, LOG_PATH, 280, 28);
    let runs = vec![
        failure(&moved, 1, &commits[1]),
        failure(&moved, 2, &commits[1]),
        failure(INDEPENDENT, 5, &commits[1]),
    ];
    let comments_before = observation_comments(&runtime, &owner);

    let output = file(&runtime, runs.clone());
    assert_eq!(output["filed_count"], 1, "{output}");
    assert_ne!(output["filed"][0]["task_id"], owner.as_str());
    let matched = &output["skipped_existing"][0];
    assert_eq!(matched["task_id"], owner.as_str(), "{output}");
    assert_eq!(matched["match_kind"], "open_compiler_diagnostics");
    assert_eq!(
        deferred_run_ids(&output, &owner),
        vec![json!(11), json!(12)],
        "{output}"
    );
    let prefix = "ci-compiler-observations/";
    assert!(receipts(&runtime, &owner, prefix).is_empty());
    assert_eq!(observation_comments(&runtime, &owner), comments_before);
    assert_eq!(deferred_rows(&runtime).len(), 2);

    let repeated = file(&runtime, runs.clone());
    assert_eq!(repeated["filed_count"], 0, "{repeated}");
    assert_eq!(deferred_run_ids(&repeated, &owner).len(), 2, "{repeated}");
    assert!(receipts(&runtime, &owner, prefix).is_empty());
    assert_eq!(deferred_rows(&runtime).len(), 2);

    release_claim(
        &runtime,
        &owner,
        &claim.claim_id,
        &claim.executed_on.machine_id,
        "release-compiler-owner",
        "follower finished",
    );
    assert_eq!(receipts(&runtime, &owner, prefix).len(), 2);
    let settled = observation_comments(&runtime, &owner);
    assert_eq!(settled.len(), comments_before.len() + 2, "{settled:?}");
    assert!(settled.iter().any(|comment| comment.contains("run `11`")));
    assert!(settled.iter().any(|comment| comment.contains("run `12`")));
    let authors = runtime.get_task_comments(&owner).unwrap();
    assert!(
        authors
            .iter()
            .filter(|comment| comment.message.contains("run `11`"))
            .all(|comment| comment.by == "system"),
        "an observation must not read as operator steering"
    );
    assert!(deferred_rows(&runtime).iter().all(|row| row.applied));

    let after = file(&runtime, runs);
    assert_eq!(after["filed_count"], 0, "{after}");
    assert!(deferred_run_ids(&after, &owner).is_empty(), "{after}");
    assert_eq!(receipts(&runtime, &owner, prefix).len(), 2);
    assert_eq!(observation_comments(&runtime, &owner), settled);
    assert_eq!(deferred_rows(&runtime).len(), 2);
}

/// Each half of a partly retained observation is completed on settlement
/// without repeating the half that already landed.
#[test]
fn partly_retained_compiler_observation_completes_on_settlement() {
    if !isolated(
        "claimed_repair_owners::partly_retained_compiler_observation_completes_on_settlement",
    ) {
        return;
    }
    use orbit_types::task::TaskArtifact;
    let (_root, runtime, commits) = operator_fixture("");
    let log = compiler_log("E0433", LOG_MESSAGE, LOG_PATH, 277, 25);
    let first = file(&runtime, vec![failure(&log, 0, &commits[0])]);
    let owner = first["filed"][0]["task_id"].as_str().unwrap().to_string();
    let moved = compiler_log("E0433", LOG_MESSAGE, LOG_PATH, 280, 28);
    let probe = file(&runtime, vec![failure(&moved, 1, &commits[1])]);
    let failure_key = probe["skipped_existing"][0]["failure_key"]
        .as_str()
        .unwrap()
        .to_string();
    let receipt_only = failure(&moved, 2, &commits[1]);
    let comment_only = failure(&moved, 3, &commits[1]);
    let receipt = format!(
        "{{\"run_id\":\"12\",\"run_url\":\"{}\",\"checkout\":\"{}\",\"failure_key\":\"{failure_key}\"}}",
        receipt_only["url"].as_str().unwrap(),
        commits[1]
    );
    let receipt_path = format!(
        "ci-compiler-observations/{}.json",
        orbit_common::security::release::sha256_hex(receipt.as_bytes())
    );
    let comment = format!(
        "CI compiler observation: run `13` {}; checkout `{}`; exact failure key `ci-failure:{failure_key}`.",
        comment_only["url"].as_str().unwrap(),
        commits[1]
    );
    runtime
        .update_task_as_human(
            &owner,
            TaskUpdateParams {
                comment: Some(comment.clone()),
                upsert_artifacts: vec![TaskArtifact::from_text(&receipt_path, &receipt)],
                plan: Some("Reproduce the compiler error and repair its source.".into()),
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .unwrap();
    let claim = protecting_branch_claim(&runtime, &owner);
    let prefix = "ci-compiler-observations/";
    let receipts_before = receipts(&runtime, &owner, prefix).len();
    let observations_before = observation_comments(&runtime, &owner);

    let output = file(&runtime, vec![receipt_only.clone(), comment_only.clone()]);
    assert_eq!(
        deferred_run_ids(&output, &owner),
        vec![json!(12), json!(13)],
        "{output}"
    );
    assert_eq!(receipts(&runtime, &owner, prefix).len(), receipts_before);
    assert_eq!(observation_comments(&runtime, &owner), observations_before);

    release_claim(
        &runtime,
        &owner,
        &claim.claim_id,
        &claim.executed_on.machine_id,
        "release-partial-owner",
        "follower finished",
    );
    assert_eq!(
        receipts(&runtime, &owner, prefix).len(),
        receipts_before + 1
    );
    let settled = observation_comments(&runtime, &owner);
    assert_eq!(settled.len(), observations_before.len() + 1, "{settled:?}");
    assert_eq!(
        settled
            .iter()
            .filter(|text| text.contains("run `12`"))
            .count(),
        1
    );
    assert_eq!(
        settled.iter().filter(|text| **text == comment).count(),
        1,
        "the comment that already landed is not repeated"
    );
    let after = file(&runtime, vec![receipt_only, comment_only]);
    assert!(deferred_run_ids(&after, &owner).is_empty(), "{after}");
    assert_eq!(observation_comments(&runtime, &owner), settled);
}

/// Make `owner` a completed repair whose evidence covers `run` at `tested`.
fn record_verified_repair(
    runtime: &OrbitRuntime,
    owner: &str,
    failure_key: &str,
    run: &Value,
    tested: &str,
    landed: &str,
) {
    use orbit_common::security::release::sha256_hex;
    use orbit_types::task::TaskArtifact;
    use orbit_types::workflow::{JobRunState, PipelineState};
    let mut job = runtime
        .insert_job_run(
            "task_pr_pipeline",
            1,
            chrono::Utc::now(),
            Some(json!({"task_ids": [owner]})),
            None,
        )
        .unwrap();
    job.state = JobRunState::Success;
    runtime
        .sqlite_store()
        .unwrap()
        .upsert_job_run_for_workspace(&runtime.workspace_id().unwrap(), &job, None)
        .unwrap();
    let mut state = PipelineState::new(
        job.run_id.clone(),
        job.job_id.clone(),
        json!({"task_ids": [owner]}),
    );
    state.record_step(
        0,
        JobRunState::Success,
        Some(json!({
            "phase": "complete",
            "merge": {"merged": true, "landed_commit": landed},
            "completed_task_ids": [owner],
        })),
        None,
    );
    runtime.write_run_state(&job.run_id, &state).unwrap();
    let record = |revision: &str, exit_code: i32, outcome: &str, output: &str| {
        json!({
            "schema_version": 1, "task_id": owner, "revision": revision,
            "command": ["cargo", "test"], "exit_code": exit_code, "outcome": outcome,
            "origin": "retrospective", "recorded_at": "2026-11-05T00:00:00Z", "output": output,
        })
        .to_string()
    };
    let before = record(tested, 101, "failed", REPAIRED_DETAIL);
    let after = record(landed, 0, "passed", "test result: ok");
    let assessment = json!({
        "schema_version": 1, "task_id": owner, "failure_key": failure_key,
        "delivery_run_id": job.run_id, "delivery_step_index": 0, "landed_revision": landed,
        "observations": [{
            "run_id": run["run_id"].to_string(), "job_id": run["job_id"].to_string(),
            "checkout": tested,
            "diagnostic_sha256": sha256_hex(run["log_excerpt"].as_str().unwrap().as_bytes()),
            "branch": "agent-main", "ref_kind": "integration",
        }],
        "before": {"path": "repair/before.json", "sha256": sha256_hex(before.as_bytes())},
        "after": {"path": "repair/after.json", "sha256": sha256_hex(after.as_bytes())},
        "command": ["cargo", "test"],
        "diagnostic_details": [REPAIRED_DETAIL],
        "coverage_reason": "The landed repair removes the failing call.",
    })
    .to_string();
    runtime
        .update_task_as_human(
            owner,
            TaskUpdateParams {
                // Only the verified evidence may cover it, not the brief's text.
                description: Some("Operator replaced the completed repair brief.".into()),
                plan: Some("Remove the failing call.".into()),
                upsert_artifacts: vec![
                    TaskArtifact::from_text("repair/before.json", before),
                    TaskArtifact::from_text("repair/after.json", after),
                    TaskArtifact::from_text("ci-repair-assessment.json", assessment),
                ],
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .unwrap();
    runtime
        .apply_task_automation_update(
            owner,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Done),
                job_run_id: Some(job.run_id.clone()),
                ..Default::default()
            },
        )
        .unwrap();
}

/// The same path's other unscoped receipt: a verified completed repair whose
/// owner a claim protects.
#[test]
fn repair_receipt_on_a_claimed_owner_waits_for_settlement() {
    if !isolated("claimed_repair_owners::repair_receipt_on_a_claimed_owner_waits_for_settlement") {
        return;
    }
    let (_root, runtime, commits) = operator_fixture("");
    let (tested, landed) = (&commits[0], &commits[1]);
    let mut branch = crate::git_test_support::command();
    let created = branch
        .args(["branch", "agent-main", landed])
        .current_dir(&runtime.paths().repo_root)
        .output()
        .unwrap();
    assert!(created.status.success(), "{created:?}");
    let mut run = failure(REPAIRED_DETAIL, 1, tested);
    run["current_ref_head_sha"] = json!(landed);
    let first = file(&runtime, vec![run.clone()]);
    let owner = first["filed"][0]["task_id"].as_str().unwrap().to_string();
    let failure_key = first["filed"][0]["failure_key"].as_str().unwrap();
    record_verified_repair(&runtime, &owner, failure_key, &run, tested, landed);
    let claim = insert_protecting_claim(&runtime, &owner);
    let prefix = "ci-repair-observations/";
    let runs = vec![run, failure(INDEPENDENT, 5, landed)];

    let output = file(&runtime, runs.clone());
    assert_eq!(output["filed_count"], 1, "{output}");
    assert_ne!(output["filed"][0]["task_id"], owner.as_str());
    assert_eq!(
        output["skipped_existing"][0]["match_kind"], "covered_by_repair",
        "{output}"
    );
    assert_eq!(
        deferred_run_ids(&output, &owner),
        vec![json!(11)],
        "{output}"
    );
    assert!(receipts(&runtime, &owner, prefix).is_empty());
    assert_eq!(deferred_rows(&runtime).len(), 1);

    let repeated = file(&runtime, runs.clone());
    assert_eq!(repeated["filed_count"], 0, "{repeated}");
    assert_eq!(deferred_run_ids(&repeated, &owner).len(), 1, "{repeated}");
    assert!(receipts(&runtime, &owner, prefix).is_empty());
    assert_eq!(deferred_rows(&runtime).len(), 1);

    // A claim settles only on an in-progress task; a completed owner carries
    // one when a reopen races the sweep. Replay that reopen under the same claim.
    let connection = rusqlite::Connection::open(runtime.global_root().join("orbit.db")).unwrap();
    connection
        .execute(
            "DELETE FROM task_coordination_rows WHERE row_id = ?1",
            rusqlite::params![claim.claim_id],
        )
        .unwrap();
    set_status(&runtime, &owner, TaskStatus::Proposed);
    protecting_branch_claim(&runtime, &owner);
    release_claim(
        &runtime,
        &owner,
        &claim.claim_id,
        &claim.executed_on.machine_id,
        "release-repair-owner",
        "follower finished",
    );
    assert_eq!(receipts(&runtime, &owner, prefix).len(), 1);
    assert!(deferred_rows(&runtime).iter().all(|row| row.applied));
    assert!(
        observation_comments(&runtime, &owner).is_empty(),
        "a repair receipt appends no comment"
    );
}
