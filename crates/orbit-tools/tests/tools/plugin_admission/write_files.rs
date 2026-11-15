//! Exact manifest file capabilities at the public profile/spawn boundary.

use super::support::scoped_spec;
use super::*;
use orbit_types::plugin::parse_grants;

fn fixture_spec(root: &Path, file: &Path) -> PluginBackendSpec {
    let plugin = root.join("plugin");
    std::fs::create_dir_all(&plugin).unwrap();
    #[cfg(unix)]
    harden_dir(&plugin);
    let mut permissions = PluginPermissions::default();
    permissions.fs.write_files = vec![file.to_string_lossy().into_owned()];
    (*spec(
        plugin.join("backend"),
        &plugin,
        permissions,
        &[PluginGrant::Fs],
    ))
    .clone()
}

#[cfg(unix)]
fn harden_dir(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .unwrap_or_else(|error| panic!("chmod fixture dir {}: {error}", path.display()));
}

#[cfg(unix)]
fn write_owner_file(path: &Path, content: impl AsRef<[u8]>) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, content)
        .unwrap_or_else(|error| panic!("write fixture file {}: {error}", path.display()));
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .unwrap_or_else(|error| panic!("chmod fixture file {}: {error}", path.display()));
}

#[cfg(unix)]
fn fixture_tempdir() -> tempfile::TempDir {
    let temp = match std::env::var_os("ORBIT_TEST_EXACT_FILE_PARENT").filter(|s| !s.is_empty()) {
        Some(parent) => tempfile::tempdir_in(parent).expect("create fixture tempdir"),
        None => tempfile::tempdir().expect("create fixture tempdir"),
    };
    harden_dir(temp.path());
    temp
}

#[cfg(unix)]
#[test]
fn exact_file_grants_do_not_open_parents_siblings_or_descendants() {
    let temp = fixture_tempdir();
    let root = temp.path().canonicalize().unwrap();
    harden_dir(&root);
    let file = root.join("installation_id");
    write_owner_file(&file, "synthetic-id");
    let spec = fixture_spec(&root, &file);
    let profile = spec
        .sandbox_profile(None)
        .expect("existing file capability");
    assert!(profile.write.is_empty());
    assert_eq!(profile.write_files.as_slice(), std::slice::from_ref(&file));
    assert_eq!(
        profile.macos_fs_rules().modify,
        [file.display().to_string()]
    );

    for (allowed, kept) in [
        (root.clone(), true),
        (file.clone(), true),
        (root.join("auth.json"), false),
        (file.join("child"), false),
    ] {
        let grants = parse_grants(&[format!("fs={}", allowed.display())]).unwrap();
        let scoped = scoped_spec(
            spec.command.clone(),
            &spec.plugin_root,
            spec.permissions.clone(),
            grants,
        );
        let profile = scoped.sandbox_profile(None).unwrap();
        assert_eq!(
            !profile.write_files.is_empty(),
            kept,
            "{}",
            allowed.display()
        );
        assert!(profile.write.is_empty());
    }
    let mut protected = spec.clone();
    protected.permissions.fs.write_files =
        vec![spec.plugin_root.join("backend").display().to_string()];
    assert!(
        protected
            .sandbox_profile(None)
            .unwrap_err()
            .to_string()
            .contains("plugin install root")
    );
}

/// [ORB-24211] The macOS compiler reads a granted path containing `*` or `?`
/// as a glob, so `grant?` would also allow the sibling `grantX`. Admission must
/// refuse such a file or ancestor before any profile reaches the compiler, and
/// an ordinary name must still compile to a rule for that one file only.
#[cfg(unix)]
#[test]
fn exact_file_glob_metacharacters_are_refused_before_the_macos_compiler() {
    let temp = fixture_tempdir();
    let root = temp.path().canonicalize().unwrap();
    harden_dir(&root);

    let glob_file = root.join("grant?");
    write_owner_file(&glob_file, "synthetic");
    let star_file = root.join("grant*");
    write_owner_file(&star_file, "synthetic");
    let glob_dir = root.join("dir*");
    std::fs::create_dir(&glob_dir).unwrap();
    harden_dir(&glob_dir);
    let nested = glob_dir.join("installation_id");
    write_owner_file(&nested, "synthetic");
    for refused in [glob_file, star_file, nested] {
        let error = fixture_spec(&root, &refused)
            .sandbox_profile(None)
            .unwrap_err();
        // Name the reason, so an unrelated ancestor refusal cannot pass this check.
        assert!(
            matches!(error, orbit_common::OrbitError::PolicyDenied(_))
                && error.to_string().contains("`*` or `?`"),
            "{} must be refused for its glob metacharacters [ORB-24211]: {error}",
            refused.display()
        );
    }

    let ordinary = root.join("installation_id");
    write_owner_file(&ordinary, "synthetic-id");
    let profile = fixture_spec(&root, &ordinary)
        .sandbox_profile(None)
        .unwrap();
    let sbpl = orbit_exec::compile_macos_sandbox_profile(&profile.macos_fs_rules(), "plugin")
        .expect("compile SBPL");
    let file_rule = format!("(allow file-write* (subpath \"{}\"))", ordinary.display());
    assert!(
        sbpl.lines().any(|line| line == file_rule),
        "an ordinary exact file compiles to a rule for that file: {sbpl}"
    );
}

