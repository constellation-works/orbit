use super::super::compile::{MacosLoginKeychainAccess, macos_login_keychain_access};
use super::super::test_support::*;

#[test]
fn compile_emits_deny_default_and_broad_read_with_modify_subpath() {
    let resolved = profile("default", &["/Users/test/repo"], &["/Users/test/repo/src"]);
    let text = compile_with_env(&resolved, NEUTRAL_PROVIDER, EnvOverrides::default());
    assert!(text.contains("(deny default)"));
    assert!(text.contains("(allow file-read*)"));
    assert!(
        text.contains("(allow file-write* (subpath \"/Users/test/repo/src\"))"),
        "missing modify subpath clause: {text}"
    );
}

#[test]
fn compile_default_profile_denies_well_known_credential_reads() {
    let resolved = profile("default", &["/Users/test/repo"], &["/Users/test/repo/src"]);
    let text = compile_with_env(
        &resolved,
        NEUTRAL_PROVIDER,
        EnvOverrides {
            home: Some("/Users/test"),
            ..Default::default()
        },
    );

    for credential_root in [
        "/Users/test/.ssh",
        "/Users/test/.aws",
        "/Users/test/.config/gh",
        "/Users/test/Library/Keychains",
        "/Library/Keychains",
        "/System/Library/Keychains",
    ] {
        let clause = format!("(deny file-read* (subpath \"{credential_root}\"))");
        assert!(
            text.contains(&clause),
            "missing default credential read deny for {credential_root}: {text}"
        );
    }

    let allow_pos = text.find("(allow file-read*)").expect("broad read allow");
    let ssh_deny = "(deny file-read* (subpath \"/Users/test/.ssh\"))";
    let ssh_deny_pos = text
        .find(ssh_deny)
        .expect("default ~/.ssh read deny for private keys such as ~/.ssh/id_rsa");
    assert!(
        allow_pos < ssh_deny_pos,
        "credential read denies must follow broad read allow for last-match-wins: {text}"
    );
}

fn assert_user_keychain_reallow_for(provider: &str) {
    let resolved = profile("default", &["/Users/test/repo"], &["/Users/test/repo/src"]);
    let text = compile_with_env(
        &resolved,
        provider,
        EnvOverrides {
            home: Some("/Users/test"),
            ..Default::default()
        },
    );

    let deny = "(deny file-read* (subpath \"/Users/test/Library/Keychains\"))";
    let allow = "(allow file-read* (subpath \"/Users/test/Library/Keychains\"))";
    let deny_pos = text.find(deny).expect("default user keychain read deny");
    let allow_pos = text.find(allow).unwrap_or_else(|| {
        panic!("missing {provider} user keychain read re-allow: {text}");
    });
    assert!(
        deny_pos < allow_pos,
        "the re-allow must follow the deny for SBPL last-match-wins ({provider}): {text}"
    );

    // The carve-out is the user's keychain only.
    for system_keychain in ["/Library/Keychains", "/System/Library/Keychains"] {
        assert!(
            !text.contains(&format!(
                "(allow file-read* (subpath \"{system_keychain}\"))"
            )),
            "system keychain {system_keychain} must stay denied even for {provider}: {text}"
        );
    }
    // Reading the credential never implies writing it.
    assert!(
        !text.contains("(allow file-write* (subpath \"/Users/test/Library/Keychains\"))"),
        "keychain writes must stay denied for {provider}: {text}"
    );
    // Unrelated credential stores keep their denies.
    for other in [
        "/Users/test/.ssh",
        "/Users/test/.aws",
        "/Users/test/.config/gh",
    ] {
        assert!(
            text.contains(&format!("(deny file-read* (subpath \"{other}\"))")),
            "missing credential read deny for {other} ({provider}): {text}"
        );
        assert!(
            !text.contains(&format!("(allow file-read* (subpath \"{other}\"))")),
            "{other} must not be re-allowed for {provider}: {text}"
        );
    }

    assert_eq!(
        macos_login_keychain_access(
            provider,
            Some(std::ffi::OsStr::new("/Users/test")),
            &resolved
        ),
        MacosLoginKeychainAccess::Allowed,
        "{provider} must report the user keychain as reachable"
    );
}

#[test]
fn compile_for_claude_reallows_user_keychain_read_after_the_default_deny() {
    // Claude Code's OAuth session lives in the macOS login keychain item
    // `Claude Code-credentials`, not in `~/.claude/.credentials.json`. With the
    // deny unqualified, every sandboxed Claude run on macOS died reporting an
    // expired OAuth session that no re-login could clear.
    assert_user_keychain_reallow_for("claude");
}

