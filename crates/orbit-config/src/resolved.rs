//! The consumer-facing resolved view of `config.toml`.
//!
//! [`ResolvedConfig`] is what every runtime consumer reads: admitted settings,
//! execution policies, crew registry, persistence paths, and config-owned PR
//! settings. Building one from a document also runs the migration guards for
//! retired keys, so a stale config fails (or warns) at load rather than at the
//! point of use.
//!
//! Merging the two layers into that single document is [`crate::layering`]'s
//! job; this module only ever sees one already-merged document.

use std::collections::BTreeMap;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::model_defaults::{
    ANTIGRAVITY_DEFAULT_MODEL, CLAUDE_DEFAULT_STRONG, CLAUDE_DEFAULT_WEAK, CLAUDE_FABLE_MODEL,
    CODEX_ASTRA_MODEL, CODEX_LUNA_MODEL, CODEX_SOL_MODEL, CODEX_TERRA_MODEL, COPILOT_DEFAULT_MODEL,
    CURSOR_DEFAULT_MODEL, GEMINI_CREW_MODEL, GROK_DEFAULT_MODEL, OPENCODE_DEFAULT_MODEL,
    PI_DEFAULT_MODEL,
};
use orbit_common::security::child_env::{allowlisted_child_env, inherited_child_env};
use orbit_common::security::redaction::redact_home_dir;
use orbit_types::identity::{Crew, CrewAssignment, ReasoningEffort, validate_antigravity_model};
use orbit_types::workflow::activity_job::{
    Provider, RETIRED_BACKEND_MIGRATION, check_retired_backend_value,
};

use crate::ConfigRoots;
use crate::layering::{load_layered_resolved, value_at_path};
use crate::operation::{OperationLayer, OperationLayerSource, OperationPolicy};
use crate::persistence::PersistenceConfig;
use crate::raw::{RawCrewEntry, RawRuntimeConfig, RawTaskSection};
use crate::registry::{ConfigSnapshot, DEFAULT_WORKFLOW_SYSTEM_CREW, LEGACY_WORKFLOW_SYSTEM_CREW};

