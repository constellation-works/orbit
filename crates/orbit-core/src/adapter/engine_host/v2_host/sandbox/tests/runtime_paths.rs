#[cfg(target_os = "linux")]
mod runtime_root_validation {
    use std::os::unix::fs::symlink;
    use std::path::Path;

    use crate::adapter::engine_host::v2_host::sandbox::runtime_paths::validated_linux_runtime_root;

    #[test]
    fn accepts_an_existing_runtime_directory_and_returns_its_canonical_path() {
        let root = tempfile::tempdir().expect("runtime root");

        let validated = validated_linux_runtime_root(root.path()).expect("validate runtime root");

        assert_eq!(
            validated,
            root.path().canonicalize().expect("canonical runtime root")
        );
    }

    #[test]
    fn rejects_missing_and_relative_runtime_roots() {
        let parent = tempfile::tempdir().expect("runtime parent");
        let missing = parent.path().join("missing");

        assert!(validated_linux_runtime_root(&missing).is_err());
        assert!(validated_linux_runtime_root(Path::new("runtime-root")).is_err());
    }

    /// Runtime roots live under `$HOME`, so they hit the same symlinked-ancestor
    /// host layouts as provider state roots. [ORB-11984]
    #[test]
    fn resolves_a_symlinked_runtime_root_to_its_canonical_directory() {
        let parent = tempfile::tempdir().expect("runtime parent");
        let real = parent.path().join("real");
        std::fs::create_dir_all(&real).expect("create runtime directory");
        let link = parent.path().join("link");
        symlink(&real, &link).expect("create runtime-root symlink");

        let validated =
            validated_linux_runtime_root(&link).expect("validate symlinked runtime root");

        assert_eq!(
            validated,
            real.canonicalize().expect("canonical runtime root")
        );
    }
}