#[cfg(unix)]
#[test]
fn unsafe_exact_files_are_refused_and_changed_files_are_revalidated_before_spawn() {
    use orbit_exec::{EnvironmentMode, ExecRequest, StdinMode, run_process};
    use std::os::unix::fs::{PermissionsExt, symlink};
    let temp = fixture_tempdir();
    let root = temp.path().canonicalize().unwrap();
    harden_dir(&root);
    let file = root.join("runtime-file");
    write_owner_file(&file, "original");
    let mut spec = fixture_spec(&root, &file);
    let profile = spec.sandbox_profile(None).unwrap();
    let alias = root.join("alias");
    symlink(&file, &alias).unwrap();
    let linked_parent = root.join("linked-parent");
    symlink(&root, &linked_parent).unwrap();
    let hardlink = root.join("hardlink");
    std::fs::hard_link(&file, &hardlink).unwrap();
    for unsafe_path in [
        file.clone(),
        alias,
        linked_parent.join("runtime-file"),
        root.join("missing"),
        root.join("plugin/../runtime-file"),
        root.clone(),
    ] {
        spec.permissions.fs.write_files = vec![unsafe_path.display().to_string()];
        let error = spec.sandbox_profile(None).unwrap_err();
        assert!(
            matches!(error, orbit_common::OrbitError::PolicyDenied(_)),
            "{unsafe_path:?}: {error}"
        );
    }
    // A foreign-owned system file is inspected only by the admission path:
    // no contents are read or changed. Root has no foreign owner here.
    use std::os::unix::fs::MetadataExt;
    let foreign = Path::new("/etc/hosts");
    if let Ok(metadata) = std::fs::symlink_metadata(foreign) {
        // SAFETY: geteuid has no preconditions.
        if metadata.uid() != unsafe { libc::geteuid() } {
            spec.permissions.fs.write_files = vec![foreign.display().to_string()];
            assert!(spec.sandbox_profile(None).is_err());
        }
    }
    std::fs::remove_file(hardlink).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o666)).unwrap();
    spec.permissions.fs.write_files = vec![file.display().to_string()];
    assert!(spec.sandbox_profile(None).is_err());
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let req = ExecRequest {
        program: "/bin/sh".into(),
        args: vec!["-c".into(), "exit 99".into()],
        current_dir: None,
        timeout_ms: Some(1000),
        stdin_mode: StdinMode::Null,
        environment_mode: EnvironmentMode::ClearAndSet(vec![]),
        debug: false,
    };
    std::fs::remove_file(&file).unwrap();
    assert!(
        run_process(&req, &profile)
            .unwrap_err()
            .to_string()
            .contains("write_files")
    );
    symlink(root.join("uncreated-target"), &file).unwrap();
    assert!(
        run_process(&req, &profile)
            .unwrap_err()
            .to_string()
            .contains("write_files")
    );
    assert!(!root.join("uncreated-target").exists());
}

/// macOS permits scratch writes, so the native boundary needs a fixture outside
/// OS scratch. The parent can be overridden for native replay.
#[cfg(unix)]
#[test]
fn a_native_exact_file_write_leaves_sibling_and_auth_sentinels_read_only() {
    use orbit_exec::{EnvironmentMode, ExecRequest, StdinMode, run_process};
    if !orbit_exec::macos_sandbox_test_guard(
        "a_native_exact_file_write_leaves_sibling_and_auth_sentinels_read_only",
    ) {
        return;
    }
    #[cfg(target_os = "macos")]
    let temp = {
        let parent = std::env::var_os("ORBIT_TEST_EXACT_FILE_PARENT")
            .filter(|s| !s.is_empty())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf());
        let temp = tempfile::tempdir_in(parent).expect("create native fixture tempdir");
        harden_dir(temp.path());
        temp
    };
    #[cfg(not(target_os = "macos"))]
    let temp = fixture_tempdir();
    let root = temp.path().canonicalize().unwrap();
    harden_dir(&root);
    assert!(
        !cfg!(target_os = "macos")
            || !(root.starts_with("/private/tmp") || root.starts_with("/private/var/folders")),
        "native exact-file fixture must be outside OS scratch; set ORBIT_TEST_EXACT_FILE_PARENT"
    );
    let file = root.join("installation_id");
    let sibling = root.join("sibling");
    let auth = root.join("auth.json");
    for path in [&file, &sibling, &auth] {
        write_owner_file(path, "sentinel");
    }
    let spec = fixture_spec(&root, &file);
    let profile = spec.sandbox_profile(None).unwrap();
    let req = ExecRequest { program: "/bin/sh".into(), args: vec!["-c".into(), "printf changed > \"$1\" || exit 11; if printf forbidden > \"$2\" 2>/dev/null; then exit 12; fi; if printf forbidden > \"$3\" 2>/dev/null; then exit 13; fi".into(), "probe".into(), file.display().to_string(), sibling.display().to_string(), auth.display().to_string()], current_dir: None, timeout_ms: Some(10000), stdin_mode: StdinMode::Null, environment_mode: EnvironmentMode::ClearAndSet(vec![("PATH".into(), "/usr/bin:/bin".into())]), debug: false };
    let output = run_process(&req, &profile).expect("confined file probe");
    assert!(output.success, "{output:?}");
    assert_eq!(std::fs::read_to_string(file).unwrap(), "changed");
    assert_eq!(std::fs::read_to_string(sibling).unwrap(), "sentinel");
    assert_eq!(std::fs::read_to_string(auth).unwrap(), "sentinel");
}
