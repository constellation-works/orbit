//! Auto-task CRUD [ORB-10149]: the shared domain surface behind both the CLI
//! (`orbit auto-task …`) and the MCP tools (`orbit.auto_task.*`). Definitions
//! are git-versioned YAML under `<local_orbit_dir>/auto_tasks/<name>.yaml`;
//! these functions are the single choke point that reads/writes them, so both
//! entry points stay consistent. In a linked worktree, `local_orbit_dir`
//! belongs to that checkout rather than the registered primary checkout.
//! Disabling is a `toggle`, never a delete.
//!
//! `mint` (CLI-only by design — see `docs/design/mcp-bridge/2_design.md`)
//! rides here too: it mints a task from a definition on demand by reusing the
//! scheduler's mint path, so there is exactly one template→task mapping.

use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_text;
use orbit_types::task::{Task, normalize_required_tools};
use orbit_types::workflow::{
    AUTO_TASK_SCHEMA_VERSION, AutoTaskDefinition, AutoTaskSchedule, AutoTaskTemplate, DedupePolicy,
};

use crate::consumers::consumer_key;
use crate::host::AutomationHost;

use super::loader::{collect_auto_tasks, definition_path};
use super::schedule::validate_schedule;
use super::scheduler::mint_task;

/// Parameters for creating a definition.
#[derive(Debug, Clone)]
pub struct AutoTaskAddParams {
    pub name: String,
    pub description: String,
    pub schedule: AutoTaskSchedule,
    pub template: AutoTaskTemplate,
    pub dedupe: DedupePolicy,
}

/// Present-field patch for updating a definition. Absent fields are unchanged;
/// `enabled` is patched through [`toggle`].
#[derive(Debug, Clone, Default)]
pub struct AutoTaskUpdateParams {
    pub waive_batch: Option<orbit_types::workflow::automation::WaiveBatchRequest>,
    pub description: Option<String>,
    pub schedule: Option<AutoTaskSchedule>,
    pub dedupe: Option<DedupePolicy>,
    pub template: Option<AutoTaskTemplate>,
}

/// Create a new auto-task definition. Fails if a definition with the same
/// name already exists (update or toggle it instead).
pub fn add<H: AutomationHost>(
    host: &H,
    mut params: AutoTaskAddParams,
) -> Result<AutoTaskDefinition, OrbitError> {
    params.template.required_tools = normalize_required_tools(params.template.required_tools);
    let now = chrono::Utc::now().to_rfc3339();
    let actor = host.write_label()?;
    let definition = AutoTaskDefinition {
        schema_version: AUTO_TASK_SCHEMA_VERSION,
        name: params.name,
        description: params.description,
        enabled: !matches!(&params.schedule, AutoTaskSchedule::Deliveries { .. }),
        schedule: params.schedule,
        template: params.template,
        dedupe: params.dedupe,
        created_by: Some(actor.clone()),
        created_at: now.clone(),
        updated_by: Some(actor),
        updated_at: now,
    };
    validate(host, &definition)?;

    let path = definition_path(&host.local_orbit_dir(), &definition.name);
    if path.exists() {
        return Err(OrbitError::InvalidInput(format!(
            "auto-task '{}' already exists; update or toggle it instead",
            definition.name
        )));
    }
    write(host, &definition)?;
    Ok(definition)
}

