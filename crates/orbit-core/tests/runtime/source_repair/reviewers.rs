//! Public recovery of reviewer-modified sources and refusal of forged provenance.
use super::*;

fn before_pr_reviewer_publication_preserves_the_pr() {
    reviewer_publication_preserves_the_pr(false);
}

fn before_landing_reviewer_publication_preserves_the_pr() {
    reviewer_publication_preserves_the_pr(true);
}

fn reviewer_publication_preserves_the_pr(landing: bool) {
    let mut f = Fixture::with_policy(if landing {
        "before-landing"
    } else {
        "before-pr"
    });
    f.publish_review(landing);
    let binding = f.inspect();
    assert_eq!(binding["head_sha"], f.head);
    assert_eq!(binding["pull_request"], "17");
    assert!(
        f.runtime
            .submit_resume_run(&f.source.run_id, Some("operator"), None)
            .is_err()
    );
    let result = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", f.submit(binding))
        .unwrap();
    let id = result["run_id"].as_str().unwrap();
    f.wait(id);
    assert_eq!(
        f.runtime.get_task(&f.task).unwrap().github_pr_number(),
        Some("17")
    );
    assert_eq!(git(&f.repo, &["rev-parse", &f.branch]), f.head);
    f.start_actions(id);
    let setup = f.action(
        "worktree_setup",
        json!({"job_run_id":id,"task_ids":[f.task],"base":"main","base_sync":"remote"}),
    );
    let checkout = PathBuf::from(setup["workspace_path"].as_str().unwrap());
    let before_branch = git(&checkout, &["branch", "--show-current"]);
    let mut changed = f.state.clone();
    let settle = if landing {
        "landing_review_gate_settle"
    } else {
        "review_gate_settle"
    };
    changed.pipeline[settle]["reviewed_head_sha"] = json!(f.base);
    f.runtime
        .write_run_state(&f.source.run_id, &changed)
        .unwrap();
    let error = execute_deterministic_action(&f.runtime, "candidate_resume", &json!({}),
        &json!({"job_run_id":id,"task_ids":[f.task],"workspace_path":checkout,"base_sha":setup["base_sha"]}),
        false, &HashMap::new(), None).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("changed before source application"),
        "{error}"
    );
    assert_eq!(git(&checkout, &["branch", "--show-current"]), before_branch);
    assert_eq!(
        std::fs::read_to_string(checkout.join("source.txt")).unwrap(),
        "base\n"
    );
    f.runtime
        .write_run_state(&f.source.run_id, &f.state)
        .unwrap();
    let resumed = f.action("candidate_resume", json!({"job_run_id":id,"task_ids":[f.task],"workspace_path":checkout,"base_sha":setup["base_sha"]}));
    assert_eq!(resumed["reused_head_sha"], f.head);
    assert_eq!(resumed["reused_branch"], f.branch);
    assert_eq!(resumed["prior_pull_request"], "17");
    assert_eq!(
        std::fs::read_to_string(checkout.join("review.txt")).unwrap(),
        "reviewer correction\n"
    );
    f.assert_original();
}

crate::scenario_table!(
    before_pr_reviewer_source,
    before_pr_reviewer_publication_preserves_the_pr
);
crate::scenario_table!(
    before_landing_reviewer_source,
    before_landing_reviewer_publication_preserves_the_pr
);

