//! Desktop review of a native publication after normal discard/requeue.
use super::published_owner::publication_number;
use super::*;
use orbit_types::desktop::{
    DesktopCriterionOutcome, DesktopReviewDecision, DesktopReviewVerdict, DesktopTaskOperation,
    DesktopTaskRequest, DesktopTaskSnapshot,
};
use orbit_types::tool::{McpCapability, ToolSessionContext};
use std::collections::BTreeSet;

fn operator() -> ToolSessionContext {
    ToolSessionContext {
        effective_capabilities: BTreeSet::from([McpCapability::Operator]),
        ..Default::default()
    }
}

fn snapshot(f: &Fixture) -> DesktopTaskSnapshot {
    f.runtime
        .desktop_task_snapshot(&f.task, &operator())
        .unwrap()
}

fn request(f: &Fixture, observed: &DesktopTaskSnapshot, key: &str) -> DesktopTaskRequest {
    DesktopTaskRequest {
        request_id: key.into(),
        operation: DesktopTaskOperation::Review {
            id: f.task.clone(),
            expected_revision: observed.revision.clone(),
            complete: false,
            verdict: DesktopReviewVerdict {
                decision: DesktopReviewDecision::Accept,
                rationale: "The authenticated current publication satisfies the task.".into(),
                criteria: observed
                    .task
                    .acceptance_criteria
                    .iter()
                    .map(|criterion| DesktopCriterionOutcome {
                        criterion: criterion.clone(),
                        met: true,
                        evidence: vec!["execution_summary".into()],
                    })
                    .collect(),
                evidence: vec!["execution_summary".into()],
                expected_run_id: observed.task.job_run_id.clone(),
                expected_head: observed.reviewed_head.clone(),
            },
        },
    }
}

fn write(
    f: &Fixture,
    request: DesktopTaskRequest,
) -> Result<orbit_types::desktop::DesktopTaskWriteResult, orbit_common::OrbitError> {
    f.runtime
        .desktop_task_write(request, None, None, &operator())
}

