//! Security invariant: only the host path and a root-owned bundled binary are
//! ever executed as the Bubblewrap wrapper. A copy any unprivileged principal
//! could have written — the sandboxed agent included — is refused before
//! spawn, which the spawn boundary cannot demonstrate without a root-owned
//! fixture.

use super::super::wrapper::{HOST_BWRAP_PATH, WrapperEntry, ownership_refusal, trusted_wrapper_at};

#[test]
fn only_root_owned_unwritable_non_setuid_entries_qualify() {
    use WrapperEntry::{Directory, File};
    for (expected, kind_matches, uid, mode, refusal) in [
        (File, true, 0, 0o755, None),
        (File, true, 0, 0o555, None),
        (File, false, 0, 0o755, Some("is not a regular file")),
        (File, true, 1000, 0o755, Some("is not owned by root")),
        (File, true, 0, 0o775, Some("is writable by group or others")),
        (File, true, 0, 0o757, Some("is writable by group or others")),
        (File, true, 0, 0o4755, Some("is setuid or setgid")),
        (File, true, 0, 0o2755, Some("is setuid or setgid")),
        (File, true, 0, 0o644, Some("is not executable")),
        (Directory, true, 0, 0o755, None),
        (Directory, false, 0, 0o755, Some("is not a real directory")),
        (Directory, true, 1000, 0o700, Some("is not owned by root")),
        (
            Directory,
            true,
            0,
            0o1777,
            Some("is writable by group or others"),
        ),
        (
            Directory,
            true,
            0,
            0o775,
            Some("is writable by group or others"),
        ),
    ] {
        assert_eq!(
            ownership_refusal(expected, kind_matches, uid, mode),
            refusal,
            "{expected:?} kind_matches={kind_matches} uid={uid} mode={mode:o}"
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn a_user_writable_or_symlinked_bundled_copy_is_refused_before_spawn() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical tempdir");

    // A byte-identical copy of a root-owned binary is still refused: what
    // makes the wrapper trustworthy is that no unprivileged process can
    // replace it, not its contents at the time of the check.
    let copy = root.join("bwrap");
    std::fs::copy("/usr/bin/true", &copy).expect("copy a system binary");
    let error = trusted_wrapper_at(&copy.display().to_string(), &copy)
        .expect_err("a user-owned bundled copy must be refused")
        .to_string();
    assert!(error.contains("is not owned by root"), "{error}");

    let link_dir = root.join("link");
    std::fs::create_dir(&link_dir).expect("link directory");
    let link = link_dir.join("bwrap");
    std::os::unix::fs::symlink("/usr/bin/true", &link).expect("symlink");
    let error = trusted_wrapper_at(&link.display().to_string(), &link)
        .expect_err("a symlinked bundled path must be refused")
        .to_string();
    assert!(error.contains("is not a regular file"), "{error}");
}

#[test]
fn only_the_two_fixed_paths_are_trusted_wrappers() {
    let bundled = std::path::Path::new("/usr/local/libexec/orbit/bwrap");
    assert_eq!(
        trusted_wrapper_at(HOST_BWRAP_PATH, bundled).expect("host path"),
        std::path::Path::new(HOST_BWRAP_PATH)
    );
    for other in [
        "/usr/local/bin/bwrap",
        "bwrap",
        "/tmp/bwrap",
        "/usr/bin/../bin/bwrap",
        "/usr/local/libexec/orbit/../orbit/bwrap",
    ] {
        let error = trusted_wrapper_at(other, bundled)
            .expect_err("any other wrapper path must be refused")
            .to_string();
        assert!(
            error.contains("refusing untrusted Bubblewrap wrapper"),
            "{error}"
        );
    }
}