#[test]
fn compile_for_copilot_reallows_user_keychain_read_after_the_default_deny() {
    // Copilot CLI 1.0.84 keeps its only login in the keychain item
    // `github-copilot-app`. Denying `$HOME/Library/Keychains` reproduces
    // `No authentication information found` even when `/login` succeeded
    // unsandboxed. [ORB-12261]
    assert_user_keychain_reallow_for("copilot");
}

#[test]
fn compile_for_cursor_reallows_user_keychain_read_after_the_default_deny() {
    // Cursor Agent CLI defaults to the login keychain on darwin
    // (`cursor-access-token` / `cursor-refresh-token`). `$HOME/.cursor/auth.json`
    // is only the `AGENT_CLI_CREDENTIAL_STORE=file` opt-in. [ORB-12261]
    assert_user_keychain_reallow_for("cursor");
}

/// [ORB-10931] The clause order *is* the policy under SBPL last-match-wins, so
/// pin all three bands: default credential denies, then the provider carve-out,
/// then the activity's own negated `read` rules. An operator who denies a
/// credential path must not be silently overridden by the carve-out.
#[test]
fn compile_orders_activity_read_denies_after_the_provider_keychain_reallow() {
    let allow = "(allow file-read* (subpath \"/Users/test/Library/Keychains\"))";
    let default_deny = "(deny file-read* (subpath \"/Users/test/Library/Keychains\"))";

    // Both the exact keychain directory and a broader ancestor must win, for
    // every provider that receives the carve-out.
    for provider in ["claude", "copilot", "cursor"] {
        for activity_deny in [
            "!/Users/test/Library/Keychains",
            "!/Users/test/Library",
            "!/Users/test/Library/**",
        ] {
            let resolved = profile(
                "hardened",
                &["/Users/test/repo", activity_deny],
                &["/Users/test/repo/src"],
            );
            let text = compile_with_env(
                &resolved,
                provider,
                EnvOverrides {
                    home: Some("/Users/test"),
                    ..Default::default()
                },
            );

            let activity_clause = format!(
                "(deny file-read* (subpath \"{}\"))",
                activity_deny
                    .trim_start_matches('!')
                    .trim_end_matches("/**")
            );
            let default_deny_pos = text.find(default_deny).expect("default keychain read deny");
            let allow_pos = text
                .find(allow)
                .unwrap_or_else(|| panic!("missing {provider} keychain read re-allow: {text}"));
            let activity_pos = text.rfind(&activity_clause).unwrap_or_else(|| {
                panic!("missing activity read deny {activity_deny} for {provider}: {text}")
            });
            assert!(
                default_deny_pos < allow_pos && allow_pos < activity_pos,
                "order must be default deny -> provider re-allow -> activity deny for \
                 {provider} {activity_deny}: {text}"
            );

            assert_eq!(
                macos_login_keychain_access(
                    provider,
                    Some(std::ffi::OsStr::new("/Users/test")),
                    &resolved
                ),
                MacosLoginKeychainAccess::DeniedByActivityRule {
                    rule: activity_deny.to_string()
                },
                "the reported access must match the compiled clause order for {provider}"
            );
        }
    }
}

/// The narrowing above must not become the default: with no overlapping
/// activity deny, Claude keeps the OAuth read that ORB-10929 delivered.
#[test]
fn keychain_access_stays_allowed_without_an_overlapping_activity_deny() {
    let home = std::ffi::OsStr::new("/Users/test");
    let unrelated = profile(
        "default",
        &["/Users/test/repo", "!/Users/test/.ssh", "!/Users/other"],
        &["/Users/test/repo/src"],
    );
    for provider in ["claude", "copilot", "cursor"] {
        assert_eq!(
            macos_login_keychain_access(provider, Some(home), &unrelated),
            MacosLoginKeychainAccess::Allowed
        );
        assert_eq!(
            macos_login_keychain_access(provider, None, &unrelated),
            MacosLoginKeychainAccess::HomeUnresolved
        );
    }
    assert_eq!(
        macos_login_keychain_access("codex", Some(home), &unrelated),
        MacosLoginKeychainAccess::DeniedByDefaultPolicy
    );
    // A non-negated `read` entry naming the keychain is not a denial.
    let positive = profile(
        "default",
        &["/Users/test/Library/Keychains"],
        &["/Users/test/repo/src"],
    );
    for provider in ["claude", "copilot", "cursor"] {
        assert_eq!(
            macos_login_keychain_access(provider, Some(home), &positive),
            MacosLoginKeychainAccess::Allowed
        );
    }
}

