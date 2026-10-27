//! Workspace presets: a validated file `orbit workspace init --preset <file>`
//! applies to a new or existing workspace (design
//! `docs/design/distributions/1_scope.md`).
//!
//! A preset is data a distribution ships. It may set the seeding policy
//! (`[seeding]`), enable definitions a plugin already seeded into the
//! workspace (`[enable]`), and set workspace config keys (`[config]`) that
//! `orbit config set` accepts. It never grants plugin permissions and never
//! enables a plugin: that consent stays with `orbit plugin enable`, and a
//! plugin still seeds its own definitions disabled.
//!
//! Everything is validated before anything is written: an unknown field, a
//! grant or plugin-enable request, a key `orbit config set` refuses, a
//! definition no plugin seeded here, and a seeded definition whose file is
//! missing or does not load all fail the whole preset. The applied preset's
//! name and the SHA-256 of its bytes are recorded under `.orbit/state/` for
//! provenance.

use std::path::{Path, PathBuf};

use chrono::Utc;
use orbit_automation::auto_tasks::loader::{AUTO_TASKS_DIR, collect_auto_tasks};
use orbit_automation::routines::loader::{
    ROUTINES_DIR, RoutineCatalogLookup, RoutineSource, collect_routines_with_skips,
};
use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_text;
use orbit_common::security::release::sha256_hex;
use orbit_config::{
    ConfigStore, SEEDING_AUTO_TASKS_KEY, SEEDING_ROUTINES_KEY, WorkspaceInitMode,
    admit_settable_config_key,
};
use serde::{Deserialize, Serialize};

use crate::OrbitRuntime;
use crate::application::job::DEFAULT_JOB_FILES;
use crate::application::plugin::seed::plugin_seeded_definitions;
use crate::application::routines::set_routine_enabled;
use crate::application::routines::status::checkout_edit_report;

/// Largest preset file Orbit reads.
const MAX_PRESET_BYTES: u64 = 256 * 1024;
/// Where the applied preset is recorded, under the workspace `.orbit` root.
const APPLIED_PRESET_FILE: &str = "state/workspace-preset.json";
const APPLIED_PRESET_SCHEMA_VERSION: u32 = 1;

