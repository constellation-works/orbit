use super::super::pin::{PluginPin, PluginPinFile};

#[test]
fn pin_file_rejects_unknown_schema_and_duplicates() {
    let mut file = PluginPinFile::default();
    file.plugins.push(PluginPin {
        name: "graph".into(),
        version: Some("not-a-version".into()),
        source: None,
        digest: None,
        enabled: true,
    });
    assert_eq!(
        file.validate().unwrap_err(),
        "plugins[0].version: invalid version 'not-a-version': expected MAJOR.MINOR.PATCH"
    );
    file.plugins[0].version = Some("^0.4.1".into());
    file.plugins.push(file.plugins[0].clone());
    assert!(file.validate().unwrap_err().contains("plugins[1].name"));
    file.plugins.pop();
    file.schema_version = 2;
    assert!(file.validate().unwrap_err().starts_with("schemaVersion"));
}

#[test]
fn pin_file_parses_the_design_example() {
    let raw = "schemaVersion: 1\nplugins:\n  - name: graph\n    version: \"0.4.x\"\n    source: git+https://github.com/constellation-works/orbit-graph#v0.4.1\n    enabled: true\n";
    assert_eq!(
        raw.as_bytes(),
        b"schemaVersion: 1\nplugins:\n  - name: graph\n    version: \"0.4.x\"\n    source: git+https://github.com/constellation-works/orbit-graph#v0.4.1\n    enabled: true\n"
    );
    let file: PluginPinFile = serde_yaml::from_str(raw).expect("parse");
    file.validate().expect("valid");
    assert_eq!(file.plugins[0].name, "graph");
}

const ARCHIVE_URL: &str = "https://example.com/orbit-graph-0.4.1.tar.gz";
const DIGEST: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn archive_pin(digest: Option<&str>) -> PluginPinFile {
    let mut file = PluginPinFile::default();
    file.plugins.push(PluginPin {
        name: "graph".into(),
        version: None,
        source: Some(ARCHIVE_URL.into()),
        digest: digest.map(str::to_string),
        enabled: true,
    });
    file
}

/// No trust on first use: the fetch an unpinned archive source would make is
/// exactly the one whoever controls the URL would tamper with, so the pin
/// file refuses to describe it at all.
#[test]
fn an_archive_source_without_a_digest_is_refused() {
    let error = archive_pin(None).validate().unwrap_err();
    assert!(
        error.starts_with("plugins[0].digest:") && error.contains(ARCHIVE_URL),
        "the refusal must name the pin entry and its source: {error}"
    );
    assert!(
        error.contains("first use"),
        "the refusal must say why a digest is mandatory: {error}"
    );
}

#[test]
fn an_archive_source_with_a_well_formed_digest_validates() {
    archive_pin(Some(DIGEST)).validate().expect("valid");
}

#[test]
fn a_malformed_or_misplaced_digest_is_refused() {
    let error = archive_pin(Some("sha256:not-hex")).validate().unwrap_err();
    assert!(
        error.starts_with("plugins[0].digest:"),
        "a malformed digest names its entry: {error}"
    );
    let error = archive_pin(Some("0123456789abcdef"))
        .validate()
        .unwrap_err();
    assert!(
        error.contains("sha256:"),
        "a digest without its algorithm prefix is refused: {error}"
    );

    // A digest on a source Orbit never downloads would claim a verification
    // the install does not perform.
    let mut git = archive_pin(Some(DIGEST));
    git.plugins[0].source = Some("git+https://example.com/graph.git#v0.4.1".into());
    let error = git.validate().unwrap_err();
    assert!(
        error.starts_with("plugins[0].digest:") && error.contains("https://"),
        "only a fetched archive is digest-verified: {error}"
    );
}

#[test]
fn remote_archive_sources_are_recognised_by_scheme_and_extension() {
    for source in [
        ARCHIVE_URL,
        "https://example.com/p.tgz",
        "https://example.com/p.ZIP",
        "https://example.com/p.tar.gz?token=1",
    ] {
        assert_eq!(
            super::super::pin::remote_archive_source(source),
            Some(source),
            "{source} is an archive Orbit fetches"
        );
    }
    for source in [
        "http://example.com/p.tar.gz",
        "https://example.com/plugin",
        "git+https://example.com/p.git",
        "./local/p.tar.gz",
    ] {
        assert_eq!(
            super::super::pin::remote_archive_source(source),
            None,
            "{source} is not an archive Orbit fetches"
        );
    }
}
