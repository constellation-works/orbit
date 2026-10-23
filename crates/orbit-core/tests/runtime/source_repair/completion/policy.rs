//! Config drift cannot turn a read-only forge outage into an operator repair.
use super::*;

fn reload(f: &mut Fixture) {
    f.runtime = orbit_core::OrbitRuntime::from_roots(
        &f._root.path().join("global"),
        &f.repo.join(".orbit"),
    )
    .unwrap()
    .with_automation_execution_location(Some(orbit_types::task::ExecutionLocation {
        machine_id: "hm_fixture".into(),
        machine_name: None,
    }));
}

fn capture_legacy_no_gate(f: &mut Fixture, checkout: &std::path::Path) {
    let policy = f.runtime.operation_policy();
    // The retired after-landing admission never gated the candidate. Its
    // provenance and unused reviewer fields need not equal today's no-gate snapshot.
    f.source.input.as_mut().unwrap()["review"] = json!({
        "contract_version": orbit_types::workflow::REVIEW_CONTRACT_VERSION,
        "policy_version": policy.version, "timing":"after-landing",
        "timing_source":"global", "crew":"fixture", "crew_source":"global",
        "budget":{"minutes":30}, "required_validation_commands":["true"],
        "baseline_commands":[], "host_evidence":[], "captured_at":Utc::now(),
    });
    published_owner::save_run(f, &f.source);
    let validation = f.action(
        "candidate_validate",
        json!({
            "job_run_id":f.source.run_id,"completed_task_ids":[f.task],
            "workspace_path":checkout,"base_sha":f.state.pipeline["sync_base"]["base_sha"],
        }),
    );
    assert_eq!(validation["decision"], "passed");
    published_owner::checkpoint(f, &f.source.run_id, 4, "validate", validation);
}

pub(crate) fn equivalent_policy_drift_retries_observation() {
    for case in ["captured", "current", "legacy", "review_provenance"] {
        let (mut f, checkout) = fixture();
        if case == "review_provenance" {
            configure_review(&mut f, &checkout);
            f.source.input.as_mut().unwrap()["review"]["timing_source"] = json!("global");
            f.source.input.as_mut().unwrap()["review"]["crew_source"] = json!("global");
            published_owner::save_run(&f, &f.source);
            let validation = f.action(
                "candidate_validate",
                json!({
                    "job_run_id":f.source.run_id,"completed_task_ids":[f.task],
                    "workspace_path":checkout,"base_sha":f.state.pipeline["sync_base"]["base_sha"],
                }),
            );
            published_owner::checkpoint(&f, &f.source.run_id, 4, "validate", validation);
        } else {
            capture_legacy_no_gate(&mut f, &checkout);
        }
        if case == "legacy" {
            let mut validation = f
                .runtime
                .read_run_state(&f.source.run_id)
                .unwrap()
                .unwrap()
                .pipeline["validate"]
                .clone();
            let policy: Value =
                serde_json::from_str(validation["completion_policy"].as_str().unwrap()).unwrap();
            validation["completion_policy"] = policy["strict"].clone();
            published_owner::checkpoint(&f, &f.source.run_id, 4, "validate", validation);
        }
        if case == "current" {
            let config = f.repo.join(".orbit/config.toml");
            let text = std::fs::read_to_string(&config).unwrap();
            std::fs::write(config, format!("{text}minutes = 31\n")).unwrap();
            reload(&mut f);
        }
        let calls = tail_forge(&f, "lost_read");
        if case == "current" {
            let gh = f._root.path().join("bin/gh");
            let script = std::fs::read_to_string(&gh).unwrap();
            std::fs::write(gh, script.replace("TLS handshake timeout", "gh: HTTP 503")).unwrap();
        }
        let before = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
        let (outcome, _) = run_tail(&f);
        assert!(outcome.as_ref().is_ok_and(|o| o.success), "{outcome:?}");
        assert_eq!(
            f.runtime.get_task(&f.task).unwrap().status,
            TaskStatus::Done
        );
        let after = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
        assert!(matches!(&after.final_recovery.as_ref().unwrap().decision,
            Some(orbit_types::workflow::FinalRecoveryDecision::Resume { rationale, .. })
            if rationale.contains("transient-observation")));
        for (index, output) in before.step_outputs {
            assert_eq!(
                after.step_output(index),
                Some(&output),
                "receipts remain immutable"
            );
        }
        let requests = std::fs::read_to_string(calls).unwrap();
        assert_eq!(
            requests
                .lines()
                .filter(|l| l.starts_with("pr view"))
                .count(),
            4,
            "failed observation, authentication, fresh status and merged readback"
        );
        assert_eq!(
            requests
                .lines()
                .filter(|l| l.starts_with("api repos/"))
                .count(),
            1,
            "one merge of the authenticated head"
        );
        assert_eq!(git(&checkout, &["rev-parse", "HEAD"]), f.head);
    }
}

