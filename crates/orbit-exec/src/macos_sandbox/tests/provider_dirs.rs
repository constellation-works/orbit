use super::super::provider_dirs::*;
use super::super::test_support::*;
use std::ffi::OsStr;
use std::path::PathBuf;

#[cfg(target_os = "macos")]
use super::super::compile::{SandboxCompileEnv, compile_macos_sandbox_profile_with_env};
#[cfg(target_os = "macos")]
use orbit_types::policy::ResolvedFsProfile;
#[test]
fn compile_grants_write_access_to_codex_home_when_set() {
    let resolved = profile("default", &["/Users/test/repo"], &["/Users/test/repo/src"]);
    let text = compile_with_env(
        &resolved,
        NEUTRAL_PROVIDER,
        EnvOverrides {
            home: Some("/Users/test"),
            codex_home: Some("/var/folders/test/codex-home"),
            ..Default::default()
        },
    );
    assert!(
        text.contains("(allow file-write* (subpath \"/var/folders/test/codex-home\"))"),
        "missing CODEX_HOME write allow: {text}"
    );
    assert!(
        !text.contains("(allow file-write* (subpath \"/Users/test/.codex\"))"),
        "CODEX_HOME should take precedence over HOME fallback: {text}"
    );
}

#[test]
fn compile_grants_write_access_to_home_codex_when_codex_home_missing() {
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
        text.contains("(allow file-write* (subpath \"/Users/test/.codex\"))"),
        "missing HOME/.codex write allow: {text}"
    );
}

#[test]
fn compile_grants_write_access_to_claude_config_dir_when_set() {
    let resolved = profile("default", &["/Users/test/repo"], &["/Users/test/repo/src"]);
    let text = compile_with_env(
        &resolved,
        NEUTRAL_PROVIDER,
        EnvOverrides {
            home: Some("/Users/test"),
            claude_config_dir: Some("/var/folders/test/claude-config"),
            ..Default::default()
        },
    );
    assert!(
        text.contains("(allow file-write* (subpath \"/var/folders/test/claude-config\"))"),
        "missing CLAUDE_CONFIG_DIR write allow: {text}"
    );
    assert!(
        !text.contains("(allow file-write* (subpath \"/Users/test/.claude\"))"),
        "CLAUDE_CONFIG_DIR should take precedence over HOME fallback: {text}"
    );
}

#[test]
fn compile_grants_write_access_to_home_claude_when_claude_config_dir_missing() {
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
        text.contains("(allow file-write* (subpath \"/Users/test/.claude\"))"),
        "missing HOME/.claude write allow: {text}"
    );
}

#[test]
fn compile_grants_write_access_to_home_claude_json_when_claude_config_dir_missing() {
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
        text.contains("(allow file-write* (literal \"/Users/test/.claude.json\"))"),
        "missing HOME/.claude.json write allow: {text}"
    );
    assert!(
        text.contains("(allow file-write* (literal \"/Users/test/.claude.json.lock\"))"),
        "missing HOME/.claude.json.lock write allow: {text}"
    );
    assert!(
            text.contains(
                "(allow file-write* (regex \"^/Users/test/\\\\.claude\\\\.json\\\\.tmp\\\\.[0-9]+\\\\.[0-9]+$\"))"
            ),
            "missing HOME/.claude.json.tmp.<pid>.<ts> regex allow: {text}"
        );
}

#[test]
fn compile_does_not_emit_home_claude_json_allow_when_claude_config_dir_set() {
    let resolved = profile("default", &["/Users/test/repo"], &["/Users/test/repo/src"]);
    let text = compile_with_env(
        &resolved,
        NEUTRAL_PROVIDER,
        EnvOverrides {
            home: Some("/Users/test"),
            claude_config_dir: Some("/var/folders/test/claude-config"),
            ..Default::default()
        },
    );
    assert!(
        !text.contains("/Users/test/.claude.json"),
        "HOME/.claude.json sibling allow must be skipped when CLAUDE_CONFIG_DIR is set: {text}"
    );
}

