use chrono::NaiveDate;

use crate::security::release::{TrustedReleaseKey, verify_checksum_signature};

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
