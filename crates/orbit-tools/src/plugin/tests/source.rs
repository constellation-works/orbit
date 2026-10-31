use std::io::Write;

use orbit_types::plugin::MANIFEST_FILE_NAME;

use super::super::loader::first_party_source;
use super::super::source::{ArchiveLimits, unpack_tar};

const MANIFEST: &str = "\
schemaVersion: 2
kind: Plugin
metadata:
  name: demo
  version: 1.0.0
  description: Fixture.
spec:
  backend:
    type: exec
    command: bin/backend.sh
  tools:
    - name: hello
      description: Hello.
      execution_kind: read_only
";

fn unpack_tar_bytes(bytes: &[u8], limits: ArchiveLimits) -> Result<tempfile::TempDir, String> {
    let dest = tempfile::tempdir().expect("tempdir");
    unpack_tar(bytes, dest.path(), "fixture.tar", limits)
        .map(|()| dest)
        .map_err(|error| error.to_string())
}

/// Append a member under a name `tar::Header::set_path` refuses to write.
///
/// `set_path` rejects `..` and absolute paths, which is exactly what a
/// hostile archive carries, so these fixtures write the name field directly.
fn append_raw_name(builder: &mut tar::Builder<Vec<u8>>, name: &str, body: &[u8]) {
    let mut header = tar::Header::new_gnu();
    header.set_size(body.len() as u64);
    header.set_mode(0o644);
    header.set_entry_type(tar::EntryType::Regular);
    let raw = header.as_mut_bytes();
    let bytes = name.as_bytes();
    assert!(
        bytes.len() < 100,
        "fixture name must fit the tar name field"
    );
    raw[..bytes.len()].copy_from_slice(bytes);
    header.set_cksum();
    builder
        .append(&header, body)
        .expect("append raw-named entry");
}

fn tar_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, body) in entries {
        append_raw_name(&mut builder, name, body);
    }
    builder.into_inner().expect("finish tar")
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(bytes).expect("gzip");
    encoder.finish().expect("finish gzip")
}

#[test]
fn a_tar_member_that_traverses_out_of_the_root_is_refused() {
    // `tar`'s own `unpack_in` silently *skips* such a member and reports
    // success, which would install a quietly incomplete tree.
    let archive = tar_with(&[
        (MANIFEST_FILE_NAME, MANIFEST.as_bytes()),
        ("../escaped.txt", b"owned"),
    ]);
    let error = unpack_tar_bytes(&archive, ArchiveLimits::DEFAULT)
        .expect_err("a traversing member must be refused");
    assert!(
        error.contains("escaped.txt") && error.contains(".."),
        "the refusal must name the entry and why: {error}"
    );
}

/// A compressed archive is bounded by what it *inflates* to, not by how many
/// bytes arrived: a few kilobytes of zeros expand without limit otherwise.
#[test]
fn a_compression_bomb_is_bounded_by_its_unpacked_size() {
    let archive = gzip(&tar_with(&[("bomb", &[0u8; 1 << 20])]));
    assert!(
        archive.len() < 8192,
        "the fixture must be small compressed, or it is not testing the bound"
    );
    let dest = tempfile::tempdir().expect("tempdir");
    let error = unpack_tar(
        flate2::read::GzDecoder::new(archive.as_slice()),
        dest.path(),
        "bomb.tar.gz",
        ArchiveLimits {
            unpacked_bytes: 4096,
            ..ArchiveLimits::DEFAULT
        },
    )
    .expect_err("an inflating archive must be refused")
    .to_string();
    assert!(
        error.contains("4096 bytes"),
        "the refusal must name the unpacked bound: {error}"
    );
}

#[test]
fn first_party_source_verifies_only_the_repository_git_would_fetch() {
    for source in [
        "git+https://github.com/constellation-works/plugin",
        "git+https://github.com/constellation-works/plugin.git#v1.0.0",
        "git+ssh://git@github.com/constellation-works/plugin.git",
        "git+git@github.com:constellation-works/plugin.git",
    ] {
        assert!(first_party_source(source), "{source} is first-party");
    }
    for source in [
        "git+https://github.com/constellation-works/../attacker/plugin",
        "git+https://github.com/constellation-works/%2e%2E/attacker/plugin",
        "git+https://github.com/constellation-works/./../attacker/plugin",
        "git+https://github.com/constellation-works//plugin",
        "git+https://github.com//constellation-works/plugin",
        "git+https://github.com/constellation-works/plugin/..",
        "git+ssh://git@github.com/constellation-works/../attacker/plugin",
        "git+ssh://git@github.com:22/constellation-works/%2e%2e/attacker/plugin",
        "git+git@github.com:constellation-works/../attacker/plugin",
        "git+git@github.com:constellation-works/%2E%2E/attacker/plugin",
        "git+git@github.com:constellation-works//plugin",
        "git+https://github.com/attacker/constellation-works",
        "git+https://example.com/constellation-works/plugin",
        // libcurl (git's https transport) ends the authority at `?`, and WHATWG
        // URL parsers end it at `\`, so the `github.com` after the delimiter is
        // not the host git contacts. A second `@` leaves the userinfo ambiguous.
        "git+https://evil.example?@github.com/constellation-works/plugin",
        "git+https://evil.example\\@github.com/constellation-works/plugin",
        "git+https://evil.example#@github.com/constellation-works/plugin",
        "git+https://a@evil.example@github.com/constellation-works/plugin",
        "git+ssh://git@evil.example?@github.com/constellation-works/plugin",
        "git+ssh://git@evil.example\\@github.com/constellation-works/plugin",
        "git+git@evil.example?@github.com:constellation-works/plugin.git",
        "git+git@evil.example\\@github.com:constellation-works/plugin.git",
    ] {
        assert!(
            !first_party_source(source),
            "{source} must not verify as first-party: the namespace is unforgeable by URL text"
        );
    }
}