#[test]
fn compile_grants_write_access_to_home_gemini() {
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
        text.contains("(allow file-write* (subpath \"/Users/test/.gemini\"))"),
        "missing HOME/.gemini write allow: {text}"
    );
}

#[test]
fn grok_state_dir_prefers_grok_home_override() {
    assert_eq!(
        grok_state_dir(
            Some(OsStr::new("/Users/test")),
            Some(OsStr::new("/tmp/grok-home"))
        ),
        Some(PathBuf::from("/tmp/grok-home"))
    );
}

#[test]
fn grok_state_dir_falls_back_to_home_dot_grok() {
    assert_eq!(
        grok_state_dir(Some(OsStr::new("/Users/test")), None),
        Some(PathBuf::from("/Users/test/.grok"))
    );
}

#[test]
fn grok_state_dir_from_env_reads_runtime_env() {
    const EXPECTED_ENV: &str = "ORBIT_TEST_EXPECTED_GROK_STATE_DIR";
    if let Some(expected) = std::env::var_os(EXPECTED_ENV) {
        if expected == OsStr::new("__none__") {
            assert_eq!(grok_state_dir_from_env(), None);
        } else {
            assert_eq!(
                grok_state_dir_from_env(),
                Some(PathBuf::from(expected)),
                "GROK_HOME should take precedence over HOME"
            );
        }
        return;
    }

    fn run_case(expected: &str, grok_home: Option<&str>, home: Option<&str>) {
        let mut command =
            std::process::Command::new(std::env::current_exe().expect("current test executable"));
        command
            .arg("grok_state_dir_from_env_reads_runtime_env")
            .arg("--exact")
            .arg("--nocapture")
            .arg("--test-threads=1")
            .env(EXPECTED_ENV, expected);
        match grok_home {
            Some(value) => {
                command.env("GROK_HOME", value);
            }
            None => {
                command.env_remove("GROK_HOME");
            }
        }
        match home {
            Some(value) => {
                command.env("HOME", value);
            }
            None => {
                command.env_remove("HOME");
            }
        }
        let status = command.status().expect("run env helper child test");
        assert!(status.success(), "child env helper case failed: {status:?}");
    }

    run_case("/tmp/grok-home", Some("/tmp/grok-home"), Some("/tmp/home"));
    run_case("/tmp/home/.grok", None, Some("/tmp/home"));
    run_case("__none__", None, None);
}

#[test]
fn compile_grants_write_access_to_grok_home_when_set() {
    let resolved = profile("default", &["/Users/test/repo"], &["/Users/test/repo/src"]);
    let text = compile_with_env(
        &resolved,
        NEUTRAL_PROVIDER,
        EnvOverrides {
            home: Some("/Users/test"),
            grok_home: Some("/var/folders/test/grok-home"),
            ..Default::default()
        },
    );
    assert!(
        text.contains("(allow file-write* (subpath \"/var/folders/test/grok-home\"))"),
        "missing GROK_HOME write allow: {text}"
    );
    assert!(
        !text.contains("(allow file-write* (subpath \"/Users/test/.grok\"))"),
        "GROK_HOME should take precedence over HOME fallback: {text}"
    );
}

#[test]
fn compile_grants_write_access_to_home_grok_when_grok_home_missing() {
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
        text.contains("(allow file-write* (subpath \"/Users/test/.grok\"))"),
        "missing HOME/.grok write allow: {text}"
    );
}

