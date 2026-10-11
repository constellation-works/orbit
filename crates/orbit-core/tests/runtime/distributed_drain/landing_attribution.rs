//! The owner's `deliveries_landed` batches name the tasks handoff landings
//! delivered [ORB-13894].

use super::*;

use orbit_core::application::automation::evaluate_auto_task;
use orbit_core::application::task::TaskAddParams;
use orbit_engine::{HandoffLandingStep, HandoffLandingUpdate, TaskAutomationUpdate};
use orbit_store::contracts::FrictionAddParams;
use orbit_store::friction_store::FrictionStore;
use orbit_types::record::FrictionStatus;
use orbit_types::task::{ExternalRef, TaskComplexity, TaskStatus};
use orbit_types::workflow::automation::{DirectLandingRequest, UNATTRIBUTED_NO_LANDING_TASK};

const REPOSITORY: &str = "owner/repository";
const CONSUMER: &str = "landed-review";

fn frictions(owner: &OrbitRuntime) -> FrictionStore {
    FrictionStore::open(
        owner.sqlite_store().unwrap(),
        owner.workspace_id().unwrap(),
        owner.data_root().join("frictions"),
    )
    .unwrap()
}

/// Exercise the same trusted owner boundary as the landing activity. The
/// existing drain wire stands in for the provider's candidate observation;
/// authority and completion still go through the real claim journal.
fn complete_landing(pair: &Pair) -> String {
    *pair.wire.accept_handoffs.lock().unwrap() = true;
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let delivered = handoff(&pair.admission(&leaf));
    pair.leaf_hands_off(&leaf);
    let settled = pair.pass(&drain);
    assert!(settled["error"].is_null(), "{settled}");
    let owner = &pair.wire.owner;
    let claim = &owner.distributed_claim_console().unwrap()["claims"][0];
    let handoff_id = claim["handoff"]["handoff_id"].as_str().unwrap().to_string();
    let context = ClaimInvocation::trusted_operator(
        delivered.task_id.clone(),
        delivered.claim_id.clone(),
        "owner-landing".into(),
    )
    .with_handoff_observation(HandoffObservation {
        candidate: delivered.candidate.clone(),
        footprint_widening: vec![],
        required_commands: vec![],
        owner_completion_authority: None,
        review: None,
        known_red: vec![],
    });
    owner
        .mutate_execution_claim(
            Some(&context),
            "approve-landing",
            &ClaimMutation::ApproveHandoff {
                handoff_id: handoff_id.clone(),
                candidate: delivered.candidate.clone(),
            },
        )
        .unwrap();
    owner
        .mutate_execution_claim(
            Some(&context),
            "dispatch-landing",
            &ClaimMutation::DispatchLanding {
                handoff_id: handoff_id.clone(),
                job_run_id: None,
            },
        )
        .unwrap();
    RuntimeHost::record_handoff_landing(owner, &HandoffLandingUpdate {
            handoff_id,
            step: HandoffLandingStep::Complete,
            observed: Some(delivered.candidate),
            evidence: json!({"delivery": {"merge_commit": "e".repeat(40), "pr_number": "42"}, "pull_request": 42}).to_string(),
        })
        .unwrap();
    assert_eq!(pair.owner_status(&delivered.task_id), "done");
    delivered.task_id
}

