//! Captured CI log shapes through the deterministic task-filing boundary.
#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used)]

orbit_common::isolate_test_process!();

use std::path::{Path, PathBuf};

use orbit_core::OrbitRuntime;
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::task::TaskComplexity;
use serde_json::{Value, json};
use tempfile::TempDir;

#[cfg(unix)]
#[path = "ci_failure_goldens/sweep.rs"]
mod sweep;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/ci_failure_goldens")
}

fn isolated(test: &str) -> bool {
    const MARKER: &str = "ORBIT_TEST_CI_LOG_GOLDEN_CHILD";
    if std::env::var_os(MARKER).is_some() {
        return true;
    }
    let home = TempDir::new().unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    command
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(MARKER, "1")
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path());
    let logs = TempDir::new().unwrap();
    let output = orbit_common::test_env::run_child_test(&mut command, test, logs.path());
    orbit_common::test_env::assert_child_test_passed(
        test,
        output.status,
        output.stdout,
        output.stderr,
    );
    false
}

fn failure(log: &str, index: usize, checkout: &str) -> Value {
    json!({
        "run_id": 10 + index, "job_id": 910 + index, "log_job_id": 910 + index,
        "checkout_identity": {"state": "observed", "provenance": {"job_id": 910 + index, "complete": true}},
        "workflow": "CI", "status": "completed", "conclusion": "failure", "event": "push",
        "url": format!("https://github.com/acme/orbit/actions/runs/{}", 10 + index),
        "created_at": "2026-09-07T07:24:42Z", "head_branch": "agent-main", "ref_kind": "integration",
        "event_reported_head_sha": checkout, "current_ref_head_sha": "1".repeat(40),
        "actual_checkout_shas": [checkout], "checkout_evidence": [format!("HEAD is now at {checkout}")],
        "checkout_evidence_scope": "all", "investigated": true, "log_excerpt": log,
        "log_truncated": false,
        "failed_jobs": [{"job_id": 910 + index, "name": "build", "conclusion": "failure",
            "failed_steps": [{"name": "Run CI", "conclusion": "failure"}]}]
    })
}

fn file(runtime: &OrbitRuntime, runs: Vec<Value>) -> Value {
    runtime.run_deterministic("file_ci_failure_tasks", &json!({}), &json!({"ci_evidence": {
        "schema_version": 2, "collected": true, "outcome_hint": "current_failures",
        "capability": {"available": true, "authenticated": true},
        "repository": {"name": "orbit", "full_name": "acme/orbit", "default_branch": "main"},
        "heads": [{"kind": "integration", "branch": "agent-main", "current_head_sha": "1".repeat(40)}],
        "latest_runs": runs.clone(), "current_failures": runs, "stale_or_superseded": [],
        "in_flight": [], "retryable_errors": [], "collected_at": "2026-09-07T08:00:00Z"
    }}), ToolContext::default()).expect("file CI failure")
}

fn compiler_log(code: &str, message: &str, path: &str, line: usize, column: usize) -> String {
    format!("error[{code}]: {message}\n   --> {path}:{line}:{column}\n")
}

#[test]
fn ci_open_compiler_owner_survives_checkout_and_coordinate_changes() {
    if !isolated("ci_open_compiler_owner_survives_checkout_and_coordinate_changes") {
        return;
    }
    use orbit_core::application::task::TaskUpdateParams;
    use orbit_engine::TaskAutomationUpdate;
    use orbit_types::task::TaskStatus;

    let path = r"crates\orbit-core\src\application\distributed\entry.rs";
    let message = "cannot find `git_sandbox` in `runtime`";
    let first_log = compiler_log("E0433", message, path, 277, 25);
    let second_log = compiler_log("E0433", message, path, 280, 28);
    for (status, legacy) in [
        (TaskStatus::Proposed, false),
        (TaskStatus::Backlog, false),
        (TaskStatus::InProgress, false),
        (TaskStatus::Review, false),
        (TaskStatus::Blocked, false),
        (TaskStatus::Proposed, true),
    ] {
        let (_root, runtime, commits) = operator_fixture("");
        let first = file(&runtime, vec![failure(&first_log, 0, &commits[0])]);
        let owner = first["filed"][0]["task_id"].as_str().unwrap();
        runtime
            .update_task_as_human(
                owner,
                TaskUpdateParams {
                    plan: Some("Reproduce the compiler error and repair its source.".into()),
                    ..Default::default()
                },
                "human:fixture".into(),
            )
            .unwrap();
        runtime
            .apply_task_automation_update(
                owner,
                TaskAutomationUpdate {
                    status: Some(status),
                    ..Default::default()
                },
            )
            .unwrap();
        if legacy {
            let description = runtime.get_task(owner).unwrap().description;
            runtime
                .update_task_as_human(
                    owner,
                    TaskUpdateParams {
                        description: Some(
                            description
                                .lines()
                                .filter(|line| {
                                    !line.starts_with("- Open compiler diagnostic set identity:")
                                })
                                .collect::<Vec<_>>()
                                .join("\n"),
                        ),
                        ..Default::default()
                    },
                    "human:fixture".into(),
                )
                .unwrap();
        }
        let before = runtime.get_task(owner).unwrap();
        let comments_before = runtime.get_task_comments(owner).unwrap().len();
        let second_run = failure(&second_log, 1, &commits[1]);
        let second = file(&runtime, vec![second_run.clone()]);
        assert_eq!(
            second["filed_count"], 0,
            "{status:?}, legacy={legacy}: {second}"
        );
        let matched = &second["skipped_existing"][0];
        assert_eq!(matched["task_id"], owner);
        assert_eq!(matched["match_kind"], "open_compiler_diagnostics");
        assert_ne!(matched["failure_key"], first["filed"][0]["failure_key"]);
        let comments = runtime.get_task_comments(owner).unwrap();
        assert_eq!(comments.len(), comments_before + 1);
        let observation = &comments.last().unwrap().message;
        assert!(observation.contains(&commits[1]), "{observation}");
        assert!(
            observation.contains(second_run["url"].as_str().unwrap()),
            "{observation}"
        );
        assert!(
            observation.contains(matched["failure_key"].as_str().unwrap()),
            "{observation}"
        );
        let after = runtime.get_task(owner).unwrap();
        assert_eq!(
            after.tags, before.tags,
            "the exact key is evidence, not rewritten"
        );
        assert_eq!(after.status, status);
        assert_eq!(after.description, before.description);
        let retry = file(&runtime, vec![second_run]);
        assert_eq!(retry["filed_count"], 0, "{retry}");
        assert_eq!(
            runtime.get_task_comments(owner).unwrap().len(),
            comments.len()
        );
        assert_eq!(runtime.list_tasks().unwrap().len(), 1);
    }

    // The first newly filed owner also covers a later cluster in this snapshot.
    let (_root, runtime, commits) = operator_fixture("");
    let snapshot = file(
        &runtime,
        vec![
            failure(&first_log, 0, &commits[0]),
            failure(&second_log, 1, &commits[1]),
        ],
    );
    assert_eq!(snapshot["filed_count"], 1, "{snapshot}");
    assert_eq!(
        snapshot["skipped_existing"][0]["task_id"],
        snapshot["filed"][0]["task_id"]
    );
    let owner = snapshot["filed"][0]["task_id"].as_str().unwrap();
    assert!(
        runtime.get_task_comments(owner).unwrap()[0]
            .message
            .contains(&commits[1])
    );
}

#[test]
fn ci_done_compiler_owner_does_not_cover_new_checkouts() {
    if !isolated("ci_done_compiler_owner_does_not_cover_new_checkouts") {
        return;
    }
    use orbit_engine::TaskAutomationUpdate;
    use orbit_types::task::TaskStatus;
    let first_log = compiler_log(
        "E0433",
        "cannot find `git_sandbox` in `runtime`",
        "crates/orbit-core/src/application/distributed/entry.rs",
        277,
        25,
    );
    let second_log = first_log.replace(":277:25", ":280:25");
    let (_root, runtime, commits) = operator_fixture("");
    let first = file(&runtime, vec![failure(&first_log, 0, &commits[0])]);
    let owner = first["filed"][0]["task_id"].as_str().unwrap();
    runtime
        .apply_task_automation_update(
            owner,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Done),
                ..Default::default()
            },
        )
        .unwrap();
    let second = file(&runtime, vec![failure(&second_log, 1, &commits[1])]);
    assert_eq!(second["filed_count"], 1, "{second}");
    assert_ne!(second["filed"][0]["task_id"], owner);
    assert!(runtime.get_task_comments(owner).unwrap().is_empty());
}

/// The compiler failure key embeds the observed checkout, so an operator's hold
/// must follow the location-free diagnostic set across agent-main advances.
#[test]
fn ci_operator_cover_on_a_compiler_owner_holds_at_a_later_checkout() {
    if !isolated("ci_operator_cover_on_a_compiler_owner_holds_at_a_later_checkout") {
        return;
    }
    use orbit_core::application::task::TaskAddParams;
    use orbit_types::task::TaskStatus;
    let first_log = compiler_log(
        "E0433",
        "cannot find `git_sandbox` in `runtime`",
        "crates/orbit-core/src/application/distributed/entry.rs",
        277,
        25,
    );
    let later_log = first_log.replace(":277:25", ":280:28");
    let (_root, runtime, commits) = operator_fixture("");
    let first = file(&runtime, vec![failure(&first_log, 0, &commits[0])]);
    let owner = first["filed"][0]["task_id"].as_str().unwrap();
    let cover = runtime
        .add_task(TaskAddParams {
            title: "Hand fix".into(),
            ..Default::default()
        })
        .unwrap();
    archive_owner(&runtime, owner, TaskStatus::Archived, Some(&cover.id));
    let tasks_before = runtime.list_tasks().unwrap().len();
    for (index, checkout) in [(1, &commits[1]), (2, &commits[2])] {
        let held = file(&runtime, vec![failure(&later_log, index, checkout)]);
        assert_eq!(held["filed_count"], 0, "{held}");
        assert_eq!(held["pilot_candidate_count"], 0, "{held}");
        assert_eq!(held["withheld"][0]["outcome"], "covered", "{held}");
        assert_eq!(held["withheld"][0]["reason"], "operator_cover_open");
        assert_eq!(held["withheld"][0]["owner"], owner);
        assert_eq!(held["withheld"][0]["cover"], cover.id);
        assert_ne!(
            held["withheld"][0]["failure_key"], first["filed"][0]["failure_key"],
            "the later checkout has its own exact key"
        );
    }
    assert_eq!(runtime.list_tasks().unwrap().len(), tasks_before);

    // A different diagnostic set is not covered by that owner.
    let other = compiler_log(
        "E0425",
        "cannot find value `foo` in this scope",
        "crates/orbit-core/src/application/distributed/entry.rs",
        300,
        10,
    );
    let unrelated = file(&runtime, vec![failure(&other, 3, &commits[2])]);
    assert_eq!(unrelated["filed_count"], 1, "{unrelated}");
}

