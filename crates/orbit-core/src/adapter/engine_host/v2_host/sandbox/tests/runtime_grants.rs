/// Runtime stores are reached by joining constant segments onto an already
/// canonical runtime root, so validating the root says nothing about them.
/// These cover that descendant boundary and the two grants built on it.
#[cfg(target_os = "linux")]
mod runtime_store_grants {
    use std::os::unix::fs::symlink;
    use std::path::Path;

    use orbit_types::policy::ResolvedFsProfile;
    use rusqlite::Connection;

    use crate::adapter::engine_host::v2_host::sandbox::runtime_grants::{
        append_runtime_directory_grant, append_runtime_sidecar_grant, append_runtime_sqlite_grants,
    };
    use crate::adapter::engine_host::v2_host::sandbox::runtime_paths::{
        open_or_create_runtime_directory, validated_linux_runtime_descendant,
    };

    fn canonical_root(root: &Path) -> std::path::PathBuf {
        root.canonicalize().expect("canonical runtime root")
    }

    fn empty_profile() -> ResolvedFsProfile {
        ResolvedFsProfile {
            name: "test".to_string(),
            read: Vec::new(),
            modify: Vec::new(),
        }
    }

    #[test]
    fn accepts_a_store_that_stays_under_its_root() {
        let root = tempfile::tempdir().expect("runtime root");
        let root = canonical_root(root.path());
        std::fs::create_dir_all(root.join("state/logs")).expect("create store");

        let resolved = validated_linux_runtime_descendant(&root, "state/logs")
            .expect("validate store")
            .expect("a store inside the root is grantable");

        assert_eq!(resolved, root.join("state/logs"));
    }

    #[test]
    fn accepts_a_store_that_has_not_been_created_yet_without_creating_it() {
        let root = tempfile::tempdir().expect("runtime root");
        let root = canonical_root(root.path());

        let resolved = validated_linux_runtime_descendant(&root, "state/logs")
            .expect("validate store")
            .expect("a store that has never been created is still grantable");

        assert_eq!(resolved, root.join("state/logs"));
        assert!(
            !root.join("state").exists(),
            "validation alone must not create the store"
        );
    }

    /// Relocating a store behind a symlink is an ordinary host configuration,
    /// so an alias that still lands inside the root keeps its grant — reported
    /// at the real location rather than at the link name. [ORB-11984]
    #[test]
    fn resolves_an_alias_that_still_lands_inside_the_root() {
        let root = tempfile::tempdir().expect("runtime root");
        let root = canonical_root(root.path());
        std::fs::create_dir_all(root.join("real-tasks")).expect("create real store");
        symlink(root.join("real-tasks"), root.join("tasks")).expect("alias the store");

        let resolved = validated_linux_runtime_descendant(&root, "tasks")
            .expect("validate store")
            .expect("an in-root alias stays grantable");

        assert_eq!(resolved, root.join("real-tasks"));
    }

    #[test]
    fn rejects_a_store_redirected_outside_the_root() {
        let root = tempfile::tempdir().expect("runtime root");
        let outside = tempfile::tempdir().expect("redirect target");
        let root = canonical_root(root.path());
        symlink(outside.path(), root.join("state")).expect("redirect the state store");

        assert_eq!(
            validated_linux_runtime_descendant(&root, "state/logs").expect("validate store"),
            None
        );
    }

    #[test]
    fn rejects_a_store_that_traverses_or_escapes_the_root() {
        let parent = tempfile::tempdir().expect("runtime parent");
        let root = parent.path().join("root");
        std::fs::create_dir_all(&root).expect("create runtime root");
        let root = canonical_root(&root);

        for relative in ["../sibling", "state/../../sibling", "/etc"] {
            assert_eq!(
                validated_linux_runtime_descendant(&root, relative).expect("validate store"),
                None,
                "`{relative}` must not resolve to a grant"
            );
        }
    }

    #[test]
    fn directory_grant_creates_and_grants_a_store_inside_the_root() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().expect("runtime root");
        let root = canonical_root(root.path());
        let mut profile = empty_profile();

