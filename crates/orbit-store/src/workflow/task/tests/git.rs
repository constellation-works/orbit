use super::super::git::GitRunner;

/// Credential-bearing arguments cannot reach this runner through inspect's
/// public boundary anymore, so exercise its diagnostic safeguard directly.
#[test]
fn git_failure_redacts_remote_credentials_in_arguments() {
    for remote in [
        "https://diagnostic-token@host.test/repo.git",
        "https://user:diagnostic-token@host.test/repo.git",
        "ssh://user:diagnostic-token@host.test/repo.git",
    ] {
        let error = GitRunner::new("publication inspect")
            .run(&["orbit-invalid-subcommand", remote, "/private/local-cache"])
            .expect_err("unknown Git command must fail")
            .to_string();
        assert!(error.contains("***@host.test/repo.git"), "{error}");
        assert!(
            !error.contains("diagnostic-token") && !error.contains("/private/local-cache"),
            "publication Git failure must mask credentials and local paths: {error}"
        );
    }
}
