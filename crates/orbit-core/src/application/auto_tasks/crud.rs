//! Auto-task CRUD [ORB-10149]: the shared domain surface behind both the CLI
//! (`orbit auto-task …`) and the MCP tools (`orbit.auto_task.*`). Definitions
//! are YAML under `<local_orbit_dir>/auto_tasks/<name>.yaml`;
//! these methods are the single choke point that reads/writes them, so both
//! entry points stay consistent. A managed worker's `orbit.auto_task.*` writes
//! are routed to the owner host before reaching this surface, so its
//! `local_orbit_dir` is the registered checkout the host clock reads.
//! Direct CLI calls from a linked worktree still use that worktree's root.
//! Disabling is a `toggle`; removal is the separate audited delete in
//! [`super::delete`].
//!
//! `mint` (CLI-only by design — see `docs/design/mcp-bridge/2_design.md`)
//! rides here too: it mints a task from a definition on demand by reusing the
//! scheduler's mint path, so there is exactly one template→task mapping.
//! `show` and `mint` resolve a definition only through the confined lookup:
//! a single in-scope name, a real `auto_tasks` directory, and a regular
//! definition file, checked before any definition bytes are read.

use std::path::{Component, Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::{atomic_write_text, with_exclusive_file_lock};
use orbit_types::task::{Task, normalize_required_tools};
use orbit_types::workflow::{
    AUTO_TASK_SCHEMA_VERSION, AutoTaskDefinition, AutoTaskSchedule, AutoTaskTemplate, DedupePolicy,
    is_valid_auto_task_name,
};

use crate::OrbitRuntime;

use super::loader::{AutoTaskCollection, auto_tasks_dir, collect_auto_tasks, definition_path};
use super::schedule::validate_schedule;
use super::scheduler::mint_task;
use super::state::cursor_state_path;

/// Parameters for creating a definition.
#[derive(Debug, Clone)]
pub struct AutoTaskAddParams {
    pub name: String,
    pub description: String,
    pub schedule: AutoTaskSchedule,
    pub template: AutoTaskTemplate,
    pub dedupe: DedupePolicy,
}

/// Present-field patch for updating a definition. Absent fields are unchanged.
/// A checked `enabled` change goes through
/// [`OrbitRuntime::auto_task_toggle_checked`] instead.
#[derive(Debug, Clone, Default)]
pub struct AutoTaskUpdateParams {
    pub waive_batch: Option<orbit_types::workflow::automation::WaiveBatchRequest>,
    pub description: Option<String>,
    pub schedule: Option<AutoTaskSchedule>,
    pub dedupe: Option<DedupePolicy>,
    pub template: Option<AutoTaskTemplate>,
    pub enabled: Option<bool>,
}

/// A lookup may name only one definition stem. Absolute paths, `..`, and
/// extra components are rejected before they can be joined onto `auto_tasks`.
fn in_scope_lookup_name(name: &str) -> bool {
    if !is_valid_auto_task_name(name) {
        return false;
    }
    let path = Path::new(name);
    if path.is_absolute() {
        return false;
    }
    let mut components = path.components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(stem)), None) => stem.to_str() == Some(name),
        _ => false,
    }
}

impl OrbitRuntime {
    /// Create a new auto-task definition. Fails if a definition with the same
    /// name already exists (update or toggle it instead).
    pub fn auto_task_add(
        &self,
        mut params: AutoTaskAddParams,
    ) -> Result<AutoTaskDefinition, OrbitError> {
        params.template.required_tools = normalize_required_tools(params.template.required_tools);
        let now = chrono::Utc::now().to_rfc3339();
        let actor = self.actor().resolve_write_label(None, None)?;
        let definition = AutoTaskDefinition {
            schema_version: AUTO_TASK_SCHEMA_VERSION,
            name: params.name,
            description: params.description,
            enabled: !matches!(&params.schedule, AutoTaskSchedule::Deliveries { .. }),
            schedule: params.schedule,
            template: params.template,
            dedupe: params.dedupe,
            // A mint-time precondition is configured in the definition YAML
            // the workspace owns, not through the create surfaces.
            skip_if_unchanged: None,
            created_by: Some(actor.clone()),
            created_at: now.clone(),
            updated_by: Some(actor),
            updated_at: now,
        };
        self.validate_auto_task(&definition)?;

        let collection = self.validated_auto_tasks(&definition.name)?;
        let path = definition_path(&self.paths().local_dir, &definition.name);
        if path.exists()
            || collection
                .definitions
                .iter()
                .any(|loaded| loaded.definition.name == definition.name)
        {
            return Err(OrbitError::InvalidInput(format!(
                "auto-task '{}' already exists; update or toggle it instead",
                definition.name
            )));
        }
        self.write_auto_task(&definition)?;
        Ok(definition)
    }