/// The normal discard/requeue path retains the old PR, then publishes a new
/// native delivery. Desktop review must bind to that delivery, in either ref order.
fn desktop_reviews_current_native_publication_after_discard() {
    let mut f = Fixture::new();
    publication_number(&mut f, false, false, true, "17", false);
    let single = snapshot(&f);
    assert_eq!(single.reviewed_head.as_deref(), Some(f.head.as_str()));
    let accepted = write(&f, request(&f, &single, "single-reference-review")).unwrap();
    assert_eq!(accepted.snapshot.task.status, TaskStatus::Review);
    let prior = f.runtime.get_task(&f.task).unwrap();
    let prior_state =
        serde_json::to_value(f.runtime.read_run_state(&f.source.run_id).unwrap()).unwrap();
    let prior_run = f.source.run_id.clone();
    f.runtime
        .update_task_as_human(
            &f.task,
            orbit_core::application::task::TaskUpdateParams {
                status: Some(TaskStatus::Backlog),
                discard_candidate: true,
                ..Default::default()
            },
            "operator".into(),
        )
        .unwrap();
    publication_number(&mut f, false, false, true, "18", true);
    let task = f.runtime.get_task(&f.task).unwrap();
    assert_eq!(task.external_refs.len(), prior.external_refs.len() + 1);
    assert_eq!(
        &task.external_refs[..prior.external_refs.len()],
        &prior.external_refs
    );
    assert_eq!(task.job_run_id.as_deref(), Some(f.source.run_id.as_str()));
    assert_eq!(
        serde_json::to_value(f.runtime.read_run_state(&prior_run).unwrap()).unwrap(),
        prior_state
    );
    let observed = snapshot(&f);
    assert_eq!(
        observed.reviewed_head.as_deref(),
        Some(f.head.as_str()),
        "{:?}",
        observed.reviewed_head_reason
    );
    assert!(
        observed.actions.complete.enabled,
        "{:?}",
        observed.actions.complete
    );
    assert_eq!(observed.task.job_run_machine, prior.job_run_machine);

    // A moved head and every provider identity component refuse both the
    // observation and a verdict captured before the provider changed.
    let provider = std::fs::read(&f.answer).unwrap();
    let answer: Value = serde_json::from_slice(&provider).unwrap();
    for (field, value) in [
        ("number", json!(19)),
        ("url", json!("https://github.com/other/repository/pull/18")),
        ("headRefName", json!("other-branch")),
        ("baseRefName", json!("other-base")),
        ("headRefOid", json!("a".repeat(40))),
        ("headRefOid", Value::Null),
    ] {
        let mut changed = answer.clone();
        changed[field] = value;
        std::fs::write(&f.answer, changed.to_string()).unwrap();
        assert!(snapshot(&f).reviewed_head.is_none(), "{field}");
        assert!(
            write(&f, request(&f, &observed, &format!("provider-{field}"))).is_err(),
            "{field}"
        );
    }
    std::fs::write(&f.answer, provider).unwrap();

    // Corrupt only refusal fixtures, restoring the exact host-produced proof
    // each time; no successful checkpoint is invented by these cases.
    let state = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
    for case in ["missing", "copy", "run", "branch"] {
        let mut changed = state.clone();
        match case {
            "missing" => {
                changed.pipeline.as_object_mut().unwrap().remove("pr_open");
            }
            "copy" => changed.pipeline["pr_open"]["pr_number"] = json!("19"),
            "run" => changed.run_id = "unrelated-run".into(),
            "branch" => changed.pipeline["push"]["branch"] = json!("other-branch"),
            _ => unreachable!(),
        }
        f.runtime
            .write_run_state(&f.source.run_id, &changed)
            .unwrap();
        assert!(snapshot(&f).reviewed_head.is_none(), "{case}");
        assert!(
            write(&f, request(&f, &observed, &format!("proof-{case}"))).is_err(),
            "{case}"
        );
    }
    f.runtime.write_run_state(&f.source.run_id, &state).unwrap();
    for case in ["task", "claimed", "unfinished"] {
        let mut changed = f.source.clone();
        match case {
            "task" => changed.input.as_mut().unwrap()["task_ids"] = json!(["unrelated-task"]),
            "claimed" => changed.input.as_mut().unwrap()["claimed"] = json!(true),
            "unfinished" => changed.state = JobRunState::Running,
            _ => unreachable!(),
        }
        published_owner::save_run(&f, &changed);
        assert!(snapshot(&f).reviewed_head.is_none(), "{case}");
    }
    published_owner::save_run(&f, &f.source);
    // The same stored run is not authority for a task bound to another host.
    let other_host =
        orbit_core::OrbitRuntime::from_roots(&f.runtime.global_root(), &f.repo.join(".orbit"))
            .unwrap()
            .with_automation_execution_location(Some(orbit_types::task::ExecutionLocation {
                machine_id: "hm_other".into(),
                machine_name: None,
            }));
    let foreign = other_host
        .desktop_task_snapshot(&f.task, &operator())
        .unwrap();
    assert!(foreign.reviewed_head.is_none(), "{foreign:?}");
    assert!(!foreign.actions.complete.enabled);

    for case in ["repository", "number", "url", "no_current"] {
        let mut refs = task.external_refs.clone();
        match case {
            "repository" => {
                refs[1].url = Some("https://github.com/other/repository/pull/18".into())
            }
            "number" => refs[1].id = "19".into(),
            "url" => refs[1].url = refs[0].url.clone(),
            "no_current" => {
                refs[1].id = "19".into();
                refs[1].url = Some("https://github.com/owner/repository/pull/19".into());
            }
            _ => unreachable!(),
        }
        f.replace_refs(refs);
        assert!(snapshot(&f).reviewed_head.is_none(), "{case}");
    }
    // Order cannot choose authority, and key-only current refs remain valid.
    let mut reversed = task.external_refs.clone();
    reversed.reverse();
    f.replace_refs(reversed);
    assert_eq!(snapshot(&f).reviewed_head, observed.reviewed_head);
    let mut key_only = task.external_refs.clone();
    key_only[1].url = None;
    f.replace_refs(key_only);
    assert_eq!(snapshot(&f).reviewed_head, observed.reviewed_head);
    f.replace_refs(task.external_refs.clone());

    let stale_baseline = snapshot(&f);
    for case in ["revision", "run", "head"] {
        let mut stale = request(&f, &stale_baseline, &format!("stale-{case}"));
        if let DesktopTaskOperation::Review {
            expected_revision,
            verdict,
            ..
        } = &mut stale.operation
        {
            match case {
                "revision" => *expected_revision = "stale".into(),
                "run" => verdict.expected_run_id = Some(prior_run.clone()),
                "head" => verdict.expected_head = Some("a".repeat(40)),
                _ => unreachable!(),
            }
        }
        assert!(write(&f, stale).is_err(), "{case}");
    }
    let fresh = snapshot(&f);
    let accepted = write(&f, request(&f, &fresh, "current-publication-review")).unwrap();
    assert_eq!(accepted.snapshot.task.status, TaskStatus::Review);
    assert_eq!(accepted.snapshot.reviewed_head, fresh.reviewed_head);
    let mut complete = request(&f, &accepted.snapshot, "current-publication-complete");
    if let DesktopTaskOperation::Review { complete, .. } = &mut complete.operation {
        *complete = true;
    }
    let done = write(&f, complete).unwrap();
    assert_eq!(done.snapshot.task.status, TaskStatus::Done);
    assert_eq!(done.snapshot.task.external_refs, task.external_refs);
    assert_eq!(done.snapshot.task.job_run_id, task.job_run_id);
    assert_eq!(done.snapshot.task.job_run_machine, task.job_run_machine);
}