pub(crate) fn relevant_policy_drift_preserves_publication() {
    for change in ["new_gate", "budget", "baseline"] {
        let already_reviewed = change != "new_gate";
        let (mut f, checkout) = fixture();
        if already_reviewed {
            configure_review(&mut f, &checkout);
        } else {
            capture_legacy_no_gate(&mut f, &checkout);
        }
        let config = f.repo.join(".orbit/config.toml");
        let text = std::fs::read_to_string(&config).unwrap();
        std::fs::write(
            config,
            match change {
                "budget" => format!("{text}minutes = 31\n"),
                "baseline" => format!("{text}baseline_commands = [\"true\"]\n"),
                _ => text.replace("before_pr = false", "before_pr = true"),
            },
        )
        .unwrap();
        reload(&mut f);
        let calls = tail_forge(&f, "lost_read");
        let (outcome, started) = run_tail(&f);
        assert!(!outcome.as_ref().is_ok_and(|o| o.success), "{outcome:?}");
        assert_eq!(started, ["complete_pr"]);
        assert_eq!(
            f.runtime.get_task(&f.task).unwrap().status,
            TaskStatus::Backlog
        );
        let after = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
        assert!(matches!(&after.final_recovery.as_ref().unwrap().decision,
            Some(orbit_types::workflow::FinalRecoveryDecision::Requeue { reason })
            if reason.contains("policy-changed") && reason.contains(match change { "budget" => "review budget", "baseline" => "baseline commands", _ => "timing" })));
        assert!(
            f.runtime
                .get_task_history(&f.task)
                .unwrap()
                .iter()
                .any(|e| e.event == "published_owner_requeued"),
            "next run retains the published candidate"
        );
        assert!(
            !std::fs::read_to_string(calls)
                .unwrap()
                .lines()
                .any(|l| l.starts_with("api repos/"))
        );
        assert_eq!(git(&checkout, &["rev-parse", "HEAD"]), f.head);
        // As the real executor does when the failed run stops, release its
        // ownership before driving the next run's governed candidate adoption.
        f.source.state = JobRunState::Failed;
        f.source.pid = None;
        f.source.finished_at = Some(Utc::now());
        published_owner::save_run(&f, &f.source);
        f.state = after;
        let (run, setup) = published_owner::next(&f);
        let resumed = f.action("candidate_resume", json!({"job_run_id":run.run_id,
            "task_ids":[f.task],"workspace_path":setup["workspace_path"],"base_sha":setup["base_sha"]}));
        assert_eq!(resumed["implement"], false);
        assert_eq!(resumed["source_run_id"], f.source.run_id);
        assert_eq!(resumed["prior_pull_request"], "17");
    }
}

pub(crate) fn candidate_drift_refuses_equivalent_policy_retry() {
    for case in ["head", "base", "receipt", "review"] {
        let (mut f, checkout) = fixture();
        if case == "review" {
            configure_review(&mut f, &checkout);
            let mut settle = f
                .runtime
                .read_run_state(&f.source.run_id)
                .unwrap()
                .unwrap()
                .pipeline["review_gate_settle"]
                .clone();
            settle["reviewed_head_sha"] = json!(f.base);
            published_owner::checkpoint(&f, &f.source.run_id, 5, "review_gate_settle", settle);
            // A real policy change still cannot bypass the stale certificate
            // on the way to current-policy recovery.
            let config = f.repo.join(".orbit/config.toml");
            let text = std::fs::read_to_string(&config).unwrap();
            std::fs::write(config, format!("{text}minutes = 31\n")).unwrap();
            reload(&mut f);
        } else {
            capture_legacy_no_gate(&mut f, &checkout);
        }
        let calls = tail_forge(&f, "lost_read");
        match case {
            "head" => {
                std::fs::write(checkout.join("source.txt"), "changed after publication").unwrap()
            }
            "base" => {
                let mut answer: Value =
                    serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
                answer["baseRefName"] = json!("other-base");
                std::fs::write(&f.answer, answer.to_string()).unwrap();
            }
            "receipt" => f
                .runtime
                .attach_task_validation_log(
                    &f.task,
                    &f.source.run_id,
                    &format!("validation/{}/0.json", f.source.run_id),
                    b"{}".to_vec(),
                )
                .unwrap(),
            "review" => {}
            _ => unreachable!(),
        }
        let (outcome, started) = run_tail(&f);
        assert!(
            !outcome.as_ref().is_ok_and(|o| o.success),
            "{case}: {outcome:?}"
        );
        assert_eq!(started, ["complete_pr"]);
        let after = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
        assert!(
            matches!(&after.final_recovery.as_ref().unwrap().decision,
            Some(orbit_types::workflow::FinalRecoveryDecision::Escalate { diagnosis, .. })
            if diagnosis.contains("candidate-changed")),
            "{case}: {:?}",
            after.final_recovery
        );
        assert!(
            !std::fs::read_to_string(calls)
                .unwrap()
                .lines()
                .any(|l| l.starts_with("api repos/"))
        );
    }
}
