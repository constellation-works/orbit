//! Shared plugin fixtures: a real Orbit root, and plugin directories written
//! outside it so `add` exercises the ordinary global-install path.

use std::path::{Path, PathBuf};

use tempfile::TempDir;

use crate::OrbitRuntime;

pub(super) struct PluginFixture {
    pub(super) _root: TempDir,
    pub(super) global_root: PathBuf,
    pub(super) workspace_root: PathBuf,
    pub(super) repo_root: PathBuf,
    /// Somewhere outside the repository to write plugin sources.
    pub(super) sources: PathBuf,
    pub(super) runtime: OrbitRuntime,
}

impl PluginFixture {
    pub(super) fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let global_root = root.path().join("global");
        let repo_root = root.path().join("repo");
        let workspace_root = repo_root.join(".orbit");
        let sources = root.path().join("sources");
        for dir in [&global_root, &workspace_root, &sources] {
            std::fs::create_dir_all(dir).expect("create fixture dir");
        }
        let runtime =
            OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build runtime");
        Self {
            _root: root,
            global_root,
            workspace_root,
            repo_root,
            sources,
            runtime,
        }
    }

    /// Rebuild the runtime so a newly installed plugin reaches the registry,
    /// exactly as the next `orbit` invocation would.
    pub(super) fn reopen(&self) -> OrbitRuntime {
        OrbitRuntime::from_roots(&self.global_root, &self.workspace_root).expect("reopen runtime")
    }

    pub(super) fn write_pin_file(&self, contents: &str) {
        std::fs::write(self.workspace_root.join("plugins.yaml"), contents).expect("write pin file");
    }

    /// A minimal working plugin directory under `sources`.
    pub(super) fn write_plugin(&self, spec: PluginSpecFixture<'_>) -> PathBuf {
        let root = self.sources.join(spec.dir);
        write_plugin_at(&root, spec)
    }
}

#[derive(Clone, Copy)]
pub(super) struct PluginSpecFixture<'a> {
    pub(super) dir: &'a str,
    pub(super) name: &'a str,
    pub(super) version: &'a str,
    /// `spec.requires.orbit`, when the fixture pins one.
    pub(super) requires_orbit: Option<&'a str>,
    pub(super) requires_host_api: Option<u32>,
    pub(super) verb: &'a str,
}

impl<'a> PluginSpecFixture<'a> {
    pub(super) fn new(dir: &'a str, name: &'a str) -> Self {
        Self {
            dir,
            name,
            version: "1.0.0",
            requires_orbit: None,
            requires_host_api: None,
            verb: "hello",
        }
    }
}

/// Write a plugin whose backend echoes its input back, so a test can prove the
/// call reached the process.
pub(super) fn write_plugin_at(root: &Path, spec: PluginSpecFixture<'_>) -> PathBuf {
    std::fs::create_dir_all(root.join("bin")).expect("create plugin bin dir");
    let backend = root.join("bin/backend.sh");
    std::fs::write(
        &backend,
        "#!/bin/sh\ninput=$(cat)\nprintf '{\"ok\":true,\"output\":{\"plugin\":\"%s\",\"envelope\":%s}}\\n' \"$ORBIT_PLUGIN\" \"$input\"\n",
    )
    .expect("write backend");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&backend, std::fs::Permissions::from_mode(0o755))
            .expect("chmod backend");
    }
    let mut requires = String::new();
    if let Some(range) = spec.requires_orbit {
        requires.push_str(&format!("    orbit: \"{range}\"\n"));
    }
    if let Some(host_api) = spec.requires_host_api {
        requires.push_str(&format!("    host_api: {host_api}\n"));
    }
    let requires_block = if requires.is_empty() {
        String::new()
    } else {
        format!("  requires:\n{requires}")
    };
    let manifest = format!(
        "schemaVersion: 2\nkind: Plugin\nmetadata:\n  name: {name}\n  version: {version}\n  description: Fixture plugin.\nspec:\n{requires_block}  backend:\n    type: exec\n    command: bin/backend.sh\n  tools:\n    - name: {verb}\n      description: Say hello.\n      execution_kind: read_only\n      mcp_scope: workspace\n      input_schema:\n        type: object\n        properties:\n          subject: {{ type: string, description: Who to greet. }}\n",
        name = spec.name,
        version = spec.version,
        verb = spec.verb,
    );
    std::fs::write(root.join("plugin.yaml"), manifest).expect("write manifest");
    root.to_path_buf()
}
