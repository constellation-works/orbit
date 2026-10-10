#[cfg(unix)]
#[test]
fn unreadable_glob_subtree_does_not_abort_profile_and_sibling_match_stays_pinned() {
    use std::os::unix::fs::PermissionsExt;

    use orbit_exec::compile_macos_sandbox_profile;
    use orbit_types::policy::ResolvedFsProfile;

    // SAFETY: geteuid only reads the current process's effective uid.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }

    let fixture = tempfile::tempdir().expect("fixture tempdir");
    let workspace = fixture.path().join("workspace");
    let unreadable = workspace.join("unreadable");
    let sibling = workspace.join("sibling");
    std::fs::create_dir_all(&unreadable).expect("unreadable directory");
    std::fs::create_dir_all(&sibling).expect("sibling directory");
    std::fs::write(sibling.join(".env"), b"fixture").expect("sibling glob match");
    std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000))
        .expect("make directory unreadable");

    let workspace_text = workspace.display().to_string();
    let rules = ResolvedFsProfile {
        name: "unreadable-glob-fixture".to_string(),
        read: Vec::new(),
        modify: vec![workspace_text.clone(), format!("!{workspace_text}/**/.env")],
    };
    let result = compile_macos_sandbox_profile(&rules, "codex");

    // Restore access before asserting so TempDir can remove the fixture even
    // when compilation fails.
    std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o700))
        .expect("restore directory permissions");
    let profile = result.expect("unreadable subtree must not fail profile compilation");
    let sibling_pin = format!("(deny file-write* (literal \"{}\"))", sibling.display());
    assert!(
        profile.contains(&sibling_pin),
        "a readable sibling glob match must remain pinned: {sibling_pin}"
    );
}
