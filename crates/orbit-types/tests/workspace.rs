//! Workspace publication contracts exercised through the public types API.

// Test assertions unwrap results, following the test-only crate-root exemption.
#![allow(clippy::expect_used)]

use orbit_types::workspace::{
    git_remote_identity, redact_git_remote, validate_publication_remote,
    validate_source_repository_fingerprint,
};

#[test]
fn scp_credentials_are_rejected_and_never_exposed_by_diagnostics_or_identity() {
    for remote in [
        "deploy:hunter2@github.com:example/tasks.git",
        "deploy:hunter2:extra@github.com:example/tasks.git",
        "deploy:@github.com:example/tasks.git",
    ] {
        assert_eq!(
            redact_git_remote(remote),
            "***@github.com:example/tasks.git"
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
        }
    }
}

#[test]
fn malformed_scp_credentials_are_redacted_even_when_parsing_fails() {
    for (remote, redacted) in [
        ("deploy:hunter2@github.com:", "***@github.com:"),
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
        }
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
