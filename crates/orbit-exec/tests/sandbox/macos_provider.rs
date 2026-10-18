use super::macos_audit_support::*;

#[test]
fn copilot_reallows_the_user_keychain_but_not_github_cli_credentials() {
    if super::audit_support::run_isolated(
        "macos_provider::copilot_reallows_the_user_keychain_but_not_github_cli_credentials",
    ) {
        return;
    }
    // Copilot's `/login` session lives in the macOS login keychain item
    // `github-copilot-app`, so the confined Copilot profile re-allows that
    // user directory. It must still not borrow the GitHub CLI's credential
    // store: that is a different tool's secret. [ORB-12261]
    let resolved = profile("default", &["/Users/test/repo"], &["/Users/test/repo/src"]);

    let text = compile_with_env(
        &resolved,
        "copilot",
        EnvOverrides {
            home: Some("/Users/test"),
            ..Default::default()
        },
    );

    let deny = "(deny file-read* (subpath \"/Users/test/Library/Keychains\"))";
    let allow = "(allow file-read* (subpath \"/Users/test/Library/Keychains\"))";
    let deny_pos = text.find(deny).expect("default user keychain deny");
    let allow_pos = text.find(allow).expect("copilot user keychain re-allow");
    assert!(
        deny_pos < allow_pos,
        "re-allow must follow the default deny"
    );
    assert!(text.contains("(deny file-read* (subpath \"/Users/test/.config/gh\"))"));
    assert!(!text.contains("(allow file-read* (subpath \"/Users/test/.config/gh\"))"));
}

#[test]
fn only_the_confined_muse_lane_may_write_muse_state_roots() {
    if super::audit_support::run_isolated(
        "macos_provider::only_the_confined_muse_lane_may_write_muse_state_roots",
    ) {
        return;
    }
    // Muse Code keeps settings and its `auth.json` login under its XDG config
    // root, tracing/plugins/skills under its XDG data root, and the session
    // name authority under Application Support. Its login is a file, so the
    // user keychain stays denied. [ORB-16577]
    let resolved = profile("default", &["/Users/test/repo"], &["/Users/test/repo/src"]);
    let grant = |dir: &str| format!("(allow file-write* (subpath \"{dir}\"))");
    let defaults = [
        "/Users/test/.config/muse",
        "/Users/test/.local/share/muse",
        "/Users/test/Library/Application Support/Muse",
    ];
    let home = || EnvOverrides {
        home: Some("/Users/test"),
        ..Default::default()
    };

    let text = compile_with_env(&resolved, "muse", home());
    for dir in defaults {
        assert!(text.contains(&grant(dir)), "muse must write {dir}");
    }
    assert!(!text.contains("(allow file-read* (subpath \"/Users/test/Library/Keychains\"))"));

    for provider in ["claude", "codex", "opencode", "pi"] {
        let text = compile_with_env(&resolved, provider, home());
        for dir in defaults {
            assert!(
                !text.contains(&grant(dir)),
                "{provider} must not inherit Muse's grant {dir}"
            );
        }
    }

    let text = compile_with_env(
        &resolved,
        "muse",
        EnvOverrides {
            home: Some("/Users/test"),
            xdg_config_home: Some("/Users/test/xdg-config"),
            xdg_data_home: Some("/Users/test/xdg-data"),
            ..Default::default()
        },
    );
    assert!(text.contains(&grant("/Users/test/xdg-config/muse")));
    assert!(text.contains(&grant("/Users/test/xdg-data/muse")));
    assert!(!text.contains(&grant("/Users/test/.config/muse")));
}
