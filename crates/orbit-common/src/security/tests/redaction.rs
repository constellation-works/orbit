use std::sync::{Mutex, MutexGuard, OnceLock};

use super::super::redaction::{
    PatternRedactor, argv_redactor, credential_safe_location, default_pattern_redactor,
    is_high_confidence_single_token_credential, is_redactable_value, is_sensitive_env_name,
    redact_all, redact_home_dir, redact_sensitive_env_text,
};

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
fn credential_safe_location_rejects_urls_and_scrubs_path_credentials() {
    assert_eq!(
        credential_safe_location("https://orbit-user:secret@example.test/repo"),
        "[REDACTED_LOCATION]"
    );
    let safe =
        credential_safe_location("/tmp/worktrees/token=Bearer abc123def456ghi789SECRETTOKEN/orbit");
    assert!(!safe.contains("abc123def456ghi789SECRETTOKEN"));
    assert!(safe.contains("[REDACTED_AUTH]"));
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

#[test]
fn redact_all_scrubs_structural_ssh_key_and_machine_nameentifiers() {
    let fingerprint = format!("SHA256:{}", "A".repeat(43));
    let public_key = format!("ssh-ed25519 {}", "B".repeat(48));
    let raw = format!(
        "256 {fingerprint} automation@build-node.example.test (ED25519)\n\
         debug1: Offering public key: automation@build-node.example.test ED25519 {fingerprint} agent\n\
         {public_key} deploy@mirror-node.example.test\n\
         debug1: Connecting to build-node.example.test [192.0.2.10] port 22.\n\
         debug1: Authenticating to build-node.example.test:22 as 'git'\n\
         Authenticated to build-node.example.test ([192.0.2.10]:22)."
    );

    let redacted = redact_all(&raw);

    assert!(!redacted.contains(&fingerprint));
    assert!(!redacted.contains("automation@build-node.example.test"));
    assert!(!redacted.contains("deploy@mirror-node.example.test"));
    assert!(!redacted.contains("build-node.example.test"));
    assert!(!redacted.contains("192.0.2.10"));
    assert_eq!(redacted.matches("[REDACTED_SSH_FINGERPRINT]").count(), 2);
    assert_eq!(redacted.matches("[REDACTED_SSH_KEY_COMMENT]").count(), 3);
    assert_eq!(redacted.matches("[REDACTED_SSH_HOST]").count(), 3);
    assert!(redacted.contains(&public_key));
}

#[test]
fn redact_all_preserves_knowledge_record_identifiers_and_paths() {
    let legitimate = concat!(
        "commit 238a89cbec9abf478d13ed2bf3ca7d28a722c21c\n",
        "run jrun-20260802-2012-4 task ORB-12345\n",
        "worktree /srv/worktrees/jrun-20260802-2012-4/src/module\n",
        "model gpt-6-sol\n",
        "blob sha256:4f1c2a709db7089bd3da48e35a3a2f77d6c0f41d8d792f0dcb163a7d89fd53e0"
    );

    assert_eq!(redact_all(legitimate), legitimate);
}

#[test]
fn provider_key_redaction_respects_identifier_boundaries() {
    let migration = "remove-task-checkout-projections";
    let compounds = [
        "disk-sk-checkout-projections",
        "risk-sk-checkout-projections",
        "task-sk-checkout-projections",
        "é-sk-checkout-projections",
    ];
    let default = PatternRedactor::default();
    let argv = PatternRedactor::with_argv_secrets();

    assert_eq!(redact_all(migration), migration);
    assert_eq!(default.apply_str(migration), migration);
    assert_eq!(argv.apply_str(migration), migration);

    for compound in compounds {
        assert_eq!(redact_all(compound), compound, "default: {compound}");
        assert_eq!(argv.apply_str(compound), compound, "argv: {compound}");
    }
}

#[test]
fn provider_key_redaction_keeps_standalone_and_argv_forms() {
    let key = "sk-abcdefghijklmnopqrstuvwxyz";
    let default = PatternRedactor::default();

    for input in [
        key.to_string(),
        format!("before {key} after"),
        format!("'{key}'"),
        format!("({key}),"),
        format!("X-Api-Key: {key}"),
        format!(r#"{{"api_key":"{key}"}}"#),
    ] {
        let redacted = default.apply_str(&input);
        assert!(!redacted.contains(key), "{input}");
        assert!(
            redacted.contains("[REDACTED_SECRET]") || redacted.contains("[REDACTED_AUTH]"),
            "{redacted}"
        );
    }

    let argv = PatternRedactor::with_argv_secrets();
    for (input, marker) in [
        (key, "[REDACTED_SECRET]"),
        ("--api-key=sk-short", "[REDACTED_API_KEY]"),
    ] {
        let redacted = argv.apply_str(input);
        assert!(!redacted.contains("sk-"), "{redacted}");
        assert!(redacted.contains(marker), "{redacted}");
    }
}

#[test]
fn http_and_argv_redactors_are_process_cached() {
    assert!(std::ptr::eq(
        default_pattern_redactor(),
        default_pattern_redactor()
    ));
    assert!(std::ptr::eq(argv_redactor(), argv_redactor()));
    assert!(!std::ptr::eq(
        default_pattern_redactor() as *const PatternRedactor,
        argv_redactor() as *const PatternRedactor
    ));

    let key = "--api-key=sk-short";
    assert_eq!(
        PatternRedactor::http_default().apply_str(key),
        default_pattern_redactor().apply_str(key)
    );
    assert_eq!(
        PatternRedactor::with_argv_secrets().apply_str(key),
        argv_redactor().apply_str(key)
    );
}

#[test]
fn redact_home_dir_ignores_root_home() {
    let _home = EnvVarGuard::set("HOME", "/");

    assert_eq!(redact_home_dir("/tmp/x"), "/tmp/x");
}

#[test]
fn redact_home_dir_matches_only_path_boundaries() {
    let _home = EnvVarGuard::set("HOME", "/Users/a");

    assert_eq!(redact_home_dir("/Users/ab/x"), "/Users/ab/x");
    assert_eq!(redact_home_dir("/Users/a/x"), "~/x");
}

#[test]
fn high_confidence_single_token_detection_covers_provider_scm_cloud_families() {
    let credentials = [
        format!("AIza{}", "A".repeat(35)),
        format!("glpat-{}", "B".repeat(20)),
        format!("github_pat_{}", "C".repeat(22)),
        format!("gho_{}", "D".repeat(36)),
        format!("ghp_{}", "E".repeat(36)),
        format!("ghs_{}", "F".repeat(36)),
        format!("ghu_{}", "G".repeat(36)),
        format!("ghr_{}", "H".repeat(36)),
        format!("AKIA{}", "1".repeat(16)),
        "aws_secret_access_key=awsSecretAccessKeyFixtureValue1234567890".to_string(),
        format!("npm_{}", "I".repeat(36)),
        "postgres://orbit_user:connection-pass@db.example.test".to_string(),
    ];

    for credential in credentials {
        assert!(
            is_high_confidence_single_token_credential(&credential),
            "{credential} was not classified as a high-confidence credential"
        );
    }
}

#[test]
fn credential_shaped_names_stay_classified_sensitive() {
    // Name classification governs which live env *values* get scrubbed out of
    // persisted text. It is no longer an admission gate for child environments
    // (see `security::child_env`), so this asserts only the redaction contract.
    for name in [
        "ANTHROPIC_API_KEY",
        "GH_TOKEN",
        "MY_SECRET",
        "DB_PASSWORD",
        "AWS_SECRET_ACCESS_KEY",
        "SOME_PRIVATE_KEY",
        "AUTH_BEARER",
        "AUTHORIZATION",
        "AUTHZ",
        "OAUTH",
        "GOOGLE_OAUTH",
        "BASIC_AUTHORIZATION",
        "AUTHKEY",
        "AUTHN",
        "XAUTH",
        "GITHUB_OAUTH",
    ] {
        assert!(
            is_sensitive_env_name(name),
            "{name} must be classified sensitive so its value is scrubbed"
        );
    }
    // Identity / runtime-context vars are NOT sensitive: their values are
    // ordinary paths and names that must stay readable in diagnostics.
    for name in [
        "USER",
        "LOGNAME",
        "HOME",
        "PATH",
        "GIT_AUTHOR_NAME",
        "GIT_AUTHOR_EMAIL",
        "GIT_AUTHOR_DATE",
        "GIT_COMMITTER_NAME",
        "GIT_COMMITTER_EMAIL",
        "GIT_COMMITTER_DATE",
    ] {
        assert!(
            !is_sensitive_env_name(name),
            "{name} must not be classified sensitive"
        );
    }
}

// [ORB-00417] redact_all_error: pattern + env redaction over OrbitError payloads.

#[test]
fn redact_all_error_scrubs_bearer_token_in_message() {
    use super::super::redaction::redact_all_error;
    use crate::OrbitError;

    let raw = OrbitError::Execution(
        "request to https://api.example.test failed \
         (Authorization: Bearer abc123def456ghi789SECRETTOKEN)"
            .to_string(),
    );
    let redacted = redact_all_error(raw);
    let message = redacted.to_string();

    assert!(
        !message.contains("abc123def456ghi789SECRETTOKEN"),
        "bearer token must be redacted from the error message: {message}"
    );
    assert!(
        message.contains("[REDACTED_AUTH]"),
        "a redaction placeholder should replace the token: {message}"
    );
    assert!(
        matches!(redacted, OrbitError::Execution(_)),
        "the error variant must be preserved"
    );
}

#[test]
fn redact_all_error_is_idempotent() {
    use super::super::redaction::redact_all_error;
    use crate::OrbitError;

    let OrbitError::Store(once) = redact_all_error(OrbitError::Store(
        "token=Bearer abc123def456ghi789SECRETTOKEN".to_string(),
    )) else {
        panic!("variant must be preserved");
    };
    let OrbitError::Store(twice) = redact_all_error(OrbitError::Store(once.clone())) else {
        panic!("variant must be preserved");
    };
    assert_eq!(
        once, twice,
        "redaction must be idempotent so read-time re-application is safe"
    );
}

#[test]
fn redact_all_error_sanitizes_artifact_origin_locations() {
    use super::super::redaction::redact_all_error;
    use crate::{ArtifactOrigin, ArtifactOriginMode, NotFoundKind, OrbitError};

    let error = OrbitError::artifact_not_local(
        NotFoundKind::Adr,
        "ADR-0234",
        ArtifactOrigin {
            mode: ArtifactOriginMode::Federated,
            worktree_root: "https://orbit-user:secret@example.test/repo".to_string(),
            branch: Some("Bearer abc123def456ghi789SECRETTOKEN".to_string()),
        },
    );
    let redacted = redact_all_error(error);
    let origin = redacted.artifact_origin().expect("artifact origin");

    assert_eq!(origin.worktree_root, "[REDACTED_LOCATION]");
    assert_eq!(origin.branch.as_deref(), Some("[REDACTED_LOCATION]"));
}

// [ORB-10867] Ordinary-word env values must not be eligible for substitution.

#[test]
fn ordinary_words_are_not_redactable_env_values() {
    for word in [
        "user", "true", "false", "none", "null", "root", "main", "test", "prod", "local", "auto",
        "User", "USER", "False", "NULL",
    ] {
        assert!(
            !is_redactable_value(word),
            "{word} looks like an ordinary word and must not be substituted"
        );
    }
    assert!(
        !is_redactable_value("  user  "),
        "trim must not promote an ordinary word into a secret"
    );
    assert!(
        !is_redactable_value("abc"),
        "values shorter than 4 characters stay ineligible"
    );
}

#[test]
fn secret_like_env_values_remain_redactable() {
    for secret in [
        "a1b2",
        "orbit-redaction-secret-value",
        "orbit-friction-secret-value",
        "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcd123456",
        "correcthorse",
    ] {
        assert!(
            is_redactable_value(secret),
            "{secret} is secret-like and must stay eligible"
        );
    }
}

#[test]
fn common_word_env_value_is_not_substituted_even_as_a_token_or_substring() {
    let _env = EnvVarGuard::set("GITHUB_TOKEN", "user");

    assert_eq!(
        redact_sensitive_env_text("No user-facing CLI behavior should change."),
        "No user-facing CLI behavior should change."
    );
    assert_eq!(
        redact_sensitive_env_text("superuser username users"),
        "superuser username users"
    );
}

#[test]
fn all_letter_secret_env_value_is_redacted_by_both_entry_points() {
    let _env = EnvVarGuard::set("GITHUB_TOKEN", "correcthorse");
    let raw = "provider diagnostic: correcthorse";

    assert_eq!(
        redact_sensitive_env_text(raw),
        "provider diagnostic: [REDACTED_ENV]"
    );
    assert_eq!(redact_all(raw), "provider diagnostic: [REDACTED_ENV]");
}

#[test]
fn git_author_name_env_value_is_not_substring_replaced() {
    // [DANI-10514] `GIT_AUTHOR_NAME` matches the bare "AUTH" substring test
    // that `is_sensitive_env_name` used to run, so an ordinary all-letter
    // name (an env var Orbit itself sets for child git processes) must not
    // be scrubbed out of logs, agent transcripts, or blob-stored output.
    let _env = EnvVarGuard::set("GIT_AUTHOR_NAME", "Daniel");
    let raw = "committed by Daniel on behalf of the automation";

    assert_eq!(redact_sensitive_env_text(raw), raw);
    assert_eq!(redact_all(raw), raw);
}

#[test]
fn boolean_and_null_sentinels_are_symmetric_in_the_value_gate() {
    // [DANI-10514] `true`/`none` were exempt but `false`/`null` were not,
    // so a sensitive-named boolean or sentinel left `true` readable while
    // scrubbing every literal `false`/`null` elsewhere in the text.
    for word in ["false", "null", "False", "NULL"] {
        assert!(
            !is_redactable_value(word),
            "{word} must be treated the same as true/none by the value gate"
        );
    }

    let _enabled = EnvVarGuard::set("ORBIT_AUTH_ENABLED", "false");
    let raw = "retry succeeded: false, fallback: false";
    assert_eq!(redact_sensitive_env_text(raw), raw);
    assert_eq!(redact_all(raw), raw);
}

#[test]
fn auth_family_credential_names_are_sensitive_and_author_identity_is_not() {
    // [ORB-12508] DANI-10514 required an exact AUTH segment so GIT_AUTHOR_*
    // stayed readable; that also dropped AUTHORIZATION / AUTHZ / OAUTH and
    // AUTH*-prefixed / *AUTH-suffixed credential names.
    for name in [
        "AUTHORIZATION",
        "AUTHZ",
        "OAUTH",
        "GOOGLE_OAUTH",
        "BASIC_AUTHORIZATION",
        "AUTHKEY",
        "AUTHN",
        "XAUTH",
        "GITHUB_OAUTH",
        "AUTH_TOKEN",
        "GH_AUTH",
    ] {
        assert!(
            is_sensitive_env_name(name),
            "{name} must be classified sensitive"
        );
    }
    for name in ["GIT_AUTHOR_NAME", "GIT_AUTHOR_EMAIL"] {
        assert!(
            !is_sensitive_env_name(name),
            "{name} must stay a non-credential author identity var"
        );
    }
}

#[test]
fn auth_family_env_values_are_redacted_while_git_author_names_survive() {
    let authorization = "SEC-AUTHORIZATION-ORB12508-AAA";
    let authz = "SEC-AUTHZ-ORB12508-BBB";
    let oauth = "SEC-OAUTH-ORB12508-CCC";
    let google_oauth = "SEC-GOAUTH-ORB12508-DDD";
    let basic_authorization = "SEC-BASICAUTHZ-ORB12508-EEE";
    let authkey = "SEC-AUTHKEY-ORB12508-FFF";
    let authn = "SEC-AUTHN-ORB12508-GGG";
    let xauth = "SEC-XAUTH-ORB12508-HHH";
    let github_oauth = "SEC-GHOAUTH-ORB12508-III";
    let author_name = "OrbitAuthorFixture";
    let author_email = "orbit-author-fixture@example.test";
    let _env = EnvVarGuard::set_many(&[
        ("AUTHORIZATION", authorization),
        ("AUTHZ", authz),
        ("OAUTH", oauth),
        ("GOOGLE_OAUTH", google_oauth),
        ("BASIC_AUTHORIZATION", basic_authorization),
        ("AUTHKEY", authkey),
        ("AUTHN", authn),
        ("XAUTH", xauth),
        ("GITHUB_OAUTH", github_oauth),
        ("GIT_AUTHOR_NAME", author_name),
        ("GIT_AUTHOR_EMAIL", author_email),
    ]);
    let raw = format!(
        "1={authorization} 2={authz} 3={oauth} 4={google_oauth} \
         5={basic_authorization} 6={authkey} 7={authn} 8={xauth} \
         9={github_oauth} author={author_name} email={author_email}"
    );

    let redacted = redact_sensitive_env_text(&raw);
    for secret in [
        authorization,
        authz,
        oauth,
        google_oauth,
        basic_authorization,
        authkey,
        authn,
        xauth,
        github_oauth,
    ] {
        assert!(
            !redacted.contains(secret),
            "credential value must be redacted: {redacted}"
        );
    }
    assert_eq!(redacted.matches("[REDACTED_ENV]").count(), 9);
    assert!(
        redacted.contains(author_name),
        "GIT_AUTHOR_NAME must survive: {redacted}"
    );
    assert!(
        redacted.contains(author_email),
        "GIT_AUTHOR_EMAIL must survive: {redacted}"
    );
}

#[test]
fn genuine_credential_all_letter_secret_still_redacted_alongside_git_author_name() {
    // [DANI-10514] Pins that the GIT_AUTHOR_NAME / false / null carve-outs
    // above don't regress the DANI-10471 fix: an all-letter secret in a
    // genuine credential variable must still be scrubbed.
    let _env = EnvVarGuard::set_many(&[
        ("GIT_AUTHOR_NAME", "Daniel"),
        ("GITHUB_TOKEN", "correcthorse"),
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

#[test]
fn secret_shaped_env_value_is_still_replaced_as_a_substring() {
    // Pin: eligible (non-letter-containing) values keep bare substring
    // matching. Mid-word occurrences of a short secret-shaped value are
    // substituted; ordinary words are the other side of the line.
    let _env = EnvVarGuard::set("GITHUB_TOKEN", "a1b2");

    assert_eq!(redact_sensitive_env_text("xa1b2y"), "x[REDACTED_ENV]y");
    assert_eq!(
        redact_sensitive_env_text("leaked a1b2 token"),
        "leaked [REDACTED_ENV] token"
    );
}

struct EnvVarGuard {
    _lock: MutexGuard<'static, ()>,
    vars: Vec<(&'static str, Option<String>)>,
}

impl EnvVarGuard {
    fn set(name: &'static str, value: &str) -> Self {
        Self::set_many(&[(name, value)])
    }

    // A single `Mutex::lock` is not reentrant: acquiring it twice from the
    // same test (one `set` call per env var) deadlocks. Callers that need
    // more than one var set at once must go through this instead.
    fn set_many(pairs: &[(&'static str, &str)]) -> Self {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let lock = LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let vars = pairs
            .iter()
            .map(|(name, value)| {
                let previous = std::env::var(name).ok();
                // SAFETY: this test guard serializes environment mutation and restores on drop.
                unsafe {
                    std::env::set_var(name, value);
                }
                (*name, previous)
            })
            .collect();
        Self { _lock: lock, vars }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // SAFETY: the guard holds the serialization lock for the full mutation window.
        unsafe {
            for (name, previous) in &self.vars {
                match previous {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
}