/// Top-level keys a preset might use to ask for plugin consent. Each is
/// refused with the command that owns that consent, rather than as a merely
/// unknown field.
const PLUGIN_CONSENT_KEYS: &[&str] = &[
    "grant",
    "grants",
    "permissions",
    "plugin",
    "plugins",
    "enable_plugins",
    "plugin_enablement",
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PresetDocument {
    name: String,
    #[serde(default)]
    seeding: PresetSeeding,
    #[serde(default)]
    enable: PresetEnable,
    #[serde(default)]
    config: toml::Table,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PresetSeeding {
    routines: Option<toml::Value>,
    auto_tasks: Option<toml::Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PresetEnable {
    #[serde(default)]
    routines: Vec<String>,
    #[serde(default)]
    auto_tasks: Vec<String>,
}

/// A parsed, structurally validated preset.
#[derive(Debug, Clone, Default)]
pub struct WorkspacePreset {
    /// The preset's declared name.
    pub name: String,
    /// `sha256:<hex>` of the file's bytes.
    pub digest: String,
    /// Workspace config writes as `(key, TOML literal)`, seeding keys first.
    settings: Vec<(String, String)>,
    /// Plugin-seeded routines to enable, by seeded name.
    pub enable_routines: Vec<String>,
    /// Plugin-seeded auto-tasks to enable, by seeded name.
    pub enable_auto_tasks: Vec<String>,
}

impl WorkspacePreset {
    /// Read and validate the preset file at `path`. Refuses an unknown field,
    /// a plugin grant or enable request, and a config key `orbit config set`
    /// does not accept; nothing is written.
    pub fn load(path: &Path) -> Result<Self, OrbitError> {
        let refuse = |message: String| {
            OrbitError::InvalidInput(format!("preset '{}': {message}", path.display()))
        };
        let metadata = std::fs::metadata(path).map_err(|error| refuse(error.to_string()))?;
        if !metadata.is_file() {
            return Err(refuse("not a regular file".to_string()));
        }
        if metadata.len() > MAX_PRESET_BYTES {
            return Err(refuse(format!(
                "is larger than the {MAX_PRESET_BYTES}-byte preset limit"
            )));
        }
        let bytes = std::fs::read(path).map_err(|error| refuse(error.to_string()))?;
        let text =
            std::str::from_utf8(&bytes).map_err(|_| refuse("is not UTF-8 TOML".to_string()))?;
        let document: toml::Table =
            toml::from_str(text).map_err(|error| refuse(error.to_string()))?;
        refuse_plugin_consent(&document).map_err(refuse)?;
        let parsed: PresetDocument = toml::Value::Table(document)
            .try_into()
            .map_err(|error: toml::de::Error| refuse(error.to_string().trim_end().to_string()))?;
        validate_preset_name(&parsed.name).map_err(refuse)?;

        let mut settings = Vec::new();
        for (key, value) in [
            (SEEDING_ROUTINES_KEY, parsed.seeding.routines),
            (SEEDING_AUTO_TASKS_KEY, parsed.seeding.auto_tasks),
        ] {
            if let Some(value) = value {
                settings.push((key.to_string(), value.to_string()));
            }
        }
        let mut config = Vec::new();
        flatten_config(String::new(), parsed.config, &mut config);
        for (key, value) in config {
            if key.starts_with("seeding.") {
                return Err(refuse(format!(
                    "set `{key}` in the preset's [seeding] table, not under [config]"
                )));
            }
            if key.starts_with("plugin_enablement") {
                return Err(refuse(format!(
                    "`{key}` would enable a plugin; a preset never enables a plugin, run \
                     `orbit plugin enable` instead"
                )));
            }
            admit_settable_config_key(&key).map_err(|error| refuse(refusal_reason(error)))?;
            settings.push((key, value.to_string()));
        }

        for name in parsed
            .enable
            .routines
            .iter()
            .chain(&parsed.enable.auto_tasks)
        {
            if name.trim().is_empty() {
                return Err(refuse("an [enable] entry is blank".to_string()));
            }
        }
        Ok(Self {
            name: parsed.name,
            digest: format!("sha256:{}", sha256_hex(&bytes)),
            settings,
            enable_routines: parsed.enable.routines,
            enable_auto_tasks: parsed.enable.auto_tasks,
        })
    }

    /// Whether the preset writes any workspace config.
    pub fn sets_config(&self) -> bool {
        !self.settings.is_empty()
    }

    /// Check, without writing, that every config key this preset sets is
    /// admitted for the workspace at `workspace_orbit_root` exactly as
    /// `orbit config set` would admit it, and that every definition it
    /// enables is one a plugin seeded there whose file still loads.
    pub fn validate_for_workspace(
        &self,
        global_root: &Path,
        workspace_orbit_root: &Path,
    ) -> Result<(), OrbitError> {
        // Loadability is decided before the config store is staged, so a
        // missing or unloadable definition refuses the preset with no write.
        self.require_plugin_seeded(workspace_orbit_root)?;
        self.require_seeded_definitions_load(global_root, workspace_orbit_root)?;
        if self.sets_config() {
            let path = workspace_orbit_root.join("config.toml");
            let store = self.staged_store(global_root, &path, WorkspaceInitMode::SeedFromGlobal)?;
            drop(store);
        }
        Ok(())
    }

    /// Write the preset's config keys into the workspace `config.toml`, in one
    /// validated save. A missing file starts from the global policy, as
    /// `orbit config set --seed-from-global` would.
    pub fn apply_config(
        &self,
        global_root: &Path,
        workspace_orbit_root: &Path,
    ) -> Result<(), OrbitError> {
        if !self.sets_config() {
            return Ok(());
        }
        let path = workspace_orbit_root.join("config.toml");
        ConfigStore::with_write_lock(&path, || {
            let mut store =
                self.staged_store(global_root, &path, WorkspaceInitMode::SeedFromGlobal)?;
            store.save()
        })
    }

    fn staged_store(
        &self,
        global_root: &Path,
        path: &Path,
        init: WorkspaceInitMode,
    ) -> Result<ConfigStore, OrbitError> {
        if global_root == path.parent().unwrap_or(path) {
            return Err(OrbitError::InvalidInput(format!(
                "preset '{}' sets workspace config, but this workspace shares the global Orbit \
                 root, so there is no workspace config layer to write",
                self.name
            )));
        }
        let mut store =
            ConfigStore::open_for_workspace_set(path, &global_root.join("config.toml"), init)?;
        let refuse = |key: &str, error: OrbitError| {
            OrbitError::InvalidInput(format!(
                "preset '{}' sets `{key}`, which `orbit config set` refuses: {}",
                self.name,
                refusal_reason(error)
            ))
        };
        for (key, literal) in &self.settings {
            store
                .set_value(key, literal)
                .map_err(|error| refuse(key, error))?;
        }
        for (key, _) in &self.settings {
            store
                .validate_workspace_for_set(key, global_root)
                .map_err(|error| refuse(key, error))?;
        }
        Ok(store)
    }

    fn require_plugin_seeded(&self, workspace_orbit_root: &Path) -> Result<(), OrbitError> {
        for (kind, dir, names) in [
            (
                "routine",
                workspace_orbit_root.join(ROUTINES_DIR),
                &self.enable_routines,
            ),
            (
                "auto-task",
                workspace_orbit_root.join(AUTO_TASKS_DIR),
                &self.enable_auto_tasks,
            ),
        ] {
            if names.is_empty() {
                continue;
            }
            let seeded = plugin_seeded_definitions(&dir)?;
            if let Some(name) = names
                .iter()
                .find(|name| !seeded.contains_key(name.as_str()))
            {
                return Err(OrbitError::InvalidInput(format!(
                    "preset '{}' enables {kind} '{name}', which no plugin seeded into this \
                     workspace; a preset enables only definitions an enabled plugin seeded, so \
                     run `orbit plugin enable <plugin>` first",
                    self.name
                )));
            }
        }
        Ok(())
    }

    /// Refuse an `[enable]` name whose seeded file is missing or does not
    /// load. Routine targets resolve against the effective post-bootstrap
    /// catalog computed in memory (see
    /// [`effective_routine_catalog_lookup`]), so a target bootstrap would
    /// seed still passes while an unresolvable one refuses before any
    /// write. Catalog membership is resolved again when the preset is
    /// enabled, after bootstrap has seeded jobs and loaded plugins.
    fn require_seeded_definitions_load(
        &self,
        global_root: &Path,
        workspace_orbit_root: &Path,
    ) -> Result<(), OrbitError> {
        self.require_routines_load(global_root, workspace_orbit_root)?;
        self.require_auto_tasks_load(workspace_orbit_root)
    }

    fn require_routines_load(
        &self,
        global_root: &Path,
        workspace_orbit_root: &Path,
    ) -> Result<(), OrbitError> {
        if self.enable_routines.is_empty() {
            return Ok(());
        }
        let source = RoutineSource {
            workspace: "workspace".to_string(),
            orbit_dir: workspace_orbit_root.to_path_buf(),
        };
        let catalog_lookup = effective_routine_catalog_lookup(global_root, workspace_orbit_root);
        let collection =
            collect_routines_with_skips(&[source], &catalog_lookup, &|_path, _definition| None);
        let dir = workspace_orbit_root.join(ROUTINES_DIR);
        for name in &self.enable_routines {
            if collection.routines.iter().any(|loaded| {
                seeded_file_matches(&dir, name, &loaded.path, &loaded.definition.name)
            }) {
                continue;
            }
            let detail = collection.errors.iter().find_map(|error| {
                error
                    .path
                    .as_ref()
                    .is_some_and(|path| file_stem_is(path, name))
                    .then_some(error.message.as_str())
            });
            return Err(unloadable_definition(
                &self.name, "routine", name, &dir, detail,
            ));
        }
        Ok(())
    }

    fn require_auto_tasks_load(&self, workspace_orbit_root: &Path) -> Result<(), OrbitError> {
        if self.enable_auto_tasks.is_empty() {
            return Ok(());
        }
        let collection = collect_auto_tasks(workspace_orbit_root);
        let dir = workspace_orbit_root.join(AUTO_TASKS_DIR);
        for name in &self.enable_auto_tasks {
            if collection.definitions.iter().any(|loaded| {
                seeded_file_matches(&dir, name, &loaded.path, &loaded.definition.name)
            }) {
                continue;
            }
            let detail = collection.errors.iter().find_map(|error| {
                error
                    .path
                    .as_ref()
                    .is_some_and(|path| file_stem_is(path, name))
                    .then_some(error.message.as_str())
            });
            return Err(unloadable_definition(
                &self.name,
                "auto-task",
                name,
                &dir,
                detail,
            ));
        }
        Ok(())
    }

    /// Record this preset as the one last applied to the workspace.
    pub fn record_applied(&self, workspace_orbit_root: &Path) -> Result<(), OrbitError> {
        let record = AppliedWorkspacePreset {
            schema_version: APPLIED_PRESET_SCHEMA_VERSION,
            name: self.name.clone(),
            digest: self.digest.clone(),
            applied_at: Utc::now().to_rfc3339(),
        };
        let encoded = serde_json::to_string_pretty(&record)
            .map_err(|error| OrbitError::Execution(format!("encode applied preset: {error}")))?;
        let path = applied_preset_path(workspace_orbit_root);
        atomic_write_text(&path, &format!("{encoded}\n")).map_err(|error| {
            OrbitError::Io(format!(
                "record applied preset '{}': {error}",
                path.display()
            ))
        })
    }
}

/// The preset last applied to a workspace, as recorded for provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppliedWorkspacePreset {
    /// Record format version.
    pub schema_version: u32,
    /// The preset's declared name.
    pub name: String,
    /// `sha256:<hex>` of the applied file's bytes.
    pub digest: String,
    /// When it was applied (RFC 3339).
    pub applied_at: String,
}

/// The preset last applied to the workspace at `workspace_orbit_root`, if any.
pub fn applied_workspace_preset(
    workspace_orbit_root: &Path,
) -> Result<Option<AppliedWorkspacePreset>, OrbitError> {
    let path = applied_preset_path(workspace_orbit_root);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "read applied preset '{}': {error}",
                path.display()
            )));
        }
    };
    serde_json::from_str(&raw).map(Some).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "applied preset record '{}' is invalid: {error}",
            path.display()
        ))
    })
}

