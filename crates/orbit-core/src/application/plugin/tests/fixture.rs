//! Shared plugin fixtures: a real Orbit root, and plugin directories written
//! outside it so `add` exercises the ordinary global-install path.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use orbit_types::tool::{McpCapability, ToolSessionContext};
use tempfile::TempDir;

use crate::OrbitRuntime;

pub(super) struct PluginFixture {
    // Keep HOME inside the fixture while in-process add/enable/sync tests run.
    // The guard drops before the temporary tree so HOME is restored first.
    _home_env: orbit_common::test_env::ScopedEnv,
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
        let home = root.path().join("home");
        std::fs::create_dir_all(&home).expect("create fixture HOME");
        let home_str = home.to_str().expect("utf8 fixture HOME");
        let home_env = orbit_common::test_env::scoped([
            ("HOME", Some(home_str)),
            ("USERPROFILE", Some(home_str)),
        ]);
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
            _home_env: home_env,
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

    /// Call one plugin tool through the audited dispatch this runtime uses,
    /// with the activity allowlist pinned to that tool so a managed
    /// executor's inherited envelope cannot decide the outcome.
    pub(super) fn call(
        &self,
        runtime: &OrbitRuntime,
        tool: &str,
    ) -> Result<serde_json::Value, orbit_common::OrbitError> {
        self.call_with_input(runtime, tool, serde_json::json!({}))
    }

    /// Same audited dispatch as [`Self::call`], with a caller-supplied input.
    pub(super) fn call_with_input(
        &self,
        runtime: &OrbitRuntime,
        tool: &str,
        input: serde_json::Value,
    ) -> Result<serde_json::Value, orbit_common::OrbitError> {
        let _activity_tools =
            crate::adapter::command::dispatch_test_support::override_activity_tools_for_test([
                tool,
            ]);
        // `execute_tool_command` builds an empty session and reads the process
        // envelope. CI has no TTY and no agent identity, so that caller
        // resolves as `unknown` and the identification floor refuses even a
        // read-only plugin tool. Name the caller here — `agent` is the floor
        // `plugin.tool.read_only` already admits — rather than depending on
        // the runner's environment.
        runtime.execute_tool_command_with_session_context(
            tool,
            input,
            None,
            None,
            ToolSessionContext {
                effective_capabilities: BTreeSet::from([McpCapability::Agent]),
                ..ToolSessionContext::default()
            },
        )
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
    /// `spec.requires.programs`, as a YAML flow list.
    pub(super) requires_programs: Option<&'a str>,
    pub(super) verb: &'a str,
    /// A `spec.permissions:` block, indented for the manifest.
    pub(super) permissions: Option<&'a str>,
    /// `spec.backend.sandbox`, when the fixture declares one.
    pub(super) sandbox: Option<&'a str>,
    /// Replacement backend script; the default echoes its envelope back.
    pub(super) backend: Option<&'a str>,
    /// The tool's `output_schema` body, indented for the manifest.
    pub(super) output_schema: Option<&'a str>,
    /// `spec.secrets` entries, indented for the manifest.
    pub(super) secrets: Option<&'a str>,
}

impl<'a> PluginSpecFixture<'a> {
    pub(super) fn new(dir: &'a str, name: &'a str) -> Self {
        Self {
            dir,
            name,
            version: "1.0.0",
            requires_orbit: None,
            requires_host_api: None,
            requires_programs: None,
            verb: "hello",
            permissions: None,
            sandbox: None,
            backend: None,
            output_schema: None,
            secrets: None,
        }
    }

    /// Declare `spec.secrets` (list entries indented four spaces).
    pub(super) fn declaring_secrets(mut self, entries: &'a str) -> Self {
        self.secrets = Some(entries);
        self
    }

    pub(super) fn with_backend(mut self, script: &'a str) -> Self {
        self.backend = Some(script);
        self
    }

    pub(super) fn with_output_schema(mut self, schema: &'a str) -> Self {
        self.output_schema = Some(schema);
        self
    }

    /// Request `fs.write` under the plugin's own state directory.
    pub(super) fn requesting_fs_write(mut self) -> Self {
        self.permissions = Some("  permissions:\n    fs:\n      write: [\"{{plugin_state}}\"]\n");
        self
    }

    /// Request `fs.write` on two sibling directories under the plugin's own
    /// state tree, so a grant can be scoped to one of them.
    pub(super) fn requesting_two_fs_writes(mut self) -> Self {
        self.permissions = Some(
            "  permissions:\n    fs:\n      write: [\"{{plugin_state}}/kept\", \"{{plugin_state}}/dropped\"]\n",
        );
        self
    }

    /// Declare `spec.requires.programs` (a YAML flow list, `[git, /opt/x]`).
    pub(super) fn requiring_programs(mut self, programs: &'a str) -> Self {
        self.requires_programs = Some(programs);
        self
    }

    pub(super) fn unsandboxed(mut self) -> Self {
        self.sandbox = Some("none");
        self
    }
}

/// Write a plugin whose backend echoes its input back, so a test can prove the
/// call reached the process.
pub(super) fn write_plugin_at(root: &Path, spec: PluginSpecFixture<'_>) -> PathBuf {
    std::fs::create_dir_all(root.join("bin")).expect("create plugin bin dir");
    let backend = root.join("bin/backend.sh");
    std::fs::write(
        &backend,
        spec.backend.unwrap_or(
            "#!/bin/sh\ninput=$(cat)\nprintf '{\"ok\":true,\"output\":{\"plugin\":\"%s\",\"envelope\":%s}}\\n' \"$ORBIT_PLUGIN\" \"$input\"\n",
        ),
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
    if let Some(programs) = spec.requires_programs {
        requires.push_str(&format!("    programs: {programs}\n"));
    }
    let requires_block = if requires.is_empty() {
        String::new()
    } else {
        format!("  requires:\n{requires}")
    };
    let sandbox = spec
        .sandbox
        .map(|sandbox| format!("    sandbox: {sandbox}\n"))
        .unwrap_or_default();
    let manifest = format!(
        "schemaVersion: 2\nkind: Plugin\nmetadata:\n  name: {name}\n  version: {version}\n  description: Fixture plugin.\nspec:\n{requires_block}  backend:\n    type: exec\n    command: bin/backend.sh\n{sandbox}{permissions}  tools:\n    - name: {verb}\n      description: Say hello.\n      execution_kind: read_only\n      mcp_scope: workspace\n      input_schema:\n        type: object\n        properties:\n          subject: {{ type: string, description: Who to greet. }}\n",
        name = spec.name,
        version = spec.version,
        verb = spec.verb,
        permissions = spec.permissions.unwrap_or_default(),
    );
    let manifest = match spec.output_schema {
        Some(schema) => format!("{manifest}      output_schema:\n{schema}"),
        None => manifest,
    };
    let manifest = match spec.secrets {
        Some(entries) => format!("{manifest}  secrets:\n{entries}"),
        None => manifest,
    };
    std::fs::write(root.join("plugin.yaml"), manifest).expect("write manifest");
    root.to_path_buf()
}