crate::scenario_table!(
    desktop_native_publication_scenarios,
    desktop_reviews_current_native_publication_after_discard
);

/// A real same-PR rework authenticates the target through public inspection
/// and admission, then refreshes a section-gap draft with its actual URL.
fn same_pr_task_link_uses_authenticated_target() {
    let mut f = Fixture::with_policy("before-landing");
    let config = f.repo.join(".orbit/config.toml");
    let configured = format!(
        "{}\n[pr]\nrequired_sections = [\"Rollback\"]\n",
        std::fs::read_to_string(&config).unwrap()
    );
    std::fs::write(&config, configured).unwrap();
    f.runtime =
        orbit_core::OrbitRuntime::from_roots(&f.runtime.global_root(), &f.repo.join(".orbit"))
            .unwrap()
            .with_automation_execution_location(Some(orbit_types::task::ExecutionLocation {
                machine_id: "hm_fixture".into(),
                machine_name: None,
            }));
    f.runtime
        .apply_task_automation_update(
            &f.task,
            TaskAutomationUpdate {
                external_refs: vec![
                    orbit_types::task::ExternalRef {
                        system: "other".into(),
                        id: "old".into(),
                        url: Some("https://unrelated.example/old".into()),
                    },
                    orbit_types::task::ExternalRef {
                        system: "github-pr".into(),
                        id: "12".into(),
                        url: Some("https://github.com/owner/repository/pull/12".into()),
                    },
                ],
                ..Default::default()
            },
        )
        .unwrap();
    publication_number(&mut f, false, false, true, "17", false);
    let initial = std::fs::read_to_string(f._root.path().join("pr-body")).unwrap();
    assert!(
        initial.starts_with(&format!("## Task\n\n{} —", f.task)),
        "{initial}"
    );
    assert!(initial.contains("Missing required section"), "{initial}");
    let refs = f.runtime.get_task(&f.task).unwrap().external_refs;
    f.runtime.update_task_with_identity(&f.task, orbit_core::application::task::TaskUpdateParams {
        description: Some("Bounded current specification: reduce redundant fixture work while preserving critical behavioral coverage.".into()),
        upsert_artifacts: vec![orbit_types::task::TaskArtifact::from_text("pr-sections.json", r#"{"sections":{"Rollback":"Revert this bounded change."}}"#)],
        ..Default::default()
    }, None, Some("codex".into())).unwrap();
    let admitted = f
        .runtime
        .run_tool_as_human(
            "orbit.workflow.run.resume",
            published_owner::rework_request(&f),
        )
        .unwrap();
    let run_id = admitted["run_id"].as_str().unwrap();
    super::generations::publish_and_stop(&f, run_id, 1, false);
    let refreshed = std::fs::read_to_string(f._root.path().join("pr-body")).unwrap();
    assert!(
        refreshed.starts_with(&format!(
            "## Task\n\n[{}](https://github.com/owner/repository/pull/17) —",
            f.task
        )),
        "{refreshed}"
    );
    assert!(
        !refreshed.contains("https://unrelated.example/old"),
        "{refreshed}"
    );
    assert!(
        !refreshed.contains("https://github.com/owner/repository/pull/12"),
        "{refreshed}"
    );
    assert_eq!(f.runtime.get_task(&f.task).unwrap().external_refs, refs);
    let calls = std::fs::read_to_string(f._root.path().join("gh-calls")).unwrap();
    assert!(calls.contains("pr edit 17 --body"), "{calls}");
    assert!(calls.contains("pr ready 17"), "{calls}");
}

crate::scenario_table!(
    same_pr_task_link_scenarios,
    same_pr_task_link_uses_authenticated_target
);
