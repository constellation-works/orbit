//! Publication through the real private provider adapter and public runtime.
use super::fixture::{Fixture, git};
use orbit_common::security::release::sha256_hex;
use orbit_engine::{RuntimeHost, execute_deterministic_action};
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

struct Publication {
    f: Fixture,
    id: String,
    checkout: PathBuf,
    candidate: String,
    state: orbit_types::workflow::PipelineState,
    input: Value,
    old: Value,
    current: Value,
}

impl Publication {
    fn new() -> Self {
        let f = Fixture::publication();
        let binding = f.inspect();
        let submitted = f
            .runtime
            .run_tool_as_human("orbit.workflow.run.resume", f.submit(binding))
            .unwrap();
        let id = submitted["run_id"].as_str().unwrap().to_string();
        f.wait(&id);
        f.start_actions(&id);
        let setup = f.action(
            "worktree_setup",
            json!({"job_run_id":id,"task_ids":[f.task],"base":"main","base_sync":"remote"}),
        );
        let checkout = PathBuf::from(setup["workspace_path"].as_str().unwrap());
        f.action("candidate_resume", json!({"job_run_id":id,"task_ids":[f.task],"workspace_path":checkout,"base_sha":setup["base_sha"]}));
        std::fs::write(checkout.join("source.txt"), "metadata settling repair\n").unwrap();
        f.action("git_commit", json!({"job_run_id":id,"scope":"all","workspace_path":checkout,"base_sha":setup["base_sha"],"base_ref":setup["base_ref"]}));
        f.action("candidate_validate", json!({"job_run_id":id,"completed_task_ids":[f.task],"workspace_path":checkout,"base_sha":setup["base_sha"],"base_ref":setup["base_ref"]}));
        let pushed = f.action("git_push", json!({"job_run_id":id,"workspace_path":checkout,"branch":f.branch,"base":"main","base_ref":setup["base_ref"]}));
        let candidate = git(&checkout, &["rev-parse", "HEAD"]);
        assert_eq!(pushed["remote_sha_before"], f.head);
        assert_eq!(pushed["local_sha"], candidate);
        assert_eq!(pushed["force_with_lease"], true);
        let mut state = f.runtime.read_run_state(&id).unwrap().unwrap();
        state.record_step(4, JobRunState::Success, Some(pushed.clone()), None);
        state.record_pipeline_output("push", pushed);
        f.runtime.write_run_state(&id, &state).unwrap();

        let old: Value = serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
        let mut current = old.clone();
        current["headRefOid"] = json!(candidate);
        let input = json!({"job_run_id":id,"completed_task_ids":[f.task],"workspace_path":checkout,"head":f.branch,"base":"main","base_ref":setup["base_ref"],"base_sha":setup["base_sha"]});
        // Observe even a no-op extra push. Every other command still runs
        // real Git against this fixture's real repository and origin.
        let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|directory| directory.join("git"))
            .find(|path| path.is_file())
            .unwrap();
        let wrapper = f._root.path().join("bin/git");
        std::fs::write(&wrapper, format!(
            "#!/bin/sh\nfor arg in \"$@\"; do\n  if [ \"$arg\" = push ]; then printf push >> '{}'; exit 98; fi\ndone\nexec '{}' \"$@\"\n",
            f._root.path().join("extra-push").display(), real_git.display()
        )).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            f,
            id,
            checkout,
            candidate,
            state,
            input,
            old,
            current,
        }
    }

    fn open(&self) -> Result<Value, orbit_common::OrbitError> {
        execute_deterministic_action(
            &self.f.runtime,
            "pr_open",
            &json!({}),
            &self.input,
            false,
            &HashMap::new(),
            None,
        )
    }

    fn evidence(&self) -> Vec<Value> {
        self.f
            .runtime
            .get_task_history(&self.f.task)
            .unwrap()
            .into_iter()
            .filter(|entry| {
                entry.event == "source_repair_publication_observed"
                    || entry.event == "source_repair_publication_refused"
            })
            .map(|entry| serde_json::from_str(entry.note.as_deref().unwrap()).unwrap())
            .collect()
    }

    fn unchanged(&self, state: &orbit_types::workflow::PipelineState) {
        assert_eq!(
            self.f.runtime.read_run_state(&self.id).unwrap().unwrap(),
            *state,
            "settling never rewrites pipeline receipts"
        );
        assert!(
            !self.f._root.path().join("extra-push").exists(),
            "settling never pushes again"
        );
        let calls = std::fs::read_to_string(self.f._root.path().join("gh-calls")).unwrap();
        assert!(
            calls
                .lines()
                .all(|line| line.starts_with("pr view 4818 --json ")),
            "only read-only PR observations: {calls}"
        );
        self.f.assert_original();
    }
}

