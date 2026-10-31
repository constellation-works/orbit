//! Review-to-backlog updates authenticate a publication before admitting it.
use super::*;
use orbit_types::task::TaskHistoryEntry;

fn put_in_review(f: &Fixture) {
    f.runtime
        .apply_task_automation_update(
            &f.task,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Review),
                ..Default::default()
            },
        )
        .unwrap();
}

/// The observed omission: the public update admitted an inline publication
/// with red checks and no binding, which the next candidate resume refused.
fn review_return_refuses_an_unauthenticated_inline_publication() {
    let (mut f, _, _) = failed_publication::escalated_completion_fixture();
    let forge = escalated_publication::green_forge(&mut f);
    forge.set(&f.head, &["Check"], &[], &["Test"]);
    put_in_review(&f);
    let task = serde_json::to_value(f.runtime.get_task(&f.task).unwrap()).unwrap();
    let history = serde_json::to_value(f.runtime.get_task_history(&f.task).unwrap()).unwrap();
    let source = serde_json::to_value(f.runtime.read_run_state(&f.source.run_id).unwrap()).unwrap();
    let refs = git(&f.repo, &["ls-remote", "origin"]);
    let answer = std::fs::read(&f.answer).unwrap();
    let update = handoff_requeue::plain_backlog(&f, false);
    if update.is_ok() {
        // Keep the base reproduction at the public resume boundary, rather
        // than stopping at the incorrectly admitted task update.
        let (run, setup) = published_owner::next(&f);
        let error = execute_deterministic_action(
            &f.runtime,
            "candidate_resume",
            &json!({}),
            &json!({"job_run_id":run.run_id,"task_ids":[f.task],
                "workspace_path":setup["workspace_path"],"base_sha":setup["base_sha"]}),
            false,
            &HashMap::new(),
            None,
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("unique requeue binding"),
            "{error}"
        );
        panic!(
            "review-to-backlog admitted an unauthenticated publication; candidate_resume then refused: {error}"
        );
    }
    let error = update.unwrap_err().to_string();
    assert!(error.contains("required checks failed"), "{error}");
    assert!(error.contains("requeue again when they pass"), "{error}");
    assert_eq!(
        serde_json::to_value(f.runtime.get_task(&f.task).unwrap()).unwrap(),
        task
    );
    assert_eq!(
        serde_json::to_value(f.runtime.get_task_history(&f.task).unwrap()).unwrap(),
        history
    );
    assert_eq!(
        serde_json::to_value(f.runtime.read_run_state(&f.source.run_id).unwrap()).unwrap(),
        source
    );
    assert_eq!(git(&f.repo, &["ls-remote", "origin"]), refs);
    assert_eq!(std::fs::read(&f.answer).unwrap(), answer);
}

/// Snapshot the public task, audit trail, immutable source and publication.
fn review_publication_snapshot(f: &Fixture) -> Value {
    json!({
        "task": f.runtime.get_task(&f.task).unwrap(),
        "history": f.runtime.get_task_history(&f.task).unwrap(),
        "source": f.runtime.read_run_state(&f.source.run_id).unwrap(),
        "run": f.runtime.show_job_run_observed(&f.source.run_id).unwrap(),
        "refs": git(&f.repo, &["ls-remote", "origin"]),
        "refs_local": git(&f.repo, &["for-each-ref", "--format=%(refname):%(objectname)"]),
        "forge": std::fs::read(&f.answer).unwrap(),
    })
}

fn review_update_refused(f: &Fixture, edits: Value, case: &str) -> String {
    let before = review_publication_snapshot(f);
    let mut input = json!({"id":f.task,"workspace":f.repo,"status":"backlog","model":"codex"});
    input
        .as_object_mut()
        .unwrap()
        .extend(edits.as_object().unwrap().clone());
    let error = f
        .runtime
        .run_tool("orbit.task.update", input)
        .err()
        .unwrap_or_else(|| panic!("{case}: unexpectedly admitted review return"))
        .to_string();
    assert_eq!(
        review_publication_snapshot(f),
        before,
        "{case}: refusal mutated state"
    );
    error
}