#[test]
fn ci_operator_plain_archive_of_a_compiler_owner_suppresses_a_later_checkout() {
    if !isolated("ci_operator_plain_archive_of_a_compiler_owner_suppresses_a_later_checkout") {
        return;
    }
    use chrono::Duration;
    use orbit_types::task::TaskStatus;
    let first_log = compiler_log(
        "E0433",
        "cannot find `git_sandbox` in `runtime`",
        "crates/orbit-core/src/application/distributed/entry.rs",
        277,
        25,
    );
    let later_log = first_log.replace(":277:25", ":280:28");
    for (status, config, hours) in [
        (TaskStatus::Archived, "", 6),
        (
            TaskStatus::Rejected,
            "[ci_failure]\noperator_suppression_hours = 2\n",
            2,
        ),
    ] {
        let (_root, runtime, commits) = operator_fixture(config);
        let first = file(&runtime, vec![failure(&first_log, 0, &commits[0])]);
        let owner = first["filed"][0]["task_id"].as_str().unwrap();
        archive_owner(&runtime, owner, status, None);
        let decision_at = runtime
            .get_task_history(owner)
            .unwrap()
            .iter()
            .rev()
            .find(|entry| entry.to_status == Some(status))
            .unwrap()
            .at;
        let before = runtime
            .clone()
            .with_ci_failure_time(decision_at + Duration::hours(hours) - Duration::nanoseconds(1));
        let withheld = file(&before, vec![failure(&later_log, 1, &commits[1])]);
        assert_eq!(withheld["filed_count"], 0, "{status:?}: {withheld}");
        assert_eq!(withheld["pilot_candidate_count"], 0);
        assert_eq!(withheld["withheld"][0]["outcome"], "withheld");
        assert_eq!(withheld["withheld"][0]["reason"], "operator_archived");
        assert_eq!(withheld["withheld"][0]["owner"], owner);
        assert_ne!(
            withheld["withheld"][0]["failure_key"], first["filed"][0]["failure_key"],
            "the later checkout has its own exact key"
        );
        let at_boundary = runtime.with_ci_failure_time(decision_at + Duration::hours(hours));
        let released = file(&at_boundary, vec![failure(&later_log, 2, &commits[2])]);
        assert_eq!(released["filed_count"], 1, "{status:?}: {released}");
        assert_eq!(released["withheld"], json!([]));
    }
}

