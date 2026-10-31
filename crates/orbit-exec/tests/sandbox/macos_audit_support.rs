#[cfg(target_os = "macos")]
use std::path::{Path, PathBuf};

use orbit_types::policy::ResolvedFsProfile;

use orbit_exec::compile_macos_sandbox_profile;

pub(super) fn profile(name: &str, read: &[&str], modify: &[&str]) -> ResolvedFsProfile {
    ResolvedFsProfile {
        name: name.to_string(),
        read: read.iter().map(|s| s.to_string()).collect(),
        modify: modify.iter().map(|s| s.to_string()).collect(),
    }
}

#[derive(Default)]
pub(super) struct EnvOverrides<'a> {
    pub(super) home: Option<&'a str>,
    pub(super) codex_home: Option<&'a str>,
    pub(super) claude_config_dir: Option<&'a str>,
    pub(super) grok_home: Option<&'a str>,
    pub(super) copilot_home: Option<&'a str>,
    pub(super) xdg_cache_home: Option<&'a str>,
    pub(super) xdg_data_home: Option<&'a str>,
    pub(super) xdg_config_home: Option<&'a str>,
    pub(super) xdg_state_home: Option<&'a str>,
    pub(super) cargo_home: Option<&'a str>,
}

/// Provider used by profile tests that are not about the per-provider
/// credential carve-out. Codex keeps every default credential deny, so a test
/// asserting the shared clauses sees them unmodified.
#[cfg(target_os = "macos")]
pub(super) const NEUTRAL_PROVIDER: &str = "codex";

pub(super) fn compile_with_env(
    resolved: &ResolvedFsProfile,
    provider: &str,
    env: EnvOverrides<'_>,
) -> String {
    let _environment = orbit_common::test_env::scoped([
        ("HOME", env.home),
        ("CODEX_HOME", env.codex_home),
        ("CLAUDE_CONFIG_DIR", env.claude_config_dir),
        ("GROK_HOME", env.grok_home),
        ("COPILOT_HOME", env.copilot_home),
        ("XDG_CACHE_HOME", env.xdg_cache_home),
        ("XDG_DATA_HOME", env.xdg_data_home),
        ("XDG_CONFIG_HOME", env.xdg_config_home),
        ("XDG_STATE_HOME", env.xdg_state_home),
        ("CARGO_HOME", env.cargo_home),
    ]);
    compile_macos_sandbox_profile(resolved, provider).expect("compile")
}

#[cfg(target_os = "macos")]
pub(super) fn shell_escape(path: &Path) -> String {
    let s = path.display().to_string();
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(target_os = "macos")]
pub(super) fn sandbox_exec_path_for_test() -> PathBuf {
    orbit_exec::sandbox_exec_path().expect("trusted sandbox-exec path")
}

/// Apply `profile_text` with the trusted `sandbox-exec` and report whether a
/// child could read `path`. Shared by the keychain ordering tests so each one
/// asserts on the kernel's verdict rather than on profile text.
#[cfg(target_os = "macos")]
pub(super) fn can_read_under_profile(profile_text: &str, path: &Path) -> bool {
    use std::io::Write;

    let mut profile_file = tempfile::Builder::new()
        .prefix("orbit-sandbox-keychain-")
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
        .arg(format!("cat {}", shell_escape(path)))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("run sandbox-exec")
        .success()
}
