//! The bundled Bubblewrap is authenticated against the signed release
//! manifest before `orbit init` may install it as root.

use chrono::NaiveDate;

use super::fixture::{Fixture, test_trusted_keys};
use crate::update::bundled_bwrap::{bundled_bwrap_asset_name, stage_bundled_bwrap};

const VERSION: &str = "0.9.0";
const BINARY: &[u8] = b"\x7fELF static bubblewrap";

fn asset() -> String {
    bundled_bwrap_asset_name("x86_64").expect("x86_64 asset")
}

fn stage(fixture: &Fixture) -> Result<crate::update::bundled_bwrap::StagedBwrap, String> {
    stage_bundled_bwrap(
        &fixture.source(),
        VERSION,
        &asset(),
        test_trusted_keys(),
        NaiveDate::from_ymd_opt(2026, 9, 5).expect("valid date"),
    )
    .map_err(|error| error.to_string())
}

#[test]
fn a_signed_listed_binary_is_staged_executable_with_its_digest() {
    let fixture = Fixture::new(VERSION);
    fixture.publish_assets(VERSION, &[(asset().as_str(), BINARY)], true);

    let staged = stage(&fixture).expect("stage a signed binary");

    assert_eq!(std::fs::read(staged.path()).expect("staged bytes"), BINARY);
    assert_eq!(
        staged.sha256,
        orbit_common::security::release::sha256_hex(BINARY)
    );
    assert_eq!(staged.signing_key_id, "orbit-test-key-1");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(staged.path())
            .expect("staged metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
    }
    let directory = staged.path().parent().expect("staging dir").to_path_buf();
    drop(staged);
    assert!(!directory.exists(), "staging is removed with the handle");
}

#[test]
fn unsigned_tampered_or_unlisted_binaries_are_refused() {
    let unsigned = Fixture::new(VERSION);
    unsigned.publish_assets(VERSION, &[(asset().as_str(), BINARY)], false);
    let error = stage(&unsigned).expect_err("an unsigned manifest is refused");
    assert!(error.contains("signature verification failed"), "{error}");

    let tampered = Fixture::new(VERSION);
    tampered.publish_assets(VERSION, &[(asset().as_str(), BINARY)], true);
    std::fs::write(
        tampered.mirror_input(VERSION, &asset()),
        b"\x7fELF something else",
    )
    .expect("tamper with the published binary");
    let error = stage(&tampered).expect_err("a mismatched binary is refused");
    assert!(error.contains("checksum verification failed"), "{error}");

    // A release published before Orbit shipped Bubblewrap lists no entry.
    let unlisted = Fixture::new(VERSION);
    unlisted.publish_assets(
        VERSION,
        &[("orbit-x86_64-unknown-linux-gnu.tar.gz", b"orbit")],
        true,
    );
    std::fs::write(unlisted.mirror_input(VERSION, &asset()), BINARY).expect("unlisted binary");
    let error = stage(&unlisted).expect_err("an unlisted binary is refused");
    assert!(
        error.contains("publishes no signed bundled Bubblewrap"),
        "{error}"
    );
}
