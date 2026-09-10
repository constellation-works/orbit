mod matching {
    use std::path::PathBuf;

    use tempfile::tempdir;

    use crate::fs::selector::*;

    #[test]
    fn anchor_path_extracts_symbol_file_path() {
        assert_eq!(
            anchor_path("symbol:src/lib.rs#run:function").unwrap(),
            PathBuf::from("src/lib.rs")
        );
    }

    #[test]
    fn exists_in_workspace_uses_anchor_paths() {
        let temp = tempdir().unwrap();
        let workspace = temp.path();
        std::fs::create_dir_all(workspace.join("src")).unwrap();
        std::fs::write(workspace.join("src/lib.rs"), "pub fn ok() {}\n").unwrap();

        assert!(exists_in_workspace(
            "symbol:src/lib.rs#run:function",
            workspace
        ));
        assert!(!exists_in_workspace(
            "symbol:src/missing.rs#run:function",
            workspace
        ));
    }

    #[test]
    fn overlaps_uses_anchor_semantics() {
        assert!(overlaps("symbol:f.rs#a:method", "symbol:f.rs#b:method"));
        assert!(overlaps("dir:src", "file:src/lib.rs"));
        assert!(overlaps("src", "file:src/lib.rs"));
        assert!(!overlaps("file:f.rs", "file:g.rs"));
        assert!(!overlaps("dir:src", "file:lib/y.rs"));
    }

    #[test]
    fn shared_anchor_prefix_depth_ignores_selector_metadata() {
        assert_eq!(
            shared_anchor_prefix_depth(
                "symbol:src/lib.rs#alpha:function",
                "file:src/nested/mod.rs"
            ),
            1
        );
    }

    #[test]
    fn anchorless_selectors_have_no_filesystem_anchor() {
        assert!(anchor_path("module:orbit_core::scheduler").is_err());
        assert!(anchor_path("command:task.update").is_err());

        let temp = tempdir().unwrap();
        assert!(!exists_in_workspace("module:orbit_core", temp.path()));
        assert!(!exists_in_workspace("command:task.update", temp.path()));

        assert_eq!(shared_anchor_prefix_depth("module:a::b", "file:a/b.rs"), 0);
    }

    #[test]
    fn anchorless_selectors_overlap_only_on_textual_equality() {
        assert!(overlaps("module:orbit_core", "module:orbit_core"));
        assert!(overlaps("command:task.update", " command:task.update "));
        assert!(!overlaps("module:orbit_core", "module:orbit_engine"));
        assert!(!overlaps("module:orbit_core", "file:src/lib.rs"));
        assert!(!overlaps("dir:src", "command:task.update"));
    }
}

mod parse {
    use std::path::{Path, PathBuf};
    use std::str::FromStr;

    use proptest::prelude::*;
    use proptest::test_runner::Config as ProptestConfig;
    use tempfile::tempdir;

    use crate::fs::selector::*;

    fn path_segment() -> impl Strategy<Value = String> {
        prop::string::string_regex("[a-z][a-z0-9_]{0,8}").expect("valid path segment regex")
    }

    fn selector_path() -> impl Strategy<Value = String> {
        prop::collection::vec(path_segment(), 1..5).prop_map(|segments| segments.join("/"))
    }

    fn identifier() -> impl Strategy<Value = String> {
        prop::string::string_regex("[A-Za-z_][A-Za-z0-9_]{0,12}").expect("valid identifier regex")
    }

    fn symbol_name() -> impl Strategy<Value = String> {
        prop_oneof![
            identifier(),
            (identifier(), identifier()).prop_map(|(module, name)| format!("{module}::{name}")),
            (identifier(), identifier(), identifier())
                .prop_map(|(ty, trait_name, method)| format!("<{ty} as {trait_name}>::{method}")),
        ]
    }

    fn kind_name() -> impl Strategy<Value = String> {
        prop::string::string_regex("[a-z][a-z_]{0,12}").expect("valid kind regex")
    }

    fn dir_selector() -> impl Strategy<Value = Selector> {
        selector_path().prop_map(|path| Selector::Dir { path })
    }

    fn file_selector() -> impl Strategy<Value = Selector> {
        selector_path().prop_map(|path| Selector::File { path })
    }

    fn symbol_selector() -> impl Strategy<Value = Selector> {
        (selector_path(), symbol_name(), kind_name())
            .prop_map(|(path, symbol, kind)| Selector::Symbol { path, symbol, kind })
    }

    fn module_selector() -> impl Strategy<Value = Selector> {
        prop::collection::vec(identifier(), 1..4).prop_map(|segments| Selector::Module {
            qualified: segments.join("::"),
        })
    }

    fn command_selector() -> impl Strategy<Value = Selector> {
        prop::collection::vec(identifier(), 1..4).prop_map(|segments| Selector::Command {
            name: segments.join("."),
        })
    }

