//! Bundled auto-task bodies and their operator settings [ORB-14909].
//!
//! A shipped default's YAML file is a managed asset that `orbit workspace
//! sync` refreshes while it still holds the bytes Orbit wrote. Operator
//! settings (`enabled`, `schedule`, `dedupe`, template `crew`, `priority`,
//! `complexity` and tag additions) live in the settings table
//! ([`orbit_automation::auto_tasks::settings`]) and are applied at load, so
//! tuning a default never edits its body. This module decides when a body is
//! still managed, routes CRUD edits to the settings table or to a fork, moves
//! settings-only forks back under management during sync, and classifies forks
//! for `show` and `orbit doctor`.

use std::fs;
use std::path::Path;

use orbit_automation::auto_tasks::settings::{
    AutoTaskOverrides, AutoTaskSettings, AutoTaskSettingsTable, load_settings_table,
    split_overrides, write_settings_table,
};
use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_text;
use orbit_common::protocol::yaml::parse_auto_task_yaml;
use orbit_common::security::release::sha256_hex;
use orbit_types::workflow::AutoTaskDefinition;
use serde::Serialize;

use crate::OrbitRuntime;
use crate::application::managed_assets::{
    ConfinedAssetPath, MANAGED_ASSET_MANIFEST_FILE, ManagedAssetAction, ManagedAssetLayout,
    ManagedAssetOutcome, ManagedAssetReconcileMode, load_managed_asset_manifest,
    resolve_confined_asset_path, restore_managed_asset,
};

use super::body_history::historical_overrides;
use super::loader::auto_tasks_dir;
use super::{DEFAULT_AUTO_TASK_FILES, render_default_auto_task};

/// The `assetKind` of the auto-task managed manifest.
pub(crate) const AUTO_TASK_ASSET_KIND: &str = "auto_task";

/// Where a definition's body comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoTaskBody {
    /// The bundled body Orbit wrote; sync keeps refreshing it.
    Managed,
    /// A shipped default whose file was edited away from the bundled body.
    /// Orbit preserves it and no longer applies upstream template changes.
    Forked,
    /// Not a shipped default.
    UserAuthored,
}

/// How one definition is layered, as `orbit auto-task show` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AutoTaskLayering {
    pub body: AutoTaskBody,
    /// The settings-table entry applied over the body, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings: Option<AutoTaskSettings>,
    /// For a fork: body fields that differ from the bundled default.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub forked_fields: Vec<&'static str>,
    /// For a fork: settings fields that differ from the bundled default.
    /// `orbit workspace sync` moves a fork without `forked_fields` into the
    /// settings table and manages its body again.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub settings_fields: Vec<&'static str>,
    /// A bundled contract the effective body omits, when one is known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_contract: Option<StaleAutoTaskContract>,
}

/// A bundled contract an effective auto-task body no longer carries.
///
/// Minting a definition in this state is held: running it would silently
/// produce a report that does not implement the bundled contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StaleAutoTaskContract {
    /// The bundled contract the effective body omits.
    pub contract: &'static str,
    /// Contract elements no single body paragraph or criterion states.
    pub missing: Vec<&'static str>,
    /// The operator-directed reconciliation that preserves the body.
    pub remediation: String,
}

/// The shipped definition whose follower deduplication is host-aware.
pub(crate) const RUN_FAILURE_PATTERNS_NAME: &str = "run-failure-patterns";

/// The contract the bundled `run-failure-patterns` body carries [ORB-19433].
pub const RUN_FAILURE_DEDUP_CONTRACT: &str =
    "run-failure follower deduplication by machine_id, leaf_run_id and task_id";

fn normalized_contract_text(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    let mut last_was_space = true;
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            normalized.extend(ch.to_lowercase());
            last_was_space = false;
        } else if !last_was_space {
            normalized.push(' ');
            last_was_space = true;
        }
    }
    normalized.trim_end().to_string()
}

/// `(machine/host, leaf run, task)` stated by one paragraph or criterion.
fn contract_categories(text: &str) -> (bool, bool, bool) {
    let normalized = normalized_contract_text(text);
    (
        normalized.contains("machine id") || normalized.contains("host"),
        normalized.contains("leaf run id")
            || normalized.contains("leaf id")
            || normalized.contains("run id"),
        normalized.contains("task id"),
    )
}

