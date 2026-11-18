use rsa::RsaPublicKey;
use rsa::pkcs8::DecodePublicKey;
use time::Date;

use super::super::trust::{
    TRUSTED_RELEASE_KEYS, TrustedReleaseKey, checksum_for, parse_trust_date, verify_manifest,
};

const MANIFEST: &[u8] = include_bytes!("fixtures/manifest.txt");
const SIGNATURE: &[u8] = include_bytes!("fixtures/manifest.txt.sig");
/// A throwaway key that signed the fixture manifest.
const TEST_KEY_PEM: &str = include_str!("fixtures/test-key.pub");

fn key(not_after: &'static str, revoked_at: Option<&'static str>) -> TrustedReleaseKey {
    TrustedReleaseKey {
        id: "orbit-release-key-test",
        not_after,
        revoked_at,
        public_key_pem: TEST_KEY_PEM,
    }
}

fn day(text: &str) -> Date {
    parse_trust_date(text).unwrap()
}

#[test]
fn accepts_a_manifest_signed_by_a_current_key() {
    let keys = [key("2030-01-01", None)];
    assert_eq!(
        verify_manifest(MANIFEST, SIGNATURE, &keys, day("2030-01-01")),
        Ok("orbit-release-key-test")
    );
}

#[test]
fn refuses_a_manifest_signed_by_a_revoked_or_expired_key() {
    let revoked = [key("2030-01-01", Some("2026-01-01"))];
    let error = verify_manifest(MANIFEST, SIGNATURE, &revoked, day("2027-01-01")).unwrap_err();
    assert!(error.contains("revoked"), "{error}");

    let expired = [key("2026-12-31", None)];
    let error = verify_manifest(MANIFEST, SIGNATURE, &expired, day("2027-01-01")).unwrap_err();
    assert!(error.contains("expired"), "{error}");
}

#[test]
fn refuses_a_key_with_an_invalid_not_after() {
    for invalid in [
        "2030-02-30",
        "2030-13-01",
        "2030-00-10",
        "not-a-date",
        "2030-1-01",
    ] {
        let keys = [key(invalid, None)];
        let error = verify_manifest(MANIFEST, SIGNATURE, &keys, day("2027-01-01")).unwrap_err();
        assert!(error.contains("invalid not_after"), "{invalid}: {error}");
    }
    // Leap day is a real date and remains valid.
    assert!(parse_trust_date("2028-02-29").is_ok());
    assert!(parse_trust_date("2027-02-29").is_err());
}

#[test]
fn refuses_a_tampered_manifest_and_an_unknown_signer() {
    let keys = [key("2030-01-01", None)];
    let mut tampered = MANIFEST.to_vec();
    tampered[0] ^= 1;
    assert!(verify_manifest(&tampered, SIGNATURE, &keys, day("2027-01-01")).is_err());
    // The compiled-in Orbit keys load, so a mismatch is reported as one, and
    // none of them signed the fixture.
    for key in TRUSTED_RELEASE_KEYS {
        assert!(
            RsaPublicKey::from_public_key_pem(key.public_key_pem).is_ok(),
            "{} must load, or every update would be refused",
            key.id
        );
    }
    let error =
        verify_manifest(MANIFEST, SIGNATURE, TRUSTED_RELEASE_KEYS, day("2027-01-01")).unwrap_err();
    assert!(error.contains("no trusted release key"), "{error}");
}

#[test]
fn finds_bare_and_path_qualified_manifest_entries() {
    let digest = "a".repeat(64);
    let manifest = format!(
        "{digest}  ./dist/orbit-desktop-version.txt\n{}  orbit-desktop-x.deb\nnot a line\n",
        "B".repeat(64)
    );
    assert_eq!(
        checksum_for(&manifest, "orbit-desktop-version.txt"),
        Some(digest)
    );
    assert_eq!(
        checksum_for(&manifest, "orbit-desktop-x.deb"),
        Some("b".repeat(64))
    );
    assert_eq!(checksum_for(&manifest, "orbit-desktop-x.AppImage"), None);
    assert_eq!(
        checksum_for("abc  orbit-desktop-x.deb\n", "orbit-desktop-x.deb"),
        None
    );
}