/// PR-rendering settings owned by configuration.
///
/// Kept as config-owned data rather than an execution-engine type: this crate
/// has no engine dependency, so the composition layer that builds a runtime
/// performs the translation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrSettings {
    /// URL template used to link a task ID in PR descriptions.
    pub task_url_template: Option<String>,
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
    /// Default base branch for ship workflows. Sourced from `[workflow]
    /// base_branch`; defaults to `"main"` when no key is set.
    pub workflow_base_branch: String,
    /// Opt-in for unattended ship dispatch (`[workflow] auto_ship`; defaults
    /// to `false`).
    pub workflow_auto_ship: bool,
    /// Named provider-model assignments from `[crews.<name>]`.
    pub crews: BTreeMap<String, Crew>,
    /// Crew used when a task declares none and no override is given.
    pub default_crew: Option<String>,
    /// Automatic admission pools; explicit task assignments take precedence.
    pub complexity_crews: crate::ComplexityCrewPools,
    /// Crew used by system activities such as step-failure recovery and
    /// failed-run triage. Resolution of the named crew is deliberately
    /// deferred to dispatch so a bad system crew does not stop unrelated
    /// activity execution.
    pub system_crew: String,
    /// Resolved operation-mode preferences with per-field provenance
    /// (`[operation]`; built-in supervised). Preferences only: authority is
    /// a separate grant [ORB-11332].
    pub operation: OperationPolicy,
    /// Optional floor for the local task-id allocator (`[tasks] id_start`).
    /// Applied forward-only on runtime build so machines can hold disjoint id
    /// ranges. `None` leaves the allocator untouched.
    pub tasks_id_start: Option<u32>,
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
            },
            scoring_enabled: snapshot.scoring_enabled,
            workflow_base_branch: snapshot.workflow_base_branch.clone(),
            workflow_auto_ship: snapshot.workflow_auto_ship,
            crews: default_crews(),
            default_crew: snapshot.workflow_default_crew.clone(),
            complexity_crews: crate::ComplexityCrewPools {
                low: Some(snapshot.workflow_low_complexity_crews.clone()),
                medium: Some(snapshot.workflow_medium_complexity_crews.clone()),
                hard: Some(snapshot.workflow_hard_complexity_crews.clone()),
            },
            system_crew: snapshot.workflow_system_crew.clone(),
            operation: OperationPolicy::built_in(),
            tasks_id_start: snapshot.tasks_id_start,
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
        Self::from_raw_str_with_warnings(raw, config_path, persistence, true)
    }

    /// Parse a merged layered document while leaving compatibility warnings
    /// to the loader, which still has each source document and its path.
    pub(crate) fn from_layered_raw_str(
        raw: &str,
        config_path: &Path,
        persistence: PersistenceConfig,
    ) -> Result<Self, OrbitError> {
        Self::from_raw_str_with_warnings(raw, config_path, persistence, false)
    }

    fn from_raw_str_with_warnings(
        raw: &str,
        config_path: &Path,
        persistence: PersistenceConfig,
        emit_compatibility_warnings: bool,
    ) -> Result<Self, OrbitError> {
        let parsed = toml::from_str::<RawRuntimeConfig>(raw).map_err(|err| {
            OrbitError::InvalidInput(format!(
                "invalid runtime config '{}': {err}",
                redact_home_dir(&config_path.display().to_string())
            ))
        })?;
        let document = toml::from_str::<toml::Value>(raw).map_err(|err| {
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
        let mut crews = crews_from_raw(parsed.crews.as_ref())?;
        let snapshot = ConfigSnapshot::admit(&document, config_path, &crews)?;
        // One document is one layer. The layered loader replaces this with
        // the exact global/workspace resolution; a single file (or the
        // store's pre-write validation) resolves it as the workspace layer.
        let operation_layer = OperationLayer::from_document(&document, config_path)?;
        let operation =
            OperationPolicy::resolve(&[(OperationLayerSource::Workspace, &operation_layer)]);
        alias_system_crew(
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
        };
        if emit_compatibility_warnings {
            compatibility_keys.warn(config_path);
        }

        Ok(Self {
            execution_env: ExecutionEnvPolicy::from_snapshot(&snapshot),
            codex_execution: CodexExecutionPolicy::from_snapshot(&snapshot),
            persistence,
            pr: PrSettings {
                task_url_template: snapshot.pr_task_url_template.clone(),
            },
            scoring_enabled: snapshot.scoring_enabled,
            workflow_base_branch: snapshot.workflow_base_branch.clone(),
            workflow_auto_ship: snapshot.workflow_auto_ship,
            crews,
            default_crew: snapshot.workflow_default_crew.clone(),
            complexity_crews: crate::ComplexityCrewPools {
                low: Some(snapshot.workflow_low_complexity_crews.clone()),
                medium: Some(snapshot.workflow_medium_complexity_crews.clone()),
                hard: Some(snapshot.workflow_hard_complexity_crews.clone()),
            },
            system_crew: snapshot.workflow_system_crew.clone(),
            operation,
            tasks_id_start: snapshot.tasks_id_start,
            snapshot,
        })
    }
}

pub(crate) fn default_crews() -> BTreeMap<String, Crew> {
    let mut crews = BTreeMap::new();
    for (name, model, provider) in [
        ("opus", CLAUDE_DEFAULT_STRONG, "claude"),
        ("sonnet", CLAUDE_DEFAULT_WEAK, "claude"),
        ("fable", CLAUDE_FABLE_MODEL, "claude"),
        ("sol", CODEX_SOL_MODEL, "codex"),
        ("terra", CODEX_TERRA_MODEL, "codex"),
        ("luna", CODEX_LUNA_MODEL, "codex"),
        ("astra", CODEX_ASTRA_MODEL, "codex"),
        ("gemini", GEMINI_CREW_MODEL, "gemini"),
        ("antigravity", ANTIGRAVITY_DEFAULT_MODEL, "antigravity"),
        ("grok", GROK_DEFAULT_MODEL, "grok"),
        ("copilot", COPILOT_DEFAULT_MODEL, "copilot"),
        ("cursor", CURSOR_DEFAULT_MODEL, "cursor"),
        ("pi", PI_DEFAULT_MODEL, "pi"),
        ("opencode", OPENCODE_DEFAULT_MODEL, "opencode"),
        // [ORB-10877] Shipped job steps name `system` directly, so the
        // built-in set used by a config with no `[crews]` table must define it
        // or those pipelines fail validation. `orbit init` overwrites this with
        // the detected family's cheapest tier; the claude tier here matches the
        // family the built-in `default_crew` already assumes.
        (DEFAULT_WORKFLOW_SYSTEM_CREW, CLAUDE_DEFAULT_WEAK, "claude"),
    ] {
        crews.insert(
            name.to_string(),
            Crew {
                name: name.to_string(),
                assignment: crew_assignment(model, provider),
                description: None,
                tags: Vec::new(),
            },
        );
    }
    crews
}

fn crew_assignment(model: &str, provider: &str) -> CrewAssignment {
    CrewAssignment {
        model: model.to_string(),
        provider: provider.to_string(),
        effort: None,
    }
}

/// The retired invocation-level agent backend override.
pub(crate) const RETIRED_BACKEND_ENV: &str = "ORBIT_BACKEND";

/// [ORB-10801] `ORBIT_BACKEND` and `[runtime] backend` were tiers 2 and 3 of
/// the retired agent-loop backend precedence chain. Both are refused rather
/// than ignored: an operator who still pins `http` must be told their runs are
/// now CLI-agent runs instead of having that substitution made for them.
/// `cli` named the surviving path, so it stays accepted and inert.
fn reject_retired_backend_overrides(
    document: &toml::Value,
    env_value: Option<&str>,
) -> Result<(), OrbitError> {
    if let Some(raw) = env_value.map(str::trim).filter(|value| !value.is_empty()) {
        check_retired_backend_value(raw).map_err(|error| {
            OrbitError::InvalidInput(format!("{RETIRED_BACKEND_ENV} is retired: {error}"))
        })?;
    }
    let Some(value) = value_at_path(document, "runtime.backend") else {
        return Ok(());
    };
    let raw = value.as_str().ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "[runtime] backend must be a string; {RETIRED_BACKEND_MIGRATION}"
        ))
    })?;
    check_retired_backend_value(raw)
        .map_err(|error| OrbitError::InvalidInput(format!("[runtime] {error}")))
}

