//  Workspace publication contracts exercised through the public types API.

// Test assertions unwrap results, following the test-only crate-root exemption.

#[test]
fn scp_credentials_are_rejected_and_never_exposed_by_diagnostics_or_identity() {
    for remote in [
        "deploy:hunter2@github.com:example/tasks.git",
        "deploy:hunter2:extra@github.com:example/tasks.git",
        "deploy:@github.com:example/tasks.git",
        "deploy:p/ss@github.com:example/tasks.git",
    ] {
        assert_eq!(
            redact_git_remote(remote),
            "***@github.com:example/tasks.git"
        );
        assert_eq!(
            redact_git_remote(&redact_git_remote(remote)),
            redact_git_remote(remote)
        );
        assert_eq!(
            git_remote_identity(remote).expect("host/path identity"),
            "github.com/example/tasks"
        );
        for result in [
            validate_publication_remote(remote),
            validate_source_repository_fingerprint(remote),
        ] {
            let diagnostic = result.expect_err("embedded scp credentials").to_string();
            assert!(
                diagnostic.contains("must not contain credentials"),
                "{diagnostic}"
            );
            assert!(!diagnostic.contains("deploy"), "{diagnostic}");
            assert!(!diagnostic.contains("hunter2"), "{diagnostic}");
            assert!(!diagnostic.contains("p/ss"), "{diagnostic}");
            assert!(!diagnostic.contains("ss"), "{diagnostic}");
        }
    }
}

#[test]
fn malformed_scp_credentials_are_redacted_even_when_parsing_fails() {
    for (remote, redacted) in [
        ("deploy:hunter2@github.com:", "***@github.com:"),
        ("deploy:p/ss@github.com:", "***@github.com:"),
        (
            "deploy:hunter2@:example/tasks.git",
            "***@:example/tasks.git",
        ),
        (
            "deploy:p@ss@github.com:example/tasks.git",
            "***@github.com:example/tasks.git",
        ),
        (
            "deploy:p@ss@github.com:example/tasks@v1:archive.git",
            "***@github.com:example/tasks@v1:archive.git",
        ),
        (
            "deploy:p@ss@@github.com:example/tasks.git",
            "***@github.com:example/tasks.git",
        ),
        (
            "deploy:p/ss@word@github.com:example/tasks@v1:archive.git",
            "***@github.com:example/tasks@v1:archive.git",
        ),
        (
            "deploy:p@ss/word@github.com:example/tasks.git",
            "***@github.com:example/tasks.git",
        ),
    ] {
        assert_eq!(redact_git_remote(remote), redacted);
        assert_eq!(redact_git_remote(redacted), redacted);
        for result in [
            git_remote_identity(remote).map(|_| ()),
            validate_publication_remote(remote),
            validate_source_repository_fingerprint(remote),
        ] {
            let diagnostic = result.expect_err("malformed scp remote").to_string();
            assert!(diagnostic.contains(redacted), "{diagnostic}");
            assert!(!diagnostic.contains("deploy"), "{diagnostic}");
            assert!(!diagnostic.contains("hunter2"), "{diagnostic}");
            assert!(!diagnostic.contains("ss"), "{diagnostic}");
            assert!(!diagnostic.contains("word"), "{diagnostic}");
        }
    }
}

#[test]
fn ambiguous_scp_credentials_are_fully_redacted() {
    let remote = "deploy:p@ss:word@github.com:example/tasks.git";

    assert_eq!(redact_git_remote(remote), "***");
    for result in [
        validate_publication_remote(remote),
        validate_source_repository_fingerprint(remote),
    ] {
        let diagnostic = result.expect_err("embedded scp credentials").to_string();
        assert!(diagnostic.contains("'***'"), "{diagnostic}");
        assert!(!diagnostic.contains("deploy"), "{diagnostic}");
        assert!(!diagnostic.contains("ss"), "{diagnostic}");
        assert!(!diagnostic.contains("word"), "{diagnostic}");
    }
}

#[test]
fn ambiguous_scp_repository_paths_fail_closed_as_credentials() {
    // A path with ':' and '/' before '@' cannot be distinguished from a
    // slash-bearing password. Credential safety takes precedence.
    let remote = "github.com:example/tasks@v1:archive.git";
    let redacted = "***@v1:archive.git";

    assert_eq!(redact_git_remote(remote), redacted);
    assert_eq!(redact_git_remote(redacted), redacted);
    for result in [
        validate_publication_remote(remote),
        validate_source_repository_fingerprint(remote),
    ] {
        let diagnostic = result.expect_err("ambiguous scp credentials").to_string();
        assert!(diagnostic.contains(redacted), "{diagnostic}");
        assert!(!diagnostic.contains("github.com"), "{diagnostic}");
        assert!(!diagnostic.contains("example/tasks"), "{diagnostic}");
    }
}

