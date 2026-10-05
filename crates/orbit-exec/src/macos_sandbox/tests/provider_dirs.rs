use super::super::test_support::*;

#[test]
fn copilot_reallows_the_user_keychain_but_not_github_cli_credentials() {
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