        let mut authority = Vec::new();
        append_runtime_directory_grant(&root, "state/logs", &mut profile, &mut authority)
            .expect("grant store");

        assert!(
            root.join("state/logs").is_dir(),
            "the store must be created"
        );
        assert_eq!(
            profile.modify,
            vec![root.join("state/logs").display().to_string()]
        );
        assert_eq!(authority[0].path, root.join("state/logs"));
        for directory in [root.join("state"), root.join("state/logs")] {
            let mode = std::fs::metadata(&directory)
                .expect("runtime store metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o700, "{} has mode {mode:04o}", directory.display());
        }
    }

    /// A store whose parent is redirected out of the runtime root must not be
    /// created at the redirect target and must not become a writable grant:
    /// either one would hand a sandboxed leaf a host path the profile never
    /// authorized.
    #[test]
    fn directory_grant_neither_creates_nor_grants_a_redirected_store() {
        let root = tempfile::tempdir().expect("runtime root");
        let outside = tempfile::tempdir().expect("redirect target");
        let root = canonical_root(root.path());
        let outside = canonical_root(outside.path());
        symlink(&outside, root.join("state")).expect("redirect the state store");
        let mut profile = empty_profile();

        let mut authority = Vec::new();
        append_runtime_directory_grant(&root, "state/logs", &mut profile, &mut authority)
            .expect("a redirected store is skipped, not a dispatch failure");

        assert!(profile.modify.is_empty(), "grants: {:?}", profile.modify);
        assert!(authority.is_empty());
        assert!(
            !outside.join("logs").exists(),
            "the redirect target must be left untouched"
        );
    }

    #[test]
    fn directory_creation_rejects_parent_replaced_after_validation() {
        let root = tempfile::tempdir().expect("runtime root");
        let outside = tempfile::tempdir().expect("outside");
        let root = canonical_root(root.path());
        std::fs::create_dir(root.join("state")).expect("state");
        let validated = validated_linux_runtime_descendant(&root, "state/logs")
            .expect("validate")
            .expect("in-root path");
        std::fs::remove_dir(root.join("state")).expect("remove state");
        symlink(outside.path(), root.join("state")).expect("replace state");

        let error = open_or_create_runtime_directory(&root, &validated)
            .expect_err("descriptor walk must reject replacement");

        assert!(error.to_string().contains("without following links"));
        assert!(!outside.path().join("logs").exists());
    }

    #[test]
    fn sidecar_grant_covers_an_existing_regular_file_and_skips_a_missing_one() {
        let root = tempfile::tempdir().expect("runtime root");
        let root = canonical_root(root.path());
        std::fs::write(root.join("orbit.db-wal"), b"").expect("create sidecar");
        let mut profile = empty_profile();

        let mut authority = Vec::new();
        append_runtime_sidecar_grant(&root, "orbit.db-wal", &mut profile, &mut authority)
            .expect("grant sidecar");
        append_runtime_sidecar_grant(&root, "orbit.db-shm", &mut profile, &mut authority)
            .expect("skip sidecar");

        assert_eq!(
            profile.modify,
            vec![root.join("orbit.db-wal").display().to_string()]
        );
        assert_eq!(authority[0].path, root.join("orbit.db-wal"));
    }

    #[test]
    fn sqlite_grants_resolve_an_in_root_database_alias_before_leasing() {
        let root = tempfile::tempdir().expect("runtime root");
        let root = canonical_root(root.path());
        let database = root.join("real-orbit.db");
        let writer = Connection::open(&database).expect("create database");
        writer
            .pragma_update(None, "journal_mode", "WAL")
            .expect("enable WAL");
        writer
            .execute_batch("CREATE TABLE fixture(value INTEGER); INSERT INTO fixture VALUES (1);")
            .expect("seed database");
        symlink(&database, root.join("orbit.db")).expect("alias database inside root");
        let mut profile = empty_profile();
        let mut authority = Vec::new();

        append_runtime_sqlite_grants(&root, "orbit.db", &mut profile, &mut authority)
            .expect("grant canonical database file set");

        let expected_paths = [
            database.clone(),
            root.join("real-orbit.db-wal"),
            root.join("real-orbit.db-shm"),
        ];
        assert_eq!(
            authority
                .iter()
                .map(|grant| grant.path.clone())
                .collect::<Vec<_>>(),
            expected_paths
        );
        assert_eq!(
            profile.modify,
            expected_paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
        );
        let lease = authority[0]
            .wal_file_set_lease
            .as_ref()
            .expect("database lease");
        assert!(authority.iter().all(|grant| {
            std::sync::Arc::ptr_eq(
                lease,
                grant.wal_file_set_lease.as_ref().expect("sidecar lease"),
            )
        }));

        drop(writer);
        assert!(
            expected_paths[1..].iter().all(|path| path.exists()),
            "the shared lease must keep both canonical sidecars linked"
        );
    }