fn applied_preset_path(workspace_orbit_root: &Path) -> PathBuf {
    workspace_orbit_root.join(APPLIED_PRESET_FILE)
}

fn file_stem_is(path: &Path, name: &str) -> bool {
    path.file_stem().and_then(|stem| stem.to_str()) == Some(name)
}

fn seeded_file_matches(dir: &Path, name: &str, loaded_path: &Path, loaded_name: &str) -> bool {
    loaded_name == name
        && std::fs::canonicalize(dir.join(format!("{name}.yaml")))
            .ok()
            .zip(std::fs::canonicalize(loaded_path).ok())
            .is_some_and(|(seeded, loaded)| seeded == loaded)
}

/// The routine-target catalog lookup for the workspace as it will exist
/// after bootstrap, computed without writing anything.
///
/// Bootstrap seeds the shipped default jobs into the global jobs directory
/// (`seed_default_jobs` over [`DEFAULT_JOB_FILES`]), so those names resolve
/// even when that directory has not been seeded yet. The remaining layers
/// mirror runtime execution membership: explicit `ORBIT_JOB_DIR` /
/// `ORBIT_V2_JOB_DIR` directories, the global on-disk jobs, and the
/// workspace on-disk jobs, parsed best-effort so a malformed file reports
/// as a catalog error exactly as the runtime loader reports it. Plugin job
/// files need a runtime plugin load and are resolved again at enable time;
/// `enable_preset_definitions` re-checks every routine before enabling
/// anything, so a target only a plugin layer supplies cannot partially
/// apply there either.
fn effective_routine_catalog_lookup(
    global_root: &Path,
    workspace_orbit_root: &Path,
) -> impl Fn(&Path, &str) -> RoutineCatalogLookup {
    use orbit_engine::activity_job::{V2JobCatalog, catalog_error_to_orbit};

    let global_jobs_dir = global_root.join("resources/jobs");
    let workspace_jobs_dir = workspace_orbit_root.join("resources/jobs");
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(raw) = std::env::var("ORBIT_JOB_DIR")
        .ok()
        .or_else(|| std::env::var("ORBIT_V2_JOB_DIR").ok())
    {
        dirs.extend(
            raw.split(':')
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
        );
    }
    dirs.push(global_jobs_dir);
    dirs.push(workspace_jobs_dir);

    let mut catalog = V2JobCatalog::new();
    let mut errors: Vec<String> = Vec::new();
    let mut seen: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
    for dir in &dirs {
        let canonical = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.clone());
        if !seen.insert(canonical) || !dir.is_dir() {
            continue;
        }
        match catalog.load_dir_prefer_existing_best_effort(dir) {
            Ok(parse_errors) => errors.extend(
                parse_errors
                    .into_iter()
                    .map(|error| catalog_error_to_orbit(error).to_string()),
            ),
            Err(error) => errors.push(catalog_error_to_orbit(error).to_string()),
        }
    }
    let mut names: std::collections::BTreeSet<String> = catalog
        .iter()
        .map(|(name, _, _)| name.to_string())
        .collect();
    names.extend(
        DEFAULT_JOB_FILES
            .iter()
            .map(|(name, _)| (*name).to_string()),
    );
    let error = (!errors.is_empty()).then(|| errors.join("; "));
    move |_root, job| RoutineCatalogLookup {
        resolves: names.contains(job),
        error: error.clone(),
    }
}

