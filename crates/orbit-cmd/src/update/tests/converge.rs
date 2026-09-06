use crate::update::converge::resolve_installed_executable;

#[test]
fn a_real_file_named_deleted_is_left_alone() {
    let dir = tempfile::tempdir().expect("fixture root");
    let executable = dir.path().join("orbit (deleted)");
    std::fs::write(&executable, "keep").expect("write real deleted-named file");

    assert_eq!(resolve_installed_executable(&executable), executable);
}

#[cfg(target_os = "linux")]
#[test]
fn a_missing_deleted_inode_path_resolves_to_the_live_install() {
    let dir = tempfile::tempdir().expect("fixture root");
    let installed = dir.path().join("orbit");
    std::fs::write(&installed, "replacement").expect("write live install");
    let deleted = installed.with_file_name("orbit (deleted)");

    assert!(
        !deleted.exists(),
        "the kernel-style deleted-inode pseudo-path must be absent"
    );
    assert_eq!(resolve_installed_executable(&deleted), installed);
}
