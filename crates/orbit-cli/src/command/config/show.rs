use clap::Args;
use orbit_config::{EffectiveConfigValue, load_effective_config};
use orbit_core::OrbitRuntime;
use serde_json::{Map, Value as JsonValue, json};

use crate::command::{CommandOut, Execute, Payload};

use super::render;
use super::support::{
    ConfigScopeArg, global_config_path, open_store_for_scope, runtime_config_roots,
};

#[derive(Args)]
pub struct ConfigShowArgs {
    /// Select the layered effective config or resolve one file in isolation,
    /// including built-in defaults for keys that file omits.
    #[arg(long, value_enum, default_value_t = ConfigScopeArg::Effective)]
    pub scope: ConfigScopeArg,
    /// List every key, including sections whose keys are all unset and
    /// therefore collapse to a one-line summary by default.
    #[arg(long)]
    pub all: bool,
    /// Emit JSON. For global/workspace scope, `source.exists` reports whether
    /// the selected config file exists, not whether individual keys are set.
    #[arg(long)]
    pub json: bool,
}

impl Execute for ConfigShowArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        if self.scope == ConfigScopeArg::Effective {
            let effective = load_effective_config(&runtime_config_roots(runtime))?;
            return Ok(Payload::detail(
                effective_json(runtime, effective.values()),
                effective_text(runtime, effective.values(), self.all),
            )
            .into());
        }

        let store = open_store_for_scope(runtime, self.scope)?;
        let snapshot = store.snapshot()?;
        let settings = snapshot.all_values();

        Ok(Payload::detail(
            scoped_json(runtime, &store, &snapshot, &settings),
            scoped_text(runtime, &store, &snapshot, &settings, self.all),
        )
        .into())
    }
}

pub(super) fn effective_json(runtime: &OrbitRuntime, values: &[EffectiveConfigValue]) -> JsonValue {
    let mut settings = Map::new();
    let mut provenance = Map::new();
    for entry in values {
        settings.insert(entry.key.clone(), entry.value.clone());
        // `provenance` is the per-key record: the pre-existing `scope`/`path`
        // fields keep their meaning and position, and the grouping, registry
        // description, three-way state, and shadowed layers are added
        // alongside them rather than in a parallel map.
        provenance.insert(entry.key.clone(), render::effective_provenance_json(entry));
    }
    let global_path = global_config_path(runtime);
    let workspace_path = runtime.shared_root().join("config.toml");
    let global_exists = global_path.exists();
    let workspace_exists = workspace_path.exists();
    let config_path = if workspace_exists && runtime.shared_root() != runtime.global_root() {
        &workspace_path
    } else {
        &global_path
    };

    json!({
        "source": {
            "scope": "effective",
            "global_path": global_path.to_string_lossy(),
            "global_exists": global_exists,
            "workspace_path": workspace_path.to_string_lossy(),
            "workspace_exists": workspace_exists,
        },
        "shadowed_global_path": JsonValue::Null,
        "settings": settings,
        "provenance": provenance,
        "workspace_binding": render::workspace_binding_json(runtime),
        "execution_env_inherit": false,
        "global_root": runtime.global_root().to_string_lossy(),
        "shared_root": runtime.shared_root().to_string_lossy(),
        "local_root": runtime.local_root().to_string_lossy(),
        "config_path": config_path.to_string_lossy(),
        "persistence": runtime.persistence_config_json(),
    })
}

pub(super) fn effective_text(
    runtime: &OrbitRuntime,
    values: &[EffectiveConfigValue],
    all: bool,
) -> String {
    render::effective_text(runtime, values, all)
}

pub(super) fn scoped_json(
    runtime: &OrbitRuntime,
    store: &orbit_config::ConfigStore,
    snapshot: &orbit_config::ConfigSnapshot,
    settings: &[(&'static str, JsonValue)],
) -> JsonValue {
    let mut settings_obj = Map::new();
    let mut provenance = Map::new();
    for (key, value) in settings {
        settings_obj.insert((*key).to_string(), value.clone());
        provenance.insert(
            (*key).to_string(),
            render::scoped_provenance_json(store, key, value),
        );
    }

    // `global_root`/`shared_root`/`local_root`/`config_path`/`persistence`
    // stay top-level (not nested under a `derived` object) because an
    // existing contract test (`worktree_resolution.rs`) already asserts on
    // `shared_root`/`local_root` at the top level of `config show --json`,
    // and explicitly forbids reintroducing renamed aliases for them. The
    // grouped `Paths` section the task asks for is rendered in the human-
    // readable text output below; the JSON shape keeps these pre-existing
    // field names and positions unchanged.
    json!({
        "source": {
            "scope": store.scope().label(),
            "path": store.path().to_string_lossy(),
            "exists": store.path().exists(),
        },
        "shadowed_global_path": JsonValue::Null,
        "settings": settings_obj,
        "provenance": provenance,
        "workspace_binding": render::workspace_binding_json(runtime),
        "execution_env_inherit": snapshot.execution_env_inherit,
        "global_root": runtime.global_root().to_string_lossy(),
        "shared_root": runtime.shared_root().to_string_lossy(),
        "local_root": runtime.local_root().to_string_lossy(),
        "config_path": store.path().to_string_lossy(),
        "persistence": runtime.persistence_config_json(),
    })
}

pub(super) fn scoped_text(
    runtime: &OrbitRuntime,
    store: &orbit_config::ConfigStore,
    snapshot: &orbit_config::ConfigSnapshot,
    settings: &[(&'static str, JsonValue)],
    all: bool,
) -> String {
    render::scoped_text(runtime, store, snapshot, settings, all)
}
