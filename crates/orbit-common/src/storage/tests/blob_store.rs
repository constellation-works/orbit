use crate::security::redaction::redact_all;
use crate::storage::blob_store::BlobStore;
use sha2::{Digest, Sha256};
use tempfile::tempdir;

#[test]
fn write_redacts_secret_patterns_in_non_utf8_blob() {
    let temp = tempdir().expect("tempdir");
    let store = BlobStore::new(temp.path());
    let secret = b"nonutf-secret-token";
    let mut raw = b"stdout prefix\nAuthorization: Bearer ".to_vec();
    raw.extend_from_slice(secret);
    raw.extend_from_slice(b"\ninvalid byte follows: ");
    raw.push(0xff);

    let hash = store.write(&raw).expect("write blob");
    let stored = store.read(&hash).expect("read blob");
    let stored_text = String::from_utf8(stored.clone()).expect("stored lossy utf8");
    let expected = redact_all(&String::from_utf8_lossy(&raw));

    assert!(!stored.windows(secret.len()).any(|window| window == secret));
    assert!(!stored_text.contains("nonutf-secret-token"));
    assert!(stored_text.contains("Authorization: [REDACTED_AUTH]"));
    assert_eq!(hash, sha256_hex(expected.as_bytes()));
}

#[test]
fn redact_for_storage_scrubs_json_escaped_multiline_private_key() {
    // [ORB-14120] JSON serialization escapes the key's newlines, so a raw
    // substring match leaves every line in the audit blob. The trailing BEL
    // makes the JSON-string body differ from the Rust `Debug` body; dropping
    // the JSON form would leave the ASCII lines in place.
    let lines = [
        "-----BEGIN PRIVATE KEY-----",
        "ORB14120FIXTURELINEONEAAAAAAAA",
        "ORB14120FIXTURELINETWOBBBBBBBBB",
        "-----END PRIVATE KEY-----",
    ];
    // Trailing BEL is not part of a key line. It exists so the JSON-string
    // body differs from the Debug body; the lines themselves stay verbatim
    // inside the JSON encoding.
    let key = format!("{}\n\u{7}", lines.join("\n"));
    let _env = crate::test_env::scoped([("GITHUB_APP_PRIVATE_KEY", Some(key.as_str()))]);
    let serialized = serde_json::to_vec(&serde_json::json!({"o": key})).expect("json");
    let dir = tempdir().expect("tempdir");
    let redacted = BlobStore::new(dir.path()).redact_for_storage(&serialized);
    let text = String::from_utf8(redacted).expect("redacted utf8");
    let json_body = json_string_body(&key);
    let debug_body = debug_string_body(&key);

    assert_ne!(
        json_body, debug_body,
        "fixture must make the JSON and Debug encodings differ"
    );
    assert!(
        text.contains("[REDACTED_ENV]"),
        "json-escaped private key was not redacted: {text}"
    );
    assert!(
        !text.contains(&json_body),
        "json-escaped private key survived blob redaction: {text}"
    );
    for line in lines {
        assert!(
            !text.contains(line),
            "private-key line survived JSON blob redaction: {line} in {text}"
        );
    }
}

fn json_string_body(value: &str) -> String {
    let encoded = serde_json::to_string(value).expect("json string");
    encoded
        .strip_prefix('"')
        .and_then(|body| body.strip_suffix('"'))
        .expect("json quotes")
        .to_string()
}

fn debug_string_body(value: &str) -> String {
    let encoded = format!("{value:?}");
    encoded
        .strip_prefix('"')
        .and_then(|body| body.strip_suffix('"'))
        .expect("debug quotes")
        .to_string()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{:02x}", byte));
    }
    out
}
