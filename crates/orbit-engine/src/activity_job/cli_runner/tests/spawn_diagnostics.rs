use std::ffi::OsStr;

use orbit_types::policy::ResolvedFsProfile;
use orbit_types::workflow::ExecutorSandboxKind;

use crate::activity_job::cli_runner::spawn_diagnostics::macos_keychain_auth_diagnostic_with;
use crate::activity_job::dispatcher::ResolvedSandbox;

fn sandbox(read_rules: Vec<String>) -> ResolvedSandbox {
    ResolvedSandbox {
        kind: ExecutorSandboxKind::MacosSandboxExec,
        fs_profile: ResolvedFsProfile {
            name: "test-profile".to_string(),
            read: read_rules,
            modify: Vec::new(),
        },
        allow_fallback: false,
        managed_worktree: false,
        runtime_write_authority: Vec::new(),
        mask: None,
    }
}

#[test]
fn keychain_backed_provider_failures_report_sandbox_access_or_real_logout() {
    let home = OsStr::new("/Users/test");
    let denied = sandbox(vec![
        "/Users/test".to_string(),
        "!/Users/test/Library/Keychains".to_string(),
    ]);
    let allowed = sandbox(vec!["/Users/test".to_string()]);
    let providers = [
        ("claude", "OAuth session expired"),
        ("copilot", "No authentication information found"),
        (
            "cursor",
            "Authentication required. Please run agent login first",
        ),
        ("antigravity", "authentication required"),
    ];

    for (provider, output) in providers {
        let diagnostic =
            macos_keychain_auth_diagnostic_with(provider, Some(&denied), output, Some(home))
                .unwrap_or_else(|| panic!("{provider} keychain failure should be diagnosed"));
        assert!(
            diagnostic.contains("sandbox hid the stored credential"),
            "{provider} activity deny should be reported as a sandbox-hidden credential"
        );

        let diagnostic = macos_keychain_auth_diagnostic_with(provider, Some(&denied), output, None)
            .unwrap_or_else(|| {
                panic!("{provider} failure with unresolved HOME should be diagnosed")
            });
        assert!(
            diagnostic.contains("HOME is unset"),
            "{provider} unresolved HOME should identify the missing sandbox grant"
        );

        let diagnostic =
            macos_keychain_auth_diagnostic_with(provider, Some(&allowed), output, Some(home))
                .unwrap_or_else(|| {
                    panic!("{provider} allowed-keychain failure should be diagnosed")
                });
        assert!(
            diagnostic.contains("real login failure"),
            "{provider} allowed keychain should distinguish an actual logout"
        );
    }
}
