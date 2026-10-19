//! Layered loading and admission of staged config documents.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_home_dir;

use crate::ConfigRoots;
use crate::aliases::translate_deprecated_keys;
use crate::crew_pools::reject_unpoolable_crew_names_in_document;
use crate::operation::{OperationLayer, OperationLayerSource, OperationPolicy};
use crate::persistence::PersistenceConfig;
use crate::plugin_enablement::{
    plugin_enablement_from_document, reject_global_plugin_enablement, strip_plugin_enablement,
};
use crate::registry::{
    CONFIG_KEY_REGISTRY, GLOBAL_ONLY_KEY_PREFIX, read_optional, resolve_percent,
};
use crate::resolved::{ResolvedConfig, warn_compatibility_keys};

use super::document::{
    empty_document, merge_tables, parse_config_document, read_config_document, remove_value_at_path,
};
use super::provenance::{effective_values, source_for_crew_field};
use super::{EffectiveConfig, WORKSPACE_REPLACE_ONLY_KEYS, set_value_at_path, value_at_path};

/// Load the layered config and attribute every value to the layer it came from.
pub fn load_effective_config(roots: &ConfigRoots) -> Result<EffectiveConfig, OrbitError> {
    let loaded = load_layered_resolved(roots)?;
    let values = effective_values(
        &loaded.resolved,
        loaded.global.as_ref(),
        loaded.workspace.as_ref(),
    );
    Ok(EffectiveConfig {
        snapshot: loaded.resolved.snapshot,
        values,
    })
}

pub(super) struct ConfigDocument {
    pub(super) path: PathBuf,
    pub(super) value: toml::Value,
}

pub(crate) struct LoadedResolvedConfig {
    pub(crate) resolved: ResolvedConfig,
    global: Option<ConfigDocument>,
    workspace: Option<ConfigDocument>,
}

pub(crate) fn load_layered_resolved(
    roots: &ConfigRoots,
) -> Result<super::LoadedResolvedConfig, OrbitError> {
    load_layered_resolved_with_staged(roots, None, None)
}

/// Admit an in-memory workspace edit against the current global layer before
/// the edited document is written. This uses the same merge and per-layer
/// checks as a normal load, including validation of plugin toggles.
pub(crate) fn validate_staged_workspace_document(
    roots: &ConfigRoots,
    workspace_path: &Path,
    raw: &str,
) -> Result<ResolvedConfig, OrbitError> {
    if !roots.has_workspace_layer() {
        return Err(OrbitError::InvalidInput(format!(
            "`--root` or `ORBIT_ROOT` pins one config root for both layers, so there is no \
             workspace config layer; rerun with `--global` to edit '{}'",
            redact_home_dir(&roots.global().join("config.toml").display().to_string())
        )));
    }
    load_layered_resolved_with_staged(roots, None, Some((workspace_path, raw)))
        .map(|loaded| loaded.resolved)
}

/// Admit an in-memory global edit against the workspace layer it would be
/// read with, through the same layered load a runtime performs. Every
/// admission rule that spans both layers applies (both review layers on,
/// resource-throttle resume against high, crews a workspace pool or
/// `default_crew` names). Without this a global edit that is valid alone saves
/// cleanly and leaves that workspace's config unloadable, including for the
/// `config set` that would repair it.
pub(crate) fn validate_staged_global_document(
    roots: &ConfigRoots,
    global_path: &Path,
    raw: &str,
) -> Result<(), OrbitError> {
    if !roots.has_workspace_layer() {
        return Ok(());
    }
    load_layered_resolved_with_staged(roots, Some((global_path, raw)), None).map(|_| ())
}

