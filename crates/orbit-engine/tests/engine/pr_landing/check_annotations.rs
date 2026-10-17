//! [ORB-16185] The provider read behind a required aggregate check's source
//! diagnostics, through the public private-VCS boundary and a substitute
//! `gh`: only a complete, unchanged answer bound to the repository, pull
//! request, head, check run and app the caller named is returned.
use super::*;

#[test]
fn check_annotations_return_only_complete_bound_unchanged_provider_answers() {
    isolated(
        "check_annotations::check_annotations_return_only_complete_bound_unchanged_provider_answers",
        |sandbox| {
            for (case, refusal) in [
                ("authentic", ""),
                ("duplicate", "2 latest check runs named 'CodeQL'"),
                ("truncated_listing", "truncated check run listing"),
                ("foreign_app", "foreign or changed check app"),
                ("foreign_repo", "foreign or changed check repository"),
                ("foreign_head_repo", "foreign or changed PR repository"),
                ("wrong_pr_link", "foreign or changed check PR head"),
                ("incomplete_annotations", "incomplete annotation pages"),
                ("changed_run", "foreign or changed check run /conclusion"),
                ("moved_head", "foreign or changed PR head"),
            ] {
                in_fresh_row(sandbox, |row| {
                    fs::write(row.join("case"), case).unwrap();
                    let gh = row.join("bin/gh");
                    fs::write(
                        &gh,
                        fs::read_to_string(
                            Path::new(env!("CARGO_MANIFEST_DIR")).join(
                                "tests/engine/pr_landing/fixtures/check-annotations-forge.sh",
                            ),
                        )
                        .unwrap()
                        .replace("__ROW__", path_str(row)),
                    )
                    .unwrap();
                    let answer = orbit_engine::run_private_vcs_operation(
                        "ci.check_annotations",
                        &json!({
                            "workspace_path": path_str(row), "pr": "42",
                            "head_sha": "a".repeat(40), "name": "CodeQL",
                            "check_run_id": 7001, "app_id": 57789,
                        }),
                    );
                    if refusal.is_empty() {
                        let answer = answer.expect("the authentic answer is returned");
                        assert_eq!(answer["repository"], "orbit/test");
                        assert_eq!(answer["pr"], 42);
                        assert_eq!(answer["base_ref"], "agent-main");
                        assert_eq!(answer["check_run"]["app_id"], 57789);
                        assert_eq!(answer["check_run"]["annotations_count"], 2);
                        assert_eq!(answer["annotations"][1]["start_line"], 9);
                    } else {
                        let error = answer.expect_err(case).to_string();
                        assert!(error.contains(refusal), "{case}: {error}");
                    }
                });
            }
        },
    );
}