fn preservation_records(f: &Fixture) -> Vec<TaskHistoryEntry> {
    f.runtime
        .get_task_history(&f.task)
        .unwrap()
        .into_iter()
        .filter(|e| e.event == "published_owner_requeued")
        .collect()
}

/// Both publication contracts work from review, and returning an already
/// preserved candidate writes no duplicate binding before the next adoption.
fn authenticated_review_returns_keep_one_binding_and_the_same_pull_request() {
    for inline in [false, true] {
        let mut f = if inline {
            failed_publication::escalated_completion_fixture().0
        } else {
            failed_publication::escalated_handoff_fixture(None, None).0
        };
        if inline {
            escalated_publication::green_forge(&mut f);
        }
        put_in_review(&f);
        handoff_requeue::plain_backlog(&f, false).unwrap();
        let records = preservation_records(&f);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].by, "system");
        put_in_review(&f);
        handoff_requeue::plain_backlog(&f, false).unwrap();
        assert_eq!(preservation_records(&f).len(), 1);
        assert_eq!(preservation_records(&f)[0].note, records[0].note);
        handoff_requeue::assert_next_run_resumes_the_published_candidate(&f);
    }
}

/// Unpublished tasks still return to backlog without publication authority.
fn review_returns_without_a_publication_remain_supported() {
    let f = Fixture::new();
    let task = f
        .runtime
        .add_task(orbit_core::application::task::TaskAddParams {
            title: "An unpublished review".into(),
            status: Some(TaskStatus::Review),
            ..Default::default()
        })
        .unwrap();
    let result = f
        .runtime
        .run_tool(
            "orbit.task.update",
            json!({
                "id":task.id,"workspace":f.repo,"status":"backlog","model":"codex"
            }),
        )
        .unwrap();
    assert_eq!(result["status"], "backlog");
    assert!(
        !f.runtime
            .get_task_history(&task.id)
            .unwrap()
            .iter()
            .any(|e| e.event == "published_owner_requeued")
    );
}

/// The same omitted edge must not admit stale checks, forged source identity
/// or a mixed status/specification update that requires re-authoring.
fn review_returns_keep_publication_identity_scope_and_check_refusals() {
    let (mut f, _, _) = failed_publication::escalated_completion_fixture();
    let forge = escalated_publication::green_forge(&mut f);
    put_in_review(&f);
    for (case, head, failed, pending, passed) in [
        (
            "pending checks",
            &f.head,
            vec![],
            vec!["Check"],
            vec!["Test"],
        ),
        ("missing checks", &f.head, vec![], vec![], vec![]),
        ("stale checks", &f.base, vec![], vec![], vec!["Check"]),
    ] {
        forge.set(head, &failed, &pending, &passed);
        review_update_refused(&f, json!({}), case);
    }
    forge.set(&f.head, &[], &[], &["Check", "Test"]);
    let original = std::fs::read(&f.answer).unwrap();
    let mut answer: Value = serde_json::from_slice(&original).unwrap();
    answer["headRefOid"] = json!(f.base);
    std::fs::write(&f.answer, answer.to_string()).unwrap();
    review_update_refused(&f, json!({}), "moved head");
    std::fs::write(&f.answer, original).unwrap();
    for edits in [
        json!({"description":"A different mandate"}),
        json!({"acceptance_criteria":["A different guarantee"]}),
    ] {
        review_update_refused(&f, edits, "mixed specification and backlog update");
    }
    let stopped = f.runtime.show_job_run_observed(&f.source.run_id).unwrap();
    for case in ["live", "mixed tasks", "claimed"] {
        let mut run = stopped.clone();
        match case {
            "live" => {
                run.state = JobRunState::Running;
                run.pid = Some(std::process::id());
                run.finished_at = None;
            }
            "mixed tasks" => run.input.as_mut().unwrap()["task_ids"] = json!([f.task, "ORB-0"]),
            "claimed" => run.input.as_mut().unwrap()["claimed"] = json!(true),
            _ => unreachable!(),
        }
        published_owner::save_run(&f, &run);
        review_update_refused(&f, json!({}), case);
        published_owner::save_run(&f, &stopped);
    }
    let local = stopped.executed_on.unwrap();
    for origin in ["hm_foreign", local.machine_id.as_str()] {
        f.runtime
            .sqlite_store()
            .unwrap()
            .with_transaction(|tx| {
                tx.connection()
                    .execute(
                        "UPDATE job_runs SET executed_on_json = ?1 WHERE run_id = ?2",
                        [
                            json!({"machine_id":origin,"machine_name":null}).to_string(),
                            f.source.run_id.clone(),
                        ],
                    )
                    .unwrap();
                Ok(())
            })
            .unwrap();
        if origin == "hm_foreign" {
            review_update_refused(&f, json!({}), "foreign source");
        }
    }
    // The original still authenticates after restoring each negative case.
    handoff_requeue::plain_backlog(&f, false).unwrap();
    assert_eq!(preservation_records(&f).len(), 1);
}

