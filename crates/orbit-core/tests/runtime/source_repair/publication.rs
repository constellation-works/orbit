//! Publication through the real private provider adapter and public runtime.
use super::fixture::{Fixture, git};
use orbit_common::security::release::sha256_hex;
use orbit_engine::execute_deterministic_action;
use orbit_types::workflow::{JobRunState, SourceRepairBinding};
use serde_json::{Value, json};
use std::{collections::HashMap, io::Write, path::PathBuf};

fn same_pr_publication_retains_each_refused_observation() {
    let f = Fixture::publication();
    let binding = f.inspect();
    let admitted: SourceRepairBinding = serde_json::from_value(binding.clone()).unwrap();
    let admission_hash = sha256_hex(&serde_json::to_vec(&admitted).unwrap());
    let submitted = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", f.submit(binding.clone()))
        .unwrap();
    let id = submitted["run_id"].as_str().unwrap();
    f.wait(id);
    f.start_actions(id);
    let setup = f.action(
        "worktree_setup",
        json!({"job_run_id":id,"task_ids":[f.task],"base":"main","base_sync":"remote"}),
    );
    let checkout = PathBuf::from(setup["workspace_path"].as_str().unwrap());
    f.action("candidate_resume", json!({"job_run_id":id,"task_ids":[f.task],"workspace_path":checkout,"base_sha":setup["base_sha"]}));
    std::fs::write(checkout.join("source.txt"), "repaired source\n").unwrap();
    f.action("git_commit", json!({"job_run_id":id,"scope":"all","workspace_path":checkout,"base_sha":setup["base_sha"],"base_ref":setup["base_ref"]}));
    f.action("candidate_validate", json!({"job_run_id":id,"completed_task_ids":[f.task],"workspace_path":checkout,"base_sha":setup["base_sha"],"base_ref":setup["base_ref"]}));
    let pushed = f.action("git_push", json!({"job_run_id":id,"workspace_path":checkout,"branch":f.branch,"base":"main","base_ref":setup["base_ref"]}));
    let repaired_head = git(&checkout, &["rev-parse", "HEAD"]);
    assert_ne!(repaired_head, f.head);
    assert_eq!(pushed["force_with_lease"], true);
    assert_eq!(pushed["remote_sha_before"], f.head);
    assert_eq!(pushed["local_sha"], repaired_head);
    assert_eq!(
        git(
            &checkout,
            &["ls-remote", "origin", &format!("refs/heads/{}", f.branch)]
        )
        .split_whitespace()
        .next(),
        Some(repaired_head.as_str())
    );
    let push_hash = sha256_hex(pushed.to_string().as_bytes());
    let mut state = f.runtime.read_run_state(id).unwrap().unwrap();
    state.record_step(4, JobRunState::Success, Some(pushed.clone()), None);
    state.record_pipeline_output("push", pushed);
    f.runtime.write_run_state(id, &state).unwrap();
    let mut answer: Value = serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
    answer["headRefOid"] = json!(repaired_head);
    // gh pr view serializes GraphQL fields. A REST provisional merge SHA is
    // unrelated evidence and must never be interpreted as GraphQL mergeCommit.
    answer["merge_commit_sha"] = json!("a".repeat(40));
    let input = json!({"job_run_id":id,"completed_task_ids":[f.task],"workspace_path":checkout,"head":f.branch,"base":"main","base_ref":setup["base_ref"],"base_sha":setup["base_sha"]});
    std::fs::write(&f.answer, answer.to_string()).unwrap();
    let opened = f.action("pr_open", input.clone());
    assert_eq!(opened["pr_number"], "4818");
    assert_eq!(opened["pr_url"], binding["pull_request_url"]);
    assert_eq!(opened["pr_reused"], true);
    assert_eq!(opened["pr_created"], false);
    assert_eq!(opened["base"], "main");
    assert_eq!(opened["head"], f.branch);

    let mut cases = Vec::new();
    // Missing merge metadata was previously treated as proof of no merge.
    for field in [
        "mergeCommit",
        "mergedAt",
        "number",
        "url",
        "state",
        "headRefName",
        "headRefOid",
        "baseRefName",
        "autoMergeRequest",
    ] {
        let mut missing = answer.clone();
        missing.as_object_mut().unwrap().remove(field);
        cases.push((field, missing.to_string(), "missing"));
        let mut invalid = answer.clone();
        invalid[field] = json!(false);
        cases.push((field, invalid.to_string(), "present"));
    }
    for (field, value) in [
        ("number", json!(4819)),
        ("number", json!("4818")),
        (
            "url",
            json!("https://github.com/other/repository/pull/4818"),
        ),
        ("state", json!("CLOSED")),
        ("state", json!("MERGED")),
        ("headRefName", json!("other")),
        ("headRefName", json!("é".repeat(2000))),
        ("headRefOid", json!(f.head)),
        ("baseRefName", json!("other")),
        ("autoMergeRequest", json!({"enabledAt":"now"})),
        ("mergeCommit", json!({"oid":"b".repeat(40)})),
        ("mergeCommit", json!({})),
        ("mergedAt", json!("2026-10-15T09:51:00Z")),
        (
            "url",
            json!(format!(
                "https://user:private-password@github.com/owner/repository/pull/4818?token={}",
                "é".repeat(2000)
            )),
        ),
    ] {
        let mut changed = answer.clone();
        changed[field] = value;
        cases.push((field, changed.to_string(), "present"));
    }
    cases.push(("observation", "{".into(), "unreadable"));
    cases.push(("observation", "null".into(), "invalid"));
    let mut count = 0;
    for (field, response, presence) in cases {
        let mut stderr = std::io::stderr().lock();
        writeln!(stderr, "publication refusal: {field} {presence}").unwrap();
        drop(stderr);
        std::fs::write(&f.answer, &response).unwrap();
        let error = execute_deterministic_action(
            &f.runtime,
            "pr_open",
            &json!({}),
            &input,
            false,
            &HashMap::new(),
            None,
        )
        .unwrap_err();
        assert!(error.to_string().contains(field), "named refusal: {error}");
        let history = f.runtime.get_task_history(&f.task).unwrap();
        let records: Vec<_> = history
            .iter()
            .filter(|entry| entry.event == "source_repair_publication_refused")
            .collect();
        count += 1;
        assert_eq!(
            records.len(),
            count,
            "each read survives handoff comment deduplication"
        );
        let note = records.last().unwrap().note.as_deref().unwrap();
        assert!(note.len() < 32768, "bounded provider evidence");
        assert!(
            !note.contains("private-password"),
            "URL credentials are never retained"
        );
        let evidence: Value = serde_json::from_str(note).unwrap();
        assert_eq!(evidence["run_id"], id);
        assert_eq!(evidence["admission_binding_sha256"], admission_hash);
        assert_eq!(evidence["successful_push"]["checkpoint_sha256"], push_hash);
        for key in [
            "source_run_id",
            "source_digest",
            "task_digest",
            "machine_id",
            "repository",
            "head_sha",
        ] {
            assert_eq!(evidence["admission"][key], binding[key], "{key}");
        }
        assert_eq!(evidence["published_head"], repaired_head);
        assert_eq!(evidence["successful_push"]["step_index"], 4);
        assert_eq!(evidence["successful_push"]["local_sha"], repaired_head);
        assert_eq!(evidence["successful_push"]["remote_sha_before"], f.head);
        assert_eq!(evidence["read"]["operation"], "pr.status");
        assert_eq!(evidence["read"]["selector"], "4818");
        for key in ["started_at", "finished_at"] {
            assert!(
                chrono::DateTime::parse_from_rfc3339(evidence["read"][key].as_str().unwrap())
                    .is_ok()
            );
        }
        let mismatch = evidence["mismatches"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["field"] == field)
            .unwrap();
        let expected = match field {
            "number" => json!(4818),
            "url" => binding["pull_request_url"].clone(),
            "state" => json!("OPEN"),
            "headRefName" => json!(f.branch),
            "headRefOid" => json!(repaired_head),
            "baseRefName" => json!("main"),
            "autoMergeRequest" | "mergeCommit" | "mergedAt" => Value::Null,
            "observation" if presence == "unreadable" => json!("readable pull_request object"),
            "observation" => json!("pull_request object"),
            _ => unreachable!(),
        };
        assert_eq!(mismatch["expected"], expected);
        assert_eq!(mismatch["observed"]["presence"], presence);
        if field == "headRefName" && response.contains(&"é".repeat(2000)) {
            let value = &mismatch["observed"]["value"];
            assert_eq!(value["truncated"], true);
            assert_eq!(value["bytes"], 4000);
            assert!(value["prefix"].as_str().unwrap().len() <= 512);
        }
        if field == "headRefOid" && presence == "present" && response.contains(&f.head) {
            assert_eq!(mismatch["expected"], repaired_head);
            assert_eq!(mismatch["observed"]["value"], f.head);
        }
    }
    // A copied or unsuccessful checkpoint must not masquerade as push provenance.
    let mut refused_answer = answer.clone();
    refused_answer["number"] = json!(4819);
    std::fs::write(&f.answer, refused_answer.to_string()).unwrap();
    for case in ["failed", "copy", "head", "run", "missing"] {
        let mut changed = state.clone();
        match case {
            "failed" => {
                changed.step_states.insert(4, JobRunState::Failed);
            }
            "copy" => {
                changed.step_outputs.insert(4, json!({"phase":"push"}));
            }
            "head" => {
                changed.pipeline["push"]["local_sha"] = json!(f.head);
            }
            "run" => {
                changed.run_id = "another-run".into();
            }
            "missing" => {
                changed.pipeline.as_object_mut().unwrap().remove("push");
            }
            _ => unreachable!(),
        }
        f.runtime.write_run_state(id, &changed).unwrap();
        let error = execute_deterministic_action(
            &f.runtime,
            "pr_open",
            &json!({}),
            &input,
            false,
            &HashMap::new(),
            None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("number"), "{case}: {error}");
        let history = f.runtime.get_task_history(&f.task).unwrap();
        let record = history
            .iter()
            .rev()
            .find(|entry| entry.event == "source_repair_publication_refused")
            .unwrap();
        let evidence: Value = serde_json::from_str(record.note.as_deref().unwrap()).unwrap();
        assert_eq!(
            evidence["successful_push"],
            Value::Null,
            "{case}: unauthenticated push provenance is not invented"
        );
    }
    f.runtime.write_run_state(id, &state).unwrap();
    let comments = f.runtime.get_task_comments(&f.task).unwrap();
    assert_eq!(
        comments
            .iter()
            .filter(|comment| comment.message.starts_with("pr handoff failed"))
            .count(),
        1,
        "per-read evidence survives the existing one-comment-per-phase handoff contract"
    );
    std::fs::write(&f.answer, answer.to_string()).unwrap();
    assert_eq!(
        f.action("pr_open", input)["pr_reused"],
        true,
        "refusal never changes same-PR idempotency"
    );
    let calls = std::fs::read_to_string(f._root.path().join("gh-calls")).unwrap();
    assert!(
        calls
            .lines()
            .all(|line| line.starts_with("pr view 4818 --json ")),
        "no replacement PR mutation: {calls}"
    );
    f.assert_original();
}

crate::scenario_table!(
    same_pr_publication_scenarios,
    same_pr_publication_retains_each_refused_observation
);
