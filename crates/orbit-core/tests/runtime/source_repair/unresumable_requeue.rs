//! [ORB-24075] A final-recovery requeue applies the source identity the next
//! run's candidate resume applies, so it never admits a successor that fails
//! there, and every identity refusal names its predicate.
use super::published_owner::{checkpoint, next, publication, requeue, save_run};
use super::*;
use orbit_core::application::task::{
    FinalRecoveryCompletion, FinalRecoveryOutcome, FinalRecoveryRequest,
};

/// Make the published source fail exactly one identity predicate.
fn break_identity(f: &mut Fixture, predicate: &str) {
    match predicate {
        "source_machine" => {
            f.runtime
                .sqlite_store()
                .unwrap()
                .with_transaction(|tx| {
                    tx.connection()
                        .execute(
                            "UPDATE job_runs SET executed_on_json = ?1 WHERE run_id = ?2",
                            [
                                json!({"machine_id":"hm_foreign","machine_name":null}).to_string(),
                                f.source.run_id.clone(),
                            ],
                        )
                        .unwrap();
                    Ok(())
                })
                .unwrap();
            f.source = f.runtime.show_job_run_observed(&f.source.run_id).unwrap();
        }
        "task_set" => {
            f.source.input.as_mut().unwrap()["task_ids"] = json!([f.task, "ORB-999999"]);
            save_run(f, &f.source);
        }
        _ => unreachable!(),
    }
}

fn final_recovery_requeue(f: &Fixture) -> FinalRecoveryOutcome {
    let observed = f
        .runtime
        .final_recovery_revision(&f.runtime.get_task(&f.task).unwrap())
        .unwrap();
    let request = FinalRecoveryRequest {
        task_id: f.task.clone(),
        run_id: f.source.run_id.clone(),
        observed,
        repo_root: f.repo.clone(),
        base_ref: "origin/main".into(),
        completion: FinalRecoveryCompletion::Review,
        requeue_bound: Default::default(),
    };
    f.runtime
        .apply_final_recovery(
            &request,
            Some(&json!({"decision":"requeue","reason":"observation connectivity recovered"})),
        )
        .unwrap()
}

/// The ORB-23890 shape: a stopped, published owner run whose source fails
/// the owner-source identity. The requeue escalates with the failed
/// predicate instead of preserving a publication the successor's
/// `candidate_resume` would refuse.
pub(super) fn requeue_refuses_a_publication_candidate_resume_would_refuse() {
    for predicate in ["source_machine", "task_set"] {
        let mut f = Fixture::new();
        publication(&mut f);
        break_identity(&mut f, predicate);
        let outcome = final_recovery_requeue(&f);
        let FinalRecoveryOutcome::Escalated {
            reason: Some(reason),
        } = &outcome
        else {
            panic!("{predicate}: requeue must not admit a successor: {outcome:?}");
        };
        assert!(
            reason.contains(&format!(": {predicate} (")),
            "{predicate}: the refusal names its predicate: {reason}"
        );
        let task = f.runtime.get_task(&f.task).unwrap();
        assert_eq!(task.status, TaskStatus::Blocked, "{predicate}");
        let history = f.runtime.get_task_history(&f.task).unwrap();
        assert!(
            !history
                .iter()
                .any(|entry| entry.event == "published_owner_requeued"),
            "{predicate}: no publication is preserved for a successor to adopt"
        );
        let recorded = history
            .iter()
            .filter(|entry| entry.event == "final_recovery_unresumable_publication")
            .map(|entry| serde_json::from_str::<Value>(entry.note.as_deref().unwrap()).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            recorded,
            vec![json!({"source_run_id":f.source.run_id,"step":"requeue","reason":predicate})],
            "{predicate}: the typed reason is recorded"
        );
    }
}

/// Every identity refusal candidate resume reaches names its predicate, and a
/// source that passes still resumes its publication.
pub(super) fn candidate_resume_identity_refusals_name_their_predicate() {
    for predicate in ["source_machine", "task_set"] {
        let mut f = Fixture::new();
        publication(&mut f);
        requeue(&mut f);
        break_identity(&mut f, predicate);
        let (run, setup) = next(&f);
        let refused = execute_deterministic_action(&f.runtime,"candidate_resume",&json!({}),&json!({"job_run_id":run.run_id,"task_ids":[f.task],"workspace_path":setup["workspace_path"],"base_sha":setup["base_sha"],"prior_job_run_id":setup["prior_job_run_id"]}),false,&HashMap::new(),None).unwrap_err();
        assert!(
            refused.to_string().contains(&format!(
                "not an exact owner-local single-task execution: {predicate} ("
            )),
            "{predicate}: {refused}"
        );
    }
    let mut f = Fixture::new();
    publication(&mut f);
    requeue(&mut f);
    let (run, setup) = next(&f);
    let resumed = f.action("candidate_resume", json!({"job_run_id":run.run_id,"task_ids":[f.task],"workspace_path":setup["workspace_path"],"base_sha":setup["base_sha"],"prior_job_run_id":setup["prior_job_run_id"]}));
    assert_eq!(resumed["outcome"], "resumed_carried");
    assert_eq!(resumed["source_run_id"], f.source.run_id);
    checkpoint(&f, &run.run_id, 6, "resume_candidate", resumed);
}