/// Refusal for one `[enable]` name, naming that definition.
fn unloadable_definition(
    preset: &str,
    kind: &str,
    name: &str,
    dir: &Path,
    detail: Option<&str>,
) -> OrbitError {
    let path = dir.join(format!("{name}.yaml"));
    let because = if !path.is_file() {
        "the seeded file is missing".to_string()
    } else if let Some(detail) = detail.filter(|detail| !detail.is_empty()) {
        detail.to_string()
    } else {
        "the seeded file does not load".to_string()
    };
    OrbitError::InvalidInput(format!(
        "preset '{preset}' enables {kind} '{name}', which a plugin seeded into this workspace \
         but which does not load: {because}"
    ))
}

fn refuse_plugin_consent(document: &toml::Table) -> Result<(), String> {
    let enable_plugins = document
        .get("enable")
        .and_then(toml::Value::as_table)
        .is_some_and(|enable| {
            enable
                .keys()
                .any(|key| PLUGIN_CONSENT_KEYS.contains(&key.as_str()))
        });
    if enable_plugins
        || document
            .keys()
            .any(|key| PLUGIN_CONSENT_KEYS.contains(&key.as_str()))
    {
        return Err(
            "a preset never grants plugin permissions or enables a plugin; that consent stays \
             with `orbit plugin enable`"
                .to_string(),
        );
    }
    Ok(())
}