fn contract_units(definition: &AutoTaskDefinition) -> Vec<&str> {
    let mut units: Vec<&str> = definition
        .template
        .description
        .split("\n\n")
        .map(str::trim)
        .filter(|unit| !unit.is_empty())
        .collect();
    units.extend(
        definition
            .template
            .acceptance_criteria
            .iter()
            .map(String::as_str),
    );
    units
}

fn definition_has_dedup_contract(definition: &AutoTaskDefinition) -> bool {
    contract_units(definition)
        .into_iter()
        .any(|unit| contract_categories(unit) == (true, true, true))
}

fn dedup_contract_missing(definition: &AutoTaskDefinition) -> Vec<&'static str> {
    let mut best = (false, false, false);
    let mut best_count = 0;
    for unit in contract_units(definition) {
        let categories = contract_categories(unit);
        let count = [categories.0, categories.1, categories.2]
            .into_iter()
            .filter(|present| *present)
            .count();
        if count > best_count {
            best = categories;
            best_count = count;
        }
    }
    let mut missing = Vec::new();
    if !best.0 {
        missing.push("machine_id/host");
    }
    if !best.1 {
        missing.push("leaf_run_id");
    }
    if !best.2 {
        missing.push("task_id");
    }
    missing
}

/// The bundled `run-failure-patterns` definition, when Orbit ships one.
pub(crate) fn bundled_run_failure_definition(base_branch: &str) -> Option<AutoTaskDefinition> {
    let rendered = rendered_bundled_body(RUN_FAILURE_PATTERNS_NAME, base_branch)?;
    parse_auto_task_yaml(&rendered).ok()
}

/// Whether `effective` is a `run-failure-patterns` body that omits the
/// bundled host-aware deduplication contract. A body that states the
/// machine/leaf/task rule in its own wording, in any one paragraph or
/// criterion, is not stale.
pub fn stale_run_failure_contract(
    effective: &AutoTaskDefinition,
    bundled: &AutoTaskDefinition,
) -> Option<StaleAutoTaskContract> {
    if effective.name != RUN_FAILURE_PATTERNS_NAME
        || !definition_has_dedup_contract(bundled)
        || definition_has_dedup_contract(effective)
    {
        return None;
    }
    Some(StaleAutoTaskContract {
        contract: RUN_FAILURE_DEDUP_CONTRACT,
        missing: dedup_contract_missing(effective),
        remediation: format!(
            "reconcile with `orbit auto-task update {RUN_FAILURE_PATTERNS_NAME} \
             --adopt-bundled-contract` to append the bundled host-aware \
             deduplication rule while preserving the rest of this body, or edit \
             the body so one paragraph or criterion states the machine-id, \
             leaf-run-id and task-id rule; minting is held until then"
        ),
    })
}

/// The bundled deduplication rule, appended verbatim by reconciliation.
pub const RUN_FAILURE_DEDUP_PARAGRAPH: &str = "A follower settlement duplicates an owner history run only when all three match: the settlement's `machine_id` equals the history row's `executed_on.machine_id`, the settlement's `leaf_run_id` equals the history row's `run_id`, and the history row's `task_ids` lists the settlement's `task_id`. A bare `leaf_run_id` match without the machine and task match is an id collision across hosts, not a duplicate: count it. A history row with null `executed_on` cannot match on machine id, so it never dedupes a settlement.";

/// Append the bundled deduplication paragraph and criterion to `effective`,
/// leaving every other field and paragraph untouched. Returns whether the
/// body changed.
pub fn adopt_bundled_run_failure_contract(
    effective: &mut AutoTaskDefinition,
    bundled: &AutoTaskDefinition,
) -> bool {
    let mut changed = false;
    if !definition_has_dedup_contract(effective) {
        if !effective.template.description.trim().is_empty() {
            effective.template.description.push_str("\n\n");
        }
        effective
            .template
            .description
            .push_str(RUN_FAILURE_DEDUP_PARAGRAPH);
        changed = true;
        if let Some(criterion) = bundled
            .template
            .acceptance_criteria
            .iter()
            .find(|unit| contract_categories(unit) == (true, true, true))
            && !effective
                .template
                .acceptance_criteria
                .iter()
                .any(|existing| existing == criterion)
        {
            effective
                .template
                .acceptance_criteria
                .push(criterion.clone());
            changed = true;
        }
    }
    changed
}

/// The rendered bundled body of shipped default `name`, if Orbit ships one.
pub(crate) fn rendered_bundled_body(name: &str, base_branch: &str) -> Option<String> {
    DEFAULT_AUTO_TASK_FILES
        .iter()
        .find(|(shipped, _)| *shipped == name)
        .map(|(_, content)| render_default_auto_task(content, base_branch).into_owned())
}