#[test]
fn ci_compiler_coverage_requires_the_same_complete_diagnostic_set() {
    if !isolated("ci_compiler_coverage_requires_the_same_complete_diagnostic_set") {
        return;
    }
    use orbit_core::application::task::TaskUpdateParams;
    let message = "cannot find `git_sandbox` in `runtime`";
    let path = "crates/orbit-core/src/application/distributed/entry.rs";
    let first_log = compiler_log("E0433", message, path, 277, 25);
    let shifted = compiler_log("E0433", message, path, 280, 25);
    let other = compiler_log(
        "E0425",
        "cannot find value `foo` in this scope",
        path,
        300,
        10,
    );
    for (label, original, observed, sweep_tag) in [
        (
            "code",
            first_log.clone(),
            compiler_log("E0425", message, path, 280, 25),
            true,
        ),
        (
            "path",
            first_log.clone(),
            compiler_log(
                "E0433",
                message,
                "crates/orbit-core/src/runtime/mod.rs",
                280,
                25,
            ),
            true,
        ),
        (
            "message",
            first_log.clone(),
            compiler_log(
                "E0433",
                "cannot find `other_sandbox` in `runtime`",
                path,
                280,
                25,
            ),
            true,
        ),
        (
            "superset",
            first_log.clone(),
            format!("{shifted}{other}"),
            true,
        ),
        (
            "subset",
            format!("{first_log}{other}"),
            shifted.clone(),
            true,
        ),
        ("manual owner", first_log.clone(), shifted.clone(), false),
    ] {
        let (_root, runtime, commits) = operator_fixture("");
        let first = file(&runtime, vec![failure(&original, 0, &commits[0])]);
        let owner = first["filed"][0]["task_id"].as_str().unwrap();
        if !sweep_tag {
            let tags = runtime
                .get_task(owner)
                .unwrap()
                .tags
                .into_iter()
                .filter(|tag| tag != "ci-failure-sweep")
                .collect();
            runtime
                .update_task_as_human(
                    owner,
                    TaskUpdateParams {
                        tags: Some(tags),
                        ..Default::default()
                    },
                    "human:fixture".into(),
                )
                .unwrap();
        }
        let second = file(&runtime, vec![failure(&observed, 1, &commits[1])]);
        assert_eq!(second["filed_count"], 1, "{label}: {second}");
        assert!(runtime.get_task_comments(owner).unwrap().is_empty());
    }

    // The identity comes from the full supplied diagnostic set, even when the
    // description window omits a later diagnostic. Older owners without the
    // new identity must fail closed if their excerpt cannot prove completeness.
    let (_root, runtime, commits) = operator_fixture("");
    let padding = "    source context\n".repeat(30);
    let first = file(
        &runtime,
        vec![failure(
            &format!("{first_log}{padding}{other}"),
            0,
            &commits[0],
        )],
    );
    let owner = first["filed"][0]["task_id"].as_str().unwrap();
    let second = file(
        &runtime,
        vec![failure(
            &format!("{shifted}{padding}{other}"),
            1,
            &commits[1],
        )],
    );
    assert_eq!(second["filed_count"], 0, "{second}");
    assert_eq!(second["skipped_existing"][0]["task_id"], owner);
    let description = runtime.get_task(owner).unwrap().description;
    runtime
        .update_task_as_human(
            owner,
            TaskUpdateParams {
                description: Some(
                    description
                        .lines()
                        .filter(|line| {
                            !line.starts_with("- Open compiler diagnostic set identity:")
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .unwrap();
    let subset = file(&runtime, vec![failure(&shifted, 2, &commits[2])]);
    assert_eq!(
        subset["filed_count"], 1,
        "a partial legacy excerpt cannot prove set equality: {subset}"
    );

    // Ordering and repeated occurrences are not differences in a diagnostic set.
    let (_root, runtime, commits) = operator_fixture("");
    let first = file(
        &runtime,
        vec![failure(&format!("{first_log}{other}"), 0, &commits[0])],
    );
    let second = file(
        &runtime,
        vec![failure(
            &format!("{other}{shifted}{shifted}"),
            1,
            &commits[1],
        )],
    );
    assert_eq!(second["filed_count"], 0, "{second}");
    assert_eq!(
        second["skipped_existing"][0]["task_id"],
        first["filed"][0]["task_id"]
    );
}

fn operator_fixture(config: &str) -> (TempDir, OrbitRuntime, Vec<String>) {
    let root = TempDir::new().unwrap();
    let global = root.path().join("home/.orbit");
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(repo.join(".orbit")).unwrap();
    std::fs::write(repo.join(".orbit/config.toml"), config).unwrap();
    let commits = commit_chain(&repo, 3);
    let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit")).unwrap();
    (root, runtime, commits)
}

fn archive_owner(
    runtime: &OrbitRuntime,
    id: &str,
    status: orbit_types::task::TaskStatus,
    cover: Option<&str>,
) {
    use orbit_core::application::task::TaskUpdateParams;
    use orbit_types::task::{TaskRelation, TaskRelationType};
    runtime
        .update_task_as_human(
            id,
            TaskUpdateParams {
                status: Some(status),
                relations: cover.map(|target| {
                    vec![TaskRelation {
                        relation_type: TaskRelationType::CoveredBy,
                        target: target.into(),
                    }]
                }),
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .unwrap();
}

#[test]
fn ci_operator_archive_and_reject_suppress_until_the_clock_boundary() {
    if !isolated("ci_operator_archive_and_reject_suppress_until_the_clock_boundary") {
        return;
    }
    use chrono::Duration;
    use orbit_types::task::TaskStatus;
    for (status, config, hours) in [
        (TaskStatus::Archived, "", 6),
        (
            TaskStatus::Rejected,
            "[ci_failure]\noperator_suppression_hours = 2\n",
            2,
        ),
    ] {
        let (_root, runtime, commits) = operator_fixture(config);
        let first = file(
            &runtime,
            vec![failure("error: store GC invariant broken", 0, &commits[0])],
        );
        let owner = first["filed"][0]["task_id"].as_str().unwrap();
        archive_owner(&runtime, owner, status, None);
        let decision_at = runtime
            .get_task_history(owner)
            .unwrap()
            .iter()
            .rev()
            .find(|entry| entry.to_status == Some(status))
            .unwrap()
            .at;
        let before = runtime
            .clone()
            .with_ci_failure_time(decision_at + Duration::hours(hours) - Duration::nanoseconds(1));
        let withheld = file(
            &before,
            vec![failure("error: store GC invariant broken", 1, &commits[2])],
        );
        assert_eq!(withheld["filed_count"], 0, "{withheld}");
        assert_eq!(withheld["pilot_candidate_count"], 0);
        assert_eq!(withheld["withheld"][0]["outcome"], "withheld");
        assert_eq!(withheld["withheld"][0]["reason"], "operator_archived");
        assert_eq!(withheld["withheld"][0]["owner"], owner);
        assert_eq!(
            withheld["withheld"][0]["failure_key"],
            first["filed"][0]["failure_key"]
        );
        assert_eq!(withheld["audit"]["withheld"], withheld["withheld"]);
        let at_boundary = runtime.with_ci_failure_time(decision_at + Duration::hours(hours));
        let released = file(
            &at_boundary,
            vec![failure("error: store GC invariant broken", 2, &commits[2])],
        );
        assert_eq!(released["filed_count"], 1, "{released}");
        assert_eq!(released["withheld"], json!([]));
    }
}

#[test]
fn ci_operator_task_cover_holds_until_a_checkout_contains_the_landing() {
    if !isolated("ci_operator_task_cover_holds_until_a_checkout_contains_the_landing") {
        return;
    }
    use orbit_core::application::task::TaskAddParams;
    use orbit_engine::TaskAutomationUpdate;
    use orbit_types::task::TaskStatus;
    let (_root, runtime, commits) = operator_fixture("");
    let log = "error: store GC invariant broken";
    let original = file(&runtime, vec![failure(log, 0, &commits[0])]);
    let owner = original["filed"][0]["task_id"].as_str().unwrap();
    let cover = runtime
        .add_task(TaskAddParams {
            title: "Hand fix".into(),
            ..Default::default()
        })
        .unwrap();
    archive_owner(&runtime, owner, TaskStatus::Archived, Some(&cover.id));
    let held = file(&runtime, vec![failure(log, 1, &commits[0])]);
    assert_eq!(held["filed_count"], 0, "{held}");
    assert_eq!(held["withheld"][0]["outcome"], "covered");
    assert_eq!(held["withheld"][0]["owner"], owner);
    assert_eq!(held["withheld"][0]["cover"], cover.id);
    runtime
        .apply_task_automation_update(
            &cover.id,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Done),
                ..Default::default()
            },
        )
        .unwrap();
    record_landing(&runtime, &cover.id, &commits[0], &commits[1]);
    let old = file(&runtime, vec![failure(log, 2, &commits[0])]);
    assert_eq!(old["filed_count"], 0, "{old}");
    assert_eq!(old["withheld"][0]["reason"], "awaiting_cover_commit");
    let reproduced = file(&runtime, vec![failure(log, 3, &commits[2])]);
    assert_eq!(reproduced["filed_count"], 1, "{reproduced}");
    let entry = &reproduced["filed"][0];
    assert_eq!(entry["failed_covers"][0]["cover"], cover.id);
    assert_eq!(entry["failed_covers"][0]["reason"], "cover_did_not_hold");
    let task = runtime
        .get_task(entry["task_id"].as_str().unwrap())
        .unwrap();
    assert!(task.description.contains(&cover.id));
    assert!(task.description.contains(&commits[1]));
}

/// A failed operator cover must not erase a later repair's ownership or
/// prevent stale failures from waiting on that repair's landing.
#[test]
fn ci_operator_failed_cover_preserves_a_second_landed_repair() {
    if !isolated("ci_operator_failed_cover_preserves_a_second_landed_repair") {
        return;
    }
    use orbit_core::application::task::TaskAddParams;
    use orbit_engine::TaskAutomationUpdate;
    use orbit_types::task::TaskStatus;

    // Exercise a Done repair match, a cross-job descendant landing, and a
    // Done failed cover that matches first but must yield to the later repair.
    for (cover_matches, other_job) in [(false, false), (false, true), (true, false)] {
        let (_root, runtime, mut commits) = operator_fixture("");
        commits.extend(commit_chain(&runtime.paths().repo_root, 2));
        let log = "error: store GC invariant broken";
        let original = file(&runtime, vec![failure(log, 0, &commits[0])]);
        let owner = original["filed"][0]["task_id"].as_str().unwrap();
        let cover = runtime
            .add_task(TaskAddParams {
                title: "Hand fix".into(),
                description: if cover_matches {
                    runtime.get_task(owner).unwrap().description
                } else {
                    String::new()
                },
                ..Default::default()
            })
            .unwrap();
        archive_owner(&runtime, owner, TaskStatus::Archived, Some(&cover.id));
        runtime
            .apply_task_automation_update(
                &cover.id,
                TaskAutomationUpdate {
                    status: Some(TaskStatus::Done),
                    ..Default::default()
                },
            )
            .unwrap();
        record_landing(&runtime, &cover.id, &commits[0], &commits[1]);

        let reproduced = file(&runtime, vec![failure(log, 1, &commits[2])]);
        assert_eq!(reproduced["filed_count"], 1, "{reproduced}");
        assert_eq!(
            reproduced["filed"][0]["failed_covers"][0]["cover"],
            cover.id
        );
        let repair = reproduced["filed"][0]["task_id"].as_str().unwrap();
        runtime
            .apply_task_automation_update(
                repair,
                TaskAutomationUpdate {
                    status: Some(TaskStatus::Done),
                    ..Default::default()
                },
            )
            .unwrap();
        record_landing(&runtime, repair, &commits[2], &commits[3]);

        let mut stale_run = failure(log, 2, &commits[2]);
        if other_job {
            stale_run["failed_jobs"][0]["name"] = json!("Coverage");
        }
        let stale = file(&runtime, vec![stale_run.clone()]);
        assert_eq!(stale["filed_count"], 0, "{stale}");
        assert_eq!(stale["pilot_candidate_count"], 0, "{stale}");
        assert_eq!(stale["withheld"], json!([]), "{stale}");
        if other_job || cover_matches {
            assert_eq!(stale["skipped_existing"], json!([]), "{stale}");
            let pending = &stale["pending_supersession"][0];
            assert_eq!(
                pending["reason"], "repaired_by_descendant_landing",
                "{stale}"
            );
            assert_eq!(pending["task_id"], repair);
            assert_eq!(pending["landed_commit"], commits[3]);
            assert_eq!(pending["tested_commit"], commits[2]);
            assert_eq!(stale["audit"]["pending_supersession_run_ids"], json!([12]));
        } else {
            assert_eq!(stale["pending_supersession"], json!([]), "{stale}");
            assert_eq!(stale["skipped_existing"][0]["task_id"], repair, "{stale}");
            assert_eq!(
                stale["skipped_existing"][0]["match_kind"],
                "material_coverage"
            );
        }
        // Repeated sweeps of the same stale observation remain idempotent.
        assert_eq!(file(&runtime, vec![stale_run])["filed_count"], 0);

        // A different job reproducing after both fixes still needs a repair.
        let mut after = failure(log, 3, &commits[4]);
        after["failed_jobs"][0]["name"] = json!("Coverage");
        let fresh = file(&runtime, vec![after]);
        assert_eq!(fresh["filed_count"], 1, "{fresh}");
        assert_eq!(fresh["pending_supersession"], json!([]));
    }
}

#[cfg(unix)]
#[test]
fn ci_operator_pr_cover_reports_open_landed_closed_and_unavailable_states() {
    if !isolated("ci_operator_pr_cover_reports_open_landed_closed_and_unavailable_states") {
        return;
    }
    use orbit_types::task::TaskStatus;
    use std::os::unix::fs::PermissionsExt;
    let (root, runtime, commits) = operator_fixture("");
    let bin = root.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let response = bin.join("response.json");
    let gh = bin.join("gh");
    std::fs::write(&gh, format!("#!/bin/sh\ncat '{}'\n", response.display())).unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    // This entire fixture is an isolated child; no other runtime sees this PATH.
    unsafe {
        std::env::set_var(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        );
    }
    let log = "error: store GC invariant broken";
    let first = file(&runtime, vec![failure(log, 0, &commits[0])]);
    let owner = first["filed"][0]["task_id"].as_str().unwrap();
    let cover = "github-pr:https://github.com/acme/orbit/pull/3854";
    archive_owner(&runtime, owner, TaskStatus::Rejected, Some(cover));
    for (state, checkout, reason, count) in [
        ("OPEN", &commits[2], "operator_cover_open", 0),
        ("UNKNOWN", &commits[2], "operator_cover_unavailable", 0),
        ("MERGED", &commits[0], "awaiting_cover_commit", 0),
        ("MERGED", &commits[2], "cover_did_not_hold", 1),
    ] {
        std::fs::write(
            &response,
            json!({"state": state, "mergeCommit": {"oid": commits[1]}}).to_string(),
        )
        .unwrap();
        let output = file(&runtime, vec![failure(log, 1, checkout)]);
        assert_eq!(output["filed_count"], count, "{output}");
        if count == 0 {
            assert_eq!(output["withheld"][0]["outcome"], "covered");
            assert_eq!(output["withheld"][0]["reason"], reason);
            assert_eq!(output["withheld"][0]["owner"], owner);
            assert_eq!(output["withheld"][0]["cover"], cover);
        } else {
            assert_eq!(output["filed"][0]["failed_covers"][0]["reason"], reason);
        }
    }
    // A closed, unmerged cover has no fix to wait for and does not suppress.
    let other = file(
        &runtime,
        vec![failure("error: different invariant broken", 2, &commits[0])],
    );
    let other_owner = other["filed"][0]["task_id"].as_str().unwrap();
    archive_owner(
        &runtime,
        other_owner,
        TaskStatus::Archived,
        Some("github-pr:3855"),
    );
    std::fs::write(&response, json!({"state": "CLOSED"}).to_string()).unwrap();
    let closed = file(
        &runtime,
        vec![failure("error: different invariant broken", 3, &commits[2])],
    );
    assert_eq!(closed["filed_count"], 1, "{closed}");
}

#[test]
fn ci_equal_test_signatures_share_one_task_across_job_and_step_wrappers() {
    if !isolated("ci_equal_test_signatures_share_one_task_across_job_and_step_wrappers") {
        return;
    }
    let (_root, runtime, commits) = operator_fixture("");
    let log = "thread 'store_gc' panicked at tests/store_gc.rs:42:3:\nassertion failed: live store is retained\ntest store_gc ... FAILED";
    let first = failure(log, 0, &commits[0]);
    let mut coverage = failure(log, 1, &commits[0]);
    coverage["run_id"] = first["run_id"].clone();
    coverage["url"] = first["url"].clone();
    coverage["failed_jobs"][0]["name"] = json!("Coverage");
    coverage["failed_jobs"][0]["failed_steps"][0]["name"] = json!("Collect coverage");
    let filed = file(&runtime, vec![first.clone(), coverage.clone()]);
    assert_eq!(filed["filed_count"], 1, "{filed}");
    assert_eq!(filed["filed"][0]["jobs"], json!(["Coverage", "build"]));
    assert_eq!(filed["filed"][0]["sources"].as_array().unwrap().len(), 2);
    let task = runtime
        .get_task(filed["filed"][0]["task_id"].as_str().unwrap())
        .unwrap();
    assert!(task.description.contains("Coverage") && task.description.contains("build"));
    let repeat = file(&runtime, vec![coverage, first]);
    assert_eq!(repeat["filed_count"], 0, "{repeat}");
    assert_eq!(
        repeat["skipped_existing"][0]["failure_key"],
        filed["filed"][0]["failure_key"]
    );
    let distinct = file(
        &runtime,
        vec![failure(
            "thread 'another_test' panicked at tests/other.rs:1:1:\nassertion failed: another invariant\ntest another_test ... FAILED",
            2,
            &commits[0],
        )],
    );
    assert_eq!(distinct["filed_count"], 1, "{distinct}");

    // A persisted pre-migration per-job tag still owns the new test key.
    // This is the shipped key for CI/build/Run CI and this fixture diagnostic.
    use orbit_core::application::task::TaskAddParams;
    use orbit_types::task::{CI_FAILURE_KEY_TAG_PREFIX, TaskStatus};
    let (_legacy_root, legacy_runtime, legacy_commits) = operator_fixture("");
    let legacy = legacy_runtime
        .add_task(TaskAddParams {
            title: "Persisted sweep finding".into(),
            tags: vec![
                "ci-failure-sweep".into(),
                format!("{CI_FAILURE_KEY_TAG_PREFIX}5e1a7df48e870766"),
            ],
            system_created: true,
            ..Default::default()
        })
        .unwrap();
    archive_owner(&legacy_runtime, &legacy.id, TaskStatus::Archived, None);
    let legacy_hold = file(&legacy_runtime, vec![failure(log, 3, &legacy_commits[2])]);
    assert_eq!(legacy_hold["filed_count"], 0, "{legacy_hold}");
    assert_eq!(legacy_hold["withheld"][0]["owner"], legacy.id);
    assert_eq!(
        legacy_hold["withheld"][0]["failure_key"],
        filed["filed"][0]["failure_key"]
    );
}

#[test]
fn ci_main_thread_panic_does_not_match_agent_main_task() {
    if !isolated("ci_main_thread_panic_does_not_match_agent_main_task") {
        return;
    }
    use orbit_core::application::task::TaskAddParams;

    let (_root, runtime, commits) = operator_fixture("");
    let unrelated = runtime
        .add_task(TaskAddParams {
            title: "Review agent-main delivery workflow".into(),
            ..Default::default()
        })
        .unwrap();
    let output = file(
        &runtime,
        vec![failure(
            "thread 'main' panicked at build.rs:3:5:",
            0,
            &commits[0],
        )],
    );

    assert_eq!(output["filed_count"], 1, "{output}");
    assert_eq!(output["skipped_existing"], json!([]), "{output}");
    assert_ne!(
        output["filed"][0]["task_id"], unrelated.id,
        "a generic panic thread must not make an unrelated agent-main task cover the failure"
    );
}

/// Hand-built collection rows: the log is the snapshot's excerpt.
fn filed_golden(case: &Value) -> Value {
    let root = TempDir::new().unwrap();
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let mut results = Vec::new();
    let logs = case["logs"].as_array().unwrap();
    let first_batch = case["first_batch"].as_u64().unwrap_or(1) as usize;
    let split = first_batch.min(logs.len());
    let groups = std::iter::once(&logs[..split]).chain(logs[split..].chunks(1));
    let mut run_index = 0;
    for (group_index, logs) in groups.enumerate() {
        let checkout = if group_index == 0 {
            "3".repeat(40)
        } else {
            "4".repeat(40)
        };
        let runs = logs
            .iter()
            .map(|log| {
                let run = failure(log.as_str().unwrap(), run_index, &checkout);
                run_index += 1;
                run
            })
            .collect();
        let output = file(&runtime, runs);
        results.push(filed_result(&runtime, &output));
    }
    json!(results)
}

/// Each log is served by a substitute `gh` as one failed job's `--log-failed`
/// output and goes through host collection before filing, so the golden
/// records the diagnostic collection bound as well as what was filed.
#[cfg(unix)]
fn collected_golden(case: &Value) -> Value {
    let mut workspace = sweep::Workspace::new();
    let mut results = Vec::new();
    for (index, parts) in case["logs"].as_array().unwrap().iter().enumerate() {
        let log = expand_parts(parts);
        let run_id = 7_000 + index as u64;
        workspace.push_red_run(run_id, 9_000 + index as u64, &log);
        let (evidence, filed) = workspace.sweep();
        let output = filed.unwrap_or_else(|error| panic!("file CI failure: {error}"));
        let finding = evidence["current_failures"]
            .as_array()
            .unwrap()
            .iter()
            .find(|failure| failure["run_id"] == json!(run_id))
            .unwrap_or_else(|| panic!("run {run_id} is current: {:#}", evidence["summary"]));
        let unit = &finding["diagnostic_unit"];
        let mut result = filed_result(&workspace.runtime, &output);
        result["collected"] = json!({
            "source_bytes": log.len(),
            "log_truncated": finding["log_truncated"],
            "retryable_errors": evidence["retryable_errors"],
            "diagnostic_kind": unit["kind"],
            "diagnostic_step": unit["step"],
            "step_attribution": unit["step_attribution"],
            "failure_anchor_count": unit["failure_anchor_count"],
        });
        results.push(result);
    }
    json!(results)
}

/// Concatenate fixture parts: strings and `{"text", "repeat"}` segments.
fn expand_parts(parts: &Value) -> String {
    parts
        .as_array()
        .unwrap()
        .iter()
        .map(|part| match part {
            Value::String(text) => text.clone(),
            segment => segment["text"]
                .as_str()
                .unwrap()
                .repeat(segment["repeat"].as_u64().unwrap() as usize),
        })
        .collect()
}

fn filed_result(runtime: &OrbitRuntime, output: &Value) -> Value {
    let mut tasks = Vec::new();
    for entry in output["filed"].as_array().unwrap() {
        let task = runtime
            .get_task(entry["task_id"].as_str().unwrap())
            .unwrap();
        let signature = task
            .description
            .lines()
            .find(|line| line.starts_with("- Normalized error signature"))
            .unwrap();
        let section = task
            .description
            .split("## Failed-step log excerpt\n")
            .nth(1)
            .unwrap()
            .split("\n## ")
            .next()
            .unwrap();
        let (excerpt, after_excerpt) = section
            .split_once("```\n")
            .unwrap()
            .1
            .split_once("\n```")
            .unwrap();
        tasks.push(json!({
            "failure_key": entry["failure_key"],
            "signature": signature.split_once('`').unwrap().1.rsplit_once('`').unwrap().0,
            "step_fallback": signature.contains("step-name fallback"),
            "excerpt": excerpt,
            "excerpt_has_note": after_excerpt.trim_start().starts_with('_'),
        }));
    }
    let skipped: Vec<_> = output["skipped_existing"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["failure_key"].clone())
        .collect();
    json!({"filed_count": output["filed_count"], "tasks": tasks, "skipped_keys": skipped})
}

#[test]
#[cfg(unix)]
fn ci_failure_fixture_goldens() {
    if !sweep::isolated_with_fake_gh("ci_failure_fixture_goldens") {
        return;
    }
    let cases: Vec<Value> =
        serde_json::from_str(&std::fs::read_to_string(fixtures().join("fixtures.json")).unwrap())
            .unwrap();
    let update = std::env::var_os("ORBIT_UPDATE_LOG_GOLDENS").is_some();
    let expected: Value = if update {
        json!({})
    } else {
        serde_json::from_str(&std::fs::read_to_string(fixtures().join("parsed.json")).unwrap())
            .unwrap()
    };
    // Each case files through a fresh runtime. All of them can outlast the
    // child's hang guard on a saturated host; name the case reached.
    let mut progress = orbit_common::test_env::FixtureProgress::start("CI log goldens");
    progress.phase("cases", cases.len());
    let mut rendered = serde_json::Map::new();
    for case in cases {
        let actual = if case["collect"] == true {
            collected_golden(&case)
        } else {
            filed_golden(&case)
        };
        let name = case["name"].as_str().unwrap();
        if !update {
            assert_eq!(
                actual, expected[name],
                "CI log fixture {name}; regenerate with make goldens UPDATE=1"
            );
        }
        rendered.insert(name.to_string(), actual);
        progress.advance();
    }
    progress.finish();
    if update {
        std::fs::write(
            fixtures().join("parsed.json"),
            serde_json::to_string_pretty(&rendered).unwrap() + "\n",
        )
        .unwrap();
    } else {
        assert_eq!(
            rendered.len(),
            expected.as_object().unwrap().len(),
            "all CI golden cases must run"
        );
    }
}

#[test]
fn ci_failure_branch_routing_retains_owner_evidence_and_only_files_landing_checkouts() {
    if !isolated(
        "ci_failure_branch_routing_retains_owner_evidence_and_only_files_landing_checkouts",
    ) {
        return;
    }
    use orbit_core::application::task::TaskAddParams;

    let root = TempDir::new().unwrap();
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let owner = runtime
        .add_task(TaskAddParams {
            title: "Sandbox implementation".into(),
            ..Default::default()
        })
        .unwrap();
    let branch = format!("orbit/{}-ddb04571", owner.id);
    let mut pr = failure("error: sandbox directory escaped", 0, &"3".repeat(40));
    pr["event"] = json!("pull_request");
    pr["head_branch"] = json!(branch);
    pr["ref_kind"] = json!("pull_request");
    pr["pr_number"] = json!(3140);

    // Legacy schema-2 collectors put PR failures in current_failures too.
    let first = file(&runtime, vec![pr.clone()]);
    assert_eq!(
        first["filed_count"], 0,
        "unmerged task PRs cannot mint landing repairs"
    );
    assert_eq!(first["pilot_candidate_count"], 0);
    assert_eq!(first["attributed"][0]["task_id"], owner.id);
    let path = first["attributed"][0]["artifact"].as_str().unwrap();
    let receipt = runtime.get_task_artifact(&owner.id, path).unwrap().unwrap();
    let retained: Value = serde_json::from_slice(&receipt.content).unwrap();
    assert_eq!(retained["failure"], pr);
    assert_eq!(runtime.get_task(&owner.id).unwrap().status, owner.status);
    let repeated = file(&runtime, vec![pr.clone()]);
    assert_eq!(repeated["attributed"], first["attributed"]);
    assert_eq!(runtime.get_task_artifacts(&owner.id).unwrap().len(), 1);

    // The new collector's separate branch_failures partition uses the same route.
    let output = runtime.run_deterministic("file_ci_failure_tasks", &json!({}), &json!({
        "ci_evidence": {
            "schema_version": 2, "collected": true,
            "heads": [{"kind": "integration", "branch": "agent-main", "current_head_sha": "1".repeat(40)}],
            "current_failures": [], "branch_failures": [pr.clone()],
        }
    }), ToolContext::default()).unwrap();
    assert_eq!(output["attributed"], first["attributed"]);
    assert_eq!(output["filed_count"], 0);

    let push = failure("error: independent landing regression", 1, &"4".repeat(40));
    let output = file(&runtime, vec![pr, push.clone()]);
    assert_eq!(
        output["filed_count"], 1,
        "push failures still file landing repairs"
    );
    assert_eq!(output["attributed"].as_array().unwrap().len(), 1);
    let repeated = file(&runtime, vec![push]);
    assert_eq!(repeated["filed_count"], 0);
    assert_eq!(repeated["skipped_existing"].as_array().unwrap().len(), 1);

    // A PR source SHA equalling the tip is insufficient: its *checkout* must match.
    for (index, event, checkout, expected) in [
        (2, "pull_request", '5', 0),
        (3, "pull_request", '1', 1),
        (4, "merge_group", '1', 1),
        (5, "merge_group", '6', 0),
        (6, "push", '7', 0),
    ] {
        let mut finding = failure(
            &format!("error: routing case {index}"),
            index,
            &checkout.to_string().repeat(40),
        );
        finding["event"] = json!(event);
        finding["event_reported_head_sha"] = json!("1".repeat(40));
        if event == "merge_group" {
            finding["head_branch"] = json!("gh-readonly-queue/agent-main/pr-3140");
            finding["ref_kind"] = json!("other");
        }
        let output = file(&runtime, vec![finding]);
        assert_eq!(
            output["filed_count"].as_u64().unwrap()
                + output["skipped_existing"].as_array().unwrap().len() as u64,
            expected,
            "event {event}, checkout {checkout}"
        );
    }

    // A scheduled or dispatched run builds its branch tip as of the trigger,
    // so like a push it files against the older landing commit it reports.
    for (index, event) in [(8, "schedule"), (9, "workflow_dispatch")] {
        let older = index.to_string().repeat(40);
        let mut finding = failure(&format!("error: {event} landing regression"), index, &older);
        finding["event"] = json!(event);
        let output = file(&runtime, vec![finding]);
        assert_eq!(output["filed_count"], 1, "event {event}: {output}");
    }

    // A newer green scheduled run of the same workflow supersedes a red one,
    // as a newer green push does.
    let mut red = failure("error: scheduled coverage regression", 10, &"1".repeat(40));
    red["event"] = json!("schedule");
    red["workflow"] = json!("Coverage");
    let mut green = red.clone();
    green["run_id"] = json!(30);
    green["conclusion"] = json!("success");
    green["created_at"] = json!("2026-09-07T07:54:42Z");
    let output = runtime.run_deterministic("file_ci_failure_tasks", &json!({}), &json!({
        "ci_evidence": {
            "schema_version": 2, "collected": true,
            "heads": [{"kind": "integration", "branch": "agent-main", "current_head_sha": "1".repeat(40)}],
            "latest_runs": [red.clone(), green], "current_failures": [red],
        }
    }), ToolContext::default()).unwrap();
    assert_eq!(output["filed_count"], 0, "{output}");
    assert_eq!(output["already_repaired"][0]["run_id"], 20, "{output}");

    let mut orphan = failure("error: unmatched branch failure", 7, &"8".repeat(40));
    orphan["event"] = json!("pull_request");
    orphan["head_branch"] = json!("orbit/ORB-999999999-deadbeef");
    let error = runtime.run_deterministic("file_ci_failure_tasks", &json!({}), &json!({
        "ci_evidence": {"schema_version": 2, "collected": true, "current_failures": [orphan]}
    }), ToolContext::default()).unwrap_err();
    assert!(
        error.to_string().contains("task_branch_owner"),
        "missing owners remain retryable rather than minting repairs"
    );
}

#[test]
fn ci_failure_branch_routing_retains_evidence_for_configurable_task_prefixes() {
    if !isolated("ci_failure_branch_routing_retains_evidence_for_configurable_task_prefixes") {
        return;
    }
    use orbit_core::application::task::TaskAddParams;
    use orbit_core::bootstrap::task_migration::seed_task_id_start;

    for prefix in ["DE", "ORBA", "ORBX", "ABCDE"] {
        let root = TempDir::new().unwrap();
        let global = root.path().join("home/.orbit");
        let workspace = root.path().join("repo/.orbit");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        seed_task_id_start(&global, Some(prefix), 1).unwrap();
        let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
        let owner = runtime
            .add_task(TaskAddParams {
                title: "Task branch owner".into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(orbit_types::task::task_id_prefix(&owner.id), Some(prefix));

        let mut pr = failure("error: task branch regression", 0, &"3".repeat(40));
        pr["event"] = json!("pull_request");
        pr["head_branch"] = json!(format!("orbit/{}-ddb04571", owner.id));
        pr["ref_kind"] = json!("pull_request");

        let first = file(&runtime, vec![pr.clone()]);
        assert_eq!(first["filed_count"], 0, "{prefix}: {first}");
        assert_eq!(first["pilot_candidate_count"], 0);
        assert_eq!(first["excluded_branch_failures"], json!([]));
        assert_eq!(first["attributed"].as_array().unwrap().len(), 1);
        assert_eq!(first["attributed"][0]["task_id"], owner.id);
        let path = first["attributed"][0]["artifact"].as_str().unwrap();
        let artifact = runtime.get_task_artifact(&owner.id, path).unwrap().unwrap();
        let retained: Value = serde_json::from_slice(&artifact.content).unwrap();
        assert_eq!(retained["failure"], pr);
        assert_eq!(runtime.get_task(&owner.id).unwrap().status, owner.status);

        // Replaying in the collector's branch partition retains the same receipt.
        let repeated = runtime.run_deterministic("file_ci_failure_tasks", &json!({}), &json!({
            "ci_evidence": {
                "schema_version": 2, "collected": true,
                "heads": [{"kind": "integration", "branch": "agent-main", "current_head_sha": "1".repeat(40)}],
                "current_failures": [], "branch_failures": [pr],
            }
        }), ToolContext::default()).unwrap();
        assert_eq!(repeated["attributed"], first["attributed"]);
        assert_eq!(repeated["excluded_branch_failures"], json!([]));
        assert_eq!(runtime.get_task_artifacts(&owner.id).unwrap().len(), 1);
    }
}

fn protecting_branch_claim(
    runtime: &OrbitRuntime,
    owner_id: &str,
) -> orbit_store::contracts::ExecutionClaim {
    use orbit_engine::TaskAutomationUpdate;
    use orbit_store::contracts::{
        AdmissionRunContext, ExecutionClaim, ExecutionClaimPhase, ExecutionLocation,
    };
    use orbit_types::task::TaskStatus;

    let claim_id = format!("claim-{owner_id}");
    let request_id = format!("request-{owner_id}");
    let claiming_run = "jrun-follower-claim";
    let machine_id = "follower-mac";
    runtime
        .apply_task_automation_update(
            owner_id,
            TaskAutomationUpdate {
                expected_status: Some(TaskStatus::Proposed),
                status: Some(TaskStatus::InProgress),
                status_event: Some("pulled_by".into()),
                status_note: Some(
                    json!({
                        "machine_id": machine_id,
                        "run_context": {"run_id": claiming_run},
                        "claim_id": claim_id,
                        "request_id": request_id,
                    })
                    .to_string(),
                ),
                ..Default::default()
            },
        )
        .unwrap();
    let claim = ExecutionClaim {
        claim_id: claim_id.clone(),
        task_id: owner_id.to_string(),
        request_id: request_id.clone(),
        executed_on: ExecutionLocation {
            machine_id: machine_id.into(),
            machine_name: None,
        },
        run_context: AdmissionRunContext {
            run_id: claiming_run.into(),
            job_name: "task_claimed_pr_pipeline".into(),
            machine_name: None,
        },
        footprint: Vec::new(),
        reservation_id: format!("reservation-{owner_id}"),
        reservation_expires_at: "2099-01-01T00:00:00Z".into(),
        phase: ExecutionClaimPhase::Claimed,
        repair: None,
    };
    let workspace_id = runtime.workspace_id().unwrap();
    {
        let connection =
            rusqlite::Connection::open(runtime.global_root().join("orbit.db")).unwrap();
        connection
            .execute(
                "INSERT INTO task_coordination_rows(workspace_id, kind, row_id, payload_json, journal_id, created_at)
                 VALUES (?1, 'distributed-execution-claim-v1', ?2, ?3, 'fixture', ?4)",
                rusqlite::params![
                    workspace_id,
                    claim.claim_id,
                    serde_json::to_string(&claim).unwrap(),
                    chrono::Utc::now().to_rfc3339()
                ],
            )
            .unwrap();
    }

    claim
}

/// A follower claim on the branch owner must not abort filing of the snapshot's
/// landing failures. The observation is listed as deferred and written when
/// that claim settles [ORB-15199].
#[test]
fn ci_failure_claimed_branch_defers_attribution_until_the_claim_settles() {
    if !isolated("ci_failure_claimed_branch_defers_attribution_until_the_claim_settles") {
        return;
    }
    use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
    use orbit_store::contracts::{
        ClaimEvidence, ClaimInvocation, ClaimMutation, DEFERRED_BRANCH_OBSERVATION_KIND,
        DeferredBranchObservation, ExecutionClaimPhase,
    };
    use orbit_types::task::TaskStatus;

    let root = TempDir::new().unwrap();
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let owner = runtime
        .add_task(TaskAddParams {
            title: "Follower-claimed branch owner".into(),
            plan: "Hold the branch while the follower runs.".into(),
            ..Default::default()
        })
        .unwrap();
    let claim = protecting_branch_claim(&runtime, &owner.id);
    let claim_id = claim.claim_id.as_str();
    let claiming_run = claim.run_context.run_id.as_str();
    let machine_id = claim.executed_on.machine_id.as_str();
    let workspace_id = runtime.workspace_id().unwrap();

    let mut pr = failure("error: sandbox directory escaped", 0, &"3".repeat(40));
    pr["event"] = json!("pull_request");
    pr["head_branch"] = json!(format!("orbit/{}-ddb04571", owner.id));
    pr["ref_kind"] = json!("pull_request");
    let push = failure("error: independent landing regression", 1, &"4".repeat(40));

    let deferred_for_owner = |output: &Value| {
        output["deferred_attribution"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| {
                entry["task_id"] == owner.id
                    && entry["run_id"] == pr["run_id"]
                    && entry["reason"] == "claimed"
            })
    };
    let queued = || -> Vec<DeferredBranchObservation> {
        let connection =
            rusqlite::Connection::open(runtime.global_root().join("orbit.db")).unwrap();
        let mut statement = connection
            .prepare(
                "SELECT payload_json FROM task_coordination_rows
                 WHERE workspace_id=?1 AND kind=?2 ORDER BY row_id",
            )
            .unwrap();
        statement
            .query_map(
                rusqlite::params![workspace_id, DEFERRED_BRANCH_OBSERVATION_KIND],
                |row| row.get::<_, String>(0),
            )
            .unwrap()
            .map(|payload| serde_json::from_str(&payload.unwrap()).unwrap())
            .collect()
    };

    let only_pr = file(&runtime, vec![pr.clone()]);
    assert_eq!(only_pr["filed_count"], 0, "{only_pr}");
    assert_eq!(only_pr["outcome"], "current_failures", "{only_pr}");
    assert!(deferred_for_owner(&only_pr), "{only_pr}");
    assert_eq!(
        only_pr["audit"]["deferred_attribution"],
        only_pr["deferred_attribution"]
    );
    assert!(runtime.get_task_artifacts(&owner.id).unwrap().is_empty());
    let rows = queued();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(!rows[0].applied);
    assert_eq!(rows[0].claiming_run_id.as_deref(), Some(claiming_run));
    assert_eq!(rows[0].task_id, owner.id);

    let refused = runtime
        .update_task_as_human(
            &owner.id,
            TaskUpdateParams {
                comment: Some("operator note while the follower holds the claim".into()),
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .unwrap_err();
    let message = refused.to_string();
    assert!(
        message.contains("active execution claim requires a claim-scoped mutation"),
        "{message}"
    );
    assert!(message.contains(&owner.id), "{message}");
    assert!(message.contains(claiming_run), "{message}");

    let filed = file(&runtime, vec![pr.clone(), push.clone()]);
    assert_eq!(filed["outcome"], "current_failures", "{filed}");
    assert_eq!(filed["filed_count"], 1, "{filed}");
    let filed_id = filed["filed"][0]["task_id"].as_str().unwrap();
    assert_ne!(filed_id, owner.id);
    assert!(deferred_for_owner(&filed), "{filed}");
    assert_eq!(
        filed["audit"]["deferred_attribution"],
        filed["deferred_attribution"]
    );
    assert!(runtime.get_task_artifacts(&owner.id).unwrap().is_empty());
    assert_eq!(queued().len(), 1);

    let repeated = file(&runtime, vec![pr.clone(), push.clone()]);
    assert_eq!(repeated["filed_count"], 0, "{repeated}");
    assert_eq!(repeated["skipped_existing"].as_array().unwrap().len(), 1);
    assert!(deferred_for_owner(&repeated), "{repeated}");
    assert!(runtime.get_task_artifacts(&owner.id).unwrap().is_empty());
    assert_eq!(queued().len(), 1);

    runtime
        .mutate_execution_claim(
            Some(&ClaimInvocation::trusted_worker(
                owner.id.clone(),
                claim_id.into(),
                machine_id.into(),
                None,
            )),
            "release-orb-x",
            &ClaimMutation::Release(ClaimEvidence {
                summary: Some("follower finished".into()),
                ..Default::default()
            }),
        )
        .unwrap();
    assert_eq!(
        runtime.get_task(&owner.id).unwrap().status,
        TaskStatus::Backlog
    );
    let settled = runtime
        .inspect_execution_claims()
        .unwrap()
        .into_iter()
        .find(|inspection| inspection.claim.claim_id == claim_id)
        .unwrap();
    assert_eq!(settled.claim.phase, ExecutionClaimPhase::Revoked);
    let artifacts = runtime.get_task_artifacts(&owner.id).unwrap();
    assert_eq!(artifacts.len(), 1, "{artifacts:?}");
    assert!(artifacts[0].path.starts_with("ci-branch-observations/"));
    let retained: Value = serde_json::from_slice(&artifacts[0].content).unwrap();
    assert_eq!(retained["failure"], pr);
    let applied = queued();
    assert_eq!(applied.len(), 1);
    assert!(applied[0].applied);
    assert_eq!(applied[0].content.as_bytes(), artifacts[0].content);

    let after = file(&runtime, vec![pr, push]);
    assert!(
        after["deferred_attribution"].as_array().unwrap().is_empty(),
        "{after}"
    );
    assert_eq!(after["attributed"][0]["task_id"], owner.id);
    assert_eq!(after["filed_count"], 0, "{after}");
    assert_eq!(after["skipped_existing"].as_array().unwrap().len(), 1);
    assert_eq!(runtime.get_task_artifacts(&owner.id).unwrap().len(), 1);
}

/// Settling one task's claim applies only that task's queued receipts, leaves
/// another task's receipt queued, and a repeated sweep or settlement adds no
/// artifact or row. A direct retention on an unclaimed owner leaves no row.
#[test]
fn ci_failure_deferred_receipts_settle_per_task_and_replay_without_growth() {
    if !isolated("ci_failure_deferred_receipts_settle_per_task_and_replay_without_growth") {
        return;
    }
    use orbit_core::application::task::TaskAddParams;
    use orbit_store::contracts::{
        ClaimEvidence, ClaimInvocation, ClaimMutation, DEFERRED_BRANCH_OBSERVATION_KIND,
        DeferredBranchObservation,
    };

    let root = TempDir::new().unwrap();
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let workspace_id = runtime.workspace_id().unwrap();
    let add = |title: &str| {
        runtime
            .add_task(TaskAddParams {
                title: title.into(),
                plan: "Hold the branch while the follower runs.".into(),
                ..Default::default()
            })
            .unwrap()
    };
    let first = add("First claimed branch owner");
    let second = add("Second claimed branch owner");
    let unclaimed = add("Unclaimed branch owner");
    let first_claim = protecting_branch_claim(&runtime, &first.id);
    let second_claim = protecting_branch_claim(&runtime, &second.id);

    let pr_for = |owner: &str, index: usize| {
        let mut pr = failure("error: sandbox directory escaped", index, &"3".repeat(40));
        pr["event"] = json!("pull_request");
        pr["head_branch"] = json!(format!("orbit/{owner}-ddb04571"));
        pr["ref_kind"] = json!("pull_request");
        pr
    };
    let runs = vec![
        pr_for(&first.id, 0),
        pr_for(&second.id, 1),
        pr_for(&unclaimed.id, 2),
    ];
    let rows = || -> Vec<DeferredBranchObservation> {
        let connection =
            rusqlite::Connection::open(runtime.global_root().join("orbit.db")).unwrap();
        let mut statement = connection
            .prepare(
                "SELECT payload_json FROM task_coordination_rows
                 WHERE workspace_id=?1 AND kind=?2 ORDER BY row_id",
            )
            .unwrap();
        statement
            .query_map(
                rusqlite::params![workspace_id, DEFERRED_BRANCH_OBSERVATION_KIND],
                |row| row.get::<_, String>(0),
            )
            .unwrap()
            .map(|payload| serde_json::from_str(&payload.unwrap()).unwrap())
            .collect()
    };

    for _ in 0..2 {
        let output = file(&runtime, runs.clone());
        assert_eq!(output["deferred_attribution"].as_array().unwrap().len(), 2);
        assert_eq!(runtime.get_task_artifacts(&unclaimed.id).unwrap().len(), 1);
        assert_eq!(rows().len(), 2, "only deferred receipts own a row");
    }

    runtime
        .mutate_execution_claim(
            Some(&ClaimInvocation::trusted_worker(
                first.id.clone(),
                first_claim.claim_id.clone(),
                first_claim.executed_on.machine_id.clone(),
                None,
            )),
            "release-first",
            &ClaimMutation::Release(ClaimEvidence {
                summary: Some("first follower finished".into()),
                ..Default::default()
            }),
        )
        .unwrap();
    assert_eq!(runtime.get_task_artifacts(&first.id).unwrap().len(), 1);
    assert!(runtime.get_task_artifacts(&second.id).unwrap().is_empty());
    let after_first = rows();
    assert_eq!(after_first.len(), 2);
    for row in &after_first {
        assert_eq!(row.applied, row.task_id == first.id, "{row:?}");
    }

    // The settled task's sweep and rows replay without growth, and the other
    // task's receipt stays queued.
    let output = file(&runtime, runs.clone());
    assert_eq!(output["deferred_attribution"].as_array().unwrap().len(), 1);
    assert_eq!(runtime.get_task_artifacts(&first.id).unwrap().len(), 1);
    assert!(runtime.get_task_artifacts(&second.id).unwrap().is_empty());
    assert_eq!(rows().len(), 2);

    runtime
        .mutate_execution_claim(
            Some(&ClaimInvocation::trusted_worker(
                second.id.clone(),
                second_claim.claim_id.clone(),
                second_claim.executed_on.machine_id.clone(),
                None,
            )),
            "release-second",
            &ClaimMutation::Release(ClaimEvidence {
                summary: Some("second follower finished".into()),
                ..Default::default()
            }),
        )
        .unwrap();
    assert_eq!(runtime.get_task_artifacts(&second.id).unwrap().len(), 1);
    assert_eq!(runtime.get_task_artifacts(&first.id).unwrap().len(), 1);
    assert!(rows().iter().all(|row| row.applied));

    let output = file(&runtime, runs);
    assert!(
        output["deferred_attribution"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(rows().len(), 2);
    for id in [&first.id, &second.id, &unclaimed.id] {
        assert_eq!(runtime.get_task_artifacts(id).unwrap().len(), 1);
    }
}

/// Deterministically replay the former sweep ordering: lookup the protecting
/// claim, settle it with an empty queue, then submit the stale receipt.
#[test]
fn ci_failure_branch_receipt_retains_after_settlement_precedes_insertion() {
    if !isolated("ci_failure_branch_receipt_retains_after_settlement_precedes_insertion") {
        return;
    }
    use orbit_core::application::task::TaskAddParams;
    use orbit_store::compose::workspace_coordinated_backends;
    use orbit_store::contracts::{
        BranchObservationOutcome, ClaimEvidence, ClaimInvocation, ClaimMutation,
        DEFERRED_BRANCH_OBSERVATION_KIND, DeferredBranchObservation,
    };
    use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};

    for already_queued in [false, true] {
        let root = TempDir::new().unwrap();
        let global = root.path().join("home/.orbit");
        let workspace = root.path().join("repo/.orbit");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
        let owner = runtime
            .add_task(TaskAddParams {
                title: "Branch owner settled before receipt insertion".into(),
                plan: "Hold the branch while the follower runs.".into(),
                ..Default::default()
            })
            .unwrap();
        let claim = protecting_branch_claim(&runtime, &owner.id);
        let snapshot = runtime.resolve_execution_claims().unwrap();
        let observed = snapshot
            .iter()
            .find(|inspection| inspection.claim.task_id == owner.id)
            .unwrap();
        assert!(observed.claim.phase.protects_footprint());

        let mut pr = failure(
            "error: receipt arrived after settlement",
            0,
            &"3".repeat(40),
        );
        pr["event"] = json!("pull_request");
        pr["head_branch"] = json!(format!("orbit/{}-ddb04571", owner.id));
        pr["ref_kind"] = json!("pull_request");
        let content = serde_json::to_string(&json!({
            "schema_version": 1, "kind": "task_branch_ci_failure", "failure": pr,
        }))
        .unwrap();
        let path = format!(
            "ci-branch-observations/{}.json",
            orbit_common::security::release::sha256_hex(content.as_bytes())
        );
        assert!(
            runtime
                .get_task_artifact(&owner.id, &path)
                .unwrap()
                .is_none()
        );
        let observation = DeferredBranchObservation {
            schema_version: 1,
            task_id: owner.id.clone(),
            run_id: pr["run_id"].clone(),
            job_id: pr["job_id"].clone(),
            artifact_path: path.clone(),
            content,
            claiming_run_id: Some(observed.claim.run_context.run_id.clone()),
            applied: false,
        };
        runtime
            .mutate_execution_claim(
                Some(&ClaimInvocation::trusted_worker(
                    owner.id.clone(),
                    claim.claim_id,
                    claim.executed_on.machine_id,
                    None,
                )),
                "release-before-receipt",
                &ClaimMutation::Release(ClaimEvidence {
                    summary: Some("follower finished before the receipt arrived".into()),
                    ..Default::default()
                }),
            )
            .unwrap();

        let backends = workspace_coordinated_backends(
            TaskRegistryStore::open(&task_registry_path(&global)).unwrap(),
            runtime.workspace_id().unwrap(),
            orbit_store::Store::open(&global.join("orbit.db")).unwrap(),
        )
        .unwrap();
        if already_queued {
            // An unapplied receipt from the former race must settle on replay too.
            let connection = rusqlite::Connection::open(global.join("orbit.db")).unwrap();
            connection.execute(
            "INSERT INTO task_coordination_rows(workspace_id, kind, row_id, payload_json, journal_id, created_at)
             VALUES (?1, ?2, ?3, ?4, 'fixture', ?5)",
            rusqlite::params![
                runtime.workspace_id().unwrap(), DEFERRED_BRANCH_OBSERVATION_KIND,
                format!("{}:{}", owner.id, orbit_common::security::release::sha256_hex(observation.content.as_bytes())),
                serde_json::to_string(&observation).unwrap(), chrono::Utc::now().to_rfc3339(),
            ],
        ).unwrap();
        }
        assert_eq!(
            backends
                .commit_boundary
                .coordination_rows(DEFERRED_BRANCH_OBSERVATION_KIND)
                .unwrap()
                .len(),
            usize::from(already_queued)
        );
        assert_eq!(
            backends
                .task
                .task
                .record_deferred_branch_observation(&observation)
                .unwrap(),
            BranchObservationOutcome::Retained
        );
        let artifacts = runtime.get_task_artifacts(&owner.id).unwrap();
        assert_eq!(
            artifacts.len(),
            1,
            "retention must not need another claim or sweep"
        );
        assert_eq!(artifacts[0].path, path);
        assert_eq!(artifacts[0].content, observation.content.as_bytes());
        let rows = backends
            .commit_boundary
            .coordination_rows(DEFERRED_BRANCH_OBSERVATION_KIND)
            .unwrap();
        // A fresh direct retention leaves no row; only a queued receipt is
        // marked applied.
        assert_eq!(rows.len(), usize::from(already_queued));
        if already_queued {
            let applied: DeferredBranchObservation =
                serde_json::from_str(&rows[0].payload_json).unwrap();
            assert!(applied.applied);
            assert!(
                applied.claiming_run_id.is_none(),
                "the stale run cannot decide current protection"
            );
        }
        let history = runtime.get_task_history(&owner.id).unwrap();
        assert_eq!(
            backends
                .task
                .task
                .record_deferred_branch_observation(&observation)
                .unwrap(),
            BranchObservationOutcome::Retained
        );
        assert_eq!(runtime.get_task_artifacts(&owner.id).unwrap().len(), 1);
        assert_eq!(runtime.get_task_history(&owner.id).unwrap(), history);
        assert_eq!(
            backends
                .commit_boundary
                .coordination_rows(DEFERRED_BRANCH_OBSERVATION_KIND)
                .unwrap(),
            rows
        );
        // The same receipt in the normal sweep is reported as retained.
        let output = file(&runtime, vec![pr]);
        assert_eq!(output["attributed"][0]["artifact"], path);
        assert!(
            output["deferred_attribution"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn ci_failure_branch_write_failure_does_not_abort_other_observations_or_landing_filing() {
    if !isolated(
        "ci_failure_branch_write_failure_does_not_abort_other_observations_or_landing_filing",
    ) {
        return;
    }
    use orbit_core::application::task::TaskAddParams;

    let root = TempDir::new().unwrap();
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let owner = runtime
        .add_task(TaskAddParams {
            title: "Branch owner whose queue write fails".into(),
            plan: "Hold the branch while the follower runs.".into(),
            ..Default::default()
        })
        .unwrap();
    protecting_branch_claim(&runtime, &owner.id);
    let other = runtime
        .add_task(TaskAddParams {
            title: "Independent branch owner".into(),
            ..Default::default()
        })
        .unwrap();
    let connection = rusqlite::Connection::open(global.join("orbit.db")).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER reject_branch_observation BEFORE INSERT ON task_coordination_rows
         WHEN NEW.kind = 'ci-branch-observation-deferred-v1'
              AND json_extract(NEW.payload_json, '$.claiming_run_id') IS NOT NULL
         BEGIN SELECT RAISE(ABORT, 'fixture branch observation write failure'); END;",
        )
        .unwrap();
    let mut pr = failure("error: claimed branch failure", 0, &"3".repeat(40));
    pr["event"] = json!("pull_request");
    pr["head_branch"] = json!(format!("orbit/{}-ddb04571", owner.id));
    pr["ref_kind"] = json!("pull_request");
    let mut other_pr = failure("error: independent branch failure", 2, &"3".repeat(40));
    other_pr["event"] = json!("pull_request");
    other_pr["head_branch"] = json!(format!("orbit/{}-abcdef12", other.id));
    other_pr["ref_kind"] = json!("pull_request");
    let push = failure("error: independent landing regression", 1, &"4".repeat(40));
    let output = file(&runtime, vec![pr, other_pr, push]);
    assert_eq!(output["filed_count"], 1, "{output}");
    assert_eq!(
        output["attributed"].as_array().unwrap().len(),
        1,
        "{output}"
    );
    assert_eq!(output["attributed"][0]["task_id"], other.id);
    assert!(
        output["deferred_attribution"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let errors = output["audit"]["branch_observation_errors"]
        .as_array()
        .unwrap();
    assert_eq!(errors.len(), 1, "{output}");
    assert_eq!(errors[0]["task_id"], owner.id);
    assert!(
        errors[0]["message"]
            .as_str()
            .unwrap()
            .contains("fixture branch observation write failure"),
        "{output}"
    );
    assert!(runtime.get_task_artifacts(&owner.id).unwrap().is_empty());
    assert_eq!(runtime.get_task_artifacts(&other.id).unwrap().len(), 1);
}

/// The sweep routes each repair to a host that can reproduce it [ORB-14005]:
/// a failing job's runner labels tag it `os:macos` or `os:linux`, a workflow's
/// literal `runs-on` stands in when the snapshot carries no labels, and a
/// Windows or unrecognised runner leaves it untagged. The evidence is recorded
/// on the filing and in the description.
#[test]
fn ci_failure_sweep_tags_repairs_with_the_failing_runner_os() {
    if !isolated("ci_failure_sweep_tags_repairs_with_the_failing_runner_os") {
        return;
    }
    let root = TempDir::new().unwrap();
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let workflows = root.path().join("repo/.github/workflows");
    std::fs::create_dir_all(&workflows).unwrap();
    std::fs::write(
        workflows.join("ci-macos.yml"),
        "name: macOS CI\non: push\njobs:\n  sandbox:\n    name: Sandbox\n    runs-on: macos-14\n    steps:\n      - run: make test\n",
    )
    .unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();

    let checkout = "3".repeat(40);
    // name, the job's runner labels (null: none), the workflow whose literal
    // `runs-on` stands in, the expected `os:` tag, and the evidence source.
    let cases = json!([
        {"name": "macos", "labels": ["macos-latest"], "tag": "os:macos", "source": "job_labels"},
        {"name": "ubuntu", "labels": ["ubuntu-24.04"], "tag": "os:linux", "source": "job_labels"},
        {"name": "windows", "labels": ["windows-latest"], "source": "job_labels"},
        {"name": "self-hosted", "labels": ["self-hosted", "gpu"], "source": "job_labels"},
        {"name": "runs-on", "workflow": "macOS CI", "tag": "os:macos", "source": "workflow_runs_on"},
        {"name": "unknown", "source": "unknown"},
    ]);
    let cases = cases.as_array().unwrap();
    let runs = cases
        .iter()
        .enumerate()
        .map(|(index, case)| {
            let mut run = failure(
                &format!(
                    "error: {} runner regression",
                    case["name"].as_str().unwrap()
                ),
                index,
                &checkout,
            );
            if !case["labels"].is_null() {
                run["failed_jobs"][0]["runner_labels"] = case["labels"].clone();
            }
            if !case["workflow"].is_null() {
                run["workflow"] = case["workflow"].clone();
                run["failed_jobs"][0]["name"] = json!("Sandbox");
            }
            run
        })
        .collect::<Vec<_>>();
    let output = runtime
        .run_deterministic(
            "file_ci_failure_tasks",
            &json!({}),
            &json!({"max_tasks": 10, "ci_evidence": {
                "schema_version": 2, "collected": true, "outcome_hint": "current_failures",
                "capability": {"available": true, "authenticated": true},
                "heads": [{"kind": "integration", "branch": "agent-main", "current_head_sha": "1".repeat(40)}],
                "latest_runs": runs.clone(), "current_failures": runs, "stale_or_superseded": [],
                "in_flight": [], "retryable_errors": [], "collected_at": "2026-10-04T08:00:00Z"
            }}),
            ToolContext::default(),
        )
        .expect("file CI failures");
    let filed = output["filed"].as_array().unwrap();
    assert_eq!(filed.len(), cases.len(), "{output}");

    for (index, case) in cases.iter().enumerate() {
        let name = &case["name"];
        let entry = filed
            .iter()
            .find(|entry| entry["run_ids"] == json!([10 + index]))
            .unwrap_or_else(|| panic!("{name} was filed: {output}"));
        let task = runtime
            .get_task(entry["task_id"].as_str().unwrap())
            .unwrap();
        let os_tags = task
            .tags
            .iter()
            .filter(|tag| tag.starts_with("os:"))
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            os_tags,
            case["tag"]
                .as_str()
                .map(|tag| vec![tag.to_string()])
                .unwrap_or_default(),
            "{name}: {:?}",
            task.tags
        );
        assert_eq!(
            entry["runner_os"][0]["source"], case["source"],
            "{name}: {entry}"
        );
        assert!(
            task.description.contains("- Runner OS: "),
            "{name}: the description records the runner evidence"
        );
    }
}

#[test]
fn ci_failure_sweep_uses_medium_pool_and_preserves_failure_key_tag() {
    if !isolated("ci_failure_sweep_uses_medium_pool_and_preserves_failure_key_tag") {
        return;
    }
    let root = TempDir::new().unwrap();
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(
        workspace.join("config.toml"),
        r#"[workflow]
default_crew = "system"
medium_complexity_crews = ["fixture"]

[crews.fixture]
provider = "codex"
model = "fixture-model"
backend = "cli"

[crews.system]
provider = "codex"
model = "system-model"
backend = "cli"
"#,
    )
    .unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let output = file(
        &runtime,
        vec![failure(
            "error: pool routing regression",
            0,
            &"3".repeat(40),
        )],
    );
    let entry = &output["filed"][0];
    let task = runtime
        .get_task(entry["task_id"].as_str().unwrap())
        .unwrap();

    assert_eq!(task.complexity, Some(TaskComplexity::Medium));
    assert_eq!(task.crew.as_deref(), Some("fixture"));
    assert!(task.tags.contains(&format!(
        "ci-failure:{}",
        entry["failure_key"].as_str().unwrap()
    )));
}

/// Commit `count` empty commits on `repo` and return their ids, oldest first.
fn commit_chain(repo: &Path, count: usize) -> Vec<String> {
    let git = |args: &[&str]| {
        let mut command = std::process::Command::new("git");
        orbit_common::test_env::clear_inherited_authority(|key| {
            command.env_remove(key);
        });
        let output = command
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.com",
            ])
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    };
    git(&["init", "-q"]);
    (0..count)
        .map(|index| {
            git(&[
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                &format!("commit {index}"),
            ]);
            git(&["rev-parse", "HEAD"])
        })
        .collect()
}

/// Record a successful delivery run that committed and merged `task_id` at
/// `landed`, the way the host pipeline checkpoints it.
fn record_landing(runtime: &OrbitRuntime, task_id: &str, base: &str, landed: &str) {
    use orbit_types::workflow::{JobRunState, PipelineState};

    // A delivery job whose commit and merge steps are the shipped host actions.
    let resources = runtime.paths().global_dir.join("resources");
    std::fs::create_dir_all(resources.join("activities")).unwrap();
    std::fs::create_dir_all(resources.join("jobs")).unwrap();
    for activity in ["git_commit", "pr_complete"] {
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("assets/activities/{activity}.yaml")),
            resources.join(format!("activities/{activity}.yaml")),
        )
        .unwrap();
    }
    std::fs::write(
        resources.join("jobs/fixture_delivery.yaml"),
        "schemaVersion: 2\nkind: Job\nmetadata:\n  name: fixture_delivery\nspec:\n  state: enabled\n  task_delivery: {}\n  steps:\n    - id: commit\n      target: activity:git_commit\n    - id: complete_pr\n      target: activity:pr_complete\n",
    )
    .unwrap();
    let mut run = runtime
        .insert_job_run(
            "fixture_delivery",
            1,
            chrono::Utc::now(),
            Some(json!({"task_ids": [task_id]})),
            None,
        )
        .unwrap();
    run.state = JobRunState::Success;
    runtime
        .sqlite_store()
        .unwrap()
        .upsert_job_run_for_workspace(&runtime.workspace_id().unwrap(), &run, None)
        .unwrap();
    let mut state = PipelineState::new(run.run_id.clone(), run.job_id.clone(), json!({}));
    for (index, step_id, output) in [
        (
            0,
            "commit",
            json!({
                "phase": "commit", "decision": "performed", "committed": true,
                "commit_sha": landed, "base_sha": base, "job_run_id": run.run_id, "task_id": task_id,
            }),
        ),
        (
            1,
            "complete_pr",
            json!({
                "phase": "complete",
                "merge": {"merged": true, "pr_number": "3507", "landed_commit": landed},
            }),
        ),
    ] {
        state.record_step(index, JobRunState::Success, Some(output.clone()), None);
        state.record_pipeline_output(step_id, output);
    }
    runtime.write_run_state(&run.run_id, &state).unwrap();
}

/// A red run that completes after its repair landed is not filed again
/// [ORB-14422]: the done repair with the same normalized signature landed at a
/// descendant of the tested commit, so the failure is held until a newer run
/// reproduces it. A failure tested on a commit that already contains the
/// landing is still filed, and collection's own holds pass through.
#[test]
fn ci_failure_predating_a_landed_repair_is_held_not_refiled() {
    if !isolated("ci_failure_predating_a_landed_repair_is_held_not_refiled") {
        return;
    }
    use orbit_engine::TaskAutomationUpdate;
    use orbit_types::task::TaskStatus;

    let root = TempDir::new().unwrap();
    let global = root.path().join("home/.orbit");
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(repo.join(".orbit")).unwrap();
    // first red run, a later red run on an older commit, the repair, and a
    // commit after the repair.
    let commits = commit_chain(&repo, 4);
    let (first, late, landed, after) = (&commits[0], &commits[1], &commits[2], &commits[3]);
    let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit")).unwrap();
    let log = "error: landing stopped on its base is not repaired under the same task";

    let original = file(&runtime, vec![failure(log, 0, first)]);
    assert_eq!(original["filed_count"], 1, "{original}");
    let repair = original["filed"][0]["task_id"]
        .as_str()
        .unwrap()
        .to_string();
    runtime
        .apply_task_automation_update(
            &repair,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Done),
                ..TaskAutomationUpdate::default()
            },
        )
        .unwrap();
    assert_eq!(runtime.get_task(&repair).unwrap().status, TaskStatus::Done);
    record_landing(&runtime, &repair, late, landed);

    // Another job with the same diagnostic, tested before the landing.
    let coverage = |index: usize, checkout: &str| {
        let mut run = failure(log, index, checkout);
        run["failed_jobs"][0]["name"] = json!("Coverage");
        run
    };
    let held = file(&runtime, vec![coverage(1, late)]);
    assert_eq!(held["filed_count"], 0, "{held}");
    assert_eq!(held["skipped_existing"], json!([]), "{held}");
    let pending = &held["pending_supersession"][0];
    assert_eq!(
        pending["reason"], "repaired_by_descendant_landing",
        "{held}"
    );
    assert_eq!(pending["task_id"], repair.as_str());
    assert_eq!(pending["landed_commit"], landed.as_str());
    assert_eq!(pending["tested_commit"], late.as_str());
    assert_eq!(pending["run_ids"], json!([11]));
    assert_eq!(held["audit"]["pending_supersession_run_ids"], json!([11]));

    // The landing is an ancestor of this checkout: the failure reproduces on
    // repaired code, so it is filed as before.
    let reproduced = file(&runtime, vec![coverage(2, after)]);
    assert_eq!(reproduced["filed_count"], 1, "{reproduced}");
    assert_eq!(reproduced["pending_supersession"], json!([]));

    // Collection's in-flight holds are reported by the filing step as well.
    let in_flight_hold = json!({
        "run_id": 37561651327u64, "workflow": "CI", "head_branch": "agent-main",
        "reason": "newer_descendant_run_in_flight",
        "pending_on": {"run_id": 37561800000u64, "status": "in_progress"},
    });
    let output = runtime.run_deterministic("file_ci_failure_tasks", &json!({}), &json!({
        "ci_evidence": {
            "schema_version": 2, "collected": true, "outcome_hint": "no_current_failure",
            "heads": [{"kind": "integration", "branch": "agent-main", "current_head_sha": "1".repeat(40)}],
            "current_failures": [], "pending_supersession": [in_flight_hold.clone()],
        }
    }), ToolContext::default()).unwrap();
    assert_eq!(output["outcome"], "no_current_failure");
    assert_eq!(output["pending_supersession"], json!([in_flight_hold]));
    assert_eq!(
        output["audit"]["pending_supersession_run_ids"],
        json!([37561651327u64])
    );
}

/// Collection releases a red run from `pending_supersession` despite a newer
/// push run still queued at a descendant commit when the previous completed
/// run failed the same way, or when the hold outlived its window [ORB-14610:
/// agent-main stayed red for 50 minutes because every sweep held the newest
/// red run behind the next queued push]. Filing files exactly one task for it
/// and the description names the run it did not wait for.
#[test]
fn ci_failure_released_from_pending_supersession_is_filed_naming_the_pending_run() {
    if !isolated("ci_failure_released_from_pending_supersession_is_filed_naming_the_pending_run") {
        return;
    }
    let log = "build\tRun CI\t2026-10-07T15:10:00.0000000Z ##[group]Run cargo check --workspace\n\
               build\tRun CI\t2026-10-07T15:10:00.0000000Z error[E0425]: cannot find value `probe_timeout` in this scope\n\
               build\tRun CI\t2026-10-07T15:10:00.0000000Z   --> crates/orbit-cmd/src/update/converge.rs:189:9\n\
               build\tRun CI\t2026-10-07T15:10:00.0000000Z ##[error]Process completed with exit code 101.\n";
    let queued = json!({
        "run_id": 37645190583u64, "status": "queued", "event": "push",
        "url": "https://github.com/acme/orbit/actions/runs/37645190583",
        "reported_head_sha": "9".repeat(40),
    });
    let previous = "5".repeat(40);
    let mut reproduced = failure(log, 1, &"6".repeat(40));
    reproduced["reproduced_on"] = json!({
        "run_id": 10, "url": "https://github.com/acme/orbit/actions/runs/10",
        "event_reported_head_sha": previous, "conclusion": "failure",
        "shared_cause": {
            "job": "build", "steps": ["Run CI"],
            "normalized_error_signature": "error[e<n>]: cannot find value `probe_timeout` in this scope",
        },
        "pending_on": queued,
    });
    let mut held = failure(log, 2, &"7".repeat(40));
    held["held_past_window"] = json!({
        "pending_on": queued, "pending_since": "2026-10-07T15:10:00+00:00", "window_minutes": 30,
    });

    // What each release must name besides the pending run.
    for (released, expected) in [
        (
            reproduced,
            vec![
                "https://github.com/acme/orbit/actions/runs/10",
                previous.as_str(),
                "error[e<n>]: cannot find value `probe_timeout` in this scope",
            ],
        ),
        (held, vec!["2026-10-07T15:10:00+00:00"]),
    ] {
        let root = TempDir::new().unwrap();
        let global = root.path().join("home/.orbit");
        let workspace = root.path().join("repo/.orbit");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();

        let output = file(&runtime, vec![released]);

        assert_eq!(output["filed_count"], 1, "{output}");
        assert_eq!(output["pending_supersession"], json!([]), "{output}");
        let task = runtime
            .get_task(output["filed"][0]["task_id"].as_str().unwrap())
            .unwrap();
        let pending = "9".repeat(40);
        for named in expected.into_iter().chain([
            "https://github.com/acme/orbit/actions/runs/37645190583",
            pending.as_str(),
        ]) {
            assert!(
                task.description.contains(named),
                "the filed description names {named}: {}",
                task.description
            );
        }
        assert!(
            task.description.contains("- Compiler cause identity: `"),
            "{}",
            task.description
        );
    }
}

#[test]
fn ci_failure_sweep_strips_ansi_codes_and_extracts_context_files() {
    if !isolated("ci_failure_sweep_strips_ansi_codes_and_extracts_context_files") {
        return;
    }
    let root = TempDir::new().unwrap();
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();

    let colored_compiler_log = "build\tRun CI\t2026-10-09T12:00:00.0000000Z \u{1b}[31merror[E0425]: cannot find value `foo` in this scope\u{1b}[0m\n\
                                build\tRun CI\t2026-10-09T12:00:00.0000000Z \u{1b}[38;5;12m  --> \u{1b}[0m\u{1b}[1mcrates/orbit-core/src/runtime/mod.rs:31:16\u{1b}[0m\n\
                                build\tRun CI\t2026-10-09T12:00:00.0000000Z \u{1b}[31m##[error]Process completed with exit code 101.\u{1b}[0m\n";

    let output = file(
        &runtime,
        vec![failure(colored_compiler_log, 0, &"2".repeat(40))],
    );
    assert_eq!(output["filed_count"], 1, "{output}");
    let task_id = output["filed"][0]["task_id"].as_str().unwrap();
    let task = runtime.get_task(task_id).unwrap();

    assert!(
        !task.description.contains('\u{1b}'),
        "task description must have no \\x1b byte: {}",
        task.description
    );
    assert!(
        !task.description.contains("[0m"),
        "task description must have no [0m remnant: {}",
        task.description
    );
    assert_eq!(
        task.context_files,
        vec!["file:crates/orbit-core/src/runtime/mod.rs".to_string()]
    );

    let colored_panic_log = "build\tRun CI\t2026-10-09T12:00:00.0000000Z \u{1b}[31mthread 'crew_pools' panicked at \u{1b}[0mcrates/orbit-core/src/application/job/tests/crew_pools.rs:277:5:\u{1b}[0m\n\
                             build\tRun CI\t2026-10-09T12:00:00.0000000Z \u{1b}[31massertion failed: `(left == right)`\u{1b}[0m\n\
                             build\tRun CI\t2026-10-09T12:00:00.0000000Z \u{1b}[31m##[error]Process completed with exit code 101.\u{1b}[0m\n";

    let panic_output = file(
        &runtime,
        vec![failure(colored_panic_log, 1, &"3".repeat(40))],
    );
    assert_eq!(panic_output["filed_count"], 1, "{panic_output}");
    let panic_task_id = panic_output["filed"][0]["task_id"].as_str().unwrap();
    let panic_task = runtime.get_task(panic_task_id).unwrap();

    assert!(
        !panic_task.description.contains('\u{1b}'),
        "panic task description must have no \\x1b byte"
    );
    assert!(
        !panic_task.description.contains("[0m"),
        "panic task description must have no [0m remnant"
    );
    assert_eq!(
        panic_task.context_files,
        vec!["file:crates/orbit-core/src/application/job/tests/crew_pools.rs".to_string()]
    );
}