fn authenticated_previous_head_metadata_settles_without_republication() {
    for stale_reads in [0, 1, 3] {
        let p = Publication::new();
        if stale_reads == 0 {
            std::fs::write(&p.f.answer, p.current.to_string()).unwrap();
        }
        let answer = p.f.answer.clone();
        let converged = p.current.clone();
        let waits = std::rc::Rc::new(std::cell::Cell::new(0));
        let waited = waits.clone();
        let opened = orbit_engine::test_support::with_vcs_retry_wait(
            move |duration| {
                assert_eq!(duration.as_secs(), 5);
                waited.set(waited.get() + 1);
                if waited.get() == stale_reads {
                    std::fs::write(&answer, converged.to_string()).unwrap();
                }
            },
            || p.open().unwrap(),
        );
        assert_eq!(opened["pr_reused"], true);
        assert_eq!(opened["pr_created"], false);
        assert_eq!(waits.get(), stale_reads);
        let records = p.evidence();
        assert_eq!(records.len(), stale_reads + 1);
        for (index, record) in records.iter().enumerate() {
            assert_eq!(record["settling"]["rereads"], (index + 1).min(stale_reads));
            assert_eq!(
                record["settling"]["waited_seconds"],
                (index + 1).min(stale_reads) * 5
            );
        }
        assert_eq!(records.last().unwrap()["settling"]["decision"], "settled");
        if stale_reads > 0 {
            for record in &records {
                assert_eq!(record["remote_branch"]["sha"], p.candidate);
            }
            // A later regression to the previous head cannot restart settling.
            std::fs::write(&p.f.answer, p.old.to_string()).unwrap();
            orbit_engine::test_support::with_vcs_retry_wait(
                |_| panic!("settled heads cannot lag again"),
                || p.open().unwrap_err(),
            );
            assert_eq!(
                p.evidence().last().unwrap()["settling"]["refusal_reason"],
                "head_regressed"
            );
        }
        p.unchanged(&p.state);
    }
}

fn publication_lag_refuses_unconfirmed_proof_and_unknown_heads() {
    let p = Publication::new();
    for case in [
        "missing",
        "failed",
        "copied",
        "head",
        "branch",
        "run",
        "job",
        "previous",
        "unleased",
        "unknown_outcome",
        "unknown_head",
        "foreign_head",
        "padded_previous",
    ] {
        let mut state = p.state.clone();
        let mut answer = p.old.clone();
        match case {
            "missing" => {
                state.pipeline.as_object_mut().unwrap().remove("push");
            }
            "failed" => {
                state.step_states.insert(4, JobRunState::Failed);
            }
            "copied" => {
                state.step_outputs.insert(4, json!({"phase":"push"}));
            }
            "head" => {
                state.pipeline["push"]["local_sha"] = json!(p.f.head);
            }
            "branch" => {
                state.pipeline["push"]["branch"] = json!("foreign");
            }
            "run" => {
                state.run_id = "foreign".into();
            }
            "job" => {
                state.job_id = "foreign".into();
            }
            "previous" | "unleased" | "unknown_outcome" => {
                match case {
                    "previous" => {
                        state.pipeline["push"]["remote_sha_before"] = json!("a".repeat(40))
                    }
                    "unleased" => state.pipeline["push"]["force_with_lease"] = json!(false),
                    _ => state.pipeline["push"]["decision"] = json!("unknown"),
                }
                state.step_outputs.insert(4, state.pipeline["push"].clone());
            }
            "unknown_head" => {
                answer.as_object_mut().unwrap().remove("headRefOid");
            }
            "foreign_head" => {
                answer["headRefOid"] = json!("b".repeat(40));
            }
            "padded_previous" => {
                answer["headRefOid"] = json!(format!(" {} ", p.f.head));
            }
            _ => unreachable!(),
        }
        p.f.runtime.write_run_state(&p.id, &state).unwrap();
        std::fs::write(&p.f.answer, answer.to_string()).unwrap();
        orbit_engine::test_support::with_vcs_retry_wait(
            |_| panic!("unconfirmed or foreign heads never wait"),
            || {
                assert!(
                    p.open().unwrap_err().to_string().contains("headRefOid"),
                    "{case}"
                );
            },
        );
        let evidence = p.evidence();
        let last = evidence.last().unwrap();
        assert_eq!(last["settling"]["decision"], "refused", "{case}");
        assert_eq!(last["settling"]["rereads"], 0, "{case}");
        if matches!(
            case,
            "missing" | "failed" | "copied" | "head" | "branch" | "run" | "job"
        ) {
            assert!(last["successful_push"].is_null(), "{case}");
        }
        p.unchanged(&state);
    }
}