#[test]
fn landing_resolves_named_frictions_and_preserves_prior_resolutions() {
    if !isolated(
        module_path!(),
        "landing_resolves_named_frictions_and_preserves_prior_resolutions",
    ) {
        return;
    }
    for (named, claimed) in [(true, true), (false, true), (true, false), (false, false)] {
        let pair = Pair::new(1);
        let owner = &pair.wire.owner;
        let store = frictions(owner);
        let records = [
            FrictionStatus::Open,
            FrictionStatus::Resolved,
            FrictionStatus::Triaged,
            FrictionStatus::Open,
        ]
        .into_iter()
        .map(|status| {
            store
                .add(FrictionAddParams {
                    model: "codex".into(),
                    title: Some("Landing fixture".into()),
                    body: "Original report with trailing whitespace.  \n".into(),
                    tags: vec!["tooling".into()],
                    status,
                    during_task: None,
                    created_at: Utc::now(),
                })
                .unwrap()
                .record
        })
        .collect::<Vec<_>>();
        if named {
            owner.run_tool("orbit.task.update", json!({
                "id": pair.tasks[0], "model": "codex",
                "description": format!("Fix `{}`; also cites {}.", records[0].id, records[1].id),
                "acceptance_criteria": [format!("Friction {} is resolved with the merge sha; duplicate {}.", records[2].id, records[0].id)],
                "relations": [{"type": "resolves", "target": records[3].id}],
            })).unwrap();
        }
        let before = Utc::now();
        let task = if claimed {
            complete_landing(&pair)
        } else {
            let task = pair.tasks[0].clone();
            owner
                .apply_task_automation_update(
                    &task,
                    TaskAutomationUpdate {
                        status: Some(TaskStatus::Review),
                        ..Default::default()
                    },
                )
                .unwrap();
            owner.apply_task_automation_update(&task, TaskAutomationUpdate {
                status: Some(TaskStatus::Done),
                expected_status: Some(TaskStatus::Review),
                status_note: Some(format!("completion authorized by run fixture; delivered by pull request #42 merged as {}", "e".repeat(40))),
                ..Default::default()
            }).unwrap();
            task
        };
        let history = owner.get_task_history(&task).unwrap();
        for (index, original) in records.iter().enumerate() {
            let actual = store.show(&original.id).unwrap().unwrap().record;
            if !named || index == 1 {
                assert_eq!(&actual, original);
                continue;
            }
            assert_eq!(actual.status, FrictionStatus::Resolved);
            assert_eq!(actual.resolved_by_task.as_deref(), Some(task.as_str()));
            assert!(actual.resolved_at.unwrap() >= before);
            assert_eq!(
                actual.body.strip_prefix(&original.body).unwrap(),
                format!(
                    "\n\nResolved by task {task}; commit {}; PR #42; at {}.\n",
                    "e".repeat(40),
                    actual.resolved_at.unwrap().to_rfc3339(),
                )
            );
            assert_eq!(
                history
                    .iter()
                    .filter(|entry| entry.event == "friction_resolved_on_landing"
                        && entry.note.as_deref().is_some_and(|note| note.lines().next()
                            == Some(format!("target={}", original.id).as_str())))
                    .count(),
                1
            );
        }
        assert_eq!(
            history
                .iter()
                .filter(|entry| entry.event == "friction_resolved_on_landing")
                .count(),
            if named { 3 } else { 0 }
        );
    }
}