#[test]
fn compile_emits_explicit_grok_json_lock_and_tmp_allows() {
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
        text.contains("(allow file-write* (literal \"/Users/test/.grok/auth.json\"))"),
        "missing Grok auth.json write allow: {text}"
    );
    assert!(
        text.contains("(allow file-write* (literal \"/Users/test/.grok/auth.json.lock\"))"),
        "missing Grok auth.json.lock write allow: {text}"
    );
    assert!(
            text.contains(
                "(allow file-write* (regex \"^/Users/test/\\\\.grok/auth\\\\.json\\\\.tmp(?:\\\\.[0-9]+)*$\"))"
            ),
            "missing Grok auth.json tmp regex allow: {text}"
        );
    assert!(
        text.contains("(allow file-write* (literal \"/Users/test/.grok/mcp_credentials.json\"))"),
        "missing Grok MCP credentials JSON write allow: {text}"
    );
    assert!(
        text.contains(
            "(allow file-write* (regex \"^/Users/test/\\\\.grok/mcp_auth_[^/]+\\\\.lock$\"))"
        ),
        "missing Grok MCP OAuth lock regex allow: {text}"
    );
}

#[test]
fn compile_emits_all_provider_state_dirs() {
    // Active provider is not threaded through SBPL compilation; emitting
    // every supported provider keeps the profile symmetric and avoids per-provider
    // branching at compile time.
    let resolved = profile("default", &["/Users/test/repo"], &["/Users/test/repo/src"]);
    let text = compile_with_env(
        &resolved,
        NEUTRAL_PROVIDER,
        EnvOverrides {
            home: Some("/Users/test"),
            ..Default::default()
        },
    );
    for dir in [".codex", ".claude", ".gemini", ".grok"] {
        let needle = format!("(allow file-write* (subpath \"/Users/test/{dir}\"))");
        assert!(
            text.contains(&needle),
            "missing provider state dir allow `{needle}`: {text}"
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn compiled_profile_allows_writes_to_provider_state_dirs() {
    // Documented equivalent for AC #2 / #3 of T20260428-14: rather than
    // executing real provider binaries, exercise the same SBPL allow
    // clause provider CLIs rely on at startup. If the kernel permits a
    // write under the synthetic provider state subpaths, the same
    // mechanism unblocks the real CLIs writing settings/sessions there.
    use std::process::Command;

    if !sandbox_exec_can_apply() {
        return;
    }

    let parent = sandbox_test_parent("provider-state");
    let _cleanup = ScopeGuard(parent.clone());
    let synthetic_home = tempfile::Builder::new()
        .prefix("synthetic-home-")
        .tempdir_in(&parent)
        .expect("synthetic home tempdir");
    let claude_dir = synthetic_home.path().join(".claude");
    let gemini_dir = synthetic_home.path().join(".gemini");
    let grok_dir = synthetic_home.path().join(".grok");
    std::fs::create_dir_all(&claude_dir).expect("claude dir");
    std::fs::create_dir_all(&gemini_dir).expect("gemini dir");
    std::fs::create_dir_all(&grok_dir).expect("grok dir");

    let resolved = ResolvedFsProfile {
        name: "default".to_string(),
        read: vec![synthetic_home.path().display().to_string()],
        modify: vec![],
    };
    let profile_text = compile_macos_sandbox_profile_with_env(
        &resolved,
        NEUTRAL_PROVIDER,
        SandboxCompileEnv {
            home: Some(synthetic_home.path().as_os_str()),
            codex_home: None,
            pi_coding_agent_dir: None,
            claude_config_dir: None,
            grok_home: None,
            copilot_home: None,
            xdg_cache_home: None,
            ..SandboxCompileEnv::default()
        },
    )
    .expect("compile sbpl");
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

    for (label, target) in [
        ("claude", claude_dir.join("ok")),
        ("gemini", gemini_dir.join("ok")),
        ("grok", grok_dir.join("ok")),
    ] {
        let status = Command::new(sandbox_exec_path_for_test())
            .arg("-f")
            .arg(profile_file.path())
            .arg("/bin/sh")
            .arg("-c")
            .arg(format!("echo ok > {}", shell_escape(&target)))
            .status()
            .expect("run sandbox-exec");
        assert!(
            status.success(),
            "expected write under synthetic ~/.{label} to succeed; status={status:?}"
        );
        assert!(
            target.exists(),
            "{label} target file was not written: {target:?}"
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn compiled_profile_allows_writes_to_grok_json_lock_and_tmp_files() {
    use std::process::Command;

    if !sandbox_exec_can_apply() {
        return;
    }

    let parent = sandbox_test_parent("grok-json-locks");
    let _cleanup = ScopeGuard(parent.clone());
    let synthetic_home = tempfile::Builder::new()
        .prefix("synthetic-home-")
        .tempdir_in(&parent)
        .expect("synthetic home tempdir");
    let grok_dir = synthetic_home.path().join(".grok");
    std::fs::create_dir_all(&grok_dir).expect("grok dir");

    let resolved = ResolvedFsProfile {
        name: "default".to_string(),
        read: vec![synthetic_home.path().display().to_string()],
        modify: vec![],
    };
    let profile_text = compile_macos_sandbox_profile_with_env(
        &resolved,
        NEUTRAL_PROVIDER,
        SandboxCompileEnv {
            home: Some(synthetic_home.path().as_os_str()),
            codex_home: None,
            pi_coding_agent_dir: None,
            claude_config_dir: None,
            grok_home: None,
            copilot_home: None,
            xdg_cache_home: None,
            ..SandboxCompileEnv::default()
        },
    )
    .expect("compile sbpl");
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

    for (label, target) in [
        ("auth.json", grok_dir.join("auth.json")),
        ("auth.json.lock", grok_dir.join("auth.json.lock")),
        (
            "auth.json.tmp.<pid>.<ts>",
            grok_dir.join("auth.json.tmp.7969.1778210964004"),
        ),
        (
            "mcp_credentials.json",
            grok_dir.join("mcp_credentials.json"),
        ),
        (
            "mcp_auth_<name>.lock",
            grok_dir.join("mcp_auth_linear.lock"),
        ),
    ] {
        let status = Command::new(sandbox_exec_path_for_test())
            .arg("-f")
            .arg(profile_file.path())
            .arg("/bin/sh")
            .arg("-c")
            .arg(format!("echo ok > {}", shell_escape(&target)))
            .status()
            .expect("run sandbox-exec");
        assert!(
            status.success(),
            "expected write to synthetic Grok {label} to succeed; status={status:?}"
        );
        assert!(
            target.exists(),
            "{label} target file was not written: {target:?}"
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn compiled_profile_allows_writes_to_claude_home_json_siblings() {
    // T20260508-13: Claude Code persists `$HOME/.claude.json` (plus
    // `.lock` and atomic-write `.tmp.<pid>.<ms_ts>` siblings) at the home
    // root, not under `$HOME/.claude/`. Without explicit allows the
    // kernel denies these writes and Claude hangs on its own lockfile
    // under sandbox-exec.
    use std::process::Command;

    if !sandbox_exec_can_apply() {
        return;
    }

    let parent = sandbox_test_parent("claude-home-json");
    let _cleanup = ScopeGuard(parent.clone());
    let synthetic_home = tempfile::Builder::new()
        .prefix("synthetic-home-")
        .tempdir_in(&parent)
        .expect("synthetic home tempdir");

    let resolved = ResolvedFsProfile {
        name: "default".to_string(),
        read: vec![synthetic_home.path().display().to_string()],
        modify: vec![],
    };
    let profile_text = compile_macos_sandbox_profile_with_env(
        &resolved,
        NEUTRAL_PROVIDER,
        SandboxCompileEnv {
            home: Some(synthetic_home.path().as_os_str()),
            codex_home: None,
            pi_coding_agent_dir: None,
            claude_config_dir: None,
            grok_home: None,
            copilot_home: None,
            xdg_cache_home: None,
            ..SandboxCompileEnv::default()
        },
    )
    .expect("compile sbpl");
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

    for (label, target) in [
        (".claude.json", synthetic_home.path().join(".claude.json")),
        (
            ".claude.json.lock",
            synthetic_home.path().join(".claude.json.lock"),
        ),
        (
            ".claude.json.tmp.<pid>.<ts>",
            synthetic_home
                .path()
                .join(".claude.json.tmp.7969.1778210964004"),
        ),
    ] {
        let status = Command::new(sandbox_exec_path_for_test())
            .arg("-f")
            .arg(profile_file.path())
            .arg("/bin/sh")
            .arg("-c")
            .arg(format!("echo ok > {}", shell_escape(&target)))
            .status()
            .expect("run sandbox-exec");
        assert!(
            status.success(),
            "expected write to synthetic {label} to succeed; status={status:?}"
        );
        assert!(
            target.exists(),
            "{label} target file was not written: {target:?}"
        );
    }
}

// [ORB-10946] Copilot's state grants are gated on the *active* provider, so
// these assert both halves: present for copilot, absent for everyone else.

#[test]
fn copilot_state_dirs_default_under_home() {
    let dirs = copilot_state_dirs(Some(OsStr::new("/Users/test")), None, None);

    assert_eq!(
        dirs,
        vec![
            PathBuf::from("/Users/test/.copilot"),
            PathBuf::from("/Users/test/.cache/copilot"),
        ]
    );
}

#[test]
fn copilot_state_dirs_honor_copilot_home_and_xdg_cache_home() {
    let dirs = copilot_state_dirs(
        Some(OsStr::new("/Users/test")),
        Some(OsStr::new("/var/folders/test/copilot-home")),
        Some(OsStr::new("/var/folders/test/cache")),
    );

    assert_eq!(
        dirs,
        vec![
            PathBuf::from("/var/folders/test/copilot-home"),
            PathBuf::from("/var/folders/test/cache/copilot"),
        ]
    );
}

#[test]
fn copilot_state_dirs_ignore_empty_overrides() {
    let dirs = copilot_state_dirs(
        Some(OsStr::new("/Users/test")),
        Some(OsStr::new("")),
        Some(OsStr::new("")),
    );

    assert_eq!(
        dirs,
        vec![
            PathBuf::from("/Users/test/.copilot"),
            PathBuf::from("/Users/test/.cache/copilot"),
        ]
    );
}

#[test]
fn copilot_state_dirs_are_empty_without_home_or_overrides() {
    assert!(copilot_state_dirs(None, None, None).is_empty());
}

#[test]
fn compile_grants_copilot_dirs_only_to_an_active_copilot_executor() {
    let resolved = profile("default", &["/Users/test/repo"], &["/Users/test/repo/src"]);

    let copilot_profile = compile_with_env(
        &resolved,
        "copilot",
        EnvOverrides {
            home: Some("/Users/test"),
            ..Default::default()
        },
    );
    assert!(
        copilot_profile.contains("(allow file-write* (subpath \"/Users/test/.copilot\"))"),
        "active copilot must be granted its state dir",
    );
    assert!(
        copilot_profile.contains("(allow file-write* (subpath \"/Users/test/.cache/copilot\"))"),
        "active copilot must be granted its package-extraction cache",
    );

    // No other provider inherits Copilot-specific write access.
    for provider in ["claude", "codex", "gemini", "grok", "cursor"] {
        let other = compile_with_env(
            &resolved,
            provider,
            EnvOverrides {
                home: Some("/Users/test"),
                ..Default::default()
            },
        );
        assert!(
            !other.contains("/Users/test/.copilot"),
            "{provider} must not inherit the copilot state dir",
        );
        assert!(
            !other.contains("/Users/test/.cache/copilot"),
            "{provider} must not inherit the copilot extraction cache",
        );
    }
}

#[test]
fn cursor_state_dir_defaults_under_home() {
    assert_eq!(
        cursor_state_dir(Some(OsStr::new("/Users/test"))),
        Some(PathBuf::from("/Users/test/.cursor"))
    );
    assert_eq!(cursor_state_dir(None), None);
}

#[test]
fn compile_grants_cursor_state_only_to_an_active_cursor_executor() {
    let resolved = profile("default", &["/Users/test/repo"], &["/Users/test/repo/src"]);
    let cursor_profile = compile_with_env(
        &resolved,
        "cursor",
        EnvOverrides {
            home: Some("/Users/test"),
            ..Default::default()
        },
    );
    assert!(
        cursor_profile.contains("(allow file-write* (subpath \"/Users/test/.cursor\"))"),
        "active Cursor must be granted its state directory",
    );

    for provider in ["claude", "codex", "gemini", "grok", "copilot"] {
        let other = compile_with_env(
            &resolved,
            provider,
            EnvOverrides {
                home: Some("/Users/test"),
                ..Default::default()
            },
        );
        assert!(
            !other.contains("/Users/test/.cursor"),
            "{provider} must not inherit Cursor state write access",
        );
    }
}

#[test]
fn pi_state_dir_defaults_under_home_and_honors_the_agent_dir_override() {
    assert_eq!(
        pi_state_dir(Some(OsStr::new("/Users/test")), None),
        Some(PathBuf::from("/Users/test/.pi"))
    );
    assert_eq!(
        pi_state_dir(
            Some(OsStr::new("/Users/test")),
            Some(OsStr::new("/var/folders/test/pi-agent"))
        ),
        Some(PathBuf::from("/var/folders/test/pi-agent"))
    );
    assert_eq!(pi_state_dir(None, None), None);
}

#[test]
fn compile_grants_pi_state_only_to_an_active_pi_executor() {
    let resolved = profile("default", &["/Users/test/repo"], &["/Users/test/repo/src"]);
    let pi_profile = compile_with_env(
        &resolved,
        "pi",
        EnvOverrides {
            home: Some("/Users/test"),
            ..Default::default()
        },
    );
    assert!(
        pi_profile.contains("(allow file-write* (subpath \"/Users/test/.pi\"))"),
        "active Pi must be granted its agent state directory",
    );

    for provider in ["claude", "codex", "gemini", "grok", "copilot", "cursor"] {
        let other = compile_with_env(
            &resolved,
            provider,
            EnvOverrides {
                home: Some("/Users/test"),
                ..Default::default()
            },
        );
        assert!(
            !other.contains("/Users/test/.pi\""),
            "{provider} must not inherit Pi state write access",
        );
    }
}

#[test]
fn compile_uses_the_pi_coding_agent_dir_override_for_the_active_executor() {
    let resolved = profile("default", &["/Users/test/repo"], &["/Users/test/repo/src"]);

    let text = compile_with_env(
        &resolved,
        "pi",
        EnvOverrides {
            home: Some("/Users/test"),
            pi_coding_agent_dir: Some("/var/folders/test/pi-agent"),
            ..Default::default()
        },
    );

    assert!(text.contains("(allow file-write* (subpath \"/var/folders/test/pi-agent\"))"));
    // With the override set, the default location is not additionally granted.
    assert!(!text.contains("\"/Users/test/.pi\""));
}

#[test]
fn compile_uses_the_copilot_home_override_for_the_active_executor() {
    let resolved = profile("default", &["/Users/test/repo"], &["/Users/test/repo/src"]);

    let text = compile_with_env(
        &resolved,
        "copilot",
        EnvOverrides {
            home: Some("/Users/test"),
            copilot_home: Some("/var/folders/test/copilot-home"),
            xdg_cache_home: Some("/var/folders/test/cache"),
            ..Default::default()
        },
    );

    assert!(text.contains("(allow file-write* (subpath \"/var/folders/test/copilot-home\"))"));
    assert!(text.contains("(allow file-write* (subpath \"/var/folders/test/cache/copilot\"))"));
    // With the override set, the default location is not additionally granted.
    assert!(!text.contains("\"/Users/test/.copilot\""));
}

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

#[test]
fn opencode_state_dirs_default_to_the_xdg_home_locations() {
    // [ORB-11295] OpenCode resolves every root through `xdg-basedir` and
    // creates data/config/state during startup, before it reads Orbit's
    // envelope, so all four roots must be present.
    assert_eq!(
        opencode_state_dirs(OpencodeDirEnv {
            home: Some(OsStr::new("/Users/test")),
            ..Default::default()
        }),
        vec![
            PathBuf::from("/Users/test/.local/share/opencode"),
            PathBuf::from("/Users/test/.config/opencode"),
            PathBuf::from("/Users/test/.local/state/opencode"),
            PathBuf::from("/Users/test/.cache/opencode"),
        ]
    );
    assert!(opencode_state_dirs(OpencodeDirEnv::default()).is_empty());
}

#[test]
fn opencode_state_dirs_honor_xdg_variables_and_the_config_override() {
    assert_eq!(
        opencode_state_dirs(OpencodeDirEnv {
            home: Some(OsStr::new("/Users/test")),
            xdg_data_home: Some(OsStr::new("/srv/data")),
            xdg_config_home: Some(OsStr::new("/srv/config")),
            xdg_state_home: Some(OsStr::new("/srv/state")),
            xdg_cache_home: Some(OsStr::new("/srv/cache")),
            opencode_config_dir: None,
        }),
        vec![
            PathBuf::from("/srv/data/opencode"),
            PathBuf::from("/srv/config/opencode"),
            PathBuf::from("/srv/state/opencode"),
            PathBuf::from("/srv/cache/opencode"),
        ]
    );

    // `OPENCODE_CONFIG_DIR` is the config root itself, not an XDG base, so it
    // is used verbatim and outranks `XDG_CONFIG_HOME`.
    let dirs = opencode_state_dirs(OpencodeDirEnv {
        home: Some(OsStr::new("/Users/test")),
        xdg_config_home: Some(OsStr::new("/srv/config")),
        opencode_config_dir: Some(OsStr::new("/srv/opencode-config")),
        ..Default::default()
    });
    assert_eq!(dirs[1], PathBuf::from("/srv/opencode-config"));
}

#[test]
fn opencode_state_dirs_ignore_empty_overrides() {
    assert_eq!(
        opencode_state_dirs(OpencodeDirEnv {
            home: Some(OsStr::new("/Users/test")),
            xdg_data_home: Some(OsStr::new("")),
            opencode_config_dir: Some(OsStr::new("")),
            ..Default::default()
        }),
        vec![
            PathBuf::from("/Users/test/.local/share/opencode"),
            PathBuf::from("/Users/test/.config/opencode"),
            PathBuf::from("/Users/test/.local/state/opencode"),
            PathBuf::from("/Users/test/.cache/opencode"),
        ]
    );
}

#[test]
fn compile_grants_opencode_state_only_to_an_active_opencode_executor() {
    let resolved = profile("default", &["/Users/test/repo"], &["/Users/test/repo/src"]);
    let opencode_profile = compile_with_env(
        &resolved,
        "opencode",
        EnvOverrides {
            home: Some("/Users/test"),
            ..Default::default()
        },
    );
    for root in [
        "/Users/test/.local/share/opencode",
        "/Users/test/.config/opencode",
        "/Users/test/.local/state/opencode",
        "/Users/test/.cache/opencode",
    ] {
        assert!(
            opencode_profile.contains(&format!("(allow file-write* (subpath \"{root}\"))")),
            "active OpenCode must be granted {root}",
        );
    }

    for provider in [
        "claude", "codex", "gemini", "grok", "copilot", "cursor", "pi",
    ] {
        let other = compile_with_env(
            &resolved,
            provider,
            EnvOverrides {
                home: Some("/Users/test"),
                ..Default::default()
            },
        );
        assert!(
            !other.contains("/opencode"),
            "{provider} must not inherit OpenCode state write access",
        );
    }
}
