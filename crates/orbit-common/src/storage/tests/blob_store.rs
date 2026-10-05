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
