//! Shared fixtures for the plugin backend tests: a stub `exec` backend, a
//! spec with chosen grants, and the availability assertion for tests that
//! exercise a live sandbox.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use orbit_types::plugin::{
    PluginExecutionKind, PluginGrant, PluginPermissions, PluginProvenance, PluginSandbox,
};
use serde_json::Value;

use super::super::backend::PluginBackendSpec;
use super::super::schema::CompiledSchema;
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
    let global_root = root.join("global");
    Arc::new(PluginBackendSpec {
        provenance: provenance(grants),
        plugin_root: root.to_path_buf(),
        state_dir: root.join("state"),
        global_root,
        command,
        args: vec!["--serve".into()],
        timeout_ms: Some(5_000),
        sandbox: PluginSandbox::Default,
        permissions,
        programs: Vec::new(),
        config: Default::default(),
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
        output_schema: output_schema
            .map(|schema| CompiledSchema::compile(schema).expect("compile output_schema")),
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

/// Live-sandbox tests are `#[ignore]` in the portable suite and selected by
/// the Linux CI sandbox gate. Once selected, an unavailable sandbox is a test
/// failure rather than a green early return.
pub(super) fn require_sandbox() {
    #[cfg(target_os = "linux")]
    {
        let probe = orbit_exec::probe_landlock();
        assert!(
            probe.available,
            "plugin sandbox unavailable: {}",
            probe.detail
        );
    }
    #[cfg(target_os = "macos")]
    {
        assert!(
            orbit_exec::sandbox_exec_available(),
            "plugin sandbox unavailable: {}",
            orbit_exec::sandbox_exec_unavailable_message()
        );
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        panic!("plugin sandbox unavailable on {}", std::env::consts::OS);
    }
}
