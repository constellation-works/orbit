#![allow(missing_docs)]

use super::super::spawn_diagnostics::{
    copilot_model_unavailable_diagnostic, linux_bwrap_failed_write_diagnostic,
    macos_keychain_auth_diagnostic_with, macos_sandbox_apply_failure_diagnostic,
};
use super::test_support::{linux_sandbox_for_test, sandbox_for_test};

/// A profile shaped like a managed-worktree implementer: the worktree is
/// writable, its `.orbit` store is not.
fn worktree_profile(worktree: &std::path::Path) -> orbit_types::policy::ResolvedFsProfile {
    orbit_types::policy::ResolvedFsProfile {
        name: "unrestricted".to_string(),
        read: vec![format!("{}/**", worktree.display())],
        modify: vec![
            format!("{}/**", worktree.display()),
            format!("!{}/.orbit/**", worktree.display()),
        ],
    }
}

/// [ORB-10879] The attribution path an operator actually depends on, exercised
/// without Bubblewrap and without consulting the host's mount table: a child
/// that reported EROFS gets a denial naming the attempted path and the rule
/// that shadowed it.
#[test]
fn failed_write_diagnostic_names_attempted_path_and_shadowing_rule() {
    let worktree = std::path::Path::new("/tmp/orbit-jrun-attribution");
    let profile = worktree_profile(worktree);
    let stderr = b"touch: cannot touch '/tmp/orbit-jrun-attribution/.orbit/state/x': Read-only file system\n";

    let diagnostic = linux_bwrap_failed_write_diagnostic(&profile, stderr, Some(worktree))
        .expect("diagnostic derivation succeeds")
        .expect("a denied write must be attributable");

    assert!(
        diagnostic.contains("/tmp/orbit-jrun-attribution/.orbit/state/x"),
        "diagnostic must name the attempted path: {diagnostic}"
    );
    assert!(
        diagnostic.contains("denyModify rule"),
        "diagnostic must name the shadowing rule: {diagnostic}"
    );
    assert!(
        diagnostic.contains(&format!("!{}/.orbit/**", worktree.display())),
        "diagnostic must quote the exact deny that shadows the path: {diagnostic}"
    );
    // The wrapper prefix is Orbit's, but the explanation body is verbatim
    // `linux_bwrap_write_grant_diagnostic` output — one message format.
    let expected =
        orbit_exec::linux_bwrap_write_grant_diagnostic(&profile, &worktree.join(".orbit/state/x"))
            .expect("grant diagnostic")
            .expect("path is denied");
    assert!(
        diagnostic.ends_with(&expected),
        "diagnostic must reuse the existing grant-diagnostic text: {diagnostic}"
    );
}

/// A path the profile *does* grant is not reported as a denial, so the
/// diagnostic cannot manufacture an attribution for an unrelated EROFS.
#[test]
fn failed_write_diagnostic_stays_silent_for_a_granted_path() {
    let worktree = std::path::Path::new("/tmp/orbit-jrun-attribution");
    let profile = worktree_profile(worktree);
    let stderr =
        b"touch: cannot touch '/tmp/orbit-jrun-attribution/docs/x.md': Read-only file system\n";

    let diagnostic = linux_bwrap_failed_write_diagnostic(&profile, stderr, Some(worktree))
        .expect("diagnostic derivation succeeds");

    assert!(
        diagnostic.is_none(),
        "a granted path must not be attributed to policy: {diagnostic:?}"
    );
}

/// The failure an operator actually sees when a Keychain-backed login cannot be
/// read: the provider says "expired", which is indistinguishable from a sandbox
/// denial unless Orbit attributes it. Both branches must be reachable, because
/// they call for different fixes.
#[test]
fn keychain_auth_diagnostic_separates_a_real_expiry_from_a_sandbox_denial() {
    let sandbox = sandbox_for_test();
    let failure = "Failed to authenticate: OAuth session expired and could not be refreshed";

    let with_home = macos_keychain_auth_diagnostic_with(
        "claude",
        Some(&sandbox),
        failure,
        Some(std::ffi::OsStr::new("/Users/test")),
    )
    .expect("claude under sandbox-exec must be attributed");
    assert!(
        with_home.contains("real login failure"),
        "a reachable keychain means re-authentication is the fix: {with_home}"
    );

    let without_home = macos_keychain_auth_diagnostic_with("claude", Some(&sandbox), failure, None)
        .expect("a HOME-less compile cannot emit the keychain allow");
    assert!(
        without_home.contains("HOME is unset"),
        "an unemitted keychain allow means re-authentication cannot help: {without_home}"
    );
}

