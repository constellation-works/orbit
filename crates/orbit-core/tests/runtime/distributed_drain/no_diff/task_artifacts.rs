//! Required task evidence crosses the same claimed commit boundary for every delivery shape.
use super::*;

fn task_artifact(fixture: &mut CleanLeaf, path: &str, bytes: &[u8]) {
    let source = fixture.pair.follower_repo.join(".orbit/tmp/task-evidence");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(&source, bytes).unwrap();
    fixture.input["implementation"]["task_artifacts"] = json!([{
        "path": path, "source_path": ".orbit/tmp/task-evidence",
    }]);
}

fn assert_owner_artifact(fixture: &CleanLeaf, path: &str, content: &[u8], media: &str) {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;

    let owner = &fixture.pair.wire.owner;
    let public = owner
        .run_tool(
            "orbit.task.artifact.get",
            json!({"id": fixture.task, "path": path}),
        )
        .expect("the artifact is readable through the owner's public tool");
    assert_eq!(public["media_type"], media);
    if let Ok(expected_text) = std::str::from_utf8(content) {
        assert_eq!(public["encoding"], "utf8");
        assert_eq!(public["content"], expected_text);
    } else {
        assert_eq!(public["encoding"], "base64");
        let encoded = public["content_base64"]
            .as_str()
            .expect("binary artifacts are returned as base64 through the public tool");
        let decoded = BASE64_STANDARD
            .decode(encoded)
            .expect("the public artifact payload is valid base64");
        assert_eq!(decoded, content);
    }

    let stored = owner
        .get_task_artifact(&fixture.task, path)
        .unwrap()
        .unwrap();
    assert_eq!(stored.content, content);
    assert_eq!(stored.media_type, media);
}

fn claimed_no_diff_transfers_report_two_logs_and_ledger() {
    let mut fixture = CleanLeaf::new(false, "done");
    let scratch = fixture.pair.follower_repo.join(".orbit/tmp");
    let report_path = scratch.join("no-diff.json");
    let mut report: Value = serde_json::from_slice(&std::fs::read(&report_path).unwrap()).unwrap();
    report["validation"].as_array_mut().unwrap().push(json!({
        "command": "git status --porcelain", "exit_code": 0, "log_artifact": "second-log.json",
    }));
    std::fs::write(&report_path, serde_json::to_vec(&report).unwrap()).unwrap();
    fixture.write_evidence(
        "second-log.json",
        json!({
            "run_id": fixture.leaf, "tested_head": fixture.base,
            "command": "git status --porcelain", "exit_code": 0, "output": "",
        }),
    );
    let ledger = br#"{"schema_version":1,"entries":["documentation checked"]}"#;
    task_artifact(&mut fixture, "doc-duties-ledger.json", ledger);
    let handoff = fixture.run_clean_pipeline("task_claimed_pr_pipeline");
    fixture.settle(&handoff).unwrap();
    let accepted = fixture
        .pair
        .wire
        .owner
        .accepted_task_handoff(&handoff.claim_id)
        .unwrap();
    engine_action(
        &fixture.pair.wire.owner,
        "handoff_land",
        &json!({
            "handoff_id": accepted.handoff_id,
        }),
    )
    .unwrap();
    assert_eq!(fixture.pair.owner_status(&fixture.task), "done");
    assert_owner_artifact(
        &fixture,
        "doc-duties-ledger.json",
        ledger,
        "application/json",
    );
}

fn claimed_diff_and_tagged_skip_transfer_binary_artifacts() {
    for tagged in [false, true] {
        let mut fixture = if tagged {
            CleanLeaf::tagged("done")
        } else {
            CleanLeaf::new(false, "done")
        };
        if !tagged {
            fixture.input["implementation"]
                .as_object_mut()
                .unwrap()
                .remove("no_diff_artifacts");
            std::fs::write(
                fixture.pair.follower_repo.join("src/lib.rs"),
                "// corrected\n",
            )
            .unwrap();
        }
        let screenshot = b"\x89PNG\r\n\x1a\n\xff\x00";
        task_artifact(&mut fixture, "screenshots/dashboard.png", screenshot);
        let result = fixture.action("git_commit", &fixture.input);
        assert_eq!(result["committed"], !tagged);
        assert_owner_artifact(
            &fixture,
            "screenshots/dashboard.png",
            screenshot,
            "image/png",
        );
    }
}