    #[test]
    fn sqlite_grants_reject_an_escaping_alias_without_touching_its_target() {
        let root = tempfile::tempdir().expect("runtime root");
        let outside = tempfile::tempdir().expect("outside");
        let root = canonical_root(root.path());
        let target = canonical_root(outside.path()).join("external.db");
        std::fs::write(&target, b"not an Orbit database").expect("create external target");
        symlink(&target, root.join("orbit.db")).expect("redirect database outside root");
        let mut profile = empty_profile();
        let mut authority = Vec::new();

        append_runtime_sqlite_grants(&root, "orbit.db", &mut profile, &mut authority)
            .expect("an escaping database is skipped before SQLite opens it");

        assert!(profile.modify.is_empty(), "grants: {:?}", profile.modify);
        assert!(authority.is_empty());
        assert_eq!(
            std::fs::read(&target).expect("read external target"),
            b"not an Orbit database"
        );
        assert!(!outside.path().join("external.db-wal").exists());
        assert!(!outside.path().join("external.db-shm").exists());
    }

    /// SQLite writes its sidecars next to the database it opens, so a sidecar
    /// that is a symlink is never Orbit's own file. Granting one would bind the
    /// link's target into the sandbox as writable.
    #[test]
    fn sidecar_grant_skips_a_sidecar_symlinked_outside_the_root() {
        let root = tempfile::tempdir().expect("runtime root");
        let outside = tempfile::tempdir().expect("redirect target");
        let root = canonical_root(root.path());
        let target = canonical_root(outside.path()).join("credentials");
        std::fs::write(&target, b"secret").expect("create redirect target");
        symlink(&target, root.join("orbit.db-wal")).expect("redirect the sidecar");
        let mut profile = empty_profile();

        let mut authority = Vec::new();
        append_runtime_sidecar_grant(&root, "orbit.db-wal", &mut profile, &mut authority)
            .expect("a redirected sidecar is skipped, not a dispatch failure");

        assert!(profile.modify.is_empty(), "grants: {:?}", profile.modify);
        assert!(authority.is_empty());
    }

    #[test]
    fn sidecar_grant_skips_a_dangling_sidecar_symlink() {
        let root = tempfile::tempdir().expect("runtime root");
        let root = canonical_root(root.path());
        symlink(root.join("never-created"), root.join("orbit.db-wal")).expect("dangling sidecar");
        let mut profile = empty_profile();

        let mut authority = Vec::new();
        append_runtime_sidecar_grant(&root, "orbit.db-wal", &mut profile, &mut authority)
            .expect("a dangling sidecar is skipped, not a dispatch failure");

        assert!(profile.modify.is_empty(), "grants: {:?}", profile.modify);
        assert!(authority.is_empty());
    }

    #[test]
    fn sidecar_grant_skips_a_nonregular_object() {
        let root = tempfile::tempdir().expect("runtime root");
        let root = canonical_root(root.path());
        std::fs::create_dir(root.join("orbit.db-wal")).expect("nonregular sidecar");
        let mut profile = empty_profile();
        let mut authority = Vec::new();

        append_runtime_sidecar_grant(&root, "orbit.db-wal", &mut profile, &mut authority)
            .expect("a nonregular sidecar is skipped");

        assert!(profile.modify.is_empty());
        assert!(authority.is_empty());
    }
}