/// Resolve workspace file values with only crew definitions inherited from
/// global. Other global settings must not appear in a scoped file snapshot.
pub(crate) fn resolve_workspace_file_document(
    global_path: &Path,
    global_raw: &str,
    workspace_path: &Path,
    raw: &str,
) -> Result<ResolvedConfig, OrbitError> {
    let mut workspace = parse_config_document(workspace_path, raw)?;
    reject_workspace_machine_table(&workspace.value, workspace_path)?;
    let mut global = parse_config_document(global_path, global_raw)?;
    translate_layers(Some(&mut global), Some(&mut workspace))?;
    for document in [&global, &workspace] {
        reject_unpoolable_crew_names_in_document(&document.value, &document.path)?;
    }

    let mut table = toml::map::Map::new();
    if let Some(crews) = global.value.get("crews") {
        table.insert("crews".to_string(), crews.clone());
    }
    let mut scoped = toml::Value::Table(table);
    // The normal recursive merge preserves global fields when the workspace
    // overrides only part of a crew definition.
    merge_tables(&mut scoped, &workspace.value);
    warn_compatibility_keys(&workspace.value, workspace_path);
    let crew_field_path = |crew: &str, field: &str| {
        source_for_crew_field(crew, field, Some(&global), Some(&workspace))
            .path
            .unwrap_or_else(|| workspace_path.to_path_buf())
    };
    ResolvedConfig::from_scoped_value(
        scoped,
        workspace_path,
        &crew_field_path,
        PersistenceConfig::default_for_data_root(workspace_path.parent().unwrap_or(workspace_path)),
    )
}

/// The layered load, with an in-memory global or workspace document standing
/// in for the file it will be written to.
fn load_layered_resolved_with_staged(
    roots: &ConfigRoots,
    staged_global: Option<(&Path, &str)>,
    staged_workspace: Option<(&Path, &str)>,
) -> Result<LoadedResolvedConfig, OrbitError> {
    let global = if let Some((path, raw)) = staged_global {
        Some(parse_config_document(path, raw)?)
    } else {
        read_config_document(&roots.global().join("config.toml"))?
    };
    if let Some(global_document) = &global {
        reject_global_plugin_enablement(&global_document.value, &global_document.path)?;
    }
    let mut workspace = if roots.has_workspace_layer() {
        if let Some((path, raw)) = staged_workspace {
            Some(parse_config_document(path, raw)?)
        } else {
            read_config_document(&roots.workspace().join("config.toml"))?
        }
    } else {
        None
    };
    // The plugin toggles are read off the workspace file before it layers:
    // they are not a config setting, and a file that holds only them is not a
    // policy layer — it must not switch the replace-only security keys away
    // from global.
    let mut plugin_enablement = BTreeMap::new();
    if let Some(mut document) = workspace.take() {
        plugin_enablement = plugin_enablement_from_document(&document.value, &document.path)?;
        if strip_plugin_enablement(&mut document.value) {
            workspace = Some(document);
        }
    }
    let persistence = PersistenceConfig::default_for_roots(roots.global(), roots.workspace());
    let mut global = global;
    translate_layers(global.as_mut(), workspace.as_mut())?;

    let global_memory_high_percent = match global.as_ref() {
        Some(document) => resolve_percent(
            read_optional(
                &document.value,
                "workflow.resource_throttle.memory_high_percent",
                &document.path,
            )?,
            90,
            "workflow.resource_throttle.memory_high_percent",
        )?,
        None => 90,
    };

    // Merging erases which file a crew came from, and a colon-named crew is
    // only repairable by hand-editing that file, so each layer is checked
    // while its own path is still known.
    for document in [global.as_ref(), workspace.as_ref()].into_iter().flatten() {
        reject_unpoolable_crew_names_in_document(&document.value, &document.path)?;
    }
    if let Some(workspace_document) = &workspace {
        reject_workspace_machine_table(&workspace_document.value, &workspace_document.path)?;
    }

    // Missing files still admit an empty document: environment defaults and
    // their validation must not depend on whether a config file exists.
    let mut merged = global
        .as_ref()
        .map(|document| document.value.clone())
        .unwrap_or_else(empty_document);
    if let Some(workspace_document) = &workspace {
        merge_tables(&mut merged, &workspace_document.value);

        // Registry table values are one config key, so a workspace value
        // replaces the global table rather than merging its members.
        // Dynamically named crews are intentionally excluded:
        // their fields layer recursively so one crew field can be overridden.
        for descriptor in CONFIG_KEY_REGISTRY {
            if let Some(value) = value_at_path(&workspace_document.value, descriptor.key) {
                set_value_at_path(&mut merged, descriptor.key, value.clone());
            }
        }
        for key in WORKSPACE_REPLACE_ONLY_KEYS {
            if value_at_path(&workspace_document.value, key).is_none() {
                remove_value_at_path(&mut merged, key);
            }
        }
    }

    let config_path = workspace
        .as_ref()
        .or(global.as_ref())
        .map(|document| document.path.as_path())
        .unwrap_or_else(|| Path::new("<built-in defaults>"));
    // The merged document has lost which layer set each crew field, so the
    // ignored-field records ask the layers directly, as `config show` does.
    let crew_field_path = |crew: &str, field: &str| {
        source_for_crew_field(crew, field, global.as_ref(), workspace.as_ref())
            .path
            .unwrap_or_else(|| config_path.to_path_buf())
    };
    let mut resolved = ResolvedConfig::from_layered_value(
        merged,
        config_path,
        &crew_field_path,
        persistence,
        global_memory_high_percent,
    )?;
    for document in [global.as_ref(), workspace.as_ref()].into_iter().flatten() {
        warn_compatibility_keys(&document.value, &document.path);
    }
    resolved.operation = resolve_operation_layers(global.as_ref(), workspace.as_ref())?;
    // The winning layer's pool is the merged one, which the snapshot has
    // admitted against the crew registry; keep its canonical entries.
    resolved.operation.review_crew.value = resolved
        .snapshot
        .workflow_purpose_crews_review
        .clone()
        .unwrap_or_default();
    resolved.operation.pre_landing_review_crew.value = resolved
        .snapshot
        .workflow_purpose_crews_pre_landing_review
        .clone()
        .unwrap_or_default();
    resolved.plugin_enablement = plugin_enablement;
    Ok(LoadedResolvedConfig {
        resolved,
        global,
        workspace,
    })
}