fn publication_lag_rechecks_identity_authority_and_remote_on_convergence() {
    for case in [
        "number",
        "url",
        "state",
        "headRefName",
        "baseRefName",
        "autoMergeRequest",
        "mergeCommit",
        "mergedAt",
        "unreadable",
        "scope",
        "footprint",
        "owner",
        "push_proof",
        "changed_proof",
        "local_branch",
        "remote_foreign",
        "remote_missing",
        "remote_unreadable",
    ] {
        let p = Publication::new();
        let mut next = p.current.clone();
        match case {
            "number" => next[case] = json!(4819),
            "url" => next[case] = json!("https://github.com/foreign/repo/pull/4818"),
            "state" => next[case] = json!("MERGED"),
            "headRefName" | "baseRefName" => next[case] = json!("foreign"),
            "autoMergeRequest" | "mergeCommit" => next[case] = json!({"oid":"a".repeat(40)}),
            "mergedAt" => {
                next.as_object_mut().unwrap().remove(case);
            }
            _ => {}
        }
        let answer = p.f.answer.clone();
        let runtime = p.f.runtime.clone();
        let task = p.f.task.clone();
        let id = p.id.clone();
        let remote = p.f._root.path().join("origin.git");
        let checkout = p.checkout.clone();
        let branch = p.f.branch.clone();
        let old_head = p.f.head.clone();
        let waits = std::rc::Rc::new(std::cell::Cell::new(0));
        let waited = waits.clone();
        orbit_engine::test_support::with_vcs_retry_wait(
            move |_| {
                waited.set(waited.get() + 1);
                assert_eq!(
                    waited.get(),
                    1,
                    "changed predicates refuse on the next observation: {case}"
                );
                std::fs::write(
                    &answer,
                    if case == "unreadable" {
                        "{".into()
                    } else {
                        next.to_string()
                    },
                )
                .unwrap();
                match case {
                    "scope" => {
                        runtime
                            .update_task_with_identity(
                                &task,
                                orbit_core::application::task::TaskUpdateParams {
                                    description: Some("unrelated work".into()),
                                    ..Default::default()
                                },
                                None,
                                Some("codex".into()),
                            )
                            .unwrap();
                    }
                    "footprint" => {
                        runtime
                            .update_task_with_identity(
                                &task,
                                orbit_core::application::task::TaskUpdateParams {
                                    context_files: Some(vec!["file:.gitignore".into()]),
                                    ..Default::default()
                                },
                                None,
                                Some("codex".into()),
                            )
                            .unwrap();
                    }
                    "owner" => {
                        runtime
                            .apply_task_automation_update(
                                &task,
                                orbit_engine::TaskAutomationUpdate {
                                    job_run_id: Some("foreign-run".into()),
                                    ..Default::default()
                                },
                            )
                            .unwrap();
                    }
                    "push_proof" => {
                        let mut state = runtime.read_run_state(&id).unwrap().unwrap();
                        state.step_states.insert(4, JobRunState::Failed);
                        runtime.write_run_state(&id, &state).unwrap();
                    }
                    "changed_proof" => {
                        let mut state = runtime.read_run_state(&id).unwrap().unwrap();
                        state.pipeline["push"]["remote_sha_before"] = json!("a".repeat(40));
                        state.step_outputs.insert(4, state.pipeline["push"].clone());
                        runtime.write_run_state(&id, &state).unwrap();
                    }
                    "local_branch" => {
                        git(&checkout, &["checkout", "-q", "--detach"]);
                    }
                    "remote_foreign" => {
                        git(
                            &remote,
                            &["update-ref", &format!("refs/heads/{branch}"), &old_head],
                        );
                    }
                    "remote_missing" => {
                        git(
                            &remote,
                            &["update-ref", "-d", &format!("refs/heads/{branch}")],
                        );
                    }
                    "remote_unreadable" => {
                        std::fs::rename(&remote, remote.with_extension("offline")).unwrap();
                    }
                    _ => {}
                }
            },
            || {
                let error = p.open().unwrap_err();
                let field = match case {
                    "scope" | "footprint" | "owner" => "authority",
                    "push_proof" | "changed_proof" => "successful_push",
                    "local_branch" => "repository_branch",
                    "remote_foreign" | "remote_missing" | "remote_unreadable" => "remote_branch",
                    "unreadable" => "observation",
                    _ => case,
                };
                assert!(error.to_string().contains(field), "{case}: {error}");
            },
        );
        assert_eq!(waits.get(), 1, "{case}");
        let records = p.evidence();
        assert_eq!(records.len(), 2, "{case}");
        assert_eq!(records[0]["settling"]["decision"], "waiting");
        assert_eq!(records[1]["settling"]["decision"], "refused");
        assert_eq!(records[1]["settling"]["rereads"], 1);
        if case == "remote_unreadable" {
            assert_eq!(records[1]["remote_branch"]["outcome"], "unreadable");
        }
        let mut state = p.state.clone();
        if case == "push_proof" {
            state.step_states.insert(4, JobRunState::Failed);
        }
        if case == "changed_proof" {
            state.pipeline["push"]["remote_sha_before"] = json!("a".repeat(40));
            state.step_outputs.insert(4, state.pipeline["push"].clone());
        }
        p.unchanged(&state);
    }
}

