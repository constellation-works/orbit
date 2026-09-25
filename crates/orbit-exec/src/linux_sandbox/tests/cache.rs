use super::*;

/// [ORB-12469] The read-only bind of `/` makes the shared Cargo download
/// caches immutable, which kills any build whose lockfile names a crate the
/// host has not cached yet. Pin the mount set: the two cache subtrees and the
/// package-cache locks, and nothing else under `$CARGO_HOME`.
#[test]
fn cargo_cache_mounts_bind_the_download_caches_and_locks_only() {
    let cargo_home = tempfile::tempdir().expect("tempdir");
    let root = cargo_home.path();
    for dir in ["registry", "git", "bin"] {
        fs::create_dir_all(root.join(dir)).expect("create dir");
    }
    for file in [
        ".package-cache",
        ".package-cache-mutate",
        "credentials.toml",
        ".global-cache",
    ] {
        fs::write(root.join(file), b"").expect("write file");
    }

    let mut args = Vec::new();
    append_cargo_download_cache_mounts(&mut args, Some(root));

    let bound: Vec<&str> = args.chunks(3).map(|mount| mount[1].as_str()).collect();
    let expected: Vec<String> = ["registry", "git", ".package-cache", ".package-cache-mutate"]
        .iter()
        .map(|relative| root.join(relative).display().to_string())
        .collect();
    assert_eq!(
        bound, expected,
        "cargo cache mounts must be exactly the download caches and their locks: {args:?}"
    );
    for mount in args.chunks(3) {
        assert_eq!(
            mount[0], "--bind",
            "cargo caches must be writable: {args:?}"
        );
        assert_eq!(
            mount[1], mount[2],
            "a cargo cache keeps its host path inside the namespace: {args:?}"
        );
    }
}

/// An absent cache is skipped rather than created on the host: Bubblewrap
/// cannot bind a missing source, and a host with no cargo home at all emits no
/// mount instead of a bind that would fail the spawn.
#[test]
fn cargo_cache_mounts_skip_absent_paths_and_an_unresolved_cargo_home() {
    let cargo_home = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(cargo_home.path().join("registry")).expect("create dir");

    let mut args = Vec::new();
    append_cargo_download_cache_mounts(&mut args, Some(cargo_home.path()));
    assert_eq!(
        args,
        vec![
            "--bind".to_string(),
            cargo_home.path().join("registry").display().to_string(),
            cargo_home.path().join("registry").display().to_string(),
        ],
        "only the paths that exist are bound"
    );
    assert!(
        !cargo_home.path().join("git").exists(),
        "an absent cache must not be created on the host"
    );

    let mut none = Vec::new();
    append_cargo_download_cache_mounts(&mut none, None);
    assert!(
        none.is_empty(),
        "an unresolved cargo home emits no mount: {none:?}"
    );
}

/// The caches are a build convenience: a write-capable profile gets them, and a
/// profile whose `modify` rules are all negated keeps a fully immutable host.
#[test]
fn cargo_cache_mounts_reach_write_capable_profiles_only() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("create workspace");

    let mut expected = Vec::new();
    append_cargo_download_cache_mounts(&mut expected, cargo_home_dir().as_deref());

    let writer = profile(vec![format!("{}/**", workspace.display())]);
    let plan = compile_linux_bwrap_argv(&writer, "/bin/true", &[], None, false)
        .expect("compile write-capable profile");
    let joined = plan.args.join(" ");
    for mount in expected.chunks(3) {
        assert!(
            joined.contains(&mount.join(" ")),
            "write-capable profile is missing cargo cache mount {mount:?}: {joined}"
        );
    }

    let reader = profile(vec![format!("!{}/**/.env", workspace.display())]);
    let plan = compile_linux_bwrap_argv(&reader, "/bin/true", &[], None, false)
        .expect("compile read-only profile");
    assert!(
        !plan.args.iter().any(|arg| arg == "--bind"),
        "a read-only profile must gain no writable bind: {:?}",
        plan.args
    );
}
