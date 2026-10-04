use super::super::pin::{PluginPin, PluginPinFile};

const ARCHIVE_URL: &str = "https://example.com/orbit-graph-0.4.1.tar.gz";
const DIGEST: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn archive_pin(digest: Option<&str>) -> PluginPinFile {
    let mut file = PluginPinFile::default();
    file.plugins.push(PluginPin {
        name: "graph".into(),
        version: None,
        source: Some(ARCHIVE_URL.into()),
        digest: digest.map(str::to_string),
        artifact_digest: None,
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

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

fn built_pin(source: &str, artifact_digest: &str) -> PluginPinFile {
    let mut file = PluginPinFile::default();
    file.plugins.push(PluginPin {
        name: "graph".into(),
        version: None,
        source: Some(source.into()),
        digest: None,
        artifact_digest: Some(artifact_digest.into()),
        enabled: true,
    });
    file
}

/// An artifact digest states what building one commit produces, so it is
/// only accepted beside a source pinned to a full commit: a branch, tag or
/// abbreviation names whatever the repository serves at fetch time.
#[test]
fn an_artifact_digest_requires_a_commit_pinned_git_source() {
    built_pin(
        &format!("git+https://example.com/graph.git#{COMMIT}"),
        DIGEST,
    )
    .validate()
    .expect("a full commit may carry an artifact digest");

    for source in [
        "git+https://example.com/graph.git#v0.4.1",
        "git+https://example.com/graph.git#0123456",
        "git+https://example.com/graph.git",
        "/srv/plugins/graph",
    ] {
        let error = built_pin(source, DIGEST).validate().unwrap_err();
        assert!(
            error.starts_with("plugins[0].artifact_digest:") && error.contains("commit"),
            "{source}: {error}"
        );
    }

    let error = built_pin(
        &format!("git+https://example.com/graph.git#{COMMIT}"),
        "abc",
    )
    .validate()
    .unwrap_err();
    assert!(error.starts_with("plugins[0].artifact_digest:"), "{error}");
}
