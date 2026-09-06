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
