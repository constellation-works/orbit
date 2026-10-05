use crate::workspace::{
    WorkspacePublicationBinding, redact_git_remote, validate_publication_remote,
};

#[test]
fn publication_remote_rejects_credentials_aliases_paths_and_source_equivalents() {
    let secret = "https://x-access-token:ghp_s3cret@github.com/example/tasks.git";
    let error = validate_publication_remote(secret)
        .expect_err("credential URL")
        .to_string();
    assert!(error.contains("must not contain credentials"), "{error}");
    assert!(!error.contains("ghp_s3cret"), "{error}");
    assert_eq!(
        redact_git_remote(secret),
        "https://***@github.com/example/tasks.git"
    );

    assert!(
        validate_publication_remote("/repos/orbit")
            .expect_err("path")
            .to_string()
            .contains("local checkout path")
    );
    assert!(
        validate_publication_remote("origin")
            .expect_err("alias")
            .to_string()
            .contains("checkout-local alias")
    );
    assert!(
        WorkspacePublicationBinding {
            workspace_id: "ws_orbit".to_string(),
            source_repository_fingerprint: "git@github.com:example/source.git".to_string(),
            publication_remote: "https://github.com/example/source.git".to_string(),
            publication_branch: "refs/heads/main".to_string(),
            publication_id: "tp_dup".to_string(),
            authority_machine_id: "hm_owner".to_string(),
            last_success_generation: None,
            last_success_commit: None,
        }
        .validated()
        .expect_err("source-equivalent remote")
        .to_string()
        .contains("equivalent to the workspace source remote")
    );
}

#[test]
fn malformed_credential_urls_are_rejected_with_redacted_diagnostics() {
    let cases = [
        (
            "https://review-user:invalid-host-secret@[invalid/tasks.git",
            "https://***@[invalid/tasks.git",
            "review-user",
            "invalid-host-secret",
        ),
        (
            "https://review-user:invalid-port-secret@github.com:not-a-port/tasks.git",
            "https://***@github.com:not-a-port/tasks.git",
            "review-user",
            "invalid-port-secret",
        ),
        (
            "ssh://deploy-user:invalid-host-secret@[invalid/tasks.git",
            "ssh://***@[invalid/tasks.git",
            "deploy-user",
            "invalid-host-secret",
        ),
        (
            "ssh://deploy-user:invalid-port-secret@github.com:not-a-port/tasks.git",
            "ssh://***@github.com:not-a-port/tasks.git",
            "deploy-user",
            "invalid-port-secret",
        ),
    ];

    for (remote, redacted, username, password) in cases {
        assert_eq!(redact_git_remote(remote), redacted);

        let diagnostic = validate_publication_remote(remote)
            .expect_err("malformed credential URL")
            .to_string();
        assert!(
            diagnostic.contains("is not a valid Git URL"),
            "{diagnostic}"
        );
        assert!(diagnostic.contains(redacted), "{diagnostic}");
        assert!(!diagnostic.contains(username), "{diagnostic}");
        assert!(!diagnostic.contains(password), "{diagnostic}");
    }
}
