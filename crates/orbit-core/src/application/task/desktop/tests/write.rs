use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_store::friction_store::{FrictionAddParams, FrictionStore};
use orbit_types::desktop::*;
use orbit_types::record::FrictionStatus;
use orbit_types::task::{ExternalRef, TaskRelation, TaskRelationType, TaskStatus};
use serde_json::json;

use super::session;
use crate::application::task::tests::{enter_isolated_child, test_runtime};
use crate::application::task::{TaskAddParams, TaskUpdateParams};
use crate::runtime::task_pr_forge::{ForgePullRequest, TaskPrForge};

const PR_HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

#[derive(Default)]
struct ReplayForge {
    open: Mutex<Vec<ForgePullRequest>>,
    closed: Mutex<Vec<u64>>,
}

impl TaskPrForge for ReplayForge {
    fn open_pull_requests(&self, _: &Path) -> Result<Vec<ForgePullRequest>, OrbitError> {
        Ok(self.open.lock().unwrap().clone())
    }

    fn authenticated_login(&self, _: &Path) -> Result<String, OrbitError> {
        Ok("orbit-bot".to_string())
    }

    fn close_pull_request(&self, _: &Path, number: u64, _: &str) -> Result<(), OrbitError> {
        self.open.lock().unwrap().retain(|pr| pr.number != number);
        self.closed.lock().unwrap().push(number);
        Ok(())
    }
}

/// A stand-in `gh`, first on `PATH` while the guard lives, that reports pull
/// request #17 open at [`PR_HEAD`].
#[cfg(unix)]
fn provider_reports_pr_17() -> orbit_common::test_env::ScopedEnv {
    let bin = PathBuf::from(std::env::var("HOME").unwrap()).join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let gh = bin.join("gh");
    let answer = json!({
        "number": 17,
        "state": "OPEN",
        "headRefName": "orbit/fixture",
        "headRefOid": PR_HEAD,
        "baseRefName": "main",
        "url": "https://github.com/owner/repository/pull/17",
    });
    std::fs::write(
        &gh,
        format!(
            "#!/bin/sh\n[ \"$1 $2 $3\" = \"pr view 17\" ] || {{ echo \"unexpected: gh $*\" >&2; exit 1; }}\n\
             printf '%s' '{answer}'\n"
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap();
    orbit_common::test_env::scoped([("PATH", Some(path.to_str().unwrap()))])
}

/// Fault injection is necessary to stop exactly after the receipt commits,
/// before its first post-lock read, without relying on process timing.
#[test]
#[cfg(unix)]
fn completion_replay_finishes_side_effects_after_a_post_lock_read_failure() {
    if !enter_isolated_child(
        module_path!(),
        "completion_replay_finishes_side_effects_after_a_post_lock_read_failure",
    ) {
        return;
    }
    let _gh = provider_reports_pr_17();
    let (_root, runtime) = test_runtime();
    let forge = Arc::new(ReplayForge::default());
    let runtime = runtime.with_task_pr_forge(forge.clone());
    let frictions = FrictionStore::open(
        runtime.sqlite_store().expect("store"),
        runtime.workspace_id().expect("workspace id"),
        runtime.data_root().join("frictions"),
    )
    .expect("friction store");
    let friction = frictions
        .add(FrictionAddParams {
            model: "codex".to_string(),
            title: None,
            body: "Desktop completion resolves this target".to_string(),
            tags: vec!["tooling".to_string()],
            status: FrictionStatus::Open,
            during_task: None,
            created_at: Utc::now(),
        })
        .expect("add friction")
        .record
        .id;
    let task = runtime
        .add_task(TaskAddParams {
            title: "Complete through desktop".to_string(),
            description: "Exercise desktop completion replay.".to_string(),
            acceptance_criteria: vec!["Side effects finish once.".to_string()],
            plan: "1) implement 2) validate".to_string(),
            relations: vec![TaskRelation {
                relation_type: TaskRelationType::Resolves,
                target: friction.clone(),
                regression: None,
            }],
            external_refs: vec![ExternalRef::github_pr("17").expect("PR reference")],
            ..Default::default()
        })
        .expect("add task");
    for status in [TaskStatus::InProgress, TaskStatus::Review] {
        runtime
            .update_task(
                &task.id,
                TaskUpdateParams {
                    status: Some(status),
                    execution_summary: Some("Delivered.".to_string()),
                    ..Default::default()
                },
            )
            .expect("advance task");
    }
    // #17 is the reviewed landing, kept open; #12 is an earlier stale one.
    let open_prs = [17, 12].map(|number| ForgePullRequest {
        number,
        head_branch: format!("orbit/{}-delivery-{number}", task.id),
        body: format!("Task {}", task.id),
        author: "orbit-bot".to_string(),
    });
    *forge.open.lock().unwrap() = open_prs.to_vec();
    let operator = session(true);
    let revision = runtime
        .desktop_task_snapshot(&task.id, &operator)
        .expect("snapshot")
        .revision;
    let criterion = DesktopCriterionOutcome {
        criterion: "Side effects finish once.".into(),
        met: true,
        evidence: vec!["execution_summary".into()],
    };
    let request = DesktopTaskRequest {
        request_id: "complete".into(),
        operation: DesktopTaskOperation::Review {
            id: task.id.clone(),
            expected_revision: revision,
            verdict: DesktopReviewVerdict {
                decision: DesktopReviewDecision::Accept,
                rationale: "Delivered.".into(),
                criteria: vec![criterion],
                evidence: vec!["execution_summary".into()],
                expected_run_id: None,
                expected_head: Some(PR_HEAD.into()),
            },
            complete: true,
        },
    };
    let friction_status = || frictions.show(&friction).unwrap().unwrap().record.status;

    super::super::write::FAIL_SIDE_EFFECT_READ.with(|fail| fail.set(true));
    let error = runtime
        .desktop_task_write(request.clone(), None, None, &operator)
        .expect_err("post-lock read fails once");
    assert!(
        matches!(&error, OrbitError::DesktopWriteAccepted { reason, .. }
            if reason.contains("injected post-lock task read failure")),
        "{error}"
    );
    assert_eq!(runtime.get_task(&task.id).unwrap().status, TaskStatus::Done);
    assert!(forge.closed.lock().unwrap().is_empty());
    assert_eq!(friction_status(), FrictionStatus::Open);

    let replay = runtime
        .desktop_task_write(request.clone(), None, None, &operator)
        .expect("replay finishes the side effects");
    assert!(replay.replayed);
    assert_eq!(*forge.closed.lock().unwrap(), vec![12]);
    assert_eq!(friction_status(), FrictionStatus::Resolved);

    // A human reopens the target and the forge lists #12 again; neither
    // successful effect is repeated on another replay.
    runtime
        .run_tool(
            "orbit.friction.update",
            json!({"id": friction, "status": "open"}),
        )
        .expect("human reopens target");
    *forge.open.lock().unwrap() = open_prs.to_vec();
    let history = runtime.get_task_history(&task.id).unwrap();
    let again = runtime
        .desktop_task_write(request, None, None, &operator)
        .expect("second replay");
    assert!(again.replayed);
    assert_eq!(again.snapshot.revision, replay.snapshot.revision);
    assert_eq!(
        runtime.get_task_history(&task.id).unwrap(),
        history,
        "successful effects are not repeated"
    );
    assert_eq!(
        *forge.closed.lock().unwrap(),
        vec![12],
        "the recorded PR closure is not run again"
    );
    assert_eq!(
        friction_status(),
        FrictionStatus::Open,
        "a successful resolve must not override a later human reopening"
    );
}
