use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_home_dir;
use orbit_config::{
    CONFIG_KEY_REGISTRY, ConfigScope, ConfigSnapshot, ConfigStore, ConfigValueSourceKind,
    admit_settable_config_key, config_key_options, load_effective_config,
};
use serde_json::{Value as JsonValue, json};

use crate::runtime::OrbitRuntime;

use super::paths::{config_layer_file, config_roots, execution_not_inherited, path_rows_json};
use super::rows::{
    CREW_FIELDS, FileRowContext, crew_rows, effective_sections, file_sections, not_inherited_keys,
    section_catalog, workspace_binding_json,
};

/// Parse the `scope` a caller selects for a file view or a write.
pub fn parse_config_scope(raw: &str) -> Option<ConfigScope> {
    match raw.trim() {
        "global" => Some(ConfigScope::Global),
        "workspace" => Some(ConfigScope::Workspace),
        _ => None,
    }
}

/// The layered view: sections with their rows, the layer paths, the workspace
/// registry binding, resolved paths, and the crew table.
pub fn effective_view(runtime: &OrbitRuntime) -> Result<JsonValue, OrbitError> {
    let effective = load_effective_config(&config_roots(runtime))?;
    let values = effective.values();
    let global_file = config_layer_file(&runtime.global_root())?;
    let workspace_file = config_layer_file(&runtime.shared_root())?;
    let workspace_file_exists = workspace_file.exists;
    let execution_not_inherited = execution_not_inherited(runtime, &workspace_file);

    let sections = effective_sections(values);
    Ok(json!({
        "scope": "effective",
        "layers": {
            "built_in": {"label": ConfigValueSourceKind::BuiltIn.label()},
            "global": global_file.json(),
            "workspace": workspace_file.json(),
            // The security exception is only in force once a workspace file
            // sets policy; before that, global values still apply and warning
            // about them would be wrong.
            "execution_not_inherited": execution_not_inherited,
            "not_inherited_keys": not_inherited_keys(values),
        },
        "workspace_binding": workspace_binding_json(runtime, values),
        "sections": sections,
        "crews": crew_rows(runtime, values),
        "crews_enumerated": true,
        "crew_purposes": super::purposes::crew_purposes_view(values),
        "paths": path_rows_json(runtime, None),
        "crew_fields": CREW_FIELDS,
        "write_scope_default": ConfigScope::Workspace.label(),
        "workspace_file_exists": workspace_file_exists,
        // The runtime may have been opened before the last config write, so
        // the review switches resolve from the files this view just read.
        "review": crate::application::review::review_switches_view(
            runtime,
            &orbit_config::ResolvedConfig::load(&config_roots(runtime))?.operation,
            chrono::Utc::now(),
        ),
    }))
}

