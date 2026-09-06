use std::ffi::OsStr;
#[cfg(target_os = "macos")]
use std::path::{Path, PathBuf};

use orbit_types::policy::ResolvedFsProfile;

use super::compile::{SandboxCompileEnv, compile_macos_sandbox_profile_with_env};

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
    pub(super) pi_coding_agent_dir: Option<&'a str>,
    pub(super) xdg_data_home: Option<&'a str>,
    pub(super) xdg_config_home: Option<&'a str>,
    pub(super) xdg_state_home: Option<&'a str>,
    pub(super) opencode_config_dir: Option<&'a str>,
}

/// Provider used by profile tests that are not about the per-provider
/// credential carve-out. Codex keeps every default credential deny, so a test
/// asserting the shared clauses sees them unmodified.
pub(super) const NEUTRAL_PROVIDER: &str = "codex";

pub(super) fn compile_with_env(
    resolved: &ResolvedFsProfile,
    provider: &str,
    env: EnvOverrides<'_>,
) -> String {
    compile_macos_sandbox_profile_with_env(
        resolved,
        provider,
        SandboxCompileEnv {
            home: env.home.map(OsStr::new),
            codex_home: env.codex_home.map(OsStr::new),
            claude_config_dir: env.claude_config_dir.map(OsStr::new),
            grok_home: env.grok_home.map(OsStr::new),
            copilot_home: env.copilot_home.map(OsStr::new),
            xdg_cache_home: env.xdg_cache_home.map(OsStr::new),
            pi_coding_agent_dir: env.pi_coding_agent_dir.map(OsStr::new),
            xdg_data_home: env.xdg_data_home.map(OsStr::new),
            xdg_config_home: env.xdg_config_home.map(OsStr::new),
            xdg_state_home: env.xdg_state_home.map(OsStr::new),
            opencode_config_dir: env.opencode_config_dir.map(OsStr::new),
        },
    )
    .expect("compile")
}

#[cfg(target_os = "macos")]
pub(super) fn shell_escape(path: &Path) -> String {
    let s = path.display().to_string();
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(target_os = "macos")]
pub(super) fn sandbox_exec_path_for_test() -> PathBuf {
    super::spawn::sandbox_exec_path().expect("trusted sandbox-exec path")
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

#[cfg(target_os = "macos")]
pub(super) fn sandbox_exec_can_apply() -> bool {
    if !super::spawn::sandbox_exec_available() {
        return false;
    }

    let mut profile_file = tempfile::Builder::new()
        .prefix("orbit-sandbox-probe-")
        .suffix(".sb")
        .tempfile()
        .expect("probe profile tempfile");
    use std::io::Write;
    profile_file
        .write_all(b"(version 1)\n(allow default)\n")
        .expect("write probe profile");
    profile_file.flush().expect("flush probe profile");

    std::process::Command::new(sandbox_exec_path_for_test())
        .arg("-f")
        .arg(profile_file.path())
        .arg("/usr/bin/true")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
static SANDBOX_TEST_PARENT_COUNTER: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(target_os = "macos")]
pub(super) fn sandbox_test_parent(label: &str) -> std::path::PathBuf {
    let roots = [
        Some(std::env::current_dir().expect("current dir")),
        std::env::var_os("HOME").map(std::path::PathBuf::from),
    ];
    let suffix = SANDBOX_TEST_PARENT_COUNTER
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        .to_string();
    let mut attempts = Vec::new();
    for root in roots.into_iter().flatten() {
        if is_default_write_allow_root(&root) {
            attempts.push(format!(
                "{} is under a broad sandbox write allow",
                root.display()
            ));
            continue;
        }
        let parent = root.join(format!(
            ".orbit-sandbox-test-{}-{label}-{suffix}",
            std::process::id()
        ));
        match std::fs::create_dir_all(&parent) {
            Ok(()) => return parent,
            Err(err) => attempts.push(format!("{}: {err}", parent.display())),
        }
    }
    panic!(
        "no writable macOS sandbox test parent outside broad write allows: {}",
        attempts.join("; ")
    );
}

#[cfg(target_os = "macos")]
fn is_default_write_allow_root(path: &Path) -> bool {
    fn default_write_allow_roots() -> Vec<PathBuf> {
        let mut roots = vec![
            PathBuf::from("/tmp"),
            PathBuf::from("/private/tmp"),
            PathBuf::from("/private/var/folders"),
            PathBuf::from("/dev"),
        ];
        let home = std::env::var_os("HOME");
        let codex_home = std::env::var_os("CODEX_HOME");
        let claude_config_dir = std::env::var_os("CLAUDE_CONFIG_DIR");
        let grok_home = std::env::var_os("GROK_HOME");
        if let Some(home) = super::provider_dirs::non_empty_env_path(home.as_deref()) {
            roots.push(home.join("Library/Caches"));
            roots.push(home.join(".orbit/state/logs"));
        }
        roots.extend(super::provider_dirs::provider_state_dirs(
            home.as_deref(),
            codex_home.as_deref(),
            claude_config_dir.as_deref(),
            grok_home.as_deref(),
        ));
        roots
    }

    fn matches_allowed(path: &Path, roots: &[PathBuf]) -> bool {
        roots.iter().any(|root| path.starts_with(root))
    }

    let allowed_roots = default_write_allow_roots();
    if matches_allowed(path, &allowed_roots) {
        return true;
    }
    match path.canonicalize() {
        Ok(canonical) => matches_allowed(&canonical, &allowed_roots),
        Err(_) => false,
    }
}

#[cfg(target_os = "macos")]
pub(super) struct ScopeGuard(pub(super) std::path::PathBuf);

#[cfg(target_os = "macos")]
impl Drop for ScopeGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
