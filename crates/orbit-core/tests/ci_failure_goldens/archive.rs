//! Historical broad keys must not transfer an archive to a different cause.

use super::*;

const DOWNLOAD: &str = "error: failed to download `aho-corasick` from registry\nCaused by: invalid argument while writing .cargo-ok (illegal byte sequence)";
const CODE: &str = "error: fields `stdout` and `stdout_over_cap` are never read\n --> crates/orbit-exec/src/build_sandbox/supervise.rs:25:5\n = note: `-D dead-code` implied by `-D warnings`";

#[test]
fn incomplete_archive_identity_holds_without_attributing_a_cover() {
    if !isolated("archive::incomplete_archive_identity_holds_without_attributing_a_cover") {
        return;
    }
    let (_root, runtime, commits) = operator_fixture("");
    let wrapper = "##[error]Process completed with exit code 1.";
    let first = file(&runtime, vec![failure(wrapper, 0, &commits[0])]);
    let owner = first["filed"][0]["task_id"].as_str().unwrap();
    let cover = add_titled(&runtime, "Unverified cover");
    archive_owner(&runtime, owner, TaskStatus::Archived, Some(&cover.id));
    for (index, commit) in commits.iter().enumerate().skip(1) {
        let held = file(&runtime, vec![failure(wrapper, index, commit)]);
        assert_eq!(held["filed_count"], 0, "{held}");
        assert_eq!(
            held["withheld"][0]["reason"],
            "archive_identity_unavailable"
        );
        assert!(held["withheld"][0].get("cover").is_none(), "{held}");
        assert!(held["withheld"][0]["owner"].is_null(), "{held}");
        assert_eq!(held["withheld"][0]["candidate_owner"], owner);
    }
}

#[test]
fn automatic_pilot_archive_reports_the_status_actor_without_rewriting_history() {
    if !isolated(
        "archive::automatic_pilot_archive_reports_the_status_actor_without_rewriting_history",
    ) {
        return;
    }
    let (_root, runtime, commits) = operator_fixture(
        "[workflow]\nbase_branch = \"main\"\ndefault_crew = \"fixture\"\n[crews.fixture]\nmodel = \"fixture-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n",
    );
    let repo = &runtime.paths().repo_root;
    let output = git_test_support::command()
        .args(["branch", "-f", "main", &commits[2]])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let first = file(&runtime, vec![failure(DOWNLOAD, 0, &commits[0])]);
    let owner = first["filed"][0]["task_id"].as_str().unwrap();
    let prepared = runtime
        .run_deterministic(
            "prepare_task_pilot",
            &json!({}),
            &json!({
                "workspace_path": repo, "task_ids": [owner], "base_branch": "main",
            }),
            ToolContext::default(),
        )
        .unwrap();
    let revision = prepared["source"]["source_revision"].as_str().unwrap();
    let tree = git_test_support::command()
        .args(["rev-parse", &format!("{revision}^{{tree}}")])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(tree.status.success());
    let evidence = format!("The registry finding is resolved by commit {}.", commits[2]);
    let applied = runtime.run_deterministic("apply_task_pilot_results", &json!({}), &json!({
        "workspace_path": repo, "prepared": prepared,
        "ci_sweep_filing": first["filed"][0], "promotion_authorized": true,
        "results": [{"partition_index": 0, "task_ids": [owner],
            "observed_revision": revision, "observed_tree": String::from_utf8(tree.stdout).unwrap().trim(),
            "tasks": [{"task_id": owner, "context_files_after": [],
                "disposition": "verified_no_diff", "evidence": evidence,
                "already_landed": null, "duplicate_of": null,
                "recommended_crew": "fixture", "recommended_complexity": "low", "confidence": "high",
                "assessment_rationale": "The cited later commit resolves the finding.",
                "validation_approach": "Read the archive history and file the recurrence.",
                "evidence_gaps": [], "reassessment_triggers": [], "blocked_by": [],
                "adr_conflicts": [], "utility_warnings": [], "surface_warnings": [],
            }]}],
    }), ToolContext::default()).unwrap();
    assert_eq!(
        runtime.get_task(owner).unwrap().status,
        TaskStatus::Archived,
        "{applied}"
    );
    let history = runtime.get_task_history(owner).unwrap();
    assert_eq!(
        history
            .iter()
            .rev()
            .find(|entry| entry.to_status == Some(TaskStatus::Archived))
            .unwrap()
            .by,
        "system"
    );
    let comments = runtime.get_task_comments(owner).unwrap();
    let held = file(&runtime, vec![failure(DOWNLOAD, 1, &commits[1])]);
    assert_eq!(held["filed_count"], 0, "{held}");
    assert_eq!(held["withheld"][0]["reason"], "automation_archived");
    assert_eq!(held["withheld"][0]["archive_actor"], "system");
    assert_eq!(runtime.get_task_history(owner).unwrap(), history);
    assert_eq!(runtime.get_task_comments(owner).unwrap(), comments);
}