/// Translate each layer's deprecated spellings ([`crate::aliases`]). The
/// workspace file goes first, against the global file as written, so a
/// deprecated switch it states alone keeps the half it inherited.
fn translate_layers(
    global: Option<&mut ConfigDocument>,
    workspace: Option<&mut ConfigDocument>,
) -> Result<(), OrbitError> {
    if let Some(workspace) = workspace {
        translate_deprecated_keys(
            &mut workspace.value,
            global.as_deref().map(|global| &global.value),
            &workspace.path,
        )?;
    }
    if let Some(global) = global {
        translate_deprecated_keys(&mut global.value, None, &global.path)?;
    }
    Ok(())
}

/// Resolve the review preferences from the exact layers rather than the
/// merged document, so each field records the layer that set it.
fn resolve_operation_layers(
    global: Option<&ConfigDocument>,
    workspace: Option<&ConfigDocument>,
) -> Result<OperationPolicy, OrbitError> {
    let global_layer = global
        .map(|document| OperationLayer::from_document(&document.value, &document.path))
        .transpose()?
        .unwrap_or_default();
    let workspace_layer = workspace
        .map(|document| OperationLayer::from_document(&document.value, &document.path))
        .transpose()?
        .unwrap_or_default();
    let policy = OperationPolicy::resolve(&[
        (OperationLayerSource::Global, &global_layer),
        (OperationLayerSource::Workspace, &workspace_layer),
    ]);
    policy.ensure_one_review_layer()?;
    Ok(policy)
}

/// Refuse a `[machine]` table in a workspace `config.toml`.
///
/// Machine identity is a per-user, per-machine fact: exactly the kind of value
/// a checkout must not be able to supply or override. This is the mirror image
/// of the replace-only security keys — there the workspace layer may set the
/// value and must restate it to keep it, here it may not set it at all.
pub(crate) fn reject_workspace_machine_table(
    document: &toml::Value,
    path: &Path,
) -> Result<(), OrbitError> {
    let table = GLOBAL_ONLY_KEY_PREFIX.trim_end_matches('.');
    if value_at_path(document, table).is_none() {
        return Ok(());
    }
    Err(OrbitError::InvalidInput(format!(
        "[{table}] is not a workspace setting: remove it from '{}'. This machine's identity \
         and worker limits live only in the global config.toml, where `orbit init` writes the \
         identity; `orbit config set --global machine.<key> <value>` edits them",
        redact_home_dir(&path.display().to_string())
    )))
}