    /// List every definition in this workspace (stable filename order).
    ///
    /// Fail-closed load errors are surfaced as an error only when nothing
    /// loaded — a single malformed file must not hide the definitions that do
    /// work. Anything skipped is still reported: it is logged here and stands
    /// as a `faulty` row on the `orbit doctor` artifacts surface, so a
    /// definition that silently stopped firing is discoverable [ORB-10800].
    pub fn auto_task_list(&self) -> Result<Vec<AutoTaskDefinition>, OrbitError> {
        let collection = collect_auto_tasks(&self.paths().local_dir);
        for error in &collection.errors {
            tracing::warn!(
                target: "orbit.core.auto_tasks",
                path = %error.path.as_ref().map_or_else(
                    || "<auto_tasks dir>".to_string(),
                    |path| path.display().to_string()
                ),
                reason = error.message.as_str(),
                "auto-task definition failed to load and is treated as absent"
            );
        }
        if collection.definitions.is_empty() && !collection.errors.is_empty() {
            let detail = collection
                .errors
                .iter()
                .map(|error| {
                    error.path.as_ref().map_or_else(
                        || error.message.clone(),
                        |path| format!("{}: {}", path.display(), error.message),
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            return Err(OrbitError::InvalidInput(format!(
                "no auto-task definition could be loaded; every definition failed fail-closed: {detail}"
            )));
        }
        Ok(collection
            .definitions
            .into_iter()
            .map(|loaded| loaded.definition)
            .collect())
    }

    /// Show one definition by name, or `None` if no regular in-scope file exists.
    ///
    /// Absolute paths, parent-directory traversal, and any other name that is
    /// not a single definition stem are rejected before the filesystem is
    /// consulted. A symlinked `auto_tasks` directory or a non-regular
    /// definition entry is refused before its bytes are read.
    pub fn auto_task_show(&self, name: &str) -> Result<Option<AutoTaskDefinition>, OrbitError> {
        let Some(path) = self.confined_definition_path(name)? else {
            return Ok(None);
        };
        let raw = std::fs::read_to_string(&path)
            .map_err(|error| OrbitError::Io(format!("read {}: {error}", path.display())))?;
        Ok(Some(orbit_common::protocol::yaml::parse_auto_task_yaml(
            &raw,
        )?))
    }

    /// Apply a present-field patch to a definition.
    pub fn auto_task_update(
        &self,
        name: &str,
        params: AutoTaskUpdateParams,
    ) -> Result<AutoTaskDefinition, OrbitError> {
        if let Some(request) = &params.waive_batch {
            let definition = self.require_validated_auto_task(name)?;
            if params.description.is_some()
                || params.schedule.is_some()
                || params.dedupe.is_some()
                || params.template.is_some()
                || params.enabled.is_some()
            {
                return Err(OrbitError::InvalidInput(
                    "waive_batch cannot be combined with a definition edit".into(),
                ));
            }
            let consumer = crate::application::automation::consumer_key(self, "auto-task", name)?;
            let actor = self.actor().resolve_write_label(None, None)?;
            orbit_automation::delivery::waive(
                self.automation_store()?.as_ref(),
                &consumer,
                request,
                &actor,
                chrono::Utc::now(),
            )
            .map_err(orbit_automation::automation_error_to_orbit)?;
            return Ok(definition);
        }

        self.edit_auto_task(name, |definition| {
            if let Some(description) = params.description {
                definition.description = description;
            }
            if let Some(schedule) = params.schedule {
                definition.schedule = schedule;
            }
            if let Some(dedupe) = params.dedupe {
                definition.dedupe = dedupe;
            }
            if let Some(mut template) = params.template {
                template.required_tools = normalize_required_tools(template.required_tools);
                definition.template = template;
            }
            if let Some(enabled) = params.enabled {
                definition.enabled = enabled;
            }
        })
    }

    /// Enable or disable a definition (the kill-switch). Disabling pauses an
    /// auto-task and preserves the definition; `auto_task_delete` removes it.
    pub fn auto_task_toggle(
        &self,
        name: &str,
        enabled: bool,
    ) -> Result<AutoTaskDefinition, OrbitError> {
        self.edit_auto_task(name, |definition| definition.enabled = enabled)
    }

    /// Compare and change the enabled flag while holding the scheduler cursor lock.
    pub fn auto_task_toggle_checked(
        &self,
        name: &str,
        expected: bool,
        enabled: bool,
    ) -> Result<AutoTaskDefinition, OrbitError> {
        self.try_edit_auto_task(name, |definition| {
            if definition.enabled != expected {
                return Err(OrbitError::InvalidInput(
                    "auto-task state changed; refresh before retrying".into(),
                ));
            }
            definition.enabled = enabled;
            Ok(())
        })
    }

    /// Mint one task from a definition on demand — the manual counterpart to a
    /// scheduler fire, so a new or edited definition can be exercised without
    /// waiting for its cron slot [ORB-10439].
    ///
    /// The mint is **unconditional**: schedule due-math, `dedupe`, and
    /// `enabled` are all ignored, and the host-local cursor at
    /// `<orbit_dir>/state/auto-tasks.json` is neither read nor written — an
    /// operator naming a definition explicitly means it, and a manual mint must
    /// not perturb scheduler state. Lookup and task creation do share that
    /// cursor's lock with deletion and scheduler admission. The definition is
    /// loaded again under the lock, so a delete that wins admission removes
    /// the file before a task exists, and a mint that wins admission is an
    /// open task by the time a non-force delete checks. Admission does not load
    /// or save the cursor, so its bytes stay identical. Because it reuses the
    /// scheduler's [`mint_task`], the result is field-for-field identical to a fired
    /// instance, provenance tag and `system_created` marker included; that also
    /// means an open manually minted instance is visible to `skip_if_open` dedupe on
    /// the next pass, exactly as a fired one would be.
    ///
    /// An unknown name is an `InvalidInput` error naming the definition.
    /// Escaped lookups fail the same way, before a task is created: mint loads
    /// the definition only through [`Self::auto_task_show`].
    pub fn auto_task_mint(&self, name: &str) -> Result<Task, OrbitError> {
        // Fail closed before admission. This copy is not mint authority: a
        // concurrent delete can remove the file before the lock is held.
        let preloaded = self.require_auto_task(name)?;
        self.validate_required_tools(&preloaded.template.required_tools)?;
        // Same sidecar lock as `with_cursor_lock` and `auto_task_delete`.
        // Do not load or save the cursor: bytes stay unchanged, and a
        // malformed cursor must not block an explicit mint.
        let state_path = cursor_state_path(&self.paths().state_dir);
        with_exclusive_file_lock(&state_path, "auto-task cursor", || {
            let definition = self.require_auto_task(name)?;
            self.validate_required_tools(&definition.template.required_tools)?;
            let task = mint_task(self, &definition)?;
            Ok(task)
        })
    }

    /// Resolve `<orbit_dir>/auto_tasks/<name>.yaml` without following a lookup
    /// outside that directory.
    ///
    /// Name checks run first, so a traversal or absolute lookup never becomes
    /// a path. The directory and file checks use `symlink_metadata`, which
    /// does not follow the final component, and only a regular file is
    /// returned for reading.
    fn confined_definition_path(&self, name: &str) -> Result<Option<PathBuf>, OrbitError> {
        if !in_scope_lookup_name(name) {
            return Err(OrbitError::InvalidInput(format!(
                "auto-task lookup '{name}' is not an in-scope definition name"
            )));
        }

        let definitions = auto_tasks_dir(&self.paths().local_dir);
        match std::fs::symlink_metadata(&definitions) {
            Ok(metadata) => {
                let file_type = metadata.file_type();
                if file_type.is_symlink() || !file_type.is_dir() {
                    return Err(OrbitError::InvalidInput(
                        "auto_tasks must be a regular directory directly under the workspace Orbit directory"
                            .into(),
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(OrbitError::Io(format!(
                    "inspect auto_tasks directory {}: {error}",
                    definitions.display()
                )));
            }
        }

        let path = definition_path(&self.paths().local_dir, name);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) => {
                let file_type = metadata.file_type();
                if file_type.is_symlink() || !file_type.is_file() {
                    return Err(OrbitError::InvalidInput(format!(
                        "auto-task '{name}' must be a regular definition file directly under auto_tasks"
                    )));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(OrbitError::Io(format!(
                    "inspect auto-task '{name}' at {}: {error}",
                    path.display()
                )));
            }
        }
        Ok(Some(path))
    }

    fn require_auto_task(&self, name: &str) -> Result<AutoTaskDefinition, OrbitError> {
        self.auto_task_show(name)?
            .ok_or_else(|| OrbitError::InvalidInput(format!("no such auto-task '{name}'")))
    }

    pub(super) fn require_validated_auto_task(
        &self,
        name: &str,
    ) -> Result<AutoTaskDefinition, OrbitError> {
        let loaded = self
            .validated_auto_tasks(name)?
            .definitions
            .into_iter()
            .find(|loaded| loaded.definition.name == name)
            .ok_or_else(|| OrbitError::InvalidInput(format!("no such auto-task '{name}'")))?;
        if !self.is_canonical_definition_path(&loaded.path, name) {
            return Err(OrbitError::InvalidInput(format!(
                "auto-task '{name}' must be stored at its canonical .yaml path before editing"
            )));
        }
        Ok(loaded.definition)
    }

    /// Whether `loaded` is exactly `<local_dir>/auto_tasks/<name>.yaml`.
    ///
    /// Discovery resolves symlinks in the Orbit directory before listing, so a
    /// loaded path is symlink-free while `local_dir` may reach the same
    /// directory through a symlinked ancestor (macOS `/tmp` and `/var`, or a
    /// symlinked home). The definition's directory is resolved the same way and
    /// its file name is appended unresolved, so a differently named file, or a
    /// definition file that is itself a link, still does not match.
    fn is_canonical_definition_path(&self, loaded: &Path, name: &str) -> bool {
        let expected = definition_path(&self.paths().local_dir, name);
        let (Some(parent), Some(file_name)) = (expected.parent(), expected.file_name()) else {
            return false;
        };
        let resolved_parent =
            std::fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
        loaded == resolved_parent.join(file_name)
    }

    pub(super) fn validated_auto_tasks(
        &self,
        name: &str,
    ) -> Result<AutoTaskCollection, OrbitError> {
        let collection = collect_auto_tasks(&self.paths().local_dir);
        if !collection.errors.is_empty() {
            return Err(OrbitError::InvalidInput(format!(
                "auto-task '{name}' definition load failed: {}",
                collection
                    .errors
                    .iter()
                    .map(|error| error.path.as_ref().map_or_else(
                        || error.message.clone(),
                        |path| format!("{}: {}", path.display(), error.message),
                    ))
                    .collect::<Vec<_>>()
                    .join("; ")
            )));
        }
        let mut names = std::collections::BTreeSet::new();
        for loaded in &collection.definitions {
            if !names.insert(&loaded.definition.name) {
                return Err(OrbitError::InvalidInput(format!(
                    "auto-task '{}' has more than one definition file",
                    loaded.definition.name
                )));
            }
        }
        Ok(collection)
    }

    /// Read, patch, and write one definition under the cursor sidecar lock.
    ///
    /// Scheduler admission revalidates its loaded revision under this lock, so
    /// once an edit or toggle returns, no pass can admit the revision it
    /// replaced. Reading inside the lock also serializes concurrent edits: each
    /// patches the latest committed definition, so neither drops the other's
    /// change. Like manual mint, this never loads or saves the cursor, so a
    /// malformed cursor cannot block the kill-switch.
    fn edit_auto_task(
        &self,
        name: &str,
        edit: impl FnOnce(&mut AutoTaskDefinition),
    ) -> Result<AutoTaskDefinition, OrbitError> {
        self.try_edit_auto_task(name, |definition| {
            edit(definition);
            Ok(())
        })
    }

    fn try_edit_auto_task(
        &self,
        name: &str,
        edit: impl FnOnce(&mut AutoTaskDefinition) -> Result<(), OrbitError>,
    ) -> Result<AutoTaskDefinition, OrbitError> {
        let state_path = cursor_state_path(&self.paths().state_dir);
        with_exclusive_file_lock(&state_path, "auto-task cursor", || {
            let mut definition = self.require_validated_auto_task(name)?;
            edit(&mut definition)?;
            definition.updated_by = Some(self.actor().resolve_write_label(None, None)?);
            definition.updated_at = chrono::Utc::now().to_rfc3339();
            self.validate_auto_task(&definition)?;
            self.write_auto_task(&definition)?;
            Ok(definition)
        })
    }

    fn validate_auto_task(&self, definition: &AutoTaskDefinition) -> Result<(), OrbitError> {
        definition.validate()?;
        self.validate_required_tools(&definition.template.required_tools)?;
        // Load-time cron validation happens in the scheduler, but validating
        // here too means CRUD never persists a schedule the scheduler would
        // reject at fire time.
        validate_schedule(&definition.schedule)?;
        if let Some(crew) = definition.template.crew.as_deref() {
            self.validate_crew_name(Some(crew))?;
        }
        Ok(())
    }

    fn write_auto_task(&self, definition: &AutoTaskDefinition) -> Result<(), OrbitError> {
        // The runtime has already selected the definition root. A managed
        // tool call reaches this method only in the registered owner host.
        let path = definition_path(&self.paths().local_dir, &definition.name);
        let yaml = serde_yaml::to_string(definition).map_err(|error| {
            OrbitError::Io(format!("encode auto-task '{}': {error}", definition.name))
        })?;
        atomic_write_text(&path, &yaml).map_err(|error| {
            OrbitError::Io(format!(
                "atomically refresh auto-task '{}' at {}: {error}",
                definition.name,
                path.display()
            ))
        })
    }
}
