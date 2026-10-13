//! The child-process environment allowlist, operator env files, redaction and
//! release-manifest signature verification.

use chrono::NaiveDate;
use orbit_common::security::child_env::allowlisted_child_env_from;
use orbit_common::security::operator_env::{parse_env_text, read_env_file};
use orbit_common::security::redaction::{redact_all, redact_sensitive_env_text};
use orbit_common::security::release::{TrustedReleaseKey, verify_checksum_signature};
use orbit_common::test_env::scoped;

fn env_names(env: &[(String, String)]) -> Vec<&str> {
    env.iter().map(|(name, _)| name.as_str()).collect()
}

#[test]
fn an_explicit_pass_list_cannot_smuggle_a_privilege_bearing_orbit_name() {
    let parent = [
        ("ORBIT_OPERATOR", "1"),
        ("ORBIT_WORKSPACE_CLAIM_TOKEN", "abc123"),
        ("ORBIT_RUN_ID", "r1"),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_string(), value.to_string()))
    .collect::<Vec<_>>();
    let pass = vec![
        "ORBIT_OPERATOR".to_string(),
        "ORBIT_WORKSPACE_CLAIM_TOKEN".to_string(),
    ];

    let env = allowlisted_child_env_from(&parent, &pass, &["ORBIT_OPERATOR"]);

    // Named in `pass` and `extras` alike; still refused. Only the named
    // envelope variable, admitted on its own terms, survives.
    let orbit_names: Vec<&str> = env_names(&env)
        .into_iter()
        .filter(|name| name.starts_with("ORBIT_"))
        .collect();
    assert_eq!(
        orbit_names,
        ["ORBIT_RUN_ID"],
        "a privilege-bearing name must not ride an explicit pass/extras list either"
    );
}

fn pass_names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| (*name).to_string()).collect()
}

#[test]
fn only_pass_listed_non_empty_names_are_taken_from_the_text() {
    let text = "\
# comment
export CLAUDE_CODE_OAUTH_TOKEN=\"quoted token\"
UNLISTED_SECRET=nope
ORBIT_OPERATOR=1
EMPTY=
 SINGLE = 'single quoted'
DUP=first
DUP=second
no equals sign
";
    let pass = pass_names(&[
        "CLAUDE_CODE_OAUTH_TOKEN",
        "ORBIT_OPERATOR",
        "EMPTY",
        "SINGLE",
        "DUP",
    ]);
    assert_eq!(
        parse_env_text(text, &pass),
        [
            (
                "CLAUDE_CODE_OAUTH_TOKEN".to_string(),
                "quoted token".to_string()
            ),
            ("SINGLE".to_string(), "single quoted".to_string()),
            ("DUP".to_string(), "second".to_string()),
        ],
        "unlisted, ORBIT_ and empty entries are dropped; the last duplicate wins"
    );
}

#[cfg(unix)]
#[test]
fn a_file_other_users_can_read_is_refused_and_an_owner_only_one_is_read() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("clock.env");
    let pass = pass_names(&["TOKEN"]);
    assert!(
        read_env_file(&path, &pass).unwrap().is_none(),
        "absent file"
    );

    std::fs::write(&path, "TOKEN=abc\n").unwrap();
    for mode in [0o644, 0o640, 0o604] {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        let error = read_env_file(&path, &pass).unwrap_err().to_string();
        assert!(
            error.contains("chmod 600") && !error.contains("abc"),
            "mode {mode:o} must be refused without echoing the value: {error}"
        );
    }

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        read_env_file(&path, &pass).unwrap().unwrap(),
        [("TOKEN".to_string(), "abc".to_string())]
    );

    let link = dir.path().join("link.env");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(
        read_env_file(&link, &pass).is_err(),
        "a symlinked env file is refused"
    );
}

/// Credential/session classification and the numeric floor prevent the owner
/// reply corruption incident without exempting session credentials.
#[test]
fn env_redaction_preserves_session_metadata_and_small_counters() {
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

#[test]
fn genuine_credential_all_letter_secret_still_redacted_alongside_git_author_name() {
    // [DANI-10514] Pins that the GIT_AUTHOR_NAME / false / null carve-outs
    // don't regress the DANI-10471 fix: an all-letter secret in a
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

/// A throwaway 2048-bit key generated for these tests only. The matching
/// private key is not held anywhere, so the fixture below is the only
/// signature that will ever verify against it.
const TEST_KEY_PEM: &str = r#"-----BEGIN PUBLIC KEY-----
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAzCOZ+2ay5FVATadsgbNO
0BlIPVszeZOpB+BRhKGjeJ8ZkBAaUXwSHbdjplsjhiqe8LJ8LEksJqAoM6tXXOFl
eVN1a/d7SmPyoWQdMFb7vPOi6zstKVyuhWMUfNG+e9gn8/0sutpRRbbFf1crGF8b
R7dTEa7m1rPGwS7jBMC+avb7EOPmVyZuIRqHwzDITTeCpT+R1YWu0DyFJdZyvPvB
+lGYDw8K58yOd5omN3P+9hFKWc63kSSwdNgfwFCMLao6GYdDqUOlpws3Ert6RmCe
23oO0DJ/uusklfzfjOstoF//Cf9QOfWn/dfAS1DN7BtmCsAUMn7uHxAbJX3O1gDr
6QIDAQAB
-----END PUBLIC KEY-----"#;

const TEST_MANIFEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  orbit-x86_64-unknown-linux-gnu.tar.gz\nbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb  ./dist/orbit-aarch64-apple-darwin.tar.gz\n";

const TEST_SIGNATURE_HEX: &str = "2efdd73ddeece32bf49e9fcab691553c117778d3e9351493a6096b6d9af9891baae264d7147cd4bd4d0a802f0c208c254a70fd06f151c726f662f9b00e157dcccf2b94d983e829a8ab631a7bbd91a1a89ee01185756631c2ce32f7060aa85ca78032cb7abc2772d612d2ced7ada71cb3dbf91e87cd6453b3ee23968dac921533b78899a02ae5936d53051cc2fcb7949aa72c462073b6d07c21c73ec9fa5e8e5921665f550d27425d53b2d16467f406dac1ef66a2cc5b743c59165aed168a055b94bbc1bda1127c79ea8a32fcab2aa1bb74aa07f2b8cb38520916ca058b0c49d64815a2e3b8d10bc1129e48e276b616a629b1b5bb182bcc8bf4a6699d4d0d3ee8";

fn decode_hex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .chunks(2)
        .map(|pair| {
            u8::from_str_radix(std::str::from_utf8(pair).expect("hex pair"), 16).expect("hex byte")
        })
        .collect()
}

fn key_set(not_after: &'static str, revoked_at: Option<&'static str>) -> Vec<TrustedReleaseKey> {
    vec![TrustedReleaseKey {
        id: "orbit-test-key-1",
        not_after,
        revoked_at,
        public_key_pem: TEST_KEY_PEM,
    }]
}

fn leaked(keys: Vec<TrustedReleaseKey>) -> &'static [TrustedReleaseKey] {
    Box::leak(keys.into_boxed_slice())
}

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 9, 5).expect("valid date")
}

#[test]
fn tampered_manifest_fails_signature_verification() {
    let keys = leaked(key_set("2099-12-31", None));
    let tampered = TEST_MANIFEST.replace("aaaa", "cccc");

    let error = verify_checksum_signature(
        tampered.as_bytes(),
        &decode_hex(TEST_SIGNATURE_HEX),
        keys,
        today(),
    )
    .expect_err("tampered manifest");

    assert!(
        error
            .to_string()
            .contains("no trusted release signing key matched"),
        "{error}"
    );
}
