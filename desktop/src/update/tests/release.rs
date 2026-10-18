use serde_json::{Value, json};

use super::super::release::{MANIFEST, SIGNATURE, VERSION_FILE, Version, newest_update};

const BUNDLE: &str = "orbit-desktop-universal-apple-darwin.dmg";

fn release(tag: &str, assets: &[&str]) -> Value {
    json!({
        "tag_name": tag,
        "draft": false,
        "prerelease": false,
        "assets": assets.iter().map(|name| json!({ "name": name })).collect::<Vec<_>>(),
    })
}

fn desktop(tag: &str) -> Value {
    release(
        tag,
        &[
            "orbit-checksums.txt",
            MANIFEST,
            SIGNATURE,
            VERSION_FILE,
            BUNDLE,
        ],
    )
}

fn version(text: &str) -> Version {
    Version::parse(text).unwrap()
}

#[test]
fn skips_releases_without_desktop_assets() {
    // The newest releases were cut with ORBIT_DESKTOP_RELEASE off, or before
    // publish-desktop finished; the older one still has a full set.
    let releases = json!([
        release("v0.31.0", &["orbit-checksums.txt"]),
        release("v0.30.0", &[MANIFEST, SIGNATURE, BUNDLE]),
        desktop("v0.29.0"),
    ]);
    assert_eq!(
        newest_update(&releases, version("0.28.3"), BUNDLE),
        Some(version("0.29.0"))
    );
    assert_eq!(
        newest_update(
            &releases,
            version("0.28.3"),
            "orbit-desktop-x86_64-unknown-linux-gnu.deb"
        ),
        None
    );
}

#[test]
fn picks_the_newest_stable_release_above_the_running_version() {
    let mut draft = desktop("v0.40.0");
    draft["draft"] = json!(true);
    let mut prerelease = desktop("v0.39.0");
    prerelease["prerelease"] = json!(true);
    let releases = json!([
        draft,
        prerelease,
        desktop("v0.38.0-rc.1"),
        desktop("v0.9.0"),
        desktop("v0.10.0"),
    ]);
    assert_eq!(
        newest_update(&releases, version("0.9.5"), BUNDLE),
        Some(version("0.10.0"))
    );
    assert_eq!(newest_update(&releases, version("0.10.0"), BUNDLE), None);
}

#[test]
fn parses_only_stable_versions() {
    assert!(version("v1.2.3") > version("1.2.0"));
    assert!(version("0.10.0") > version("0.9.9"));
    for invalid in ["1.2", "1.2.3.4", "1.2.3-rc.1", "v1.2.x", "1..3", "+1.2.3"] {
        assert_eq!(Version::parse(invalid), None, "{invalid}");
    }
}