    #[test]
    fn canonical_selector_handles_raw_paths_and_ranges() {
        assert_eq!(canonical_selector("src/lib.rs").unwrap(), "file:src/lib.rs");
        assert_eq!(
            canonical_selector("src/lib.rs:42").unwrap(),
            "file:src/lib.rs"
        );
        assert_eq!(
            canonical_selector("src/lib.rs:42:7").unwrap(),
            "file:src/lib.rs"
        );
        assert_eq!(
            canonical_selector("src/mod.rs:10-20").unwrap(),
            "file:src/mod.rs"
        );
        assert_eq!(canonical_selector("src/").unwrap(), "dir:src");
    }

    #[test]
    fn canonical_selector_in_workspace_rewrites_absolute_and_directory_paths() {
        let temp = tempdir().unwrap();
        let workspace = temp.path();
        std::fs::create_dir_all(workspace.join("src/nested")).unwrap();
        std::fs::write(workspace.join("src/lib.rs"), "pub fn ok() {}\n").unwrap();

        assert_eq!(
            canonical_selector_in_workspace(
                &workspace.join("src/lib.rs").to_string_lossy(),
                workspace
            )
            .unwrap(),
            "file:src/lib.rs"
        );
        assert_eq!(
            canonical_selector_in_workspace("src/nested", workspace).unwrap(),
            "dir:src/nested"
        );
    }

    #[cfg(unix)]
    #[test]
    fn canonical_selector_in_workspace_accepts_macos_var_alias() {
        let (_guard, presented_workspace) = var_aliased_workspace();
        std::fs::create_dir_all(presented_workspace.join("src/nested")).unwrap();
        std::fs::write(presented_workspace.join("src/lib.rs"), "pub fn ok() {}\n").unwrap();

        let presented_file = presented_workspace.join("src/lib.rs");
        let presented_dir = presented_workspace.join("src/nested");
        assert_eq!(
            canonical_selector_in_workspace(
                &presented_file.to_string_lossy(),
                &presented_workspace
            )
            .unwrap(),
            "file:src/lib.rs"
        );
        assert_eq!(
            canonical_selector_in_workspace(&presented_dir.to_string_lossy(), &presented_workspace)
                .unwrap(),
            "dir:src/nested"
        );
        assert!(exists_in_workspace(
            &format!("file:{}", presented_file.display()),
            &presented_workspace
        ));
    }

    /// Present a workspace through `/var` when the host aliases it to
    /// `/private/var`. Otherwise build a local `var` → `private/var` replica
    /// so the case does not depend on tempfile's layout.
    #[cfg(unix)]
    fn var_aliased_workspace() -> (tempfile::TempDir, PathBuf) {
        let temp = tempdir().unwrap();
        let canonical = temp.path().canonicalize().unwrap();
        if let Some(presented) = rewrite_private_var_to_var(&canonical) {
            return (temp, presented);
        }

        let private_var = temp.path().join("private/var");
        std::fs::create_dir_all(&private_var).unwrap();
        std::os::unix::fs::symlink(&private_var, temp.path().join("var")).unwrap();
        let workspace = private_var.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let presented = temp.path().join("var/workspace");
        (temp, presented)
    }

    #[cfg(unix)]
    fn rewrite_private_var_to_var(path: &Path) -> Option<PathBuf> {
        let var = Path::new("/var");
        if !var.is_symlink() {
            return None;
        }
        if var.canonicalize().ok()?.as_path() != Path::new("/private/var") {
            return None;
        }
        let rest = path.to_str()?.strip_prefix("/private/var")?;
        Some(PathBuf::from(format!("/var{rest}")))
    }

    #[test]
    fn canonical_selector_in_workspace_rejects_anchors_outside_the_workspace() {
        let workspace = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let outside_file = outside.path().join("outside.rs");
        std::fs::write(&outside_file, "fn outside() {}\n").unwrap();

        assert!(
            canonical_selector_in_workspace(
                &format!("symbol:{}#run:function", outside_file.display()),
                workspace.path()
            )
            .is_err()
        );
        assert!(
            canonical_selector_in_workspace("symbol:../outside.rs#run:function", workspace.path())
                .is_err()
        );
        assert!(!exists_in_workspace(
            &format!("symbol:{}#run:function", outside_file.display()),
            workspace.path()
        ));
    }

    #[cfg(unix)]
    #[test]
    fn workspace_containment_follows_anchor_symlinks() {
        let workspace = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let outside_file = outside.path().join("outside.rs");
        std::fs::write(&outside_file, "fn outside() {}\n").unwrap();
        std::os::unix::fs::symlink(&outside_file, workspace.path().join("linked.rs")).unwrap();

        assert!(
            canonical_selector_in_workspace("symbol:linked.rs#run:function", workspace.path())
                .is_err()
        );
        assert!(!exists_in_workspace(
            "symbol:linked.rs#run:function",
            workspace.path()
        ));
    }

