use super::super::super::build::{install_plugin_build_outputs, plugin_artifact_digest};
use super::spec;

#[cfg(unix)]
fn build_dir_with(files: &[(&str, &[u8], u32)]) -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("build dir");
    for (path, bytes, mode) in files {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        std::fs::write(&path, bytes).expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(*mode)).expect("chmod");
    }
    dir
}

/// §3.4: only a declared output that is a physical, singly linked regular
/// file inside the build directory crosses back, it never replaces a file
/// the reviewed tree ships, and its installed mode keeps only the
/// owner-execute bit.
#[cfg(unix)]
#[test]
fn only_physical_declared_outputs_cross_back_into_the_install() {
    use std::os::unix::fs::PermissionsExt;

    let build = build_dir_with(&[
        ("target/backend", b"#!/bin/sh\n", 0o4775),
        ("target/data.json", b"{}", 0o600),
        ("plugin.yaml", b"hostile", 0o644),
    ]);
    let secret = tempfile::tempdir().expect("outside");
    std::fs::write(secret.path().join("id_ed25519"), "key").expect("secret");
    std::os::unix::fs::symlink(
        secret.path().join("id_ed25519"),
        build.path().join("linked"),
    )
    .expect("link");
    std::os::unix::fs::symlink(secret.path(), build.path().join("linked_dir")).expect("link dir");
    std::fs::hard_link(build.path().join("plugin.yaml"), build.path().join("hard"))
        .expect("hard link");
    let fifo = std::ffi::CString::new(build.path().join("fifo").as_os_str().as_encoded_bytes())
        .expect("FIFO path");
    // SAFETY: the path is NUL terminated and inside this disposable fixture.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);

    let refused = [
        ("linked", "bin/x", "symbolic link"),
        ("linked_dir/id_ed25519", "bin/x", "symbolic link"),
        ("hard", "bin/x", "hard link"),
        ("fifo", "bin/x", "not a regular file"),
        ("../outside", "bin/x", "plain relative path"),
        ("missing", "bin/x", "was not produced"),
        ("target", "bin/x", "not a regular file"),
        ("target/backend", "shipped.txt", "would replace"),
    ];
    for (from, to, reason) in refused {
        let staging = tempfile::tempdir().expect("staging");
        std::fs::write(staging.path().join("shipped.txt"), "reviewed").expect("pristine");
        let error = install_plugin_build_outputs(
            build.path(),
            &spec(&[(from, to)]).outputs,
            staging.path(),
        )
        .expect_err(&format!("{from} -> {to} must be refused"));
        assert!(
            error.to_string().contains(reason),
            "{from} -> {to}: expected '{reason}' in: {error}"
        );
        assert!(
            !staging.path().join(to).exists() || to == "shipped.txt",
            "nothing is installed from a refused output"
        );
        assert_eq!(
            std::fs::read_to_string(staging.path().join("shipped.txt")).expect("pristine"),
            "reviewed"
        );
    }

    let outputs = [
        ("target/data.json", "share/data.json"),
        ("target/backend", "bin/backend"),
    ];
    let staging = tempfile::tempdir().expect("staging");
    let records =
        install_plugin_build_outputs(build.path(), &spec(&outputs).outputs, staging.path())
            .expect("install outputs");
    let mode = |path: &str| {
        std::fs::metadata(staging.path().join(path))
            .expect("installed")
            .permissions()
            .mode()
            & 0o7777
    };
    assert_eq!(
        mode("bin/backend"),
        0o755,
        "setuid is cleared, owner-execute kept"
    );
    assert_eq!(mode("share/data.json"), 0o644);
    assert_eq!(
        records.iter().map(|record| record.mode).collect::<Vec<_>>(),
        vec![0o644, 0o755]
    );

    // The digest is a function of the installed bytes and modes only.
    let reversed: Vec<(&str, &str)> = outputs.iter().rev().copied().collect();
    let again = tempfile::tempdir().expect("staging");
    let reordered =
        install_plugin_build_outputs(build.path(), &spec(&reversed).outputs, again.path())
            .expect("install outputs");
    assert_eq!(
        plugin_artifact_digest(&records),
        plugin_artifact_digest(&reordered)
    );
}

/// The total output cap rejects an oversized regular file before reading
/// its bytes; a sparse hostile artifact cannot make installation unbounded.
#[cfg(unix)]
#[test]
fn oversized_build_outputs_are_refused_before_copying() {
    let build = tempfile::tempdir().expect("build dir");
    let oversized = std::fs::File::create(build.path().join("large")).expect("output");
    oversized
        .set_len(super::super::super::source::MAX_UNPACKED_BYTES + 1)
        .expect("sparse output");
    let staging = tempfile::tempdir().expect("staging");
    let error = install_plugin_build_outputs(
        build.path(),
        &spec(&[("large", "bin/out")]).outputs,
        staging.path(),
    )
    .expect_err("the output cap must bound the copy");
    assert!(matches!(error, orbit_common::OrbitError::PolicyDenied(_)));
    assert!(!staging.path().join("bin/out").exists());
}