fn claimed_artifact_refusals_name_path_and_attach_nothing() {
    for case in [
        "missing",
        "oversized",
        "outside",
        "symlink",
        "report",
        "log",
        "duplicate",
        "invalid-path",
        "no-diff-extra",
        "handoff-report",
        "handoff-log",
        "review",
        "review-log",
    ] {
        let mut fixture = CleanLeaf::new(false, "done");
        task_artifact(&mut fixture, "ledger.json", b"{}");
        let claim = fixture.bound.claim_execution_context().unwrap().claim_id;
        let reference = &mut fixture.input["implementation"]["task_artifacts"][0];
        let named = match case {
            "missing" => {
                reference["source_path"] = json!(".orbit/tmp/missing.json");
                "missing.json"
            }
            "oversized" => {
                std::fs::write(
                    fixture.pair.follower_repo.join(".orbit/tmp/task-evidence"),
                    vec![0; 1_048_577],
                )
                .unwrap();
                "ledger.json"
            }
            "outside" => {
                reference["source_path"] = json!(".gitignore");
                "ledger.json"
            }
            "symlink" => {
                #[cfg(unix)]
                {
                    let link = fixture.pair.follower_repo.join(".orbit/tmp/escape");
                    std::os::unix::fs::symlink(
                        fixture.pair.follower_repo.join(".gitignore"),
                        &link,
                    )
                    .unwrap();
                    reference["source_path"] = json!(".orbit/tmp/escape");
                }
                #[cfg(not(unix))]
                {
                    reference["source_path"] = json!(".gitignore");
                }
                "ledger.json"
            }
            "report" => {
                reference["path"] = json!(" ./no-diff.json ");
                "no-diff.json"
            }
            "log" => {
                reference["path"] = json!("./implementation-validation.json");
                "implementation-validation.json"
            }
            "handoff-report" => {
                reference["path"] = json!(format!("no-diff-handoff/{claim}.json"));
                "no-diff-handoff/"
            }
            "handoff-log" => {
                reference["path"] = json!(format!("validation/{claim}/0.json"));
                "validation/"
            }
            "invalid-path" => {
                reference["path"] = json!("../ledger.json");
                "../ledger.json"
            }
            "duplicate" => {
                let duplicate = reference.clone();
                fixture.input["implementation"]["task_artifacts"]
                    .as_array_mut()
                    .unwrap()
                    .push(duplicate);
                "ledger.json"
            }
            "no-diff-extra" => {
                let extra = reference.clone();
                fixture.input["implementation"]["no_diff_artifacts"]
                    .as_array_mut()
                    .unwrap()
                    .push(extra);
                fixture.input["implementation"]
                    .as_object_mut()
                    .unwrap()
                    .remove("task_artifacts");
                "ledger.json"
            }
            "review" => {
                // Ordinary ledger first: a refusal must still attach nothing.
                let mut review = reference.clone();
                review["path"] = json!("Review-x.json");
                fixture.input["implementation"]["task_artifacts"]
                    .as_array_mut()
                    .unwrap()
                    .push(review);
                "Review-x.json"
            }
            "review-log" => {
                let report_path = fixture.pair.follower_repo.join(".orbit/tmp/no-diff.json");
                let mut report: Value =
                    serde_json::from_slice(&std::fs::read(&report_path).unwrap()).unwrap();
                report["validation"].as_array_mut().unwrap().push(json!({
                    "command": "git status --porcelain", "exit_code": 0, "log_artifact": "review-log.json",
                }));
                std::fs::write(&report_path, serde_json::to_vec(&report).unwrap()).unwrap();
                fixture.write_evidence(
                    "review-log.json",
                    json!({
                        "run_id": fixture.leaf, "tested_head": fixture.base,
                        "command": "git status --porcelain", "exit_code": 0, "output": "",
                    }),
                );
                "review-log.json"
            }
            _ => unreachable!(),
        };
        let refused = engine_action(&fixture.bound, "git_commit", &fixture.input)
            .expect_err(case)
            .to_string();
        assert!(
            refused.contains(named),
            "{case} must name {named}: {refused}"
        );
        let reason = match case {
            "missing" => "missing.json",
            "oversized" => "content limit",
            "outside" | "symlink" => "outside workspace_root",
            "report" | "log" | "handoff-report" | "handoff-log" => "collides",
            "duplicate" => "duplicate path",
            "invalid-path" => "must not contain '..'",
            "no-diff-extra" => "not a clean-tree report",
            "review" | "review-log" => "reserved review- namespace",
            _ => unreachable!(),
        };
        assert!(refused.contains(reason), "{case}: {refused}");
        assert!(
            fixture
                .pair
                .wire
                .owner
                .get_task_artifact_manifest(&fixture.task)
                .unwrap()
                .is_empty(),
            "{case}: validate the whole batch before attachment"
        );
        assert_eq!(
            git(&fixture.pair.follower_repo, &["rev-parse", "HEAD"]).trim(),
            fixture.base
        );
    }
}

crate::scenario_table!(
    claimed_task_artifact_contracts,
    claimed_no_diff_transfers_report_two_logs_and_ledger,
    claimed_diff_and_tagged_skip_transfer_binary_artifacts,
    claimed_artifact_refusals_name_path_and_attach_nothing,
);