#[test]
fn compile_for_providers_without_keychain_credentials_keeps_the_user_keychain_denied() {
    // The keychain grant is per-provider on purpose: it is the confined CLI's
    // own credential store, not a shared allowance. Codex, Grok, and Gemini
    // authenticate from files under their own state dirs and must never see
    // the keychain. Claude, Copilot, and Cursor are the exceptions, covered
    // above.
    let resolved = profile("default", &["/Users/test/repo"], &["/Users/test/repo/src"]);
    let allow = "(allow file-read* (subpath \"/Users/test/Library/Keychains\"))";
    for provider in ["codex", "grok", "gemini", "ollama", "not-a-provider", ""] {
        let text = compile_with_env(
            &resolved,
            provider,
            EnvOverrides {
                home: Some("/Users/test"),
                ..Default::default()
            },
        );
        assert!(
            text.contains("(deny file-read* (subpath \"/Users/test/Library/Keychains\"))"),
            "provider {provider} must keep the user keychain read deny: {text}"
        );
        assert!(
            !text.contains(allow),
            "provider {provider} must not receive the keychain read re-allow: {text}"
        );
    }
}

#[test]
fn compile_for_keychain_backed_providers_without_home_emits_no_keychain_reallow() {
    // Without HOME there is no path to re-allow. The profile must not fall back
    // to a broader clause; the run fails the same way it did before instead.
    let resolved = profile("default", &["/Users/test/repo"], &["/Users/test/repo/src"]);
    for provider in ["claude", "copilot", "cursor"] {
        let text = compile_with_env(&resolved, provider, EnvOverrides::default());
        assert!(
            !text
                .lines()
                .any(|line| line.starts_with("(allow file-read*") && line.contains("Keychains")),
            "no keychain read allow may be emitted without HOME for {provider}: {text}"
        );
    }
}

#[test]
fn compile_grants_write_access_to_global_orbit_log_dir() {
    // The agent CLI inherits the sandbox into `orbit mcp serve` and any
    // other `orbit ...` calls. The JSONL tracing layer resolves its
    // HOME-based path before runtime root resolution, so only the log
    // directory is granted here; store and artifact roots are appended by
    // the runtime sandbox resolver.
    let resolved = profile("default", &["/Users/test/repo"], &["/Users/test/repo/src"]);
    let text = compile_with_env(
        &resolved,
        NEUTRAL_PROVIDER,
        EnvOverrides {
            home: Some("/Users/test"),
            ..Default::default()
        },
    );
    assert!(
        text.contains("(allow file-write* (subpath \"/Users/test/.orbit/state/logs\"))"),
        "missing ~/.orbit/state/logs write allow: {text}"
    );
    assert!(
        !text.contains("(allow file-write* (subpath \"/Users/test/.orbit\"))"),
        "profile must not broadly allow HOME/.orbit writes: {text}"
    );
}

#[test]
fn compile_with_env_does_not_mutate_process_home() {
    let home_before = std::env::var_os("HOME");
    let resolved = profile("default", &["/Users/test/repo"], &["/Users/test/repo/src"]);
    let text = compile_with_env(
        &resolved,
        NEUTRAL_PROVIDER,
        EnvOverrides {
            home: Some("/Users/test"),
            ..Default::default()
        },
    );
    assert!(
        text.contains("(allow file-write* (subpath \"/Users/test/.orbit/state/logs\"))"),
        "missing injected HOME/.orbit/state/logs write allow: {text}"
    );
    assert_eq!(
        std::env::var_os("HOME"),
        home_before,
        "profile compilation tests must not mutate process HOME"
    );
}

#[test]
fn compile_allows_macos_sandbox_provenance_syscall() {
    let resolved = profile("default", &["/Users/test/repo"], &["/Users/test/repo/src"]);
    let text = compile_with_env(
        &resolved,
        NEUTRAL_PROVIDER,
        EnvOverrides {
            home: Some("/Users/test"),
            ..Default::default()
        },
    );
    assert!(
        text.contains("(allow system-mac-syscall (mac-policy-name \"vnguard\"))"),
        "missing vnguard mac syscall allow: {text}"
    );
    assert!(
        text.contains(
            "(allow system-mac-syscall (require-all (mac-policy-name \"Sandbox\") (mac-syscall-number 67)))"
        ),
        "missing Sandbox mac syscall allow: {text}"
    );
}
#[cfg(target_os = "macos")]
use super::super::compile_macos_sandbox_profile;
#[cfg(target_os = "macos")]
use orbit_types::policy::ResolvedFsProfile;