fn tampered_reviewer_publications_refuse_before_reservation() {
    for landing in [false, true] {
        let mut f = Fixture::with_policy(if landing {
            "before-landing"
        } else {
            "before-pr"
        });
        f.publish_review(landing);
        let binding = f.inspect();
        let clean = f.state.clone();
        let job_path = f
            ._root
            .path()
            .join("global/resources/jobs/task_pr_pipeline.yaml");
        let job_text = std::fs::read_to_string(&job_path).unwrap();
        let clean_job: Value = serde_yaml::from_str(&job_text).unwrap();
        let clean_answer = std::fs::read_to_string(&f.answer).unwrap();
        let settle = if landing {
            "landing_review_gate_settle"
        } else {
            "review_gate_settle"
        };
        let validate = if landing {
            "landing_review_validate"
        } else {
            "review_validate"
        };
        let publication = if landing { "landing_push" } else { "push" };
        for case in [
            "missing_review",
            "failed_review",
            "review_copy",
            "review_host",
            "review_head",
            "review_base",
            "review_implementation",
            "review_fixed",
            "review_verdict",
            "missing_validation",
            "failed_validation",
            "validation_copy",
            "validation_head",
            "validation_branch",
            "validation_decision",
            "ownership_binding",
            "missing_publication",
            "failed_publication",
            "publication_copy",
            "publication_head",
            "publication_branch",
            "publication_lease",
            "pr_identity",
            "lease_binding",
            "pr_binding",
        ] {
            if !landing && matches!(case, "lease_binding" | "pr_binding") {
                continue;
            }
            f.state = clean.clone();
            let mut job = clean_job.clone();
            let index = |id: &str| {
                job["spec"]["steps"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .position(|step| step["id"] == id)
                    .unwrap() as u32
            };
            let (step, key, value) = match case {
                "missing_review" | "failed_review" | "review_copy" | "review_host" => {
                    (settle, "", Value::Null)
                }
                "review_head" => (settle, "reviewed_head_sha", json!(f.base)),
                "review_base" => (settle, "reviewed_base_sha", json!(f.head)),
                "review_implementation" => (settle, "implementation_head_sha", json!(f.base)),
                "review_fixed" => (settle, "reviewer_fixed", json!(false)),
                "review_verdict" => (settle, "gate", json!("rejected")),
                "missing_validation" | "failed_validation" | "validation_copy"
                | "ownership_binding" => (validate, "", Value::Null),
                "validation_head" => (validate, "tested_head", json!(f.base)),
                "validation_branch" => (validate, "branch", json!("foreign")),
                "validation_decision" => (validate, "decision", json!("failed")),
                "publication_head" => (publication, "local_sha", json!(f.base)),
                "publication_branch" => (publication, "branch", json!("foreign")),
                "publication_lease" => (publication, "remote_sha_before", json!(f.base)),
                _ => (publication, "", Value::Null),
            };
            let index = index(step);
            if case.starts_with("missing_") {
                f.state.pipeline.as_object_mut().unwrap().remove(step);
            } else if case.starts_with("failed_") {
                f.state.step_states.insert(index, JobRunState::Failed);
            } else if case.ends_with("_copy") {
                f.state
                    .step_outputs
                    .insert(index, json!({"foreign":"output"}));
            } else if case == "review_host" {
                job["spec"]["steps"][index as usize]["target"] = json!("activity:git_push");
            } else if case == "ownership_binding" {
                job["spec"]["steps"][index as usize]["default_input"]["ownership_base_sha"] =
                    json!(f.base);
            } else if matches!(case, "lease_binding" | "pr_binding") {
                let key = if case == "lease_binding" {
                    "lease_remote_sha"
                } else {
                    "pr_number"
                };
                job["spec"]["steps"][index as usize]["default_input"][key] = json!("foreign");
            } else if case == "pr_identity" {
                let mut answer: Value = serde_json::from_str(&clean_answer).unwrap();
                answer["number"] = json!(18);
                std::fs::write(&f.answer, answer.to_string()).unwrap();
            } else {
                f.state.pipeline[step][key] = value;
            }
            f.runtime
                .write_run_state(&f.source.run_id, &f.state)
                .unwrap();
            std::fs::write(&job_path, serde_yaml::to_string(&job).unwrap()).unwrap();
            for input in [f.request("inspect"), f.submit(binding.clone())] {
                assert!(
                    f.runtime
                        .run_tool_as_human("orbit.workflow.run.resume", input)
                        .is_err(),
                    "landing={landing}, {case}"
                );
            }
            assert!(
                !f.runtime
                    .get_task_history(&f.task)
                    .unwrap()
                    .iter()
                    .any(|entry| entry.event == "source_repair_reserved"),
                "{case} must not reserve authority"
            );
            assert_eq!(
                f.runtime.get_task(&f.task).unwrap().status,
                TaskStatus::Review
            );
            assert_eq!(git(&f.repo, &["rev-parse", &f.branch]), f.head);
            std::fs::write(&f.answer, &clean_answer).unwrap();
        }
    }
}

crate::scenario_table!(
    reviewer_publication_tampering,
    tampered_reviewer_publications_refuse_before_reservation
);