/// Differences from a shipped body, with stale historical copies distinguished
/// from settings-only edits of the current template.
pub(crate) struct BundledFileOverrides {
    pub overrides: AutoTaskOverrides,
    pub stale_shipped_body: bool,
}

/// `None` means the manifest proves Orbit wrote these exact bytes, or they
/// equal the current bundle. Other files are compared with the current body
/// first, then with shipped history if their body differs.
pub(crate) fn classify_bundled_file(
    rendered: &str,
    on_disk: &str,
    tracked: Option<&String>,
    settings: Option<&AutoTaskSettings>,
    base_branch: &str,
) -> Result<Option<BundledFileOverrides>, OrbitError> {
    let digest = sha256_hex(on_disk.as_bytes());
    if tracked == Some(&digest) || digest == sha256_hex(rendered.as_bytes()) {
        return Ok(None);
    }
    let definition = parse_auto_task_yaml(on_disk)?;
    let body = parse_auto_task_yaml(rendered)?;
    let mut effective = definition.clone();
    if let Some(settings) = settings {
        settings.apply(&mut effective);
    }
    let mut overrides = split_overrides(&body, &effective);
    // A YAML comment is body content no settings entry can hold: an operator's
    // note keeps the file a fork rather than being dropped by migration.
    if has_added_comment(rendered, on_disk) {
        overrides.body_fields.push("comments");
    }
    if !overrides.body_fields.is_empty()
        && definition.name == body.name
        && let Some(overrides) =
            historical_overrides(&body, &definition, on_disk, base_branch, settings)?
    {
        return Ok(Some(BundledFileOverrides {
            overrides,
            stale_shipped_body: true,
        }));
    }
    Ok(Some(BundledFileOverrides {
        overrides,
        stale_shipped_body: false,
    }))
}

/// Whether `on_disk` carries a `#` line the bundled body lacks. Lines inside
/// block scalars (Markdown headings) match their bundled counterparts, and a
/// definition Orbit re-serialized carries no comments at all.
fn has_added_comment(rendered: &str, on_disk: &str) -> bool {
    let bundled: std::collections::BTreeSet<&str> = rendered
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with('#'))
        .collect();
    on_disk
        .lines()
        .map(str::trim)
        .any(|line| line.starts_with('#') && !bundled.contains(line))
}

fn settings_error(error: String) -> OrbitError {
    OrbitError::InvalidInput(error)
}

fn definition_file(dir: &Path, name: &str) -> std::path::PathBuf {
    dir.join(ManagedAssetLayout::YamlStem.relative_path(name))
}

fn manifest_digest(dir: &Path, name: &str) -> Result<Option<String>, OrbitError> {
    Ok(load_managed_asset_manifest(
        &dir.join(MANAGED_ASSET_MANIFEST_FILE),
        AUTO_TASK_ASSET_KIND,
        ManagedAssetLayout::YamlStem,
    )?
    .and_then(|manifest| manifest.assets.get(name).cloned()))
}

impl OrbitRuntime {
    fn auto_task_settings_dir(&self) -> std::path::PathBuf {
        auto_tasks_dir(&self.paths().local_dir)
    }

    /// The managed body of shipped default `name`: its file still holds the
    /// bytes the manifest records Orbit writing. `None` for a fork, a
    /// user-authored definition, or an untracked file.
    fn managed_auto_task_body(&self, name: &str) -> Result<Option<AutoTaskDefinition>, OrbitError> {
        if rendered_bundled_body(name, self.workspace_base_branch()).is_none() {
            return Ok(None);
        }
        let dir = self.auto_task_settings_dir();
        let Some(digest) = manifest_digest(&dir, name)? else {
            return Ok(None);
        };
        let path = definition_file(&dir, name);
        let raw = fs::read_to_string(&path)
            .map_err(|error| OrbitError::Io(format!("read {}: {error}", path.display())))?;
        if sha256_hex(raw.as_bytes()) != digest {
            return Ok(None);
        }
        parse_auto_task_yaml(&raw).map(Some)
    }