#[cfg(target_os = "macos")]
#[test]
fn compiled_profile_lets_keychain_backed_providers_read_the_user_keychain_directory() {
    // Kernel-level complement to the profile-text assertions: prove the
    // last-match-wins ordering actually resolves the way the clauses read. A
    // synthetic HOME stands in for the real login keychain so the test never
    // touches the operator's credentials.
    if !sandbox_exec_can_apply() {
        return;
    }

    let fixture = SyntheticKeychainHome::create("keychain-read");
    let resolved = ResolvedFsProfile {
        name: "default".to_string(),
        read: vec![fixture.home_text()],
        modify: vec![],
    };

    for (provider, should_read) in [
        ("claude", true),
        ("copilot", true),
        ("cursor", true),
        ("codex", false),
    ] {
        assert_eq!(
            fixture.credential_readable(&resolved, provider),
            should_read,
            "provider {provider} keychain read should_succeed={should_read}"
        );
    }
}

/// [ORB-10931] The kernel-level half of the ordering contract: an activity that
/// denies the keychain directory — or an ancestor of it — must actually lose
/// Claude the read, while a profile without such a rule keeps it. Asserted
/// through `sandbox-exec` because clause ordering is only a claim until the
/// kernel resolves it.
#[cfg(target_os = "macos")]
#[test]
fn compiled_profile_honors_an_activity_keychain_deny_for_keychain_backed_providers() {
    if !sandbox_exec_can_apply() {
        return;
    }

    let fixture = SyntheticKeychainHome::create("keychain-deny");
    let home_text = fixture.home_text();

    let default_allow = ResolvedFsProfile {
        name: "default".to_string(),
        read: vec![home_text.clone()],
        modify: vec![],
    };
    for provider in ["claude", "copilot", "cursor"] {
        assert!(
            fixture.credential_readable(&default_allow, provider),
            "without an overlapping deny, {provider} keeps its keychain read"
        );

        for deny in [
            format!("!{home_text}/Library/Keychains"),
            format!("!{home_text}/Library"),
        ] {
            let hardened = ResolvedFsProfile {
                name: "hardened".to_string(),
                read: vec![home_text.clone(), deny.clone()],
                modify: vec![],
            };
            assert!(
                !fixture.credential_readable(&hardened, provider),
                "activity rule {deny} must deny {provider} the keychain read"
            );
            assert_eq!(
                macos_login_keychain_access(
                    provider,
                    Some(std::ffi::OsStr::new(&home_text)),
                    &hardened
                ),
                MacosLoginKeychainAccess::DeniedByActivityRule { rule: deny.clone() },
                "the reported access must match what the kernel enforced for {provider} {deny}"
            );
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn compiled_codex_profile_reads_public_ca_material_but_not_private_credentials() {
    if !sandbox_exec_can_apply() {
        return;
    }

    let fixture = SyntheticKeychainHome::create("codex-ca-access");
    let ssh = fixture.home.path().join(".ssh");
    std::fs::create_dir_all(&ssh).expect("synthetic ssh directory");
    let private_key = ssh.join("id_fixture");
    std::fs::write(&private_key, "private fixture").expect("write synthetic private key");
    // `/etc` is an alias of `/private/etc` on macOS. `sandbox-exec` evaluates
    // file predicates against the resolved path, so use the same canonical
    // spelling for the probe and its explicit deny. This direct compiler test
    // deliberately supplies already-resolved absolute rules, as production's
    // policy resolver does for workspace-relative policy entries.
    let public_ca = std::path::Path::new("/etc/ssl/cert.pem")
        .canonicalize()
        .expect("canonicalize macOS public CA bundle");
    assert!(public_ca.is_file(), "macOS public CA bundle must exist");

    let resolved = ResolvedFsProfile {
        name: "default".to_string(),
        read: vec![fixture.home_text()],
        modify: vec![],
    };
    let profile_text = compile_with_env(
        &resolved,
        "codex",
        EnvOverrides {
            home: Some(&fixture.home_text()),
            ..Default::default()
        },
    );

    assert!(
        can_read_under_profile(&profile_text, &public_ca),
        "Codex must be able to read the public CA file selected by its child environment"
    );
    for private in [&fixture.credential, &private_key] {
        assert!(
            !can_read_under_profile(&profile_text, private),
            "private credential must stay denied: {}",
            private.display()
        );
    }

    let denied = ResolvedFsProfile {
        name: "deny-public-ca".to_string(),
        read: vec![fixture.home_text(), format!("!{}", public_ca.display())],
        modify: vec![],
    };
    let denied_profile = compile_with_env(
        &denied,
        "codex",
        EnvOverrides {
            home: Some(&fixture.home_text()),
            ..Default::default()
        },
    );
    assert!(
        !can_read_under_profile(&denied_profile, &public_ca),
        "an explicit denyRead must still outrank the public CA default"
    );
}

/// A disposable `$HOME` holding a stand-in login keychain, so keychain tests
/// exercise the real clause set without touching operator credentials.
#[cfg(target_os = "macos")]
struct SyntheticKeychainHome {
    // Declaration order is drop order: the tempdir must go before the guard
    // that removes its parent.
    home: tempfile::TempDir,
    _cleanup: ScopeGuard,
    credential: std::path::PathBuf,
}

#[cfg(target_os = "macos")]
impl SyntheticKeychainHome {
    fn create(label: &str) -> Self {
        let parent = sandbox_test_parent(label);
        let cleanup = ScopeGuard(parent.clone());
        let home = tempfile::Builder::new()
            .prefix("synthetic-home-")
            .tempdir_in(&parent)
            .expect("synthetic home tempdir");
        let keychains = home.path().join("Library/Keychains");
        std::fs::create_dir_all(&keychains).expect("synthetic keychain dir");
        let credential = keychains.join("login.keychain-db");
        std::fs::write(&credential, b"synthetic-credential").expect("write synthetic credential");
        Self {
            home,
            _cleanup: cleanup,
            credential,
        }
    }

    fn home_text(&self) -> String {
        self.home.path().to_string_lossy().into_owned()
    }

    fn credential_readable(&self, resolved: &ResolvedFsProfile, provider: &str) -> bool {
        let home = self.home_text();
        let profile_text = compile_with_env(
            resolved,
            provider,
            EnvOverrides {
                home: Some(&home),
                ..Default::default()
            },
        );
        can_read_under_profile(&profile_text, &self.credential)
    }
}

#[cfg(target_os = "macos")]
#[test]
fn compiled_profile_allows_nested_orbit_runtime_writes_without_home_orbit_reallow() {
    use std::process::Command;

    if !sandbox_exec_can_apply() {
        return;
    }

    let parent = sandbox_test_parent("orbit-runtime-roots");
    let _cleanup = ScopeGuard(parent.clone());
    let home = parent.join("home");
    let global = home.join(".orbit");
    let workspace = parent.join("repo/.orbit");
    std::fs::create_dir_all(global.join("state/logs")).expect("global log dir");
    std::fs::create_dir_all(global.join("tasks")).expect("global tasks dir");
    std::fs::create_dir_all(workspace.join("state")).expect("workspace state dir");
    std::fs::create_dir_all(workspace.join("adrs/.locks")).expect("workspace adr locks dir");

    let log_path = global.join("state/logs/orbit.jsonl");
    let db_wal_path = global.join("orbit.db-wal");
    let artifact_path = global
        .join("tasks/workspaces/orbit-test/ORB-00009/artifacts/files/reports")
        .join("planner_a.md");
    let global_audit_path = global.join("state/audit/v2_loop/nested.jsonl");
    let workspace_audit_path = workspace.join("state/audit/blobs/aa/placeholder");
    let id_alloc_lock_path = workspace.join("state/.id_alloc.lock");
    let semantic_wal_path = workspace.join("state/semantic.db-wal");
    let denied_path = global.join("not-allowed.txt");
    let denied_workspace_path = workspace.join("adrs/.locks/should-stay-denied.lock");

    let resolved = ResolvedFsProfile {
        name: "gemini-direct-agent".to_string(),
        read: vec![parent.display().to_string()],
        modify: vec![
            format!("{}/state/logs/**", global.display()),
            format!("{}/state/audit/**", global.display()),
            format!("{}/orbit.db*", global.display()),
            format!("{}/tasks/**", global.display()),
            format!("!{}/**", workspace.display()),
            format!("{}/state/audit/**", workspace.display()),
            format!("{}/state/.id_alloc.lock", workspace.display()),
            format!("{}/state/semantic.db*", workspace.display()),
        ],
    };
    let home_str = home.to_string_lossy().into_owned();
    let profile_text = compile_with_env(
        &resolved,
        NEUTRAL_PROVIDER,
        EnvOverrides {
            home: Some(&home_str),
            ..Default::default()
        },
    );
    let mut profile_file = tempfile::Builder::new()
        .prefix("orbit-sandbox-test-")
        .suffix(".sb")
        .tempfile()
        .expect("tempfile");
    use std::io::Write;
    profile_file
        .write_all(profile_text.as_bytes())
        .expect("write profile");
    profile_file.flush().expect("flush");

    let script = format!(
        "set -e\n: > {}\n: > {}\nmkdir -p {}\nprintf '%s\\n' '*authored by: gemini / gemini-3.1-pro*' > {}\nmkdir -p {}\n: > {}\nmkdir -p {}\n: > {}\n: > {}\n: > {}\nif : > {} 2>/dev/null; then exit 99; fi\nif : > {} 2>/dev/null; then exit 98; fi\n",
        shell_escape(&log_path),
        shell_escape(&db_wal_path),
        shell_escape(artifact_path.parent().expect("artifact parent")),
        shell_escape(&artifact_path),
        shell_escape(global_audit_path.parent().expect("global audit parent")),
        shell_escape(&global_audit_path),
        shell_escape(
            workspace_audit_path
                .parent()
                .expect("workspace audit parent")
        ),
        shell_escape(&workspace_audit_path),
        shell_escape(&id_alloc_lock_path),
        shell_escape(&semantic_wal_path),
        shell_escape(&denied_path),
        shell_escape(&denied_workspace_path),
    );
    let status = Command::new(sandbox_exec_path_for_test())
        .arg("-f")
        .arg(profile_file.path())
        .arg("/bin/sh")
        .arg("-c")
        .arg(script)
        .env("HOME", &home)
        .status()
        .expect("run sandbox-exec");

    assert!(
        status.success(),
        "expected Orbit runtime writes to succeed while arbitrary HOME/.orbit write is denied; status={status:?}"
    );
    assert!(log_path.exists(), "log file should be writable");
    assert!(db_wal_path.exists(), "SQLite sidecar should be writable");
    assert!(
        artifact_path.exists(),
        "planner artifact should be writable"
    );
    assert!(
        global_audit_path.exists(),
        "global audit store should be writable"
    );
    assert!(
        workspace_audit_path.exists(),
        "workspace audit store should be writable"
    );
    assert!(
        id_alloc_lock_path.exists(),
        "workspace id allocator lock should be writable"
    );
    assert!(
        semantic_wal_path.exists(),
        "semantic sidecar should be writable"
    );
    assert!(
        !denied_path.exists(),
        "arbitrary HOME/.orbit write should remain denied"
    );
    assert!(
        !denied_workspace_path.exists(),
        "unrelated workspace .orbit write should remain denied"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn compiled_profile_blocks_writes_outside_modify_scope() {
    use std::process::Command;

    if !sandbox_exec_can_apply() {
        return;
    }

    // The compiled profile broadly allows writes under /tmp,
    // /private/tmp, /private/var/folders, and ~/Library/Caches so
    // agent CLIs can use scratch space. To exercise modify-scope
    // enforcement we need a parent that lives outside all of those.
    let parent = sandbox_test_parent("modify-scope");
    let _cleanup = ScopeGuard(parent.clone());
    let dir = tempfile::Builder::new()
        .prefix("compile-")
        .tempdir_in(&parent)
        .expect("tempdir in parent");
    let allowed = dir.path().join("allowed");
    let blocked = dir.path().join("blocked");
    std::fs::create_dir_all(&allowed).expect("allowed dir");
    std::fs::create_dir_all(&blocked).expect("blocked dir");

    let resolved = ResolvedFsProfile {
        name: "default".to_string(),
        read: vec![dir.path().display().to_string()],
        modify: vec![allowed.display().to_string()],
    };
    let profile_text =
        compile_macos_sandbox_profile(&resolved, NEUTRAL_PROVIDER).expect("compile sbpl");
    let mut profile_file = tempfile::Builder::new()
        .prefix("orbit-sandbox-test-")
        .suffix(".sb")
        .tempfile()
        .expect("tempfile");
    use std::io::Write;
    profile_file
        .write_all(profile_text.as_bytes())
        .expect("write profile");
    profile_file.flush().expect("flush");

    let allowed_target = allowed.join("ok");
    let allow_status = Command::new(sandbox_exec_path_for_test())
        .arg("-f")
        .arg(profile_file.path())
        .arg("/bin/sh")
        .arg("-c")
        .arg(format!("echo ok > {}", shell_escape(&allowed_target)))
        .status()
        .expect("run sandbox-exec");
    assert!(
        allow_status.success(),
        "expected write inside modify scope to succeed; status={allow_status:?}"
    );
    assert!(
        allowed_target.exists(),
        "allowed file was not written: {allowed_target:?}"
    );

    let blocked_target = blocked.join("nope");
    let deny_status = Command::new(sandbox_exec_path_for_test())
        .arg("-f")
        .arg(profile_file.path())
        .arg("/bin/sh")
        .arg("-c")
        .arg(format!("echo bad > {}", shell_escape(&blocked_target)))
        .status()
        .expect("run sandbox-exec");
    assert!(
        !deny_status.success(),
        "expected write outside modify scope to fail; status={deny_status:?}"
    );
    assert!(
        !blocked_target.exists(),
        "blocked file should not exist: {blocked_target:?}"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn compiled_profile_denies_reads_to_negated_read_path() {
    // Invariant: an SBPL profile compiled from `read: [base, !secrets]`
    // must let the kernel block reads of `secrets/...` while still
    // allowing reads of sibling paths under `base`. This is the
    // runtime complement to `compile_emits_explicit_read_deny_for_negated_read_rule`.
    use std::process::Command;

    if !sandbox_exec_can_apply() {
        return;
    }

    let parent = sandbox_test_parent("read-deny");
    let _cleanup = ScopeGuard(parent.clone());
    let dir = tempfile::Builder::new()
        .prefix("compile-readdeny-")
        .tempdir_in(&parent)
        .expect("tempdir in parent");
    let secrets_dir = dir.path().join("secrets");
    std::fs::create_dir_all(&secrets_dir).expect("secrets dir");
    let secret_path = secrets_dir.join("api.key");
    std::fs::write(&secret_path, b"top-secret").expect("write secret");
    let public_path = dir.path().join("public.txt");
    std::fs::write(&public_path, b"public-data").expect("write public");

    let resolved = ResolvedFsProfile {
        name: "default".to_string(),
        read: vec![
            dir.path().display().to_string(),
            format!("!{}", secrets_dir.display()),
        ],
        modify: vec![],
    };
    let profile_text =
        compile_macos_sandbox_profile(&resolved, NEUTRAL_PROVIDER).expect("compile sbpl");
    let mut profile_file = tempfile::Builder::new()
        .prefix("orbit-sandbox-test-")
        .suffix(".sb")
        .tempfile()
        .expect("tempfile");
    use std::io::Write;
    profile_file
        .write_all(profile_text.as_bytes())
        .expect("write profile");
    profile_file.flush().expect("flush");

    // Allowed read of public_path succeeds.
    let allow_status = Command::new(sandbox_exec_path_for_test())
        .arg("-f")
        .arg(profile_file.path())
        .arg("/bin/sh")
        .arg("-c")
        .arg(format!("cat {}", shell_escape(&public_path)))
        .status()
        .expect("run sandbox-exec");
    assert!(
        allow_status.success(),
        "public read should be allowed; status={allow_status:?}"
    );

    // Denied read of secret_path fails.
    let deny_status = Command::new(sandbox_exec_path_for_test())
        .arg("-f")
        .arg(profile_file.path())
        .arg("/bin/sh")
        .arg("-c")
        .arg(format!("cat {}", shell_escape(&secret_path)))
        .status()
        .expect("run sandbox-exec");
    assert!(
        !deny_status.success(),
        "secrets read should be denied by negated read rule; status={deny_status:?}"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn compiled_profile_for_realistic_agent_loop_profile_allows_repo_writes_denies_dotenv() {
    // Realistic activity profile boundary test (AC #2). Synthesize an
    // `agent_loop`-style profile: read=[repo], modify=[repo, !repo/.env].
    // Exercise allow + deny in one process: writing `repo/src/foo.rs`
    // succeeds; writing `repo/.env` fails. Mirrors how an `agent_loop`
    // step would be sandboxed at runtime.
    use std::process::Command;

    if !sandbox_exec_can_apply() {
        return;
    }

    let parent = sandbox_test_parent("agent-loop-realistic");
    let _cleanup = ScopeGuard(parent.clone());
    let repo = tempfile::Builder::new()
        .prefix("agent-loop-")
        .tempdir_in(&parent)
        .expect("repo tempdir");
    let src_dir = repo.path().join("src");
    std::fs::create_dir_all(&src_dir).expect("src dir");

    let resolved = ResolvedFsProfile {
        name: "agent_loop".to_string(),
        read: vec![repo.path().display().to_string()],
        modify: vec![
            repo.path().display().to_string(),
            format!("!{}/.env", repo.path().display()),
        ],
    };
    let profile_text =
        compile_macos_sandbox_profile(&resolved, NEUTRAL_PROVIDER).expect("compile sbpl");
    let mut profile_file = tempfile::Builder::new()
        .prefix("orbit-sandbox-test-")
        .suffix(".sb")
        .tempfile()
        .expect("tempfile");
    use std::io::Write;
    profile_file
        .write_all(profile_text.as_bytes())
        .expect("write profile");
    profile_file.flush().expect("flush");

    let source_target = src_dir.join("foo.rs");
    let env_target = repo.path().join(".env");

    let source_status = Command::new(sandbox_exec_path_for_test())
        .arg("-f")
        .arg(profile_file.path())
        .arg("/bin/sh")
        .arg("-c")
        .arg(format!(
            "echo 'fn main() {{}}' > {}",
            shell_escape(&source_target)
        ))
        .status()
        .expect("run sandbox-exec");
    assert!(
        source_status.success(),
        "agent_loop must be able to write source files; status={source_status:?}"
    );
    assert!(source_target.exists(), "source file not written");

    let env_status = Command::new(sandbox_exec_path_for_test())
        .arg("-f")
        .arg(profile_file.path())
        .arg("/bin/sh")
        .arg("-c")
        .arg(format!("echo 'KEY=secret' > {}", shell_escape(&env_target)))
        .status()
        .expect("run sandbox-exec");
    assert!(
        !env_status.success(),
        "agent_loop must be blocked from writing .env; status={env_status:?}"
    );
    assert!(!env_target.exists(), ".env should not have been written");
}

#[cfg(target_os = "macos")]
#[test]
fn compiled_profile_denies_env_glob_without_blocking_other_writes() {
    use std::process::Command;

    if !sandbox_exec_can_apply() {
        return;
    }

    let parent = sandbox_test_parent("env-glob");
    let _cleanup = ScopeGuard(parent.clone());
    let dir = tempfile::Builder::new()
        .prefix("compile-env-")
        .tempdir_in(&parent)
        .expect("tempdir in parent");

    let resolved = ResolvedFsProfile {
        name: "default".to_string(),
        read: vec![dir.path().display().to_string()],
        modify: vec![
            dir.path().display().to_string(),
            format!("!{}/**/*.env", dir.path().display()),
        ],
    };
    let profile_text =
        compile_macos_sandbox_profile(&resolved, NEUTRAL_PROVIDER).expect("compile sbpl");
    let mut profile_file = tempfile::Builder::new()
        .prefix("orbit-sandbox-test-")
        .suffix(".sb")
        .tempfile()
        .expect("tempfile");
    use std::io::Write;
    profile_file
        .write_all(profile_text.as_bytes())
        .expect("write profile");
    profile_file.flush().expect("flush");

    let allowed_target = dir.path().join("ok.txt");
    let allow_status = Command::new(sandbox_exec_path_for_test())
        .arg("-f")
        .arg(profile_file.path())
        .arg("/bin/sh")
        .arg("-c")
        .arg(format!("echo ok > {}", shell_escape(&allowed_target)))
        .status()
        .expect("run sandbox-exec");
    assert!(
        allow_status.success(),
        "env glob deny should not block non-env writes; status={allow_status:?}"
    );

    let env_target = dir.path().join("blocked.env");
    let deny_status = Command::new(sandbox_exec_path_for_test())
        .arg("-f")
        .arg(profile_file.path())
        .arg("/bin/sh")
        .arg("-c")
        .arg(format!("echo bad > {}", shell_escape(&env_target)))
        .status()
        .expect("run sandbox-exec");
    assert!(
        !deny_status.success(),
        "expected env glob write to fail; status={deny_status:?}"
    );
    assert!(
        !env_target.exists(),
        "env file should not exist: {env_target:?}"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn compiled_profile_with_mid_path_glob_rule_is_accepted_by_sandbox_exec() {
    // Regression for ORB-00372. A modify rule with a mid-path glob — like the
    // default `orbit.db*` / `semantic.db*` SQLite-sidecar rules emitted on
    // every run — takes the glob->regex path in `glob_rule_to_regex`. The
    // emitted regex must compile under real `sandbox-exec`. Prefixing it with
    // the Perl `(?i)` inline flag made sandbox-exec reject the whole profile
    // with "unexpected ^ operator in middle of expression" (exit 65,
    // EX_DATAERR), killing every macOS CLI run before the agent started.
    use std::process::Command;

    if !sandbox_exec_can_apply() {
        return;
    }

    let resolved = ResolvedFsProfile {
        name: "default".to_string(),
        read: vec!["/Users/test/repo".to_string()],
        modify: vec![
            "/Users/test/.orbit/orbit.db*".to_string(),
            "/Users/test/repo/.orbit/state/semantic.db*".to_string(),
            "/Users/test/repo/**/*.env".to_string(),
        ],
    };
    let profile_text =
        compile_macos_sandbox_profile(&resolved, NEUTRAL_PROVIDER).expect("compile sbpl");
    assert!(
        !profile_text.contains("(?i)"),
        "compiled profile must not contain the unsupported (?i) inline flag: {profile_text}"
    );

    let mut profile_file = tempfile::Builder::new()
        .prefix("orbit-sandbox-glob-")
        .suffix(".sb")
        .tempfile()
        .expect("tempfile");
    use std::io::Write;
    profile_file
        .write_all(profile_text.as_bytes())
        .expect("write profile");
    profile_file.flush().expect("flush");

    let output = Command::new(sandbox_exec_path_for_test())
        .arg("-f")
        .arg(profile_file.path())
        .arg("/usr/bin/true")
        .output()
        .expect("run sandbox-exec");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("unexpected ^ operator"),
        "sandbox-exec rejected the compiled profile regex: {stderr}"
    );
    // Exit 65 (EX_DATAERR) is sandbox-exec's profile-compile failure. Any other
    // outcome — success, or a runtime denial — means the profile compiled.
    assert_ne!(
        output.status.code(),
        Some(65),
        "sandbox-exec failed to compile the mid-path-glob profile (exit 65); stderr: {stderr}"
    );
}