#[cfg(test)]
pub(crate) fn retired_backend_override_check(
    document: &toml::Value,
    env_value: Option<&str>,
) -> Result<(), OrbitError> {
    reject_retired_backend_overrides(document, env_value)
}

fn reject_stale_agent_tables(
    raw: Option<&BTreeMap<String, toml::Value>>,
) -> Result<(), OrbitError> {
    if raw.is_some() {
        // ORB-00058: source provenance for retiring the old agent-role schema.
        return Err(OrbitError::InvalidInput(
            "config schema no longer supports [agent.<role>] tables; migrate to [crews.<name>] with [workflow].default_crew".to_string(),
        ));
    }
    Ok(())
}

fn crews_from_raw(
    raw: Option<&BTreeMap<String, RawCrewEntry>>,
) -> Result<BTreeMap<String, Crew>, OrbitError> {
    let Some(raw_crews) = raw else {
        return Ok(default_crews());
    };
    let mut crews = BTreeMap::new();
    for (name, entry) in raw_crews {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(OrbitError::InvalidInput(
                "[crews] names must not be empty".to_string(),
            ));
        }
        let crew = Crew {
            name: trimmed.to_string(),
            assignment: crew_assignment_from_raw(trimmed, entry)?,
            description: normalized_crew_description(entry.description.as_deref()),
            tags: normalized_crew_tags(&entry.tags),
        };
        if crews.insert(trimmed.to_string(), crew).is_some() {
            return Err(OrbitError::InvalidInput(format!(
                "[crews] contains duplicate name '{trimmed}' after whitespace normalization"
            )));
        }
    }
    Ok(crews)
}

