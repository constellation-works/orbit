use std::ffi::OsString;
use std::path::PathBuf;

use crate::credential_paths::{CredentialReadDeny, credential_read_denies};

fn paths(denies: &[CredentialReadDeny], file: bool) -> Vec<PathBuf> {
    denies
        .iter()
        .filter(|deny| deny.file == file)
        .map(|deny| deny.path.clone())
        .collect()
}

#[test]
fn home_credential_trees_are_denied_as_subtrees() {
    let home = OsString::from("/home/worker");
    let denies = credential_read_denies(Some(&home), None);
    let trees = paths(&denies, false);
    for expected in [
        "/home/worker/.ssh",
        "/home/worker/.aws",
        "/home/worker/.config/gh",
    ] {
        assert!(
            trees.contains(&PathBuf::from(expected)),
            "`{expected}` must be a denied credential tree: {trees:?}"
        );
    }
}

#[test]
fn cargo_publish_tokens_follow_cargo_home_and_default_to_home() {
    let home = OsString::from("/home/worker");
    let defaulted = credential_read_denies(Some(&home), None);
    assert_eq!(
        paths(&defaulted, true),
        vec![
            PathBuf::from("/home/worker/.cargo/credentials"),
            PathBuf::from("/home/worker/.cargo/credentials.toml"),
        ],
        "both spellings of the token are denied as files under the default CARGO_HOME"
    );

    let cargo_home = OsString::from("/opt/cargo");
    let overridden = credential_read_denies(Some(&home), Some(&cargo_home));
    assert_eq!(
        paths(&overridden, true),
        vec![
            PathBuf::from("/opt/cargo/credentials"),
            PathBuf::from("/opt/cargo/credentials.toml"),
        ]
    );
}

#[test]
fn an_unset_or_empty_home_yields_no_home_relative_entries() {
    let empty = OsString::new();
    for home in [None, Some(&empty)] {
        let denies = credential_read_denies(home.map(OsString::as_os_str), None);
        assert!(
            paths(&denies, true).is_empty(),
            "no cargo token path can be derived without HOME or CARGO_HOME: {denies:?}"
        );
        assert!(
            denies
                .iter()
                .all(|deny| deny.path.is_absolute() && !deny.path.ends_with(".ssh")),
            "only system-wide entries remain: {denies:?}"
        );
    }
}