    #[cfg(unix)]
    #[test]
    fn legacy_path_dir_check_rejects_symlink_escaping_workspace() {
        let workspace = tempdir().unwrap();
        let outside = tempdir().unwrap();
        std::fs::create_dir_all(outside.path().join("secret")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("secret"),
            workspace.path().join("linked"),
        )
        .unwrap();

        // The legacy (no-prefix) path form is the one that probes the
        // filesystem with `Path::is_dir()` to pick `dir:` vs `file:`. That
        // probe must only ever run against the already containment-checked
        // anchor, so a symlink escaping the workspace is rejected before any
        // `is_dir()` call touches the outside target.
        assert!(canonical_selector_in_workspace("linked", workspace.path()).is_err());
        assert!(!exists_in_workspace("linked", workspace.path()));
    }

    #[test]
    fn symbol_selector_preserves_opaque_qualified_name() {
        let selector: Selector = "symbol:src/lib.rs#<Foo as Runnable>::run#2:method"
            .parse()
            .unwrap();

        assert_eq!(
            selector,
            Selector::Symbol {
                path: "src/lib.rs".to_string(),
                symbol: "<Foo as Runnable>::run#2".to_string(),
                kind: "method".to_string(),
            }
        );
        assert_eq!(
            selector.to_string(),
            "symbol:src/lib.rs#<Foo as Runnable>::run#2:method"
        );
        assert_eq!(
            anchor_path(&selector.to_string()).unwrap(),
            PathBuf::from("src/lib.rs")
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 256, .. ProptestConfig::default() })]

        #[test]
        fn dir_selector_display_parse_roundtrips(selector in dir_selector()) {
            prop_assert_eq!(Selector::from_str(&selector.to_string()).unwrap(), selector);
        }

        #[test]
        fn file_selector_display_parse_roundtrips(selector in file_selector()) {
            prop_assert_eq!(Selector::from_str(&selector.to_string()).unwrap(), selector);
        }

        #[test]
        fn symbol_selector_display_parse_roundtrips(selector in symbol_selector()) {
            prop_assert_eq!(Selector::from_str(&selector.to_string()).unwrap(), selector);
        }

        #[test]
        fn module_selector_display_parse_roundtrips(selector in module_selector()) {
            prop_assert_eq!(Selector::from_str(&selector.to_string()).unwrap(), selector);
        }

        #[test]
        fn command_selector_display_parse_roundtrips(selector in command_selector()) {
            prop_assert_eq!(Selector::from_str(&selector.to_string()).unwrap(), selector);
        }
    }

    #[test]
    fn every_selector_variant_roundtrips_through_display() {
        let variants = [
            "dir:src/command",
            "file:src/lib.rs",
            "symbol:src/lib.rs#hello:function",
            "module:orbit_core::scheduler",
            "command:task.update",
        ];
        for raw in variants {
            let parsed: Selector = raw.parse().unwrap();
            assert_eq!(parsed.to_string(), raw);
            let reparsed: Selector = parsed.to_string().parse().unwrap();
            assert_eq!(reparsed, parsed);
        }
    }

    #[test]
    fn module_and_command_selectors_parse_and_validate() {
        assert_eq!(
            "module:orbit_core::scheduler".parse::<Selector>().unwrap(),
            Selector::Module {
                qualified: "orbit_core::scheduler".to_string(),
            }
        );
        assert_eq!(
            " command: task.update ".parse::<Selector>().unwrap(),
            Selector::Command {
                name: "task.update".to_string(),
            }
        );
        assert!("module:".parse::<Selector>().is_err());
        assert!("command:  ".parse::<Selector>().is_err());
    }

    #[test]
    fn canonical_selector_passes_anchorless_selectors_through() {
        assert_eq!(
            canonical_selector("module:orbit_core::scheduler").unwrap(),
            "module:orbit_core::scheduler"
        );
        assert_eq!(
            canonical_selector("command:task.update").unwrap(),
            "command:task.update"
        );

        let temp = tempdir().unwrap();
        assert_eq!(
            canonical_selector_in_workspace("module:orbit_core::scheduler", temp.path()).unwrap(),
            "module:orbit_core::scheduler"
        );
        assert_eq!(
            canonical_selector_in_workspace("command:task.update", temp.path()).unwrap(),
            "command:task.update"
        );
    }

    #[test]
    fn unknown_prefixes_reject_with_full_grammar_hint() {
        let error = "package:orbit-core".parse::<Selector>().unwrap_err();
        assert!(error.reason.contains("`module:`"));
        assert!(error.reason.contains("`command:`"));
    }
}

mod translation {
    use crate::OrbitError;
    use crate::fs::selector::{SelectorParseError, selector_error_to_orbit};

    #[test]
    fn selector_error_to_orbit_maps_to_invalid_input() {
        let error = SelectorParseError {
            input: "bogus:thing".to_string(),
            reason: "unknown selector kind".to_string(),
        };
        let rendered = error.to_string();
        assert!(matches!(
            selector_error_to_orbit(error),
            OrbitError::InvalidInput(m) if m == format!("invalid selector: {rendered}")
        ));
    }
}
