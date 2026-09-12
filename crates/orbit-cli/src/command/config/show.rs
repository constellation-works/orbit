use clap::Args;
use orbit_common::security::redaction::redact_home_dir;
use orbit_config::{EffectiveConfigValue, load_effective_config};
use orbit_core::OrbitRuntime;
use serde_json::{Map, Value as JsonValue, json};

use crate::command::{CommandOut, Execute, Payload};

use super::support::{
    ConfigScopeArg, global_config_path, open_store_for_scope, runtime_config_roots,
};

#[derive(Args)]
pub struct ConfigShowArgs {
    /// Select the layered effective config or resolve one file in isolation,
    /// including built-in defaults for keys that file omits.
    #[arg(long, value_enum, default_value_t = ConfigScopeArg::Effective)]
    pub scope: ConfigScopeArg,
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
                effective_text(runtime, effective.values()),
            )
            .into());
        }

        let store = open_store_for_scope(runtime, self.scope)?;
        let snapshot = store.snapshot()?;
        let settings = snapshot.all_values();

        Ok(Payload::detail(
            scoped_json(runtime, &store, &snapshot, &settings),
            scoped_text(runtime, &store, &snapshot, &settings),
        )
        .into())
    }
}

pub(super) fn effective_json(runtime: &OrbitRuntime, values: &[EffectiveConfigValue]) -> JsonValue {
    let mut settings = Map::new();
    let mut provenance = Map::new();
    for entry in values {
        settings.insert(entry.key.clone(), entry.value.clone());
        provenance.insert(
            entry.key.clone(),
            json!({
                "scope": entry.source.kind().label(),
                "path": entry.source.path().map(|path| path.to_string_lossy().into_owned()),
            }),
        );
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
        "execution_env_inherit": false,
        "global_root": runtime.global_root().to_string_lossy(),
        "shared_root": runtime.shared_root().to_string_lossy(),
        "local_root": runtime.local_root().to_string_lossy(),
        "config_path": config_path.to_string_lossy(),
        "persistence": runtime.persistence_config_json(),
    })
}

pub(super) fn effective_text(runtime: &OrbitRuntime, values: &[EffectiveConfigValue]) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "source: effective layered configuration");
    let global_path = global_config_path(runtime);
    let workspace_path = runtime.shared_root().join("config.toml");
    let global_status = if global_path.exists() {
        ""
    } else {
        " (absent)"
    };
    let workspace_status = if workspace_path.exists() {
        ""
    } else {
        " (absent)"
    };
    let _ = writeln!(
        out,
        "  global:    {}{}",
        redact_home_dir(&global_path.display().to_string()),
        global_status
    );
    let _ = writeln!(
        out,
        "  workspace: {}{}",
        redact_home_dir(&workspace_path.display().to_string()),
        workspace_status
    );
    let _ = writeln!(out);

    let _ = writeln!(out, "settings:");
    for entry in values {
        let source = match entry.source.path() {
            Some(path) => format!(
                "{} ({})",
                entry.source.kind().label(),
                redact_home_dir(&path.display().to_string())
            ),
            None => entry.source.kind().label().to_string(),
        };
        let _ = writeln!(
            out,
            "  {:<36} {:<24} [{}]",
            entry.key,
            render_value(&entry.value),
            source
        );
    }
    let _ = writeln!(out);

    let _ = writeln!(out, "derived:");
    let _ = writeln!(
        out,
        "  {:<36} {}",
        "global_root",
        runtime.global_root().to_string_lossy()
    );
    let _ = writeln!(
        out,
        "  {:<36} {}",
        "shared_root",
        runtime.shared_root().to_string_lossy()
    );
    let _ = writeln!(
        out,
        "  {:<36} {}",
        "local_root",
        runtime.local_root().to_string_lossy()
    );
    let _ = writeln!(
        out,
        "  {:<36} {}",
        "persistence",
        runtime.persistence_config_json()
    );
    out
}

pub(super) fn scoped_json(
    runtime: &OrbitRuntime,
    store: &orbit_config::ConfigStore,
    snapshot: &orbit_config::ConfigSnapshot,
    settings: &[(&'static str, JsonValue)],
) -> JsonValue {
    let mut settings_obj = Map::new();
    for (key, value) in settings {
        settings_obj.insert((*key).to_string(), value.clone());
    }

    // `global_root`/`shared_root`/`local_root`/`config_path`/`persistence`
    // stay top-level (not nested under a `derived` object) because an
    // existing contract test (`worktree_resolution.rs`) already asserts on
    // `shared_root`/`local_root` at the top level of `config show --json`,
    // and explicitly forbids reintroducing renamed aliases for them. The
    // `derived:` grouping the task asks for is rendered in the human-
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
) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let store_status = if store.path().exists() {
        ""
    } else {
        " (absent)"
    };
    let _ = writeln!(
        out,
        "source: {} ({}){}",
        store.scope().label(),
        redact_home_dir(&store.path().display().to_string()),
        store_status
    );
    let _ = writeln!(out);

    let _ = writeln!(out, "settings:");
    for (key, value) in settings {
        let _ = writeln!(out, "  {key:<36} {}", render_value(value));
    }
    let _ = writeln!(out);

    let _ = writeln!(out, "derived:");
    let _ = writeln!(
        out,
        "  {:<36} {}",
        "execution_env_inherit", snapshot.execution_env_inherit
    );
    let _ = writeln!(
        out,
        "  {:<36} {}",
        "global_root",
        runtime.global_root().to_string_lossy()
    );
    let _ = writeln!(
        out,
        "  {:<36} {}",
        "shared_root",
        runtime.shared_root().to_string_lossy()
    );
    let _ = writeln!(
        out,
        "  {:<36} {}",
        "local_root",
        runtime.local_root().to_string_lossy()
    );
    let _ = writeln!(
        out,
        "  {:<36} {}",
        "config_path",
        store.path().to_string_lossy()
    );
    let _ = writeln!(
        out,
        "  {:<36} {}",
        "persistence",
        runtime.persistence_config_json()
    );
    out
}

fn render_value(value: &JsonValue) -> String {
    match value {
        JsonValue::Null => "[unset]".to_string(),
        JsonValue::String(s) => format!("\"{s}\""),
        other => other.to_string(),
    }
}
