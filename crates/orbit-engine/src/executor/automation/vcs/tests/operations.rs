use serde_json::json;

use super::super::operations::normalize_merge_capabilities;

#[test]
fn private_vcs_boundary_normalizes_repository_and_branch_merge_policy() {
    let output = normalize_merge_capabilities(
        &json!({
            "data": {
                "repository": {
                    "autoMergeAllowed": true,
                    "squashMergeAllowed": false,
                    "rebaseMergeAllowed": true,
                    "mergeCommitAllowed": true,
                    "pullRequest": {
                        "baseRefName": "agent-main",
                        "baseRef": {
                            "branchProtectionRule": {
                                "requiresLinearHistory": true
                            }
                        }
                    }
                }
            }
        }),
        "constellation-works/orbit",
    )
    .expect("normalize capability response");

    assert_eq!(
        output,
        json!({
            "repository": {
                "name_with_owner": "constellation-works/orbit",
                "base_branch": "agent-main",
                "allow_squash_merge": false,
                "allow_rebase_merge": true,
                "allow_merge_commit": true,
                "allow_auto_merge": true,
                "requires_linear_history": true,
            }
        })
    );
}

#[test]
fn private_vcs_boundary_fails_closed_on_incomplete_capability_data() {
    let error = normalize_merge_capabilities(
        &json!({
            "data": {
                "repository": {
                    "squashMergeAllowed": false,
                    "rebaseMergeAllowed": true,
                    "pullRequest": {
                        "baseRefName": "agent-main",
                        "baseRef": { "branchProtectionRule": null }
                    }
                }
            }
        }),
        "constellation-works/orbit",
    )
    .expect_err("missing mergeCommitAllowed must not be guessed");

    assert!(error.to_string().contains("mergeCommitAllowed"));
}

#[test]
fn private_vcs_boundary_does_not_guess_when_base_policy_data_is_missing() {
    let error = normalize_merge_capabilities(
        &json!({
            "data": {
                "repository": {
                    "squashMergeAllowed": false,
                    "rebaseMergeAllowed": true,
                    "mergeCommitAllowed": true,
                    "pullRequest": { "baseRefName": "agent-main" }
                }
            }
        }),
        "constellation-works/orbit",
    )
    .expect_err("missing baseRef policy data must not imply no protection");

    assert!(error.to_string().contains("baseRef policy data"));
}

#[cfg(unix)]
#[test]
fn private_merge_executes_conditional_provider_args_and_preserves_ungated_modes() {
    use std::fs;

    use super::super::operations::{PR_MERGE, run};
    use super::with_fake_gh;

    let script = r#"#!/bin/sh
set -eu
printf '%s\n' "$@" > provider-args
if [ -f provider-response ]; then
    cat provider-response
else
    printf '%s\n' '{"merged":true,"sha":"landed"}'
fi
"#;
    if !with_fake_gh(
        module_path!(),
        "private_merge_executes_conditional_provider_args_and_preserves_ungated_modes",
        script,
    ) {
        return;
    }
    let workspace = tempfile::tempdir().expect("workspace");
    let reviewed = "1111111111111111111111111111111111111111";
    for strategy in ["squash", "merge", "rebase"] {
        let input = json!({
            "workspace_path": workspace.path(),
            "pr": "42",
            "strategy": strategy,
            "reviewed_head_sha": reviewed,
        });
        let result = run(PR_MERGE, &input).expect("synchronous conditional merge");
        assert_eq!(result["landed_commit"], "landed");
        assert_eq!(
            fs::read_to_string(workspace.path().join("provider-args")).expect("argv"),
            format!(
                "api\nrepos/{{owner}}/{{repo}}/pulls/42/merge\n--method\nPUT\n-f\nsha={reviewed}\n-f\nmerge_method={strategy}\n"
            )
        );

        for auto in [false, true] {
            let input = json!({
                "workspace_path": workspace.path(),
                "pr": "42",
                "strategy": strategy,
                "auto": auto,
            });
            let result = run(PR_MERGE, &input).expect("ungated merge");
            assert!(result.get("landed_commit").is_none());
            let auto_flag = if auto { "--auto\n" } else { "" };
            assert_eq!(
                fs::read_to_string(workspace.path().join("provider-args")).expect("argv"),
                format!("pr\nmerge\n42\n--{strategy}\n{auto_flag}")
            );
        }
    }

    fs::remove_file(workspace.path().join("provider-args")).expect("clear invocation log");
    let input = json!({
        "workspace_path": workspace.path(),
        "pr": "42",
        "auto": true,
        "reviewed_head_sha": reviewed,
    });
    let error = run(PR_MERGE, &input).expect_err("gated deferred request refused");
    assert!(error.to_string().contains("deferred auto-merge"));
    assert!(
        !workspace.path().join("provider-args").exists(),
        "refused before provider mutation"
    );

    for response in [
        r#"{"merged":false}"#,
        r#"{"status":"queued"}"#,
        r#"{"merged":true}"#,
        "invalid json",
    ] {
        fs::write(workspace.path().join("provider-response"), response).expect("provider response");
        run(
            PR_MERGE,
            &json!({"workspace_path": workspace.path(), "pr": "42", "reviewed_head_sha": reviewed}),
        )
        .expect_err("only confirmed synchronous merges are accepted");
    }
}