#[test]
fn landing_retries_failed_prose_targets_with_the_original_delivery_evidence() {
    if !isolated(
        module_path!(),
        "landing_retries_failed_prose_targets_with_the_original_delivery_evidence",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let owner = &pair.wire.owner;
    let store = frictions(owner);
    let friction = store
        .add(FrictionAddParams {
            model: "codex".into(),
            title: None,
            body: "Original report".into(),
            tags: vec!["tooling".into()],
            status: FrictionStatus::Open,
            during_task: None,
            created_at: Utc::now(),
        })
        .unwrap()
        .record;
    owner.run_tool("orbit.task.update", json!({
        "id": pair.tasks[0], "model": "codex", "description": format!("Fix {}.", friction.id),
    })).unwrap();
    let database = rusqlite::Connection::open(owner.global_root().join("orbit.db")).unwrap();
    database.execute_batch("CREATE TRIGGER fail_landing_resolution BEFORE UPDATE ON friction_records WHEN NEW.status = 'resolved' BEGIN SELECT RAISE(ABORT, 'injected landing write failure'); END;").unwrap();
    let task = complete_landing(&pair);
    assert_eq!(
        store.show(&friction.id).unwrap().unwrap().record.status,
        FrictionStatus::Open
    );
    assert!(
        owner
            .get_task_history(&task)
            .unwrap()
            .iter()
            .any(|entry| entry.event == "resolves_side_effect_failed")
    );
    database
        .execute_batch("DROP TRIGGER fail_landing_resolution;")
        .unwrap();
    owner
        .run_tool(
            "orbit.task.update",
            json!({"id": task, "comment": "Store recovered.", "model": "codex"}),
        )
        .unwrap();
    let resolved = store.show(&friction.id).unwrap().unwrap().record;
    assert_eq!(resolved.status, FrictionStatus::Resolved);
    assert!(resolved.body.contains(&"e".repeat(40)) && resolved.body.contains("PR #42"));
    assert!(
        owner
            .get_task_history(&task)
            .unwrap()
            .iter()
            .any(|entry| entry.event == "resolves_side_effect_recovered")
    );
    owner
        .run_tool(
            "orbit.friction.update",
            json!({"id": friction.id, "status": "open"}),
        )
        .unwrap();
    owner
        .run_tool(
            "orbit.task.update",
            json!({"id": task, "comment": "Reopened by an operator.", "model": "codex"}),
        )
        .unwrap();
    assert_eq!(
        store.show(&friction.id).unwrap().unwrap().record.status,
        FrictionStatus::Open
    );
    assert_eq!(owner.get_task(&task).unwrap().status, TaskStatus::Done);
}

/// A follower's pull request carries no reference on the owner's task: the
/// owner's accepted handoff is what names the task it delivered. Once that PR
/// lands, the owner's next review batch lists it under the claimed task. An
/// owner-run PR keeps the task its promotion stamped, an owner-local handoff
/// that fast-forwards keeps the task its landing intent names, even across a
/// retried attempt, and a PR no record names is reported as unattributed,
/// with the reason.
#[test]
fn the_owners_review_batch_names_the_tasks_handoff_landings_delivered() {
    if !isolated(
        module_path!(),
        "the_owners_review_batch_names_the_tasks_handoff_landings_delivered",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let owner = &pair.wire.owner;
    *pair.wire.accept_handoffs.lock().unwrap() = true;
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let claimed = pair.claimed_task(&leaf);
    pair.leaf_hands_off(&leaf);
    let settled = pair.pass(&drain);
    assert!(settled["error"].is_null(), "{settled}");
    assert_eq!(pair.owner_status(&claimed), "review");

    // Promotion stamps an owner-run PR on the task it delivers.
    let owner_run = owner
        .add_task(TaskAddParams {
            title: "Owner-run delivery".into(),
            acceptance_criteria: vec!["Lands through a pull request.".into()],
            complexity: TaskComplexity::Low,
            external_refs: vec![ExternalRef::github_pr("7").unwrap()],
            ..TaskAddParams::default()
        })
        .unwrap()
        .id
        .to_string();
    let owner_local = owner
        .add_task(TaskAddParams {
            title: "Owner-local claimed delivery".into(),
            acceptance_criteria: vec!["Lands by fast-forward.".into()],
            complexity: TaskComplexity::Low,
            ..TaskAddParams::default()
        })
        .unwrap()
        .id
        .to_string();

    let repo = &pair.owner_repo;
    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitignore"), "/.orbit/\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "baseline"]);
    git(
        repo,
        &[
            "remote",
            "add",
            "origin",
            &format!("https://github.com/{REPOSITORY}.git"),
        ],
    );
    owner
        .run_tool(
            "orbit.auto_task.add",
            json!({
                "name": CONSUMER,
                "schedule": {"deliveries_landed": {
                    "branch": "main",
                    "owner_machine": OWNER,
                    "threshold": 4,
                    "max_wait_minutes": 60,
                    "coverage": "landed_code_review_v1",
                    "max_items": 20,
                    "retries": 0,
                }},
                "template": {"title": "Review landed deliveries"},
            }),
        )
        .expect("consumer");
    owner.auto_task_toggle(CONSUMER, true).unwrap();
    let definition = owner.auto_task_show(CONSUMER).unwrap().unwrap();
    publish_origin(repo);
    let baseline = evaluate_auto_task(owner, &definition, false, Utc::now()).expect("baseline");
    assert!(baseline.state.is_some(), "{baseline:#?}");

    // Each PR squash-merges as one first-parent commit; the stand-in `gh`
    // answers the provider's commit-to-PR lookup from these records.
    let home = PathBuf::from(std::env::var("HOME").unwrap());
    let pulls = home.join("pulls");
    std::fs::create_dir_all(&pulls).unwrap();
    for pr in [42_u64, 7, 9] {
        std::fs::write(repo.join("landed.txt"), format!("landed by #{pr}\n")).unwrap();
        git(repo, &["add", "landed.txt"]);
        git(
            repo,
            &["commit", "-q", "-m", &format!("Squash-merge #{pr}")],
        );
        let sha = git(repo, &["rev-parse", "HEAD"]).trim().to_string();
        let response = json!([{
            "number": pr,
            "html_url": format!("https://github.com/{REPOSITORY}/pull/{pr}"),
            "merge_commit_sha": sha,
            "merged_at": "2026-10-04T00:00:00Z",
            "base": {"ref": "main", "repo": {"full_name": REPOSITORY}},
        }]);
        std::fs::write(pulls.join(format!("{sha}.json")), response.to_string()).unwrap();
    }

    // An owner-local candidate fast-forwards main. The landing job retains
    // its intent first; a retried attempt runs under a new run id and
    // re-records the same intent rather than a second one.
    let before = git(repo, &["rev-parse", "HEAD"]).trim().to_string();
    git(repo, &["checkout", "-q", "-b", "orbit/local"]);
    std::fs::write(repo.join("local.txt"), "landed locally\n").unwrap();
    git(repo, &["add", "local.txt"]);
    git(repo, &["commit", "-q", "-m", "Owner-local candidate"]);
    let after = git(repo, &["rev-parse", "HEAD"]).trim().to_string();
    git(repo, &["checkout", "-q", "main"]);
    for run_id in ["jrun-landing", "jrun-landing-retry"] {
        owner
            .record_direct_landing_intent(&DirectLandingRequest {
                run_id: run_id.into(),
                branch: "main".into(),
                before_commit: before.clone(),
                after_commit: after.clone(),
                task_ids: vec![owner_local.clone()],
                handoff_id: Some("handoff-local".into()),
            })
            .unwrap();
    }
    git(repo, &["merge", "-q", "--ff-only", "orbit/local"]);

    std::fs::create_dir_all(home.join("bin")).unwrap();
    let gh = home.join("bin/gh");
    std::fs::write(
        &gh,
        "#!/bin/sh\nsha=${2#*/commits/}\nexec cat \"$HOME/pulls/${sha%%/*}.json\"\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    publish_origin(repo);
    let diagnostic = evaluate_auto_task(owner, &definition, false, Utc::now()).unwrap();
    let state = diagnostic
        .state
        .clone()
        .unwrap_or_else(|| panic!("no consumer state: {diagnostic:#?}"));
    let batch = state
        .active
        .unwrap_or_else(|| panic!("no batch admitted: {:#?}", state.unresolved))
        .batch;
    let attributed = batch
        .deliveries
        .iter()
        .map(|delivery| {
            (
                delivery.key.as_str(),
                delivery.task_ids.clone(),
                delivery.unattributed.as_deref(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        attributed,
        vec![
            ("pr:owner/repository:main:42", vec![claimed], None),
            ("pr:owner/repository:main:7", vec![owner_run], None),
            (
                "pr:owner/repository:main:9",
                vec![],
                Some(UNATTRIBUTED_NO_LANDING_TASK)
            ),
            (
                "direct:owner/repository:main:handoff-local",
                vec![owner_local],
                None
            ),
        ]
    );
}
