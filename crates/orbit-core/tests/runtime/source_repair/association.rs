//! Task references corroborate the authenticated publication, independent of order.
use super::*;
use orbit_types::task::ExternalRef;

fn historical_refs_preserve_inspect_identity() {
    historical_refs_preserve_identity("inspect");
}

fn historical_refs_preserve_submit_identity() {
    historical_refs_preserve_identity("submit");
}

fn historical_refs_preserve_identity(action: &str) {
    let f = Fixture::with_historical_pr();
    let refs = f.runtime.get_task(&f.task).unwrap().external_refs;
    assert_eq!(
        refs.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        ["4805", "4817"]
    );
    // Capture a submit binding before introducing the historical-first order,
    // so both dispatcher actions independently reproduce the original refusal.
    f.replace_refs(vec![refs[1].clone()]);
    let binding = f.inspect();
    f.replace_refs(refs.clone());
    let input = if action == "inspect" {
        f.request(action)
    } else {
        f.submit(binding)
    };
    let result = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", input)
        .unwrap();
    let binding = if action == "inspect" {
        result["binding"].clone()
    } else {
        let id = result["run_id"].as_str().unwrap();
        f.wait(id);
        let run = f.runtime.show_job_run_observed(id).unwrap();
        assert_eq!(
            run.retry_source_run_id.as_deref(),
            Some(f.source.run_id.as_str())
        );
        run.input.unwrap()["__source_repair"]["binding"].clone()
    };
    assert_eq!(binding["task_id"], f.task);
    assert_eq!(binding["source_run_id"], f.source.run_id);
    assert_eq!(binding["branch"], f.branch);
    assert_eq!(binding["pull_request"], "4817");
    assert_eq!(
        binding["pull_request_url"],
        "https://github.com/owner/repository/pull/4817"
    );
    assert_eq!(binding["head_sha"], f.head);
    assert_eq!(binding["base_sha"], f.base);
    assert_eq!(f.runtime.get_task(&f.task).unwrap().external_refs, refs);
    f.assert_original();
    if action == "inspect" {
        let mut key_only = refs;
        key_only[1].url = None;
        f.replace_refs(key_only.clone());
        assert_eq!(f.inspect(), binding);
        assert_eq!(f.runtime.get_task(&f.task).unwrap().external_refs, key_only);
    }
}

fn missing_or_conflicting_associations_refuse_before_reservation() {
    for case in [
        "missing",
        "foreign_repository",
        "wrong_number",
        "conflicting_number",
        "conflicting_url",
        "wrong_system",
    ] {
        let f = Fixture::with_historical_pr();
        let binding = f.inspect();
        let mut refs = f.runtime.get_task(&f.task).unwrap().external_refs;
        match case {
            "missing" => {
                refs.pop();
            }
            "foreign_repository" => {
                refs[1].url = Some("https://github.com/foreign/repository/pull/4817".into())
            }
            "wrong_number" => refs[1].id = "4818".into(),
            "wrong_system" => refs[1].system = "other".into(),
            "conflicting_number" => refs.push(ExternalRef {
                system: "github-pr".into(),
                id: "4817".into(),
                url: Some("https://github.com/foreign/repository/pull/4817".into()),
            }),
            "conflicting_url" => refs.push(ExternalRef {
                system: "github-pr".into(),
                id: "4818".into(),
                url: refs[1].url.clone(),
            }),
            _ => unreachable!(),
        }
        f.replace_refs(refs.clone());
        for input in [f.request("inspect"), f.submit(binding.clone())] {
            let error = f
                .runtime
                .run_tool_as_human("orbit.workflow.run.resume", input)
                .unwrap_err();
            assert!(
                error.to_string().contains("task PR association"),
                "{case}: {error}"
            );
        }
        assert!(
            !f.runtime
                .get_task_history(&f.task)
                .unwrap()
                .iter()
                .any(|e| e.event == "source_repair_reserved"),
            "{case}"
        );
        assert_eq!(
            f.runtime.get_task(&f.task).unwrap().status,
            TaskStatus::Review
        );
        assert_eq!(f.runtime.get_task(&f.task).unwrap().external_refs, refs);
        assert_eq!(git(&f.repo, &["rev-parse", &f.branch]), f.head);
        assert_eq!(
            git(
                &f.repo,
                &["ls-remote", "origin", &format!("refs/heads/{}", f.branch)]
            )
            .split_whitespace()
            .next(),
            Some(f.head.as_str())
        );
        f.assert_original();
    }
}

crate::scenario_table!(
    historical_pr_inspect_association,
    historical_refs_preserve_inspect_identity
);
crate::scenario_table!(
    historical_pr_submit_association,
    historical_refs_preserve_submit_identity
);
crate::scenario_table!(
    invalid_pr_association,
    missing_or_conflicting_associations_refuse_before_reservation
);