#[test]
fn malformed_https_credentials_with_url_delimiters_are_fully_redacted() {
    for (remote, password_fragments) in [
        (
            "https://deploy:abc/def@github.com/example/tasks.git",
            ["abc", "def"],
        ),
        ("https://deploy:ab?cd@host/repo", ["ab", "cd"]),
        ("https://deploy:ab#cd@host/repo", ["ab", "cd"]),
    ] {
        assert_eq!(redact_git_remote(remote), "***");

        let diagnostic = validate_publication_remote(remote)
            .expect_err("malformed credential-bearing HTTPS URL")
            .to_string();
        assert!(diagnostic.contains("'***'"), "{diagnostic}");
        assert!(!diagnostic.contains("deploy"), "{diagnostic}");
        for fragment in password_fragments {
            assert!(!diagnostic.contains(fragment), "{diagnostic}");
        }
    }
}

#[test]
fn valid_https_credentials_and_credential_free_urls_keep_existing_redaction() {
    let credentialed = "https://deploy:hunter2@github.com/example/tasks.git";
    assert_eq!(
        redact_git_remote(credentialed),
        "https://***@github.com/example/tasks.git"
    );

    let credential_free = "https://github.com/example/tasks.git";
    assert_eq!(redact_git_remote(credential_free), credential_free);
}

#[test]
fn scp_credential_redaction_preserves_bracketed_ipv6_colons() {
    let remote = "deploy:secret@[::1]:example/tasks.git";
    let redacted = "***@[::1]:example/tasks.git";

    assert_eq!(redact_git_remote(remote), redacted);
    for result in [
        validate_publication_remote(remote),
        validate_source_repository_fingerprint(remote),
    ] {
        let diagnostic = result.expect_err("embedded scp credentials").to_string();
        assert!(diagnostic.contains(redacted), "{diagnostic}");
        assert!(!diagnostic.contains("secret"), "{diagnostic}");
    }
}

#[test]
fn ordinary_scp_remotes_keep_usernames_and_repository_path_punctuation() {
    for (remote, identity) in [
        (
            "git@github.com:example/tasks.git",
            "github.com/example/tasks",
        ),
        ("github.com:example/tasks.git", "github.com/example/tasks"),
        (
            "github.com:example/tasks@v1.git",
            "github.com/example/tasks@v1",
        ),
        ("github.com:tasks@v1.git", "github.com/tasks@v1"),
        (
            "git@github.com:tasks@v1:archive.git",
            "github.com/tasks@v1:archive",
        ),
        (
            "git@github.com:example/tasks:archive.git",
            "github.com/example/tasks:archive",
        ),
    ] {
        validate_publication_remote(remote).expect("ordinary scp publication remote");
        validate_source_repository_fingerprint(remote).expect("ordinary scp source remote");
        assert_eq!(redact_git_remote(remote), remote);
        assert_eq!(
            git_remote_identity(remote).expect("remote identity"),
            identity
        );
    }
}

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

/// PR admission counts a remote as a forge candidate when it names any network
/// host, across the URL forms Git accepts; local paths never do.
#[test]
fn network_host_is_read_from_every_git_url_form_and_never_from_a_local_path() {
    for (remote, host) in [
        ("https://github.com/example/orbit.git", Some("github.com")),
        ("git@github.com:example/orbit.git", Some("github.com")),
        (
            "ssh://git@GHE.Example.com:2222/team/orbit.git",
            Some("ghe.example.com"),
        ),
        (
            "https://git.example.com:8443/team/orbit",
            Some("git.example.com"),
        ),
        ("forge-alias:team/orbit.git", Some("forge-alias")),
        ("/srv/git/orbit.git", None),
        ("file:///srv/git/orbit.git", None),
        ("./orbit.git", None),
        ("~/git/orbit.git", None),
        ("C:\\git\\orbit.git", None),
        ("origin", None),
    ] {
        assert_eq!(git_remote_network_host(remote).as_deref(), host, "{remote}");
    }
}