    /// Persist an edited effective definition. Called under the cursor lock.
    ///
    /// A shipped default whose body is still managed keeps that body: when
    /// the edit differs from it only in settings fields, only the settings
    /// table changes. Any body edit forks the file, as does every edit to a
    /// fork or a user-authored definition; the file then holds the whole
    /// effective definition and its settings entry is dropped, so the file
    /// alone is authoritative.
    pub(super) fn persist_auto_task_edit(
        &self,
        definition: &AutoTaskDefinition,
    ) -> Result<(), OrbitError> {
        let dir = self.auto_task_settings_dir();
        let mut table = load_settings_table(&dir).map_err(settings_error)?;
        if let Some(body) = self.managed_auto_task_body(&definition.name)? {
            let overrides = split_overrides(&body, definition);
            if overrides.body_fields.is_empty() {
                // The entry stays even with no override left: its edit stamp
                // records that an operator configured the definition.
                let mut settings = overrides.settings;
                settings.updated_by = definition.updated_by.clone();
                settings.updated_at = definition.updated_at.clone();
                table.insert(definition.name.clone(), settings);
                return write_settings_table(&dir, &table);
            }
        }
        self.write_auto_task(definition)?;
        if table.remove(&definition.name).is_some() {
            write_settings_table(&dir, &table)?;
        }
        Ok(())
    }

    /// Drop the settings entry of a definition that no longer exists or is
    /// being created anew, so stale settings never apply to it.
    pub(super) fn drop_auto_task_settings(&self, name: &str) -> Result<(), OrbitError> {
        let dir = self.auto_task_settings_dir();
        let mut table = load_settings_table(&dir).map_err(settings_error)?;
        if table.remove(name).is_some() {
            write_settings_table(&dir, &table)?;
        }
        Ok(())
    }

    /// How definition `name` is layered: managed, forked or user-authored
    /// body, its settings entry, and for a fork the fields that differ from
    /// the bundled default.
    pub fn auto_task_layering(&self, name: &str) -> Result<AutoTaskLayering, OrbitError> {
        let (root, path) = self
            .auto_task_read_source(name)?
            .ok_or_else(|| self.auto_task_missing(name, true))?;
        let dir = auto_tasks_dir(&root);
        let table = load_settings_table(&dir).map_err(settings_error)?;
        let settings = table.get(name).cloned();
        let Some(rendered) = rendered_bundled_body(name, self.workspace_base_branch()) else {
            return Ok(AutoTaskLayering {
                body: AutoTaskBody::UserAuthored,
                settings,
                forked_fields: Vec::new(),
                settings_fields: Vec::new(),
                stale_contract: None,
            });
        };
        let on_disk = fs::read_to_string(&path)
            .map_err(|error| OrbitError::Io(format!("read {}: {error}", path.display())))?;
        let tracked = manifest_digest(&dir, name)?;
        let layering = match classify_bundled_file(
            &rendered,
            &on_disk,
            tracked.as_ref(),
            settings.as_ref(),
            self.workspace_base_branch(),
        )? {
            None => AutoTaskLayering {
                body: AutoTaskBody::Managed,
                settings,
                forked_fields: Vec::new(),
                settings_fields: Vec::new(),
                stale_contract: None,
            },
            Some(classified) => {
                let stale_contract = parse_auto_task_yaml(&on_disk)
                    .ok()
                    .map(|mut effective| {
                        if let Some(entry) = &settings {
                            entry.apply(&mut effective);
                        }
                        effective
                    })
                    .and_then(|effective| {
                        bundled_run_failure_definition(self.workspace_base_branch())
                            .and_then(|bundled| stale_run_failure_contract(&effective, &bundled))
                    });
                AutoTaskLayering {
                    body: AutoTaskBody::Forked,
                    settings,
                    settings_fields: classified.overrides.settings.field_names(),
                    forked_fields: classified.overrides.body_fields,
                    stale_contract,
                }
            }
        };
        Ok(layering)
    }

    /// The stale bundled contract of an effective definition, if any.
    pub fn stale_auto_task_contract(
        &self,
        definition: &AutoTaskDefinition,
    ) -> Option<StaleAutoTaskContract> {
        bundled_run_failure_definition(self.workspace_base_branch())
            .and_then(|bundled| stale_run_failure_contract(definition, &bundled))
    }

    /// Hold minting a definition whose effective body omits its bundled
    /// contract, naming the reconciliation instead of minting silently.
    pub fn refuse_stale_auto_task_contract(
        &self,
        definition: &AutoTaskDefinition,
    ) -> Result<(), OrbitError> {
        let Some(stale) = self.stale_auto_task_contract(definition) else {
            return Ok(());
        };
        Err(OrbitError::InvalidInput(format!(
            "auto-task '{}' is held: its effective body omits the bundled contract \
             ({}) (missing: {}); a scan minted from it would undercount \
             host-distinct follower failures and must not be represented as \
             reconciled; {}",
            definition.name,
            stale.contract,
            stale.missing.join(", "),
            stale.remediation
        )))
    }
}

