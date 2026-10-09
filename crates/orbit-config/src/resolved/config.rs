//! Resolved configuration construction and admitted consumer settings.

use std::collections::BTreeMap;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_home_dir;
use orbit_types::identity::Crew;

use super::compatibility::{
    CompatibilityKeys, RETIRED_BACKEND_ENV, deprecated_keys_present,
    reject_retired_backend_overrides, reject_stale_agent_tables, removed_keys_present,
    validate_task_artifact_store_from_raw,
};
use super::crew::{IgnoredCrewProperty, alias_system_crew, crews_from_raw, default_crews};
use super::execution_env::{CodexExecutionPolicy, ExecutionEnvPolicy};
use crate::ConfigRoots;
use crate::layering::load_layered_resolved;
use crate::operation::{
    OperationLayer, OperationLayerSource, OperationPolicy, translate_legacy_review_keys,
};
use crate::persistence::PersistenceConfig;
use crate::raw::RawRuntimeConfig;
use crate::registry::ConfigSnapshot;

/// PR rendering and lifecycle settings owned by configuration.
///
/// Kept as config-owned data rather than an execution-engine type: this crate
/// has no engine dependency, so the composition layer that builds a runtime
/// performs the translation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrSettings {
    /// URL template used to link a task ID in PR descriptions.
    pub task_url_template: Option<String>,
    /// Close a task's open Orbit-authored PRs when it reaches done, rejected
    /// or archived (`pr.close_on_terminal`, default `true`).
    pub close_on_terminal: bool,
    /// Normalized forge logins whose PRs count as Orbit-authored; empty means
    /// the forge CLI's authenticated login (`pr.delivery_authors`).
    pub delivery_authors: Vec<String>,
}

/// Every setting a runtime consumer needs, admitted and defaulted.
#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    /// Admitted values for every fixed registry key.
    pub snapshot: ConfigSnapshot,
    /// Environment passthrough policy for agent subprocesses.
    pub execution_env: ExecutionEnvPolicy,
    /// Codex sandbox and approval policy.
    pub codex_execution: CodexExecutionPolicy,
    /// Artifact store paths derived from the two roots.
    pub persistence: PersistenceConfig,
    /// Config-owned PR settings.
    pub pr: PrSettings,
    /// Whether scoreboard metrics are recorded for task runs.
    pub scoring_enabled: bool,
    /// Minutes a deferred delivery-automation reason may persist before it is
    /// escalated (`[automation] stall_window_minutes`; default 60).
    pub automation_stall_window_minutes: u32,
    /// Config-only `[workflow] base_branch` fallback (default `"main"`).
    /// Delivery defaults prefer the registered workspace base branch.
    pub workflow_base_branch: String,
    /// Opt-in for unattended ship dispatch (`[workflow] auto_ship`; defaults
    /// to `false`).
    pub workflow_auto_ship: bool,
    /// Host pressure thresholds and the throttle enable switch.
    pub resource_throttle: crate::registry::ResourceThrottleSettings,
    /// Named provider-model assignments from `[crews.<name>]`, disabled crews
    /// included (see [`Crew::enabled`]).
    pub crews: BTreeMap<String, Crew>,
    /// The crew the synthesized `system` entry mirrors when no `[crews.system]`
    /// table exists (see `alias_system_crew`). `None` when `system` is its own
    /// table or could not be resolved.
    pub system_crew_alias: Option<String>,
    /// Crew used when a task declares none and no override is given.
    pub default_crew: Option<String>,
    /// Automatic admission pools; explicit task assignments take precedence.
    pub complexity_crews: crate::ComplexityCrewPools,
    /// Crew used by system activities such as step-failure recovery and the
    /// task pilot. Resolution of the named crew is deliberately
    /// deferred to dispatch so a bad system crew does not stop unrelated
    /// activity execution.
    pub system_crew: String,
    /// Resolved review preferences with per-field provenance (built-in: no
    /// before-PR review) [ORB-11333] [ORB-13992].
    pub operation: OperationPolicy,
    /// Optional floor for the local task-id allocator (`[tasks] id_start`).
    /// Applied forward-only on runtime build so machines can hold disjoint id
    /// ranges. `None` leaves the allocator untouched.
    pub tasks_id_start: Option<u32>,
    /// Optional crew tunables ignored at admission (warn-and-unset).
    pub ignored_crew_properties: Vec<IgnoredCrewProperty>,
    /// `[plugins.<ns>]` sections, one JSON object per namespace. Admitted
    /// structurally here; the plugin's own JSON Schema is applied by the layer
    /// that knows which plugins are installed (design §1).
    pub plugins: BTreeMap<String, serde_json::Value>,
    /// Workspace `[plugin_enablement]` toggles: `false` switches an
    /// otherwise host-enabled plugin off in this workspace; an absent entry
    /// inherits the host state. Always empty for the global layer, which
    /// refuses the table.
    pub plugin_enablement: BTreeMap<String, bool>,
}