fn publication_lag_budget_survives_repeated_action_calls() {
    let p = Publication::new();
    let waits = std::rc::Rc::new(std::cell::Cell::new(0));
    let waited = waits.clone();
    orbit_engine::test_support::with_vcs_retry_wait(
        move |delay| {
            assert_eq!(delay.as_secs(), 5);
            waited.set(waited.get() + 1);
        },
        || {
            for _ in 0..2 {
                assert!(p.open().unwrap_err().to_string().contains("headRefOid"));
            }
        },
    );
    assert_eq!(
        waits.get(),
        3,
        "action retry cannot reset the settling budget"
    );
    let records = p.evidence();
    assert_eq!(records.len(), 5);
    for record in &records[3..] {
        assert_eq!(record["settling"]["rereads"], 3);
        assert_eq!(record["settling"]["waited_seconds"], 15);
        assert_eq!(record["settling"]["refusal_reason"], "budget_exhausted");
        assert_eq!(record["remote_branch"]["sha"], p.candidate);
    }
    // Once metadata catches up, the same read-only action can reconcile the
    // successful push without replaying it, even after the wait is exhausted.
    std::fs::write(&p.f.answer, p.current.to_string()).unwrap();
    assert_eq!(p.open().unwrap()["pr_reused"], true);
    p.unchanged(&p.state);
}

crate::scenario_table!(
    same_pr_publication_scenarios,
    same_pr_publication_retains_each_refused_observation,
    authenticated_previous_head_metadata_settles_without_republication,
    publication_lag_refuses_unconfirmed_proof_and_unknown_heads,
    publication_lag_rechecks_identity_authority_and_remote_on_convergence,
    publication_lag_budget_survives_repeated_action_calls
);