/// One physical file's settings and defaults, mirroring `orbit config show
/// --scope global|workspace`. Workspace crew references use layered crew
/// definitions, while displayed settings retain their scoped values.
///
/// Rows the file sets also say how they sit in the layered config: a
/// workspace row's fallback and whether it restates the global or built-in
/// value, a global row's reach into the bound workspace. Those facts need
/// both layers to load; when they do not, they are null and the file itself
/// still renders, so a broken layer stays repairable from here.
pub fn file_view(runtime: &OrbitRuntime, scope: ConfigScope) -> Result<JsonValue, OrbitError> {
    let global_file = config_layer_file(&runtime.global_root())?;
    let workspace_file = config_layer_file(&runtime.shared_root())?;
    let file = match scope {
        ConfigScope::Global => &global_file,
        ConfigScope::Workspace => &workspace_file,
    };
    let file_error = |error| match error {
        OrbitError::InvalidInput(reason) => OrbitError::InvalidInput(format!(
            "config file '{}': {reason}",
            redact_home_dir(&file.path.display().to_string())
        )),
        other => other,
    };
    let store = ConfigStore::open(scope, file.path.clone()).map_err(file_error)?;
    let snapshot = store
        .snapshot_with_global(&runtime.global_root())
        .map_err(file_error)?;
    let settings = snapshot.all_values();
    let layered = match load_effective_config(&config_roots(runtime)) {
        Ok(layered) => Some(layered),
        Err(error) => {
            tracing::warn!(%error, "layered config unavailable; file view omits fallback and reach");
            None
        }
    };
    let context = FileRowContext {
        store: &store,
        layer: match scope {
            ConfigScope::Workspace if runtime.shared_root() != runtime.global_root() => {
                ConfigValueSourceKind::Workspace
            }
            _ => ConfigValueSourceKind::Global,
        },
        layered: layered.as_ref().map(|layered| layered.values()),
        built_in: ConfigSnapshot::default(),
    };
    let sections = file_sections(&context, &settings);

    // Reuse the bytes `ConfigStore::open` already read through the no-follow
    // opener. A second `read_to_string` of the joined path would follow a
    // leaf swapped for a symlink after the exists probe, and swallowing its
    // error as empty text would render a failure as "(empty file)". A
    // missing leaf opens as empty text; a refused or unreadable leaf is
    // already an error from `ConfigStore::open` above.
    let text = store.raw_text().to_owned();
    Ok(json!({
        "scope": scope.label(),
        "text": text,
        "file": file.json(),
        "layers": {
            "built_in": {"label": ConfigValueSourceKind::BuiltIn.label()},
            "global": global_file.json(),
            "workspace": workspace_file.json(),
            "execution_not_inherited": execution_not_inherited(runtime, &workspace_file),
            "not_inherited_keys": JsonValue::Array(Vec::new()),
        },
        "workspace_binding": JsonValue::Null,
        "sections": sections,
        // Crew tables are not registry keys, so a single-file snapshot cannot
        // build crew rows; the effective view is where crews are shown.
        // `crews_enumerated: false` says so, and `crew_tables` names the
        // tables this file defines, so an empty `crews` never reads as "no
        // crew is defined".
        "crews": JsonValue::Array(Vec::new()),
        "crews_enumerated": false,
        "crew_tables": store.crew_table_names(),
        "paths": path_rows_json(runtime, Some(store.path())),
        "crew_fields": CREW_FIELDS,
        "write_scope_default": scope.label(),
        "workspace_file_exists": workspace_file.exists,
    }))
}

/// The `orbit config keys` reference: every registered key with its write
/// availability, type, section, description, and accepted choices.
pub fn key_catalog() -> JsonValue {
    let keys = CONFIG_KEY_REGISTRY
        .iter()
        .map(|descriptor| {
            json!({
                "key": descriptor.key,
                "value_type": descriptor.value_type,
                "options": config_key_options(descriptor.key),
                "section": descriptor.section.token(),
                "section_title": descriptor.section.title(),
                "description": descriptor.description,
                "settable": admit_settable_config_key(descriptor.key).is_ok(),
            })
        })
        .collect::<Vec<_>>();
    json!({
        "keys": keys,
        "sections": section_catalog(),
        "crew_fields": CREW_FIELDS,
    })
}

/// The layered value of one key before a write, so the audit record can name
/// what the edit replaced.
pub(super) fn effective_value(runtime: &OrbitRuntime, key: &str) -> Result<JsonValue, OrbitError> {
    Ok(load_effective_config(&config_roots(runtime))?
        .value_for(key)
        .unwrap_or(JsonValue::Null))
}

/// The layered value of one crew, as the object its row renders.
pub(super) fn crew_value(runtime: &OrbitRuntime, name: &str) -> Result<JsonValue, OrbitError> {
    let effective = load_effective_config(&config_roots(runtime))?;
    Ok(crew_rows(runtime, effective.values())
        .into_iter()
        .find(|crew| crew["name"] == json!(name))
        .unwrap_or(JsonValue::Null))
}