/// [ORB-10877] Shipped job steps name the `system` crew directly so the
/// definition says which crew does the work. A config written before that crew
/// was seeded has no `[crews.system]` table, so resolve the name rather than
/// failing those hosts at dispatch.
///
/// `configured` is `workflow.system_crew`, which is how such a config already
/// says where system work belongs. A defined configured crew wins. For the two
/// names Orbit itself has used for this lane (`system` and legacy `qa`), fall
/// back to `qa` and then the already-validated default crew. That final fallback
/// keeps pre-system Gemini- and Grok-only configs portable: those versions never
/// seeded `qa`, but they did seed their family default. Unknown custom names do
/// not receive this compatibility fallback, so a typo still fails closed at
/// dispatch. An explicit `[crews.system]` always wins.
fn alias_system_crew(
    crews: &mut BTreeMap<String, Crew>,
    configured: &str,
    default_crew: Option<&str>,
) {
    if crews.contains_key(DEFAULT_WORKFLOW_SYSTEM_CREW) {
        return;
    }
    let source = crews.get(configured).cloned().or_else(|| {
        if !matches!(
            configured,
            DEFAULT_WORKFLOW_SYSTEM_CREW | LEGACY_WORKFLOW_SYSTEM_CREW
        ) {
            return None;
        }
        crews
            .get(LEGACY_WORKFLOW_SYSTEM_CREW)
            .or_else(|| default_crew.and_then(|name| crews.get(name)))
            .cloned()
    });
    let Some(source) = source else {
        return;
    };
    crews.insert(
        DEFAULT_WORKFLOW_SYSTEM_CREW.to_string(),
        Crew {
            name: DEFAULT_WORKFLOW_SYSTEM_CREW.to_string(),
            ..source
        },
    );
}

fn normalized_crew_description(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn normalized_crew_tags(raw: &[String]) -> Vec<String> {
    let mut tags = raw
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    tags.sort();
    tags.dedup();
    tags
}

fn crew_assignment_from_raw(crew: &str, raw: &RawCrewEntry) -> Result<CrewAssignment, OrbitError> {
    let has_legacy = raw.planner.is_some() || raw.implementer.is_some() || raw.reviewer.is_some();
    if has_legacy {
        return Err(OrbitError::InvalidInput(format!(
            "[crews.{crew}] uses retired planner/implementer/reviewer role tables; rewrite it with flat `model` and `provider` fields only"
        )));
    }
    reject_retired_crew_backend(crew, raw.backend.as_deref())?;
    let model = required_crew_field(crew, "model", raw.model.as_deref())?;
    let provider = required_crew_field(crew, "provider", raw.provider.as_deref())?;
    if Provider::parse(&provider).ok() == Some(Provider::Antigravity) {
        validate_antigravity_model(Some(model.as_str()))
            .map_err(|error| OrbitError::InvalidInput(format!("[crews.{crew}].model {error}")))?;
    }
    Ok(CrewAssignment {
        model,
        provider,
        effort: crew_effort_from_raw(
            crew,
            raw.effort.as_deref(),
            raw.provider.as_deref(),
            raw.model.as_deref(),
        )?,
    })
}

/// Validate the provider-model-specific crew setting at config admission.
fn crew_effort_from_raw(
    crew: &str,
    raw_effort: Option<&str>,
    raw_provider: Option<&str>,
    raw_model: Option<&str>,
) -> Result<Option<ReasoningEffort>, OrbitError> {
    let Some(raw_effort) = raw_effort else {
        return Ok(None);
    };
    let effort = raw_effort
        .parse::<ReasoningEffort>()
        .map_err(|error| OrbitError::InvalidInput(format!("[crews.{crew}].{error}")))?;
    let provider = required_crew_field(crew, "provider", raw_provider)?;
    let provider = Provider::resolve_name(&provider).map_err(|_| {
        OrbitError::InvalidInput(format!(
            "[crews.{crew}].effort requires a supported effort provider; provider '{provider}' is unsupported"
        ))
    })?;
    effort
        .validate_for_provider_model(provider.provider.as_str(), raw_model)
        .map_err(|error| OrbitError::InvalidInput(format!("[crews.{crew}].effort {error}")))?;
    Ok(Some(effort))
}

/// [ORB-10801] `[crews.<name>] backend` selected the agent execution backend.
/// Only the CLI agent path survives, so `cli` stays accepted and inert while
/// the removed values are refused: remapping `http` onto the CLI agent would
/// change which runtime the crew dispatches to without saying so.
fn reject_retired_crew_backend(crew: &str, raw: Option<&str>) -> Result<(), OrbitError> {
    let Some(value) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(());
    };
    check_retired_backend_value(value)
        .map_err(|error| OrbitError::InvalidInput(format!("[crews.{crew}] {error}")))
}

