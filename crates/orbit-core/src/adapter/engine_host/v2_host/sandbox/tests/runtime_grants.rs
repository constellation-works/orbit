/// Runtime stores are reached by joining constant segments onto an already
/// canonical runtime root, so validating the root says nothing about them.
/// These cover that descendant boundary and the two grants built on it.
#[cfg(target_os = "linux")]
mod runtime_store_grants {
    use std::os::unix::fs::symlink;
    use std::path::Path;

    use orbit_types::policy::ResolvedFsProfile;

    use crate::adapter::engine_host::v2_host::sandbox::runtime_grants::{
        append_runtime_sidecar_grant, append_runtime_sqlite_grants,
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
}
