//! Short-lived, advisory-only cache shared by readiness readers across processes.

use std::io::Read;
use std::path::Path;
use std::time::SystemTime;

use orbit_common::fs::io::{atomic_write_text_volatile, open_read_only_no_follow};
use serde::{Deserialize, Serialize};

use super::OrbitRuntime;

#[derive(Deserialize, Serialize)]
struct CachedWarning {
    key: String,
    warning: Option<String>,
}

impl OrbitRuntime {
    /// Readiness polls and fresh CLI processes share only the warning, never
    /// an environment used to execute validation. A missing, expired or
    /// unwritable cache simply uses the regular resolver.
    pub(crate) fn readiness_validation_env_warning(&self) -> Option<String> {
        if self.workflow_required_validation_commands().is_empty() {
            return None;
        }
        let policy = self.validation_env_policy();
        if !policy.login_shell {
            return self.validation_env_preflight_warning();
        }
        let base = self.execution_env_policy().agent_subprocess_env(&[]);
        let shell = orbit_exec::LoginShell::for_current_user(&base);
        // Hash environment values rather than persisting the allowlist, which
        // can include credentials. One entry per local checkout bounds storage
        // even when launcher environments or workspace policy change often.
        let key = blake3::hash(
            format!(
                "readiness-warning-v1:{}:{base:?}:{policy:?}:{shell:?}",
                env!("CARGO_PKG_VERSION")
            )
            .as_bytes(),
        )
        .to_hex()
        .to_string();
        let path = self
            .paths()
            .local_dir
            .join("state/validation-env-warning.json");
        if let Some(cached) = read_warning(&path, &key) {
            return cached.warning;
        }
        let warning =
            orbit_exec::ValidationEnvironment::resolve(base, policy, &shell).preflight_warning();
        let cached = CachedWarning {
            key,
            warning: warning.clone(),
        };
        if let Ok(encoded) = serde_json::to_string(&cached)
            && let Err(error) = atomic_write_text_volatile(&path, &encoded)
        {
            tracing::debug!(%error, "readiness validation warning cache unavailable");
        }
        warning
    }
}

fn read_warning(path: &Path, key: &str) -> Option<CachedWarning> {
    let file = open_read_only_no_follow(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file()
        || SystemTime::now()
            .duration_since(metadata.modified().ok()?)
            .ok()?
            >= orbit_exec::LOGIN_SHELL_CACHE_TTL
    {
        return None;
    }
    let mut encoded = String::new();
    file.take(64 * 1024 + 1).read_to_string(&mut encoded).ok()?;
    if encoded.len() > 64 * 1024 {
        return None;
    }
    let cached: CachedWarning = serde_json::from_str(&encoded).ok()?;
    (cached.key == key).then_some(cached)
}