/// Move every settings-only fork of a shipped default back under management:
/// write its settings into the settings table, restore the bundled body, and
/// record the body's digest. A fork with a body edit is left alone (managed
/// reconciliation preserves it); `Check` mode reports without writing.
///
/// Runs only on a catalog that already has a manifest, before the managed
/// reconciliation that then finds the restored body current. In apply mode,
/// the caller holds the workspace auto-task cursor lock across this entire
/// operation, including the table load, body restoration and digest update.
pub(crate) fn migrate_settings_only_forks(
    dir: &Path,
    base_branch: &str,
    mode: ManagedAssetReconcileMode,
) -> Result<Vec<ManagedAssetAction>, OrbitError> {
    let Some(manifest) = load_managed_asset_manifest(
        &dir.join(MANAGED_ASSET_MANIFEST_FILE),
        AUTO_TASK_ASSET_KIND,
        ManagedAssetLayout::YamlStem,
    )?
    else {
        return Ok(Vec::new());
    };
    // An unreadable table already fails every definition closed, and doctor
    // names it; migrating over it would discard the settings it holds.
    let mut table: AutoTaskSettingsTable = match load_settings_table(dir) {
        Ok(table) => table,
        Err(error) => {
            tracing::warn!(
                target: "orbit.core.auto_tasks",
                %error,
                "skipped migrating auto-task forks into the settings table"
            );
            return Ok(Vec::new());
        }
    };
    let mut actions = Vec::new();
    for (name, _) in DEFAULT_AUTO_TASK_FILES {
        if manifest.opted_out.contains(*name) {
            continue;
        }
        let relative = ManagedAssetLayout::YamlStem.relative_path(name);
        let ConfinedAssetPath::File(_) = resolve_confined_asset_path(dir, &relative)? else {
            continue;
        };
        let path = dir.join(&relative);
        let on_disk = fs::read_to_string(&path).map_err(|error| {
            OrbitError::io_with_context(&error, format!("read {}: {error}", path.display()))
        })?;
        let Some(rendered) = rendered_bundled_body(name, base_branch) else {
            continue;
        };
        // An unparsable fork stays where it is; doctor reports it faulty.
        let Ok(Some(classified)) = classify_bundled_file(
            &rendered,
            &on_disk,
            manifest.assets.get(*name),
            table.get(*name),
            base_branch,
        ) else {
            continue;
        };
        let overrides = classified.overrides;
        if !overrides.body_fields.is_empty() {
            continue;
        }
        let fields = overrides.settings.field_names();
        if mode.writes() {
            // Settings first: until the body is restored, the fork carries the
            // same values, so an interruption at any step loads identically.
            // The fork's edit stamp moves with its settings, so a definition
            // an operator configured still reads as configured.
            let effective = parse_auto_task_yaml(&on_disk)?;
            let previous = table.get(*name);
            let mut settings = overrides.settings;
            settings.updated_by = previous
                .and_then(|entry| entry.updated_by.clone())
                .or(effective.updated_by);
            settings.updated_at = previous
                .map(|entry| entry.updated_at.clone())
                .filter(|stamp| !stamp.is_empty())
                .unwrap_or(effective.updated_at);
            table.insert((*name).to_string(), settings);
            write_settings_table(dir, &table)?;
            atomic_write_text(&path, &rendered).map_err(|error| {
                OrbitError::io_with_context(
                    &error,
                    format!("restore bundled auto-task '{}': {error}", path.display()),
                )
            })?;
            restore_managed_asset(
                dir,
                AUTO_TASK_ASSET_KIND,
                ManagedAssetLayout::YamlStem,
                name,
                sha256_hex(rendered.as_bytes()),
            )?;
        }
        actions.push(ManagedAssetAction {
            name: (*name).to_string(),
            path,
            outcome: ManagedAssetOutcome::Migrated,
            detail: Some(if fields.is_empty() {
                "fork matched the bundled body apart from edit stamps; the bundled body is managed again"
                    .to_string()
            } else {
                format!(
                    "operator settings ({}) moved into the auto-task settings table; the bundled body is managed again",
                    fields.join(", ")
                )
            }),
        });
    }
    Ok(actions)
}