/// List every definition in this workspace (stable filename order).
///
/// Fail-closed load errors are surfaced as an error only when nothing
/// loaded — a single malformed file must not hide the definitions that do
/// work. Anything skipped is still reported: it is logged here and stands
/// as a `faulty` row on the `orbit doctor` artifacts surface, so a
/// definition that silently stopped firing is discoverable [ORB-10800].
pub fn list<H: AutomationHost>(host: &H) -> Result<Vec<AutoTaskDefinition>, OrbitError> {
    let collection = collect_auto_tasks(&host.local_orbit_dir());
    for error in &collection.errors {
        // Log targets are an observed surface, so they keep the names they were
        // emitted under before this domain moved crates [ORB-12262].
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

/// Show one definition by name, or `None` if it does not exist.
pub fn show<H: AutomationHost>(
    host: &H,
    name: &str,
) -> Result<Option<AutoTaskDefinition>, OrbitError> {
    let path = definition_path(&host.local_orbit_dir(), name);
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)
        .map_err(|error| OrbitError::Io(format!("read {}: {error}", path.display())))?;
    Ok(Some(orbit_common::protocol::yaml::parse_auto_task_yaml(
        &raw,
    )?))
}

/// Apply a present-field patch to a definition.
pub fn update<H: AutomationHost>(
    host: &H,
    name: &str,
    params: AutoTaskUpdateParams,
) -> Result<AutoTaskDefinition, OrbitError> {
    let mut definition = require(host, name)?;
    if let Some(request) = &params.waive_batch {
        if params.description.is_some()
            || params.schedule.is_some()
            || params.dedupe.is_some()
            || params.template.is_some()
        {
            return Err(OrbitError::InvalidInput(
                "waive_batch cannot be combined with a definition edit".into(),
            ));
        }
        let consumer = consumer_key(host, "auto-task", name)?;
        let actor = host.write_label()?;
        crate::delivery::waive(
            host.automation_store()?.as_ref(),
            &consumer,
            request,
            &actor,
            chrono::Utc::now(),
        )
        .map_err(crate::automation_error_to_orbit)?;
        return Ok(definition);
    }

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
    stamp_and_write(host, definition)
}

/// Enable or disable a definition (the kill-switch). Disabling is how an
/// auto-task is retired — the definition and its history are preserved.
pub fn toggle<H: AutomationHost>(
    host: &H,
    name: &str,
    enabled: bool,
) -> Result<AutoTaskDefinition, OrbitError> {
    let mut definition = require(host, name)?;
    definition.enabled = enabled;
    stamp_and_write(host, definition)
}

/// Mint one task from a definition on demand — the manual counterpart to a
/// scheduler fire, so a new or edited definition can be exercised without
/// waiting for its cron slot [ORB-10439].
///
/// The mint is **unconditional**: schedule due-math, `dedupe`, and
/// `enabled` are all ignored, and the host-local cursor at
/// `<orbit_dir>/state/auto-tasks.json` is neither read nor written — an
/// operator naming a definition explicitly means it, and a manual mint must
/// not perturb scheduler state. Because it reuses the scheduler's
/// [`mint_task`], the result is field-for-field identical to a fired
/// instance, provenance tag and `system_created` marker included; that also
/// means an open manually minted instance is visible to `skip_if_open` dedupe on
/// the next pass, exactly as a fired one would be.
///
/// An unknown name is an `InvalidInput` error naming the definition.
pub fn mint<H: AutomationHost>(host: &H, name: &str) -> Result<Task, OrbitError> {
    let definition = require(host, name)?;
    host.validate_required_tools(&definition.template.required_tools)?;
    mint_task(host, &definition)
}

fn require<H: AutomationHost>(host: &H, name: &str) -> Result<AutoTaskDefinition, OrbitError> {
    show(host, name)?.ok_or_else(|| OrbitError::InvalidInput(format!("no such auto-task '{name}'")))
}

fn stamp_and_write<H: AutomationHost>(
    host: &H,
    mut definition: AutoTaskDefinition,
) -> Result<AutoTaskDefinition, OrbitError> {
    definition.updated_by = Some(host.write_label()?);
    definition.updated_at = chrono::Utc::now().to_rfc3339();
    validate(host, &definition)?;
    write(host, &definition)?;
    Ok(definition)
}

fn validate<H: AutomationHost>(
    host: &H,
    definition: &AutoTaskDefinition,
) -> Result<(), OrbitError> {
    definition.validate()?;
    host.validate_required_tools(&definition.template.required_tools)?;
    // Load-time cron validation happens in the scheduler, but validating
    // here too means CRUD never persists a schedule the scheduler would
    // reject at fire time.
    validate_schedule(&definition.schedule)?;
    if let Some(crew) = definition.template.crew.as_deref() {
        host.validate_crew_name(Some(crew))?;
    }
    Ok(())
}

fn write<H: AutomationHost>(host: &H, definition: &AutoTaskDefinition) -> Result<(), OrbitError> {
    // ADR-0286: tracked definition mutation belongs to the active
    // worktree; shared state remains under `orbit_dir`.
    let path = definition_path(&host.local_orbit_dir(), &definition.name);
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
