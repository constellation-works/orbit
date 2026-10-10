use super::super::redaction::redact_all;
use crate::test_env::scoped;

/// Credential/session classification and the numeric floor prevent the owner
/// reply corruption incident without exempting session credentials.
#[test]
fn env_redaction_preserves_session_metadata_and_small_counters() {
    use super::super::redaction::redact_sensitive_env_text;

    for name in [
        "XDG_SESSION_ID",
        "XDG_SESSION_TYPE",
        "XDG_SESSION_CLASS",
        "DBUS_SESSION_BUS_ADDRESS",
        "SESSION_MANAGER",
        "TERM_SESSION_ID",
    ] {
        let _env = scoped([(name, Some("ordinary-session-metadata-35912"))]);
        assert_eq!(
            redact_sensitive_env_text("ordinary-session-metadata-35912"),
            "ordinary-session-metadata-35912",
            "non-credential session metadata: {name}"
        );
    }
    for digits in ["35912", "12345678901", "123456789012"] {
        let _env = scoped([("MY_SESSION_TOKEN", Some(digits))]);
        let raw = format!("hash-prefix-{digits}-suffix");
        let expected = if digits.len() < 12 {
            raw.clone()
        } else {
            "hash-prefix-[REDACTED_ENV]-suffix".into()
        };
        assert_eq!(
            redact_sensitive_env_text(&raw),
            expected,
            "numeric floor: {digits}"
        );
    }
    for name in [
        "GITHUB_TOKEN",
        "AWS_SECRET_ACCESS_KEY",
        "MY_SESSION_TOKEN",
        "ANTHROPIC_API_KEY",
        "OAUTH_ACCESS",
        "SESSION_COOKIE",
        "SESSION_KEY_ID",
        "XDG_SESSION_TOKEN",
        "XDG_SESSION_KEY_ID",
        "MY_SESSION_ID",
    ] {
        let _env = scoped([(name, Some("realistic-credential-value-a8b92c6d"))]);
        assert_eq!(
            redact_sensitive_env_text("prefix realistic-credential-value-a8b92c6d suffix"),
            "prefix [REDACTED_ENV] suffix",
            "credential still scrubbed: {name}"
        );
    }
}

#[test]
fn redact_all_scrubs_key_query_params_case_insensitively() {
    let raw = concat!(
        "failed for url (https://example.test/v1beta/models/m:generateContent",
        "?key=AIzaSyQuerySecret&alt=sse) and ",
        "https://example.test/v1beta/cachedContents?foo=1&KEY=second-secret"
    );

    let redacted = redact_all(raw);

    assert!(!redacted.contains("AIzaSyQuerySecret"));
    assert!(!redacted.contains("second-secret"));
    assert!(redacted.contains("?key=[REDACTED_AUTH]&alt=sse"));
    assert!(redacted.contains("&KEY=[REDACTED_AUTH]"));
}

#[test]
fn redact_all_scrubs_provider_scm_cloud_tokens_and_connection_passwords() {
    let google = format!("AIza{}", "A".repeat(35));
    let gitlab = format!("glpat-{}", "B".repeat(20));
    let github_fine_grained = format!("github_pat_{}", "C".repeat(22));
    let github_oauth = format!("gho_{}", "D".repeat(36));
    let github_classic = format!("ghp_{}", "E".repeat(36));
    let github_server = format!("ghs_{}", "F".repeat(36));
    let github_user_server = format!("ghu_{}", "G".repeat(36));
    let github_refresh = format!("ghr_{}", "H".repeat(36));
    let aws_access_key_id = format!("AKIA{}", "1".repeat(16));
    let aws_secret_key = "aws_secret_access_key=awsSecretAccessKeyFixtureValue1234567890";
    let npm = format!("npm_{}", "I".repeat(36));
    let connection_string = "postgres://orbit_user:connection-pass@db.example.test/orbit";

    let raw = format!(
        "google={google}\n\
         gitlab={gitlab}\n\
         github_fine_grained={github_fine_grained}\n\
         github_oauth={github_oauth}\n\
         github_classic={github_classic}\n\
         github_server={github_server}\n\
         github_user_server={github_user_server}\n\
         github_refresh={github_refresh}\n\
         aws_access_key_id={aws_access_key_id}\n\
         {aws_secret_key}\n\
         npm={npm}\n\
         dsn={connection_string}"
    );

    let redacted = redact_all(&raw);

    for secret in [
        google.as_str(),
        gitlab.as_str(),
        github_fine_grained.as_str(),
        github_oauth.as_str(),
        github_classic.as_str(),
        github_server.as_str(),
        github_user_server.as_str(),
        github_refresh.as_str(),
        aws_access_key_id.as_str(),
        "awsSecretAccessKeyFixtureValue1234567890",
        npm.as_str(),
        "connection-pass",
    ] {
        assert!(!redacted.contains(secret), "{secret} was not redacted");
    }

    assert!(redacted.contains("postgres://orbit_user:[REDACTED_SECRET]@db.example.test/orbit"));
}

// [ORB-00417] redact_all_error: pattern + env redaction over OrbitError payloads.

// [ORB-10867] Ordinary-word env values must not be eligible for substitution.

#[test]
fn genuine_credential_all_letter_secret_still_redacted_alongside_git_author_name() {
    // [DANI-10514] Pins that the GIT_AUTHOR_NAME / false / null carve-outs
    // above don't regress the DANI-10471 fix: an all-letter secret in a
    // genuine credential variable must still be scrubbed.
    let _env = scoped([
        ("GIT_AUTHOR_NAME", Some("Daniel")),
        ("GITHUB_TOKEN", Some("correcthorse")),
    ]);
    let raw = "committed by Daniel using token correcthorse";

    let redacted = redact_all(raw);
    assert!(redacted.contains("Daniel"), "author name must survive");
    assert!(
        !redacted.contains("correcthorse"),
        "credential value must still be redacted: {redacted}"
    );
    assert!(redacted.contains("[REDACTED_ENV]"));
}
