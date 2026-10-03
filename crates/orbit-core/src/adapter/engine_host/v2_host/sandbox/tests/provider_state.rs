// [ORB-10946] The Linux half of the Copilot state-root gate. Every entry this
// returns is created by `ensure_linux_provider_directory`, so an ungated entry
// would mkdir a `~/.copilot` on hosts that never installed the CLI.
#[cfg(target_os = "linux")]
mod copilot_state_roots {
    use std::path::{Path, PathBuf};

    use crate::adapter::engine_host::v2_host::sandbox::provider_state::linux_copilot_state_roots_with;

    #[test]
    fn active_copilot_gets_its_state_and_extraction_cache_roots() {
        let roots =
            linux_copilot_state_roots_with("copilot", Some(Path::new("/home/test")), None, None);

        assert_eq!(
            roots,
            vec![
                PathBuf::from("/home/test/.copilot"),
                PathBuf::from("/home/test/.cache/copilot"),
            ]
        );
    }

    #[test]
    fn overrides_are_honored() {
        let roots = linux_copilot_state_roots_with(
            "copilot",
            Some(Path::new("/home/test")),
            Some(Path::new("/srv/copilot-home")),
            Some(Path::new("/srv/cache")),
        );

        assert_eq!(
            roots,
            vec![
                PathBuf::from("/srv/copilot-home"),
                PathBuf::from("/srv/cache/copilot"),
            ]
        );
    }

    #[test]
    fn non_copilot_providers_get_nothing() {
        for provider in [
            "claude",
            "codex",
            "gemini",
            "grok",
            "cursor",
            "ollama",
            "local-shell",
            "not-a-provider",
        ] {
            assert!(
                linux_copilot_state_roots_with(provider, Some(Path::new("/home/test")), None, None)
                    .is_empty(),
                "{provider} must not inherit copilot roots",
            );
        }
    }
}

#[cfg(target_os = "linux")]
mod provider_state_root_validation {
    use std::os::unix::fs::symlink;
    use std::path::Path;

    use crate::adapter::engine_host::v2_host::sandbox::provider_state::{
        ensure_linux_provider_directory, validated_linux_provider_state_root,
    };

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

    #[test]
    fn accepts_absolute_custom_root_beneath_an_existing_parent() {
        let parent = tempfile::tempdir().expect("parent");
        let custom = parent.path().join("provider").join("state");

        let validated = validated_linux_provider_state_root(&custom, Some(parent.path()))
            .expect("validate custom provider root");

        assert_eq!(
            validated,
            parent
                .path()
                .canonicalize()
                .expect("canonical parent")
                .join("provider")
                .join("state")
        );
    }

    /// OSTree hosts ship `/home -> /var/home`, so a symlinked ancestor is an
    /// ordinary host layout rather than a redirected root. [ORB-11984]
    #[test]
    fn accepts_a_missing_root_beneath_a_symlinked_ancestor() {
        let parent = tempfile::tempdir().expect("parent");
        let real = parent.path().join("real");
        std::fs::create_dir_all(&real).expect("create real ancestor");
        let linked = parent.path().join("linked");
        symlink(&real, &linked).expect("create ancestor symlink");
        let custom = linked.join("provider").join("state");

        let validated = validated_linux_provider_state_root(&custom, Some(parent.path()))
            .expect("validate root beneath a symlinked ancestor");

        assert_eq!(
            validated,
            real.canonicalize()
                .expect("canonical real ancestor")
                .join("provider")
                .join("state")
        );
    }