fn validate_preset_name(name: &str) -> Result<(), String> {
    let valid = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'));
    if valid {
        Ok(())
    } else {
        Err(format!(
            "name '{name}' must be 1-64 ASCII letters, digits, '-', '_' or '.'"
        ))
    }
}

/// Flatten `[config]` into dotted keys, so `"workflow.crew_performance"` and
/// a nested `[config.workflow]` table both name the same key.
fn flatten_config(prefix: String, table: toml::Table, out: &mut Vec<(String, toml::Value)>) {
    for (key, value) in table {
        let key = if prefix.is_empty() {
            key
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            toml::Value::Table(table) => flatten_config(key, table, out),
            value => out.push((key, value)),
        }
    }
}

impl OrbitRuntime {
    /// Enable the plugin-seeded routines and auto-tasks `preset` names, as an
    /// operator act. Each must still be one a plugin seeded into this
    /// workspace. Returns the names whose state changed.
    pub fn enable_preset_definitions(
        &self,
        preset: &WorkspacePreset,
    ) -> Result<Vec<String>, OrbitError> {
        preset.require_plugin_seeded(&self.shared_root())?;
        // Resolve every routine before enabling anything: the status report
        // is where the runtime catalog and plugin skips apply, so a routine
        // that does not load there must not leave an auto-task from the
        // same preset already enabled.
        let report = if preset.enable_routines.is_empty() {
            None
        } else {
            Some(checkout_edit_report(self)?)
        };
        if let Some(report) = &report {
            for name in &preset.enable_routines {
                report
                    .statuses
                    .iter()
                    .map(|status| &status.routine)
                    .find(|routine| routine.definition.name == *name)
                    .ok_or_else(|| {
                        OrbitError::InvalidInput(format!(
                            "plugin-seeded routine '{name}' does not load in this workspace; run \
                             `orbit routine list` to see why"
                        ))
                    })?;
            }
        }
        let mut changed = Vec::new();
        for name in &preset.enable_auto_tasks {
            let before = self
                .auto_task_show(name)?
                .is_some_and(|definition| definition.enabled);
            self.auto_task_toggle(name, true)?;
            if !before {
                changed.push(name.clone());
            }
        }
        if let Some(report) = &report {
            for name in &preset.enable_routines {
                let routine = report
                    .statuses
                    .iter()
                    .map(|status| &status.routine)
                    .find(|routine| routine.definition.name == *name)
                    .ok_or_else(|| {
                        OrbitError::InvalidInput(format!(
                            "plugin-seeded routine '{name}' does not load in this workspace; run \
                             `orbit routine list` to see why"
                        ))
                    })?;
                if !routine.definition.enabled {
                    set_routine_enabled(routine, false, true)?;
                    changed.push(name.clone());
                }
            }
        }
        Ok(changed)
    }
}

/// The reason inside a refused input, without the error kind's own prefix, so
/// a preset refusal reads once.
fn refusal_reason(error: OrbitError) -> String {
    match error {
        OrbitError::InvalidInput(message) | OrbitError::InvalidInputDiagnostic { message, .. } => {
            message
        }
        other => other.to_string(),
    }
}