/// History must be fully read before a retry is admitted: an identical first
/// record cannot hide a duplicate, malformed or untrusted preservation later.
fn review_returns_refuse_ambiguous_or_unreadable_preservations() {
    for case in ["duplicate", "malformed", "untrusted"] {
        let (f, _) = failed_publication::escalated_handoff_fixture(None, None);
        put_in_review(&f);
        handoff_requeue::plain_backlog(&f, false).unwrap();
        let mut entry = preservation_records(&f).pop().unwrap();
        put_in_review(&f);
        match case {
            "malformed" => entry.note = Some("{".into()),
            "untrusted" => entry.by = "human".into(),
            _ => (),
        }
        f.runtime
            .apply_task_automation_update(
                &f.task,
                TaskAutomationUpdate {
                    append_history: vec![entry],
                    ..Default::default()
                },
            )
            .unwrap();
        review_update_refused(&f, json!({}), case);
    }
}

fn review_returns_refuse_discarded_candidates_and_newer_executions() {
    for case in ["discarded", "newer execution"] {
        let (f, _) = failed_publication::escalated_handoff_fixture(None, None);
        put_in_review(&f);
        if case == "discarded" {
            f.runtime
                .apply_task_automation_update(
                    &f.task,
                    TaskAutomationUpdate {
                        append_history: vec![TaskHistoryEntry {
                            at: Utc::now(),
                            by: "system".into(),
                            event: orbit_types::task::CANDIDATE_DISCARDED_EVENT.into(),
                            note: Some("fixture discard".into()),
                            from_status: None,
                            to_status: None,
                            operation_id: None,
                            decision: None,
                        }],
                        ..Default::default()
                    },
                )
                .unwrap();
        } else {
            f.runtime
                .insert_job_run(
                    "task_pr_pipeline",
                    1,
                    Utc::now(),
                    Some(json!({"task_ids":[f.task]})),
                    None,
                )
                .unwrap();
        }
        review_update_refused(&f, json!({}), case);
    }
}

/// Re-homing a foreign binding is not authority to adopt its publication.
fn review_recovery_refuses_wrong_workspace_and_foreign_bindings() {
    let f = Fixture::new();
    let binding = f.inspect();
    for (field, value) in [
        ("workspace_id", "ws_foreign"),
        ("source_run_id", "jrun_foreign"),
    ] {
        let mut altered = binding.clone();
        altered[field] = json!(value);
        let before = review_publication_snapshot(&f);
        let error = f
            .runtime
            .run_tool_as_human("orbit.workflow.run.resume", f.submit(altered))
            .unwrap_err();
        assert!(error.to_string().contains("refused"), "{field}: {error}");
        assert_eq!(
            review_publication_snapshot(&f),
            before,
            "{field}: refused recovery mutated state"
        );
    }
}

crate::scenario_table!(
    review_requeue_scenarios,
    review_return_refuses_an_unauthenticated_inline_publication,
    authenticated_review_returns_keep_one_binding_and_the_same_pull_request,
    review_returns_without_a_publication_remain_supported,
    review_returns_keep_publication_identity_scope_and_check_refusals,
    review_returns_refuse_discarded_candidates_and_newer_executions,
    review_recovery_refuses_wrong_workspace_and_foreign_bindings
);

crate::scenario_table!(
    review_preservation_scenarios,
    review_returns_refuse_ambiguous_or_unreadable_preservations
);