    /// Dotfile managers relocate provider directories through a symlink, so an
    /// existing symlinked target resolves to its destination. [ORB-11984]
    #[test]
    fn resolves_a_symlinked_root_target_to_its_destination() {
        let parent = tempfile::tempdir().expect("parent");
        let target = parent.path().join("target");
        std::fs::create_dir_all(&target).expect("create symlink target");
        let symlinked_root = parent.path().join("symlink_root");
        symlink(&target, &symlinked_root).expect("create root symlink");
        let canonical_target = target.canonicalize().expect("canonical target");

        assert_eq!(
            validated_linux_provider_state_root(&symlinked_root, Some(parent.path()))
                .expect("validate symlinked provider root"),
            canonical_target
        );
        assert_eq!(
            ensure_linux_provider_directory(&symlinked_root, Some(parent.path()))
                .expect("create symlinked provider root"),
            canonical_target
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
    fn creates_a_validated_custom_root() {
        let parent = tempfile::tempdir().expect("parent");
        let custom = parent.path().join("provider").join("state");

        let created = ensure_linux_provider_directory(&custom, Some(parent.path()))
            .expect("create custom provider root");

        assert!(custom.is_dir());
        assert_eq!(
            created,
            custom.canonicalize().expect("canonical custom root")
        );
    }

    #[test]
    fn rejects_relative_and_traversal_paths() {
        assert!(validated_linux_provider_state_root(Path::new("provider/state"), None).is_err());
        assert!(validated_linux_provider_state_root(Path::new("/tmp/../provider"), None).is_err());
    }
}

#[cfg(target_os = "linux")]
mod cursor_state_roots {
    use std::path::{Path, PathBuf};

    use crate::adapter::engine_host::v2_host::sandbox::provider_state::linux_cursor_state_roots_with;

    #[test]
    fn active_cursor_gets_only_its_home_state_root() {
        assert_eq!(
            linux_cursor_state_roots_with("cursor", Some(Path::new("/home/test"))),
            vec![PathBuf::from("/home/test/.cursor")]
        );
        assert!(linux_cursor_state_roots_with("cursor", None).is_empty());
    }

    #[test]
    fn other_and_unknown_providers_get_nothing() {
        for provider in [
            "claude",
            "codex",
            "gemini",
            "grok",
            "copilot",
            "ollama",
            "not-a-provider",
        ] {
            assert!(
                linux_cursor_state_roots_with(provider, Some(Path::new("/home/test"))).is_empty(),
                "{provider} must not inherit Cursor state roots",
            );
        }
    }
}

#[cfg(target_os = "linux")]
mod opencode_state_roots {
    use std::path::{Path, PathBuf};

    use crate::adapter::engine_host::v2_host::sandbox::provider_state::{
        OpencodeStateEnv, linux_opencode_state_roots_with,
    };

    #[test]
    fn active_opencode_gets_its_four_xdg_roots_from_home_defaults() {
        assert_eq!(
            linux_opencode_state_roots_with(
                "opencode",
                Some(Path::new("/home/test")),
                OpencodeStateEnv::default(),
            ),
            vec![
                PathBuf::from("/home/test/.local/share/opencode"),
                PathBuf::from("/home/test/.config/opencode"),
                PathBuf::from("/home/test/.local/state/opencode"),
                PathBuf::from("/home/test/.cache/opencode"),
            ]
        );
        assert!(
            linux_opencode_state_roots_with("opencode", None, OpencodeStateEnv::default())
                .is_empty()
        );
    }

    #[test]
    fn xdg_variables_and_the_config_override_replace_the_home_defaults() {
        assert_eq!(
            linux_opencode_state_roots_with(
                "opencode",
                Some(Path::new("/home/test")),
                OpencodeStateEnv {
                    xdg_data_home: Some(PathBuf::from("/srv/data")),
                    xdg_config_home: Some(PathBuf::from("/srv/config")),
                    xdg_state_home: Some(PathBuf::from("/srv/state")),
                    xdg_cache_home: Some(PathBuf::from("/srv/cache")),
                    opencode_config_dir: None,
                },
            ),
            vec![
                PathBuf::from("/srv/data/opencode"),
                PathBuf::from("/srv/config/opencode"),
                PathBuf::from("/srv/state/opencode"),
                PathBuf::from("/srv/cache/opencode"),
            ]
        );

        // `OPENCODE_CONFIG_DIR` is the config root itself, not an XDG base, so
        // it is used verbatim and outranks `XDG_CONFIG_HOME`.
        let roots = linux_opencode_state_roots_with(
            "opencode",
            Some(Path::new("/home/test")),
            OpencodeStateEnv {
                xdg_config_home: Some(PathBuf::from("/srv/config")),
                opencode_config_dir: Some(PathBuf::from("/srv/opencode-config")),
                ..OpencodeStateEnv::default()
            },
        );
        assert_eq!(roots[1], PathBuf::from("/srv/opencode-config"));
    }

    #[test]
    fn other_and_unknown_providers_get_nothing() {
        for provider in [
            "claude",
            "codex",
            "gemini",
            "grok",
            "copilot",
            "cursor",
            "pi",
            "ollama",
            "not-a-provider",
        ] {
            assert!(
                linux_opencode_state_roots_with(
                    provider,
                    Some(Path::new("/home/test")),
                    OpencodeStateEnv::default(),
                )
                .is_empty(),
                "{provider} must not inherit OpenCode state roots",
            );
        }
    }
}

#[cfg(target_os = "linux")]
mod pi_state_roots {
    use std::path::{Path, PathBuf};

    use crate::adapter::engine_host::v2_host::sandbox::provider_state::linux_pi_state_roots_with;

    #[test]
    fn active_pi_gets_only_its_agent_state_root() {
        assert_eq!(
            linux_pi_state_roots_with("pi", Some(Path::new("/home/test")), None),
            vec![PathBuf::from("/home/test/.pi")]
        );
        assert!(linux_pi_state_roots_with("pi", None, None).is_empty());
    }

    #[test]
    fn the_agent_dir_override_replaces_the_home_default() {
        assert_eq!(
            linux_pi_state_roots_with(
                "pi",
                Some(Path::new("/home/test")),
                Some(Path::new("/srv/pi-agent")),
            ),
            vec![PathBuf::from("/srv/pi-agent")]
        );
    }

    #[test]
    fn other_and_unknown_providers_get_nothing() {
        for provider in [
            "claude",
            "codex",
            "gemini",
            "grok",
            "copilot",
            "cursor",
            "ollama",
            "not-a-provider",
        ] {
            assert!(
                linux_pi_state_roots_with(provider, Some(Path::new("/home/test")), None).is_empty(),
                "{provider} must not inherit Pi state roots",
            );
        }
    }
}