fn required_crew_field(crew: &str, field: &str, value: Option<&str>) -> Result<String, OrbitError> {
    let value = value.map(str::trim).filter(|value| !value.is_empty());
    value.map(ToOwned::to_owned).ok_or_else(|| {
        OrbitError::InvalidInput(format!("[crews.{crew}].{field} must not be empty"))
    })
}

fn validate_task_artifact_store_from_raw(raw: Option<&RawTaskSection>) -> Result<(), OrbitError> {
    let Some(value) = raw.and_then(|section| section.artifact_store.as_deref()) else {
        return Ok(());
    };
    let trimmed = value.trim();
    Err(OrbitError::InvalidInput(format!(
        "[task] artifact_store is no longer supported; remove the key because v2 task artifacts are always enabled (found '{trimmed}')"
    )))
}

fn warn_deprecated_task_id_pattern(config_path: &Path) {
    let path = redact_home_dir(&config_path.display().to_string());
    tracing::warn!(
        config = %path,
        "knowledge.task_id_pattern is deprecated and ignored",
    );
}

struct CompatibilityKeys {
    deprecated_task_id_pattern: bool,
    retired_duel: bool,
    retired_routines: bool,
}

impl CompatibilityKeys {
    fn warn(&self, config_path: &Path) {
        if self.deprecated_task_id_pattern {
            warn_deprecated_task_id_pattern(config_path);
        }
        if self.retired_duel {
            warn_retired_duel_config(config_path);
        }
        if self.retired_routines {
            warn_retired_routines_config(config_path);
        }
    }
}

pub(crate) fn warn_compatibility_keys(document: &toml::Value, config_path: &Path) {
    CompatibilityKeys {
        deprecated_task_id_pattern: value_at_path(document, "knowledge.task_id_pattern").is_some(),
        retired_duel: value_at_path(document, "duel").is_some(),
        retired_routines: value_at_path(document, "routines").is_some(),
    }
    .warn(config_path);
}

pub(crate) const RETIRED_DUEL_CONFIG_WARNING: &str =
    "[duel] and [duel.models] are retired and ignored; remove both keys from config.toml";

fn warn_retired_duel_config(config_path: &Path) {
    let path = redact_home_dir(&config_path.display().to_string());
    tracing::warn!(
        config = %path,
        RETIRED_DUEL_CONFIG_WARNING,
    );
}