#[test]
fn copilot_unavailable_model_diagnostic_names_model_crew_and_config_key() {
    let stderr = "Error: Model \"claude-sonnet-4.5\" from --model flag is not available.";

    let diagnostic = copilot_model_unavailable_diagnostic("copilot", "qa", stderr)
        .expect("Copilot unavailable-model stderr must be actionable");

    assert!(diagnostic.contains("claude-sonnet-4.5"));
    assert!(diagnostic.contains("crew `qa`"));
    assert!(diagnostic.contains("`crews.qa.model`"));
    assert!(diagnostic.contains("`/model`"));
    assert!(copilot_model_unavailable_diagnostic("codex", "qa", stderr).is_none());
    assert!(copilot_model_unavailable_diagnostic("copilot", "qa", "request timed out").is_none());
}

/// [ORB-10931] The third case: the operator's own `denyRead` outranks the
/// provider carve-out, so Orbit — not the provider — hid the credential.
/// Recommending re-authentication here sends the operator to a fix that cannot
/// work, so the message must name the rule instead.
#[test]
fn keychain_auth_diagnostic_attributes_an_activity_deny_instead_of_a_relogin() {
    let failure = "Failed to authenticate: OAuth session expired and could not be refreshed";
    let home = std::ffi::OsStr::new("/Users/test");

    for deny in ["!/Users/test/Library/Keychains", "!/Users/test/Library"] {
        let mut sandbox = sandbox_for_test();
        sandbox.fs_profile.name = "hardened".to_string();
        sandbox.fs_profile.read.push(deny.to_string());

        let diagnostic =
            macos_keychain_auth_diagnostic_with("claude", Some(&sandbox), failure, Some(home))
                .expect("an activity keychain deny must be attributed");

        assert!(
            diagnostic.contains(deny) && diagnostic.contains("hardened"),
            "diagnostic must name the profile and the exact deny rule: {diagnostic}"
        );
        assert!(
            diagnostic.contains("Re-authenticating will not help"),
            "an Orbit-authored denial must not send the operator to re-login: {diagnostic}"
        );
        assert!(
            !diagnostic.contains("real login failure"),
            "a denied keychain must never be reported as reachable: {diagnostic}"
        );
    }
}

#[test]
fn keychain_auth_diagnostic_stays_silent_outside_its_exact_failure_shape() {
    let sandbox = sandbox_for_test();
    let failure = "Failed to authenticate: OAuth session expired and could not be refreshed";
    let home = std::ffi::OsStr::new("/Users/test");

    // Providers that never read the keychain, unsandboxed runs, and unrelated
    // failures must not collect a keychain explanation. A Copilot or Cursor
    // failure also must not match Claude's wording, and vice versa.
    assert!(
        macos_keychain_auth_diagnostic_with("codex", Some(&sandbox), failure, Some(home)).is_none()
    );
    assert!(macos_keychain_auth_diagnostic_with("claude", None, failure, Some(home)).is_none());
    assert!(
        macos_keychain_auth_diagnostic_with(
            "claude",
            Some(&linux_sandbox_for_test(false)),
            failure,
            Some(home)
        )
        .is_none()
    );
    assert!(
        macos_keychain_auth_diagnostic_with(
            "claude",
            Some(&sandbox),
            "error: request timed out",
            Some(home)
        )
        .is_none()
    );
    assert!(
        macos_keychain_auth_diagnostic_with("copilot", Some(&sandbox), failure, Some(home))
            .is_none()
    );
    assert!(
        macos_keychain_auth_diagnostic_with("cursor", Some(&sandbox), failure, Some(home))
            .is_none()
    );
}