impl ResolvedConfig {
    /// Built-in defaults for every setting, with caller-supplied persistence
    /// paths. There is no cwd-derived variant: persistence is always a
    /// function of the roots the caller resolved.
    pub fn built_in(persistence: PersistenceConfig) -> Self {
        let snapshot = ConfigSnapshot::default();
        Self {
            execution_env: ExecutionEnvPolicy::from_snapshot(&snapshot),
            codex_execution: CodexExecutionPolicy::from_snapshot(&snapshot),
            persistence,
            pr: PrSettings {
                task_url_template: snapshot.pr_task_url_template.clone(),
                close_on_terminal: snapshot.pr_close_on_terminal,
                delivery_authors: snapshot.pr_delivery_authors.clone(),
            },
            scoring_enabled: snapshot.scoring_enabled,
            automation_stall_window_minutes: snapshot.automation_stall_window_minutes,
            workflow_base_branch: snapshot.workflow_base_branch.clone(),
            workflow_auto_ship: snapshot.workflow_auto_ship,
            resource_throttle: snapshot.resource_throttle(),
            crews: default_crews(),
            system_crew_alias: None,
            default_crew: snapshot.workflow_default_crew.clone(),
            complexity_crews: crate::ComplexityCrewPools {
                low: Some(snapshot.workflow_low_complexity_crews.clone()),
                medium: Some(snapshot.workflow_medium_complexity_crews.clone()),
                hard: Some(snapshot.workflow_hard_complexity_crews.clone()),
                xhard: Some(snapshot.workflow_xhard_complexity_crews.clone()),
            },
            system_crew: snapshot.workflow_system_crew.clone(),
            operation: OperationPolicy::built_in(),
            tasks_id_start: snapshot.tasks_id_start,
            ignored_crew_properties: Vec::new(),
            plugins: BTreeMap::new(),
            plugin_enablement: BTreeMap::new(),
            snapshot,
        }
    }

    /// Load config with per-key workspace-over-global layering.
    ///
    /// Persistence paths are always derived from the two roots (not configurable).
    ///
    /// Ordinary keys inherit from global when absent from the workspace file.
    /// Sandbox mode, approval policy, and the environment allowlist are the
    /// exception: whenever a distinct workspace file exists, omissions for
    /// those keys resolve to built-in defaults rather than global values.
    pub fn load(roots: &ConfigRoots) -> Result<Self, OrbitError> {
        load_layered_resolved(roots).map(|loaded| loaded.resolved)
    }

    /// Parse and validate a raw `config.toml` document string into a fully
    /// resolved config, running it through the exact same validation pipeline
    /// as [`Self::load`].
    ///
    /// `config_path` is used only to build human-readable error messages
    /// (it need not exist on disk — this is also the entry point used by
    /// [`crate::ConfigStore::validate`] to check an in-memory edit before it is
    /// written to disk). `persistence` is supplied by the caller because
    /// persistence paths are derived from the two data roots, not from the
    /// config document itself.
    pub(crate) fn from_raw_str(
        raw: &str,
        config_path: &Path,
        persistence: PersistenceConfig,
    ) -> Result<Self, OrbitError> {
        let mut document = toml::from_str::<toml::Value>(raw).map_err(|err| {
            OrbitError::InvalidInput(format!(
                "invalid runtime config '{}': {err}",
                redact_home_dir(&config_path.display().to_string())
            ))
        })?;
        translate_legacy_review_keys(&mut document, config_path)?;
        let resolved =
            Self::from_document_with_warnings(document, config_path, persistence, true, false)?;
        // A merged layered document is checked once its layers resolve, with
        // each switch's real source; one file is its own workspace layer.
        resolved.operation.ensure_one_review_layer()?;
        Ok(resolved)
    }

    /// Resolve an already-merged layered document while leaving compatibility
    /// warnings to the loader, which still has each source document and its
    /// path. Takes ownership of the merged `toml::Value` directly rather than
    /// a re-serialized string, so the document is parsed once by the loader
    /// and never re-parsed here.
    pub(crate) fn from_layered_value(
        document: toml::Value,
        config_path: &Path,
        persistence: PersistenceConfig,
    ) -> Result<Self, OrbitError> {
        Self::from_document_with_warnings(document, config_path, persistence, false, false)
    }

    /// Admit a file snapshot with contextual crews, without requiring the
    /// scoped file to choose a runtime default crew when it omits that key.
    pub(crate) fn from_scoped_value(
        document: toml::Value,
        config_path: &Path,
        persistence: PersistenceConfig,
    ) -> Result<Self, OrbitError> {
        Self::from_document_with_warnings(document, config_path, persistence, false, true)
    }

