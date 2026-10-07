use super::super::compile::{MacosLoginKeychainAccess, macos_login_keychain_access};
#[cfg(target_os = "macos")]
use super::super::compile_macos_sandbox_profile;
#[cfg(target_os = "macos")]
use super::super::test_support::*;

use orbit_types::policy::ResolvedFsProfile;

#[test]
fn keychain_access_diagnostic_grants_antigravity_and_keeps_unknown_providers_denied() {
    let resolved = ResolvedFsProfile {
        name: "default".to_string(),
        read: vec!["/Users/test".to_string()],
        modify: vec![],
    };
    let home = std::ffi::OsStr::new("/Users/test");

    assert_eq!(
        macos_login_keychain_access("antigravity", Some(home), &resolved),
        MacosLoginKeychainAccess::Allowed,
        "Antigravity's login-keychain diagnostic must report its provider carve-out"
    );
    assert_eq!(
        macos_login_keychain_access("future-provider", Some(home), &resolved),
        MacosLoginKeychainAccess::DeniedByDefaultPolicy,
        "unknown providers must retain the fail-closed keychain policy"
    );
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
    for provider in ["claude", "copilot", "cursor", "antigravity"] {
        assert_eq!(
            macos_login_keychain_access(
                provider,
                Some(std::ffi::OsStr::new(&home_text)),
                &default_allow
            ),
            MacosLoginKeychainAccess::Allowed,
            "the access diagnostic must report the compiled grant for {provider}"
        );
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
fn compiled_profile_keeps_unknown_provider_keychain_access_denied() {
    if !sandbox_exec_can_apply() {
        return;
    }

    let fixture = SyntheticKeychainHome::create("unknown-keychain-provider");
    let resolved = ResolvedFsProfile {
        name: "default".to_string(),
        read: vec![fixture.home_text()],
        modify: vec![],
    };

    assert!(
        !fixture.credential_readable(&resolved, "future-provider"),
        "an unknown provider must retain the default user keychain deny"
    );
    assert_eq!(
        macos_login_keychain_access(
            "future-provider",
            Some(std::ffi::OsStr::new(&fixture.home_text())),
            &resolved
        ),
        MacosLoginKeychainAccess::DeniedByDefaultPolicy,
        "unknown providers must fail closed in the access diagnostic"
    );
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

/// [ORB-12469] Kernel-level complement to the profile-text assertions: prove
/// the compiled clauses actually resolve into a writable download cache, an
/// unwritable `bin`, and an unreadable publish token. A synthetic
/// `$CARGO_HOME` stands in for the operator's real one, so nothing here
/// touches the host registry.
#[cfg(target_os = "macos")]
#[test]
fn compiled_profile_makes_the_cargo_download_caches_writable_but_not_bin_or_the_token() {
    use std::process::Command;

    if !sandbox_exec_can_apply() {
        return;
    }

    // The broad `/tmp` and `~/Library/Caches` write allows would mask the
    // grant under test, so the synthetic cargo home has to live outside them.
    let parent = sandbox_test_parent("cargo-cache");
    let _cleanup = ScopeGuard(parent.clone());
    let dir = tempfile::Builder::new()
        .prefix("compile-cargo-")
        .tempdir_in(&parent)
        .expect("tempdir in parent");
    let cargo_home = dir.path().join("cargo");
    let workspace = dir.path().join("workspace");
    // The layout any host that has fetched once already has.
    for subdir in ["registry", "git", "bin"] {
        std::fs::create_dir_all(cargo_home.join(subdir)).expect("cargo home subdir");
    }
    std::fs::create_dir_all(&workspace).expect("workspace dir");
    std::fs::write(cargo_home.join(".package-cache"), b"").expect("write package cache lock");
    let token = cargo_home.join("credentials.toml");
    std::fs::write(&token, b"token = \"synthetic\"\n").expect("write token");

    let read_root = dir.path().display().to_string();
    let modify_root = workspace.display().to_string();
    let resolved = profile("implementer", &[&read_root], &[&modify_root]);
    let cargo_home_text = cargo_home.display().to_string();
    let profile_text = compile_with_env(
        &resolved,
        NEUTRAL_PROVIDER,
        EnvOverrides {
            cargo_home: Some(&cargo_home_text),
            ..Default::default()
        },
    );
    let mut profile_file = tempfile::Builder::new()
        .prefix("orbit-sandbox-cargo-")
        .suffix(".sb")
        .tempfile()
        .expect("tempfile");
    use std::io::Write;
    profile_file
        .write_all(profile_text.as_bytes())
        .expect("write profile");
    profile_file.flush().expect("flush");

    let run = |script: String| {
        Command::new(sandbox_exec_path_for_test())
            .arg("-f")
            .arg(profile_file.path())
            .arg("/bin/sh")
            .arg("-c")
            .arg(script)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("run sandbox-exec")
            .success()
    };

    // What `cargo fetch` does: land the `.crate`, unpack it, clone a git
    // dependency, and take the package-cache lock.
    for writable in [
        cargo_home.join("registry/cache/index.example/lebe-0.5.3.crate"),
        cargo_home.join("registry/src/index.example/lebe-0.5.3/lib.rs"),
        cargo_home.join("git/db/marker"),
    ] {
        let created = run(format!(
            "mkdir -p {} && echo ok > {}",
            shell_escape(writable.parent().expect("cache parent")),
            shell_escape(&writable)
        ));
        assert!(
            created && writable.exists(),
            "a sandboxed build must be able to write {}",
            writable.display()
        );
    }
    assert!(
        run(format!(
            "echo lock > {}",
            shell_escape(&cargo_home.join(".package-cache"))
        )),
        "cargo's package-cache lock must be takeable"
    );

    // What the grant must not reach: the installed toolchain, the cargo home
    // itself, and the publish token beside the caches.
    for blocked in [cargo_home.join("bin/cargo"), cargo_home.join("config.toml")] {
        assert!(
            !run(format!("echo bad > {}", shell_escape(&blocked))),
            "{} must stay read-only",
            blocked.display()
        );
        assert!(
            !blocked.exists(),
            "{} must not have been created",
            blocked.display()
        );
    }
    assert!(
        !run(format!("cat {}", shell_escape(&token))),
        "the crates.io publish token must stay unreadable"
    );
}

/// The same boundary under the real `sandbox-exec`: the plugin's own state
/// subtree is readable and listable, a sibling namespace and the tree holding
/// both are not.
#[cfg(target_os = "macos")]
#[test]
fn read_boundary_keeps_a_sibling_state_namespace_unreadable_under_sandbox_exec() {
    if !sandbox_exec_can_apply() {
        return;
    }
    let parent = sandbox_test_parent("plugin-state");
    let _cleanup = ScopeGuard(parent.clone());
    let root = parent.canonicalize().expect("canonical test parent");
    let plugins = root.join("state/plugins");
    for name in ["demo", "other"] {
        std::fs::create_dir_all(plugins.join(name)).expect("plugin state");
        std::fs::write(plugins.join(name).join("secret"), name).expect("secret");
    }
    let resolved = profile("plugin", &[], &[]);
    let mut text = compile_with_env(&resolved, NEUTRAL_PROVIDER, EnvOverrides::default());
    super::super::append_macos_read_boundary(
        &mut text,
        std::slice::from_ref(&plugins),
        &[plugins.join("demo")],
        &[],
    );
    let lists = |path: &std::path::Path| {
        use std::io::Write;
        let mut profile_file = tempfile::Builder::new()
            .suffix(".sb")
            .tempfile()
            .expect("tempfile");
        profile_file
            .write_all(text.as_bytes())
            .expect("write profile");
        std::process::Command::new(sandbox_exec_path_for_test())
            .arg("-f")
            .arg(profile_file.path())
            .arg("/bin/ls")
            .arg(path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("run sandbox-exec")
            .success()
    };

    assert!(can_read_under_profile(&text, &plugins.join("demo/secret")));
    assert!(lists(&plugins.join("demo")));
    assert!(!can_read_under_profile(
        &text,
        &plugins.join("other/secret")
    ));
    assert!(!lists(&plugins.join("other")));
    assert!(!lists(&plugins));
}

/// Kernel-level half: under a profile that grants writes to the whole global
/// root, the masked trees still refuse reads, listings and writes.
#[cfg(target_os = "macos")]
#[test]
fn subpath_mask_hides_the_tree_from_a_sandboxed_child() {
    if !sandbox_exec_can_apply() {
        return;
    }

    let parent = sandbox_test_parent("plugin-mask");
    let _cleanup = ScopeGuard(parent.clone());
    let global = parent.join("global");
    let masked = global.join("state/plugins");
    let visible = global.join("state/other");
    std::fs::create_dir_all(&masked).expect("masked tree");
    std::fs::create_dir_all(&visible).expect("visible tree");
    std::fs::write(masked.join("secret"), b"hidden").expect("masked file");
    std::fs::write(visible.join("note"), b"shown").expect("visible file");
    let global_text = global.display().to_string();
    let resolved = ResolvedFsProfile {
        name: "agent".to_string(),
        read: vec![global_text.clone()],
        modify: vec![format!("{global_text}/**")],
    };
    let mut text = compile_with_env(&resolved, NEUTRAL_PROVIDER, EnvOverrides::default());
    super::super::append_macos_subpath_mask(&mut text, std::slice::from_ref(&masked));

    assert!(can_read_under_profile(&text, &visible.join("note")));
    assert!(!can_read_under_profile(&text, &masked.join("secret")));
    for probe in [
        format!("ls {}", shell_escape(&masked)),
        format!("echo x > {}", shell_escape(&masked.join("planted"))),
    ] {
        assert!(
            !shell_succeeds_under_profile(&text, &probe),
            "`{probe}` must fail under the mask"
        );
    }
    assert!(!masked.join("planted").exists());
}

#[cfg(target_os = "macos")]
fn shell_succeeds_under_profile(profile_text: &str, script: &str) -> bool {
    use std::io::Write;

    let mut profile_file = tempfile::Builder::new()
        .prefix("orbit-sandbox-mask-")
        .suffix(".sb")
        .tempfile()
        .expect("tempfile");
    profile_file
        .write_all(profile_text.as_bytes())
        .expect("write profile");
    profile_file.flush().expect("flush");
    std::process::Command::new(sandbox_exec_path_for_test())
        .arg("-f")
        .arg(profile_file.path())
        .arg("/bin/sh")
        .arg("-c")
        .arg(script)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("run sandbox-exec")
        .success()
}
