//! Shared fixtures for the plugin backend tests: a stub `exec` backend, a
//! spec with chosen grants, and the skip rule for hosts without a sandbox.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use orbit_types::plugin::{
    PluginExecutionKind, PluginGrant, PluginPermissions, PluginProvenance, PluginSandbox,
};
use serde_json::Value;

use super::super::backend::PluginBackendSpec;
use super::super::tool::{PluginBackend, PluginTool, PluginToolBinding};
use crate::ToolContext;

pub(super) fn stub_backend(dir: &Path, script: &str) -> PathBuf {
    let path = dir.join("backend.sh");
    std::fs::write(&path, script).expect("write backend");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    path
}

pub(super) fn provenance(grants: &[PluginGrant]) -> PluginProvenance {
    PluginProvenance {
        name: "demo".into(),
        version: "1.0.0".into(),
        manifest_digest: "abc".into(),
        grants: grants
            .iter()
            .map(|grant| grant.as_str().to_string())
            .collect(),
    }
}

pub(super) fn spec(
    command: PathBuf,
    root: &Path,
    permissions: PluginPermissions,
    grants: &[PluginGrant],
) -> Arc<PluginBackendSpec> {
    Arc::new(PluginBackendSpec {
        provenance: provenance(grants),
        plugin_root: root.to_path_buf(),
        state_dir: root.join("state"),
        global_root: root.join("global"),
        command,
        args: vec!["--serve".into()],
        timeout_ms: Some(5_000),
        sandbox: PluginSandbox::Default,
        permissions,
        programs: Vec::new(),
        config_defaults: BTreeMap::new(),
        grants: grants.to_vec(),
    })
}

pub(super) fn tool(spec: Arc<PluginBackendSpec>, output_schema: Option<Value>) -> PluginTool {
    PluginTool {
        name: "demo.hello".into(),
        verb: "hello".into(),
        description: "demo".into(),
        parameters: vec![],
        execution_kind: PluginExecutionKind::ReadOnly,
        output_schema,
        binding: Arc::new(PluginToolBinding {
            provenance: spec.provenance.clone(),
            execution_kind: PluginExecutionKind::ReadOnly,
            diagnostic: None,
        }),
        backend: PluginBackend::Exec(spec),
    }
}

pub(super) fn context(cwd: &Path) -> ToolContext {
    ToolContext {
        cwd: Some(cwd.to_string_lossy().into_owned()),
        ..ToolContext::default()
    }
}

/// The plugin sandbox is enforced by the kernel; a host that cannot hold it
/// refuses to spawn, which is its own test. Tests that need a *running*
/// confined child report a skip here.
///
/// The crate denies `print_stderr` so no shipped code writes to the terminal
/// behind `tracing`; a test reporting why it did nothing is the exception the
/// lint is not aimed at.
#[allow(clippy::print_stderr)]
pub(super) fn sandbox_unavailable() -> bool {
    #[cfg(target_os = "linux")]
    {
        let probe = orbit_exec::probe_landlock();
        if !probe.available {
            eprintln!("skipping: {}", probe.detail);
        }
        !probe.available
    }
    #[cfg(target_os = "macos")]
    {
        let available = orbit_exec::sandbox_exec_available();
        if !available {
            eprintln!(
                "skipping: {}",
                orbit_exec::sandbox_exec_unavailable_message()
            );
        }
        !available
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        eprintln!("skipping: no plugin sandbox on {}", std::env::consts::OS);
        true
    }
}
