use std::path::{Path, PathBuf};

use orbit_types::plugin::{MANIFEST_FILE_NAME, PluginGrantSet, PluginProvenance};

use super::super::backend::PluginBackendSpec;
use super::super::loader::{
    LoadedPlugin, first_party_source, fs_write_root_covers, load_plugin_dir,
    refuse_covering_fs_write_roots,
};

const MANIFEST: &str = "\
schemaVersion: 2
kind: Plugin
metadata:
  name: demo
  version: 1.0.0
  description: Fixture.
spec:
  backend:
    type: exec
    command: bin/backend.sh
  tools:
    - name: hello
      description: Hello.
      execution_kind: read_only
";

fn write_plugin(root: &Path) {
    std::fs::create_dir_all(root.join("bin")).expect("mkdir");
    std::fs::write(root.join(MANIFEST_FILE_NAME), MANIFEST).expect("manifest");
    std::fs::write(root.join("bin/backend.sh"), "#!/bin/sh\n").expect("backend");
}

fn backend_spec(
    plugin: &LoadedPlugin,
    global_root: &Path,
    state_dir: PathBuf,
) -> PluginBackendSpec {
    let grants = PluginGrantSet::from_grants(plugin.manifest.required_grants());
    PluginBackendSpec {
        provenance: PluginProvenance {
            name: plugin.namespace().to_string(),
            version: plugin.manifest.metadata.version.clone(),
            manifest_digest: plugin.manifest_digest.clone(),
            grants: grants.to_recorded(),
        },
        plugin_root: plugin.root.clone(),
        state_dir,
        global_root: global_root.to_path_buf(),
        command: plugin.backend_command.clone(),
        args: plugin.manifest.spec.backend.args.clone(),
        timeout_ms: plugin.manifest.spec.backend.timeout_ms,
        sandbox: plugin.manifest.spec.backend.sandbox,
        permissions: plugin.manifest.spec.permissions.clone(),
        programs: plugin.manifest.spec.requires.programs.clone(),
        program_paths: Default::default(),
        config: Default::default(),
        grants,
    }
}

mod load;
mod validation;