/// [ORB-12236] Registering an owner checkout is the automation opt-in, so the
/// versioned `[routines] role` key no longer selects anything. Accepted and
/// ignored for one release; delete this guard after 2026-12-01, when the key
/// becomes an ordinary unknown section.
pub(crate) const RETIRED_ROUTINES_CONFIG_WARNING: &str = "[routines] is retired and ignored; every registered owner checkout is a routine source — \
     remove the section from config.toml";

fn warn_retired_routines_config(config_path: &Path) {
    let path = redact_home_dir(&config_path.display().to_string());
    tracing::warn!(
        config = %path,
        RETIRED_ROUTINES_CONFIG_WARNING,
    );
}

/// Codex sandbox and approval policy resolved from `[execution.codex]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexExecutionPolicy {
    sandbox: String,
    approval_policy: Option<String>,
}

impl Default for CodexExecutionPolicy {
    fn default() -> Self {
        Self {
            sandbox: "workspace-write".to_string(),
            approval_policy: None,
        }
    }
}

impl CodexExecutionPolicy {
    fn from_snapshot(snapshot: &ConfigSnapshot) -> Self {
        Self {
            sandbox: snapshot.codex_sandbox.clone(),
            approval_policy: snapshot.codex_approval_policy.clone(),
        }
    }

    /// Configured sandbox mode.
    pub fn sandbox(&self) -> &str {
        &self.sandbox
    }

    /// Configured approval policy, when one is set.
    pub fn approval_policy(&self) -> Option<&str> {
        self.approval_policy.as_deref()
    }
}

/// Environment passthrough policy for agent subprocesses, resolved from
/// `[execution.env]`.
#[derive(Debug, Clone)]
pub struct ExecutionEnvPolicy {
    /// Whether a child inherits the parent environment wholesale.
    ///
    /// `ConfigSnapshot::execution_env_inherit` is a derived invariant pinned to
    /// `false` — `execution.env.inherit` stopped being settable in ORB-00365,
    /// because a workspace `config.toml` could flip it and replace the global
    /// value. The flag survives as the single seam that decides between full
    /// inheritance and the allowlist, so the two behaviors stay one branch in
    /// one place rather than two spawn paths.
    pub(crate) inherit: bool,
    /// `execution.env.pass`: the names an operator admits by name.
    pub(crate) pass: Vec<String>,
}

impl Default for ExecutionEnvPolicy {
    fn default() -> Self {
        Self {
            inherit: false,
            pass: default_pass_list(),
        }
    }
}

impl ExecutionEnvPolicy {
    fn from_snapshot(snapshot: &ConfigSnapshot) -> Self {
        Self {
            inherit: snapshot.execution_env_inherit,
            pass: snapshot.execution_env_pass.clone(),
        }
    }

    /// Whether the full process environment is inherited rather than
    /// allow-listed.
    pub fn inherit(&self) -> bool {
        self.inherit
    }

    /// The complete environment an agent subprocess is launched with.
    ///
    /// This is the only place the policy becomes a concrete child environment,
    /// and every subprocess launcher starts from a cleared environment and
    /// applies exactly this — so `inherit = false` really is allowlist-based
    /// rather than a filter over ambient variables. `extras` carries the names
    /// a provider declares it requires.
    pub fn agent_subprocess_env(&self, extras: &[&str]) -> Vec<(String, String)> {
        if self.inherit {
            return inherited_child_env();
        }
        allowlisted_child_env(&self.pass, extras)
    }

    /// Required variables that this policy would not deliver to a subprocess.
    pub fn missing_required(&self, required_env_vars: &[&str]) -> Vec<String> {
        required_env_vars
            .iter()
            .copied()
            .filter(|name| !self.is_required_var_available(name))
            .map(ToString::to_string)
            .collect()
    }

    fn is_required_var_available(&self, name: &str) -> bool {
        if self.inherit {
            return std::env::var(name).is_ok();
        }
        self.pass.iter().any(|candidate| candidate == name) && std::env::var(name).is_ok()
    }
}

fn default_pass_list() -> Vec<String> {
    ConfigSnapshot::default().execution_env_pass
}