#[test]
fn archived_download_does_not_suppress_a_distinct_code_diagnostic() {
    if !isolated("archive::archived_download_does_not_suppress_a_distinct_code_diagnostic") {
        return;
    }
    // Recreate the persisted collision from the incident: an older owner has
    // the new cluster's broad key but retains registry/download evidence.
    // Creating an old-shape task through the API preserves its original history.
    for status in [TaskStatus::Archived, TaskStatus::Rejected] {
        for covered in [false, true] {
            let (_root, runtime, commits) = operator_fixture("");
            let (_probe_root, probe) = bare_runtime();
            let key = file(&probe, vec![failure(CODE, 1, &commits[1])])["filed"][0]["failure_key"]
                .as_str()
                .unwrap()
                .to_string();
            let first = file(&runtime, vec![failure(DOWNLOAD, 0, &commits[0])]);
            let original = first["filed"][0]["task_id"].as_str().unwrap();
            let task = runtime.get_task(original).unwrap();
            let historical = runtime
                .add_task(TaskAddParams {
                    title: task.title,
                    description: if covered {
                        // Step-fallback owners did not persist a concrete
                        // signature, even when the excerpt retained a cause.
                        task.description
                            .lines()
                            .filter(|line| !line.starts_with("- Normalized error signature"))
                            .collect::<Vec<_>>()
                            .join("\n")
                    } else {
                        task.description
                    },
                    tags: vec!["ci-failure-sweep".into(), format!("ci-failure:{key}")],
                    creation_decision: covered.then(|| {
                        orbit_types::task::TaskDecision::CiFailureIdentity(
                            orbit_types::task::CiFailureIdentity {
                                failure_key: key.clone(),
                                compiler_cause_digest: None,
                                open_compiler_set: None,
                                signature: None,
                                tested_commit: commits[0].clone(),
                                sources: Vec::new(),
                            },
                        )
                    }),
                    system_created: true,
                    ..Default::default()
                })
                .unwrap();
            let cover = covered.then(|| add_titled(&runtime, "Registry repair"));
            if status == TaskStatus::Rejected {
                // A rejected collision must not transfer ownership through
                // the shared deduper's inbound supersedes relation either.
                let superseding = add_titled(&runtime, "Download owner");
                runtime
                    .update_task_as_human(
                        &superseding.id,
                        TaskUpdateParams {
                            relations: Some(vec![orbit_types::task::TaskRelation {
                                relation_type: orbit_types::task::TaskRelationType::Supersedes,
                                target: historical.id.clone(),
                                regression: None,
                            }]),
                            ..Default::default()
                        },
                        "human:fixture".into(),
                    )
                    .unwrap();
            }
            archive_owner(
                &runtime,
                &historical.id,
                status,
                cover.as_ref().map(|task| task.id.as_str()),
            );
            let history = runtime.get_task_history(&historical.id).unwrap();
            let observed = file(&runtime, vec![failure(CODE, 1, &commits[1])]);
            assert_eq!(
                observed["filed_count"], 1,
                "{status:?}, covered={covered}: {observed}"
            );
            assert!(
                observed["withheld"].as_array().unwrap().is_empty(),
                "{observed}"
            );
            let repair = runtime
                .get_task(observed["filed"][0]["task_id"].as_str().unwrap())
                .unwrap();
            assert!(repair.description.contains("stdout_over_cap"));
            assert!(repair.description.contains(&commits[1]));
            assert!(!repair.description.contains("aho-corasick"));
            assert_eq!(runtime.get_task_history(&historical.id).unwrap(), history);
            let repeat = file(&runtime, vec![failure(CODE, 2, &commits[2])]);
            assert_eq!(repeat["filed_count"], 0, "{repeat}");
            assert_eq!(repeat["skipped_existing"][0]["task_id"], repair.id);
        }
    }
}