    fn from_document_with_warnings(
        document: toml::Value,
        config_path: &Path,
        persistence: PersistenceConfig,
        emit_compatibility_warnings: bool,
        scoped: bool,
    ) -> Result<Self, OrbitError> {
        let parsed = document
            .clone()
            .try_into::<RawRuntimeConfig>()
            .map_err(|err| {
                OrbitError::InvalidInput(format!(
                    "invalid runtime config '{}': {err}",
                    redact_home_dir(&config_path.display().to_string())
                ))
            })?;

        if parsed.watch.is_some() {
            return Err(OrbitError::InvalidInput(
                "watch config is no longer supported; remove the [watch] section from config.toml"
                    .to_string(),
            ));
        }

        validate_task_artifact_store_from_raw(parsed.task.as_ref())?;
        reject_stale_agent_tables(parsed.agent.as_ref())?;
        reject_retired_backend_overrides(
            &document,
            std::env::var(RETIRED_BACKEND_ENV).ok().as_deref(),
        )?;
        let (mut crews, ignored_crew_properties) =
            crews_from_raw(parsed.crews.as_ref(), config_path)?;
        let snapshot = if scoped {
            ConfigSnapshot::admit_scoped(&document, config_path, &crews)?
        } else {
            ConfigSnapshot::admit(&document, config_path, &crews)?
        };
        // One document is one layer. The layered loader replaces this with
        // the exact global/workspace resolution; a single file (or the
        // store's pre-write validation) resolves it as the workspace layer.
        let operation_layer = OperationLayer::from_document(&document, config_path)?;
        let operation =
            OperationPolicy::resolve(&[(OperationLayerSource::Workspace, &operation_layer)]);
        let system_crew_alias = alias_system_crew(
            &mut crews,
            &snapshot.workflow_system_crew,
            snapshot.workflow_default_crew.as_deref(),
        );

        let compatibility_keys = CompatibilityKeys {
            deprecated_task_id_pattern: parsed
                .knowledge
                .as_ref()
                .and_then(|section| section.task_id_pattern.as_ref())
                .is_some(),
            retired_duel: parsed.duel.is_some(),
            retired_routines: parsed.routines.is_some(),
            retired_docs: parsed.docs.is_some(),
            removed_keys: removed_keys_present(&document),
            deprecated_keys: deprecated_keys_present(&document),
        };
        if emit_compatibility_warnings {
            compatibility_keys.warn(config_path);
        }

        let plugins = plugin_sections_from_raw(parsed.plugins.as_ref(), config_path)?;
        let plugin_enablement =
            crate::plugin_enablement::plugin_enablement_from_document(&document, config_path)?;

        Ok(Self {
            execution_env: ExecutionEnvPolicy::from_snapshot(&snapshot),
            codex_execution: CodexExecutionPolicy::from_snapshot(&snapshot),
            persistence,
            pr: PrSettings {
                task_url_template: snapshot.pr_task_url_template.clone(),
                close_on_terminal: snapshot.pr_close_on_terminal,
                delivery_authors: snapshot.pr_delivery_authors.clone(),
            },
            scoring_enabled: snapshot.scoring_enabled,
            automation_stall_window_minutes: snapshot.automation_stall_window_minutes,
            workflow_base_branch: snapshot.workflow_base_branch.clone(),
            workflow_auto_ship: snapshot.workflow_auto_ship,
            resource_throttle: snapshot.resource_throttle(),
            crews,
            system_crew_alias,
            default_crew: snapshot.workflow_default_crew.clone(),
            complexity_crews: crate::ComplexityCrewPools {
                low: Some(snapshot.workflow_low_complexity_crews.clone()),
                medium: Some(snapshot.workflow_medium_complexity_crews.clone()),
                hard: Some(snapshot.workflow_hard_complexity_crews.clone()),
                xhard: Some(snapshot.workflow_xhard_complexity_crews.clone()),
            },
            system_crew: snapshot.workflow_system_crew.clone(),
            operation,
            tasks_id_start: snapshot.tasks_id_start,
            ignored_crew_properties,
            plugins,
            plugin_enablement,
            snapshot,
        })
    }
}

/// Project every `[plugins.<ns>]` table as JSON.
///
/// Only the shape is checked here — a section must be a table, and the
/// namespace must be one a manifest could declare. Which keys are legal is the
/// plugin's own schema, applied where installed plugins are known.
fn plugin_sections_from_raw(
    raw: Option<&BTreeMap<String, toml::Value>>,
    config_path: &Path,
) -> Result<BTreeMap<String, serde_json::Value>, OrbitError> {
    let Some(raw) = raw else {
        return Ok(BTreeMap::new());
    };
    let mut sections = BTreeMap::new();
    for (namespace, value) in raw {
        if !orbit_types::plugin::is_valid_namespace(namespace) {
            return Err(OrbitError::InvalidInput(format!(
                "invalid runtime config '{}': '[plugins.{namespace}]' is not a plugin namespace:                  use lowercase letters, digits, '_' or '-', starting with a letter",
                redact_home_dir(&config_path.display().to_string())
            )));
        }
        let json = serde_json::to_value(value).map_err(|error| {
            OrbitError::InvalidInput(format!(
                "invalid runtime config '{}': '[plugins.{namespace}]' is not representable:                  {error}",
                redact_home_dir(&config_path.display().to_string())
            ))
        })?;
        if !json.is_object() {
            return Err(OrbitError::InvalidInput(format!(
                "invalid runtime config '{}': 'plugins.{namespace}' must be a table of that                  plugin's settings",
                redact_home_dir(&config_path.display().to_string())
            )));
        }
        sections.insert(namespace.clone(), json);
    }
    Ok(sections)
}
