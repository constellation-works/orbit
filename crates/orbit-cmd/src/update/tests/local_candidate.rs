//! Header and manifest decisions a process test cannot enumerate: every
//! supported and refused executable shape, and every way a manifest can fail
//! to bind the bytes to the operator's expected commit.

use crate::update::local_candidate::{
    CandidateManifest, executable_target, normalize_source_commit,
};

fn elf(class: u8, data: u8, machine: u16) -> Vec<u8> {
    let mut header = vec![0x7f, b'E', b'L', b'F', class, data];
    header.resize(18, 0);
    header.extend_from_slice(&machine.to_le_bytes());
    header.resize(64, 0);
    header
}

fn macho(magic: [u8; 4], cpu: u32) -> Vec<u8> {
    let mut header = magic.to_vec();
    header.extend_from_slice(&cpu.to_le_bytes());
    header.resize(64, 0);
    header
}

#[test]
fn executable_headers_map_to_release_targets_or_refuse() {
    let cases: [(Vec<u8>, Result<&str, &str>); 9] = [
        (elf(2, 1, 62), Ok("x86_64-unknown-linux-gnu")),
        (elf(2, 1, 183), Ok("aarch64-unknown-linux-gnu")),
        (elf(1, 1, 62), Err("64-bit little-endian ELF")),
        (elf(2, 2, 62), Err("64-bit little-endian ELF")),
        (elf(2, 1, 40), Err("machine Some(40)")),
        (
            macho([0xcf, 0xfa, 0xed, 0xfe], 0x0100_0007),
            Ok("x86_64-apple-darwin"),
        ),
        (
            macho([0xcf, 0xfa, 0xed, 0xfe], 0x0100_000c),
            Ok("aarch64-apple-darwin"),
        ),
        (macho([0xca, 0xfe, 0xba, 0xbe], 2), Err("universal Mach-O")),
        (
            b"#!/bin/sh\nexit 0\n".to_vec(),
            Err("not a native executable"),
        ),
    ];
    for (header, expected) in cases {
        match (executable_target(&header), expected) {
            (Ok(actual), Ok(expected)) => assert_eq!(actual, expected),
            (Err(error), Err(needle)) => {
                assert!(error.to_string().contains(needle), "{needle}: {error}")
            }
            (actual, expected) => panic!("{header:?}: got {actual:?}, expected {expected:?}"),
        }
    }
    // A truncated header refuses rather than reading past the input.
    assert!(executable_target(&elf(2, 1, 62)[..12]).is_err());
}

#[test]
fn source_commits_must_be_full_object_ids() {
    let sha1 = "0123456789ABCDEF0123456789abcdef01234567";
    assert_eq!(
        normalize_source_commit(sha1).expect("full SHA-1"),
        sha1.to_ascii_lowercase()
    );
    normalize_source_commit(&"a".repeat(64)).expect("full SHA-256");
    for refused in ["0123456", "", &"g".repeat(40), &"a".repeat(41), "HEAD"] {
        let error = normalize_source_commit(refused).expect_err(refused);
        assert!(error.to_string().contains("full Git commit"), "{error}");
    }
}

#[test]
fn manifests_are_v1_operator_attested_documents_only() {
    let valid = serde_json::json!({
        "schema_version": 1,
        "kind": "orbit-local-candidate",
        "trust": "operator_attested",
        "source_commit": "A".repeat(40),
        "target": "x86_64-unknown-linux-gnu",
        "executable_sha256": "B".repeat(64),
    });
    let parsed = CandidateManifest::parse(valid.to_string().as_bytes()).expect("valid manifest");
    assert_eq!(parsed.source_commit, "a".repeat(40));
    assert_eq!(parsed.executable_sha256, "b".repeat(64));

    let refusals: [(&str, serde_json::Value, &str); 6] = [
        (
            "schema",
            serde_json::json!(2),
            "unsupported local-candidate manifest",
        ),
        (
            "kind",
            serde_json::json!("orbit-release"),
            "unsupported local-candidate manifest",
        ),
        (
            "trust",
            serde_json::json!("signed_release"),
            "never a signed release",
        ),
        (
            "source_commit",
            serde_json::json!("abc1234"),
            "full Git commit",
        ),
        (
            "executable_sha256",
            serde_json::json!("abc"),
            "64-character hex",
        ),
        ("extra", serde_json::json!(true), "unknown field"),
    ];
    for (field, value, needle) in refusals {
        let mut manifest = valid.clone();
        let key = if field == "schema" {
            "schema_version"
        } else {
            field
        };
        manifest[key] = value;
        let error = CandidateManifest::parse(manifest.to_string().as_bytes()).expect_err(field);
        assert!(error.to_string().contains(needle), "{field}: {error}");
    }
    let mut missing = valid;
    missing
        .as_object_mut()
        .expect("object")
        .remove("source_commit");
    assert!(CandidateManifest::parse(missing.to_string().as_bytes()).is_err());
    assert!(CandidateManifest::parse(b"not json").is_err());
}
