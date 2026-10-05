#[cfg(target_os = "linux")]
mod provider_state_root_validation {
    use std::os::unix::fs::symlink;
    use std::path::Path;

    use crate::adapter::engine_host::v2_host::sandbox::provider_state::validated_linux_provider_state_root;

    #[test]
    fn rejects_root_and_home_wide_targets() {
        let home = tempfile::tempdir().expect("home");

        assert!(validated_linux_provider_state_root(Path::new("/"), Some(home.path())).is_err());
        assert!(validated_linux_provider_state_root(home.path(), Some(home.path())).is_err());
        assert!(
            validated_linux_provider_state_root(
                home.path().parent().expect("home parent"),
                Some(home.path()),
            )
            .is_err()
        );
    }

    /// Following symlinks must not let one widen the grant: containment is
    /// re-checked against the resolved destination. [ORB-11984]
    #[test]
    fn rejects_a_symlinked_root_that_resolves_into_the_home_directory() {
        let parent = tempfile::tempdir().expect("parent");
        let home = parent.path().join("home");
        std::fs::create_dir_all(&home).expect("create home");
        let escaping_root = home.join("provider");
        symlink(&home, &escaping_root).expect("create escaping root symlink");

        let error = validated_linux_provider_state_root(&escaping_root, Some(&home))
            .expect_err("reject a symlink resolving onto home");

        assert!(
            error.to_string().contains("broader than the user's home"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn rejects_relative_and_traversal_paths() {
        assert!(validated_linux_provider_state_root(Path::new("provider/state"), None).is_err());
        assert!(validated_linux_provider_state_root(Path::new("/tmp/../provider"), None).is_err());
    }
}