/// Copilot and Cursor use different auth-failure wording from Claude. Each
/// must still distinguish a reachable keychain (real logout) from an Orbit
/// denyRead that hid the credential. [ORB-12261]
#[test]
fn keychain_auth_diagnostic_recognises_copilot_and_cursor_auth_failures() {
    let home = std::ffi::OsStr::new("/Users/test");
    let copilot_failure = "Error: No authentication information found.";
    let cursor_quoted = "Error: Authentication required. Please run 'agent login' first, or set CURSOR_API_KEY environment variable.";
    let cursor_live = "Error: Authentication required. Please run agent login first, or set CURSOR_API_KEY environment variable.";

    for (provider, failure) in [
        ("copilot", copilot_failure),
        ("cursor", cursor_quoted),
        ("cursor", cursor_live),
    ] {
        let sandbox = sandbox_for_test();
        let reachable =
            macos_keychain_auth_diagnostic_with(provider, Some(&sandbox), failure, Some(home))
                .unwrap_or_else(|| panic!("{provider} under sandbox-exec must be attributed"));
        assert!(
            reachable.contains("real login failure") && reachable.contains(provider),
            "a reachable keychain means re-authentication is the fix: {reachable}"
        );

        let mut denied = sandbox_for_test();
        denied.fs_profile.name = "hardened".to_string();
        denied
            .fs_profile
            .read
            .push("!/Users/test/Library/Keychains".to_string());
        let diagnostic =
            macos_keychain_auth_diagnostic_with(provider, Some(&denied), failure, Some(home))
                .unwrap_or_else(|| panic!("{provider} activity deny must be attributed"));
        assert!(
            diagnostic.contains("!/Users/test/Library/Keychains")
                && diagnostic.contains("hardened")
                && diagnostic.contains("Re-authenticating will not help"),
            "an Orbit-authored denial must name the rule, not send the operator to re-login: {diagnostic}"
        );
        assert!(
            !diagnostic.contains("real login failure"),
            "a denied keychain must never be reported as reachable: {diagnostic}"
        );
    }
}

/// The wrapper's own refusal is a host condition, so the diagnosis is
/// provider-independent and names the remedies that actually apply.
/// [DANI-10509]
#[test]
fn sandbox_apply_failure_diagnostic_is_provider_independent_and_names_the_remedy() {
    let sandbox = sandbox_for_test();
    let stderr = "sandbox-exec: sandbox_apply: Operation not permitted\n";

    for provider in ["claude", "codex", "copilot", "gemini", "cursor", "grok"] {
        let diagnostic =
            macos_sandbox_apply_failure_diagnostic(provider, Some(&sandbox), Some(71), stderr)
                .unwrap_or_else(|| panic!("provider `{provider}` must get the diagnosis"));
        assert!(
            diagnostic.contains(provider),
            "the diagnosis names the CLI that never started: {diagnostic}"
        );
        assert!(
            diagnostic.contains("sandbox_apply: Operation not permitted"),
            "the diagnosis quotes the wrapper's own failure: {diagnostic}"
        );
        assert!(
            diagnostic.contains("Run Orbit outside the enclosing sandbox")
                && diagnostic.contains("`sandbox: off`"),
            "the diagnosis names remedies that work: {diagnostic}"
        );
        assert!(
            diagnostic.contains("`allow_fallback` does not cover this case"),
            "allow_fallback only covers a missing wrapper, so say so: {diagnostic}"
        );
        assert!(
            !diagnostic.to_lowercase().contains("keychain"),
            "no credential store is involved before the CLI starts: {diagnostic}"
        );
    }
}

/// Exit 71 alone, a different exit code, a non-macOS backend, and an
/// unsandboxed run are all ordinary failures the generic path owns.
/// [DANI-10509]
#[test]
fn sandbox_apply_failure_diagnostic_stays_silent_outside_its_exact_failure_shape() {
    let sandbox = sandbox_for_test();
    let stderr = "sandbox-exec: sandbox_apply: Operation not permitted\n";

    assert!(
        macos_sandbox_apply_failure_diagnostic(
            "copilot",
            Some(&sandbox),
            Some(71),
            "copilot: internal service error\n"
        )
        .is_none()
    );
    assert!(
        macos_sandbox_apply_failure_diagnostic("copilot", Some(&sandbox), Some(1), stderr)
            .is_none()
    );
    assert!(
        macos_sandbox_apply_failure_diagnostic("copilot", Some(&sandbox), None, stderr).is_none()
    );
    assert!(macos_sandbox_apply_failure_diagnostic("copilot", None, Some(71), stderr).is_none());
    assert!(
        macos_sandbox_apply_failure_diagnostic(
            "copilot",
            Some(&linux_sandbox_for_test(false)),
            Some(71),
            stderr
        )
        .is_none()
    );
}
