//! Canonical `orbit.task.show` field-projection vocabulary.
//!
//! CLI help, MCP/tool schema text, validation errors, and JSON projectors
//! must all draw from this list. Every stable key in the public task DTO is
//! projectable, and so is every enrichment the unprojected readout adds to it.
//! Sidecars (`comments`, `history`, `artifacts`), fields whose canonical form
//! depends on other tasks (`resolved_dependencies`, `relations`), and the
//! host-local crew enrichments (`resolved_crew`, `crew_model`,
//! `crew_unresolved`) still need a runtime fetch; the remaining fields are
//! projected from the record itself.

use serde_json::{Value, json};

use crate::task::{Task, TaskOsRequirement, TaskStatus, task_readiness_json};

/// String-literal CSV of [`TASK_SHOW_PROJECTION_FIELDS`], for `concat!` in
/// clap help and other const contexts.
#[macro_export]
macro_rules! task_show_projection_fields_csv {
    () => {
        "id, parent_id, title, description, acceptance_criteria, dependencies, resolved_dependencies, tags, required_tools, plan, execution_summary, context_files, created_by, planned_by, implemented_by, status, terminal, priority, complexity, type, pr_status, external_refs, relations, source_task_id, job_run_id, job_run_machine, crew, resolved_crew, crew_model, crew_unresolved, os_requirement, readiness, orchestrator, created_at, updated_at, comments, history, artifacts"
    };
}

/// Authoritative `orbit.task.show` `--fields` / `fields` / `field` vocabulary.
pub const TASK_SHOW_PROJECTION_FIELDS: &[&str] = &[
    "id",
    "parent_id",
    "title",
    "description",
    "acceptance_criteria",
    "dependencies",
    "resolved_dependencies",
    "tags",
    "required_tools",
    "plan",
    "execution_summary",
    "context_files",
    "created_by",
    "planned_by",
    "implemented_by",
    "status",
    "terminal",
    "priority",
    "complexity",
    "type",
    "pr_status",
    "external_refs",
    "relations",
    "source_task_id",
    "job_run_id",
    "job_run_machine",
    "crew",
    "resolved_crew",
    "crew_model",
    "crew_unresolved",
    "os_requirement",
    "readiness",
    "orchestrator",
    "created_at",
    "updated_at",
    "comments",
    "history",
    "artifacts",
];

/// Stable top-level keys in the unprojected public task DTO.
///
/// A projector-vs-DTO drift test compares this policy with the actual DTO.
/// Sidecars are deliberately absent because they are attached after the base
/// DTO is serialized, but remain projectable through the vocabulary above.
pub const TASK_SHOW_PUBLIC_DTO_FIELDS: &[&str] = &[
    "id",
    "parent_id",
    "title",
    "description",
    "acceptance_criteria",
    "dependencies",
    "resolved_dependencies",
    "tags",
    "required_tools",
    "plan",
    "execution_summary",
    "context_files",
    "created_by",
    "planned_by",
    "implemented_by",
    "status",
    "priority",
    "complexity",
    "type",
    "pr_status",
    "external_refs",
    "relations",
    "source_task_id",
    "job_run_id",
    "job_run_machine",
    "crew",
    "orchestrator",
    "created_at",
    "updated_at",
];

/// Response envelope keys that are intentionally not task-field projections.
///
/// These describe the read itself — who resolved it, what else was asked for —
/// rather than the task, so a caller selecting one alone would be projecting
/// the response rather than the record.
///
/// The crew enrichments are deliberately *not* here. They are host-local and
/// conditional, but the unprojected readout emits them, and a key a reader can
/// see is a key it must be able to ask for: rejecting `resolved_crew` while
/// printing it was the defect in [ORB-12113].
pub const TASK_SHOW_DERIVED_RESPONSE_FIELDS: &[(&str, &str)] = &[(
    "workspace",
    "lookup-owner metadata attached by the CLI, not task data",
)];

/// Comma-separated form of [`TASK_SHOW_PROJECTION_FIELDS`].
pub const TASK_SHOW_PROJECTION_FIELDS_CSV: &str = crate::task_show_projection_fields_csv!();

/// The `orbit.task.show` selector that answers with the task's delivery
/// observation instead of a record projection. It is not a task field: it
/// reads the run that delivered the task and combines with no other field.
pub const TASK_SHOW_DELIVERY_FIELD: &str = "delivery";

/// Whether `name` is in the canonical show-projection vocabulary.
pub fn is_task_show_projection_field(name: &str) -> bool {
    TASK_SHOW_PROJECTION_FIELDS.contains(&name)
}

/// Actionable error for a name that is not in the vocabulary.
pub fn unknown_task_show_field_message(name: &str) -> String {
    format!("unknown field selector `{name}`. Valid values: {TASK_SHOW_PROJECTION_FIELDS_CSV}")
}

/// Whether the task status refuses the writes an implementer must make.
///
/// This is the canonical predicate for both the injected task envelope and
/// the derived `orbit.task.show` `terminal` projection. `Done` rejects every
/// non-comment mutation, while `Archived` admits only a restore to backlog;
/// neither permits an implementation execution summary.
pub fn refuses_implementer_writes(status: TaskStatus) -> bool {
    matches!(status, TaskStatus::Done | TaskStatus::Archived)
}

/// The `os_requirement` enrichment: the host operating systems the task's
/// `os:` tags admit, parsed. `None` for a task without an `os:` tag, which
/// any host runs; the unprojected readout then omits the key.
pub fn task_os_requirement_json(task: &Task) -> Option<Value> {
    let requirement = TaskOsRequirement::from_tags(&task.tags);
    (!requirement.is_unrestricted()).then(|| {
        json!({
            "any_of": requirement.any_of,
            "invalid": requirement.invalid,
            "describe": requirement.describe_hosts(),
        })
    })
}

/// JSON for a Task-local (non-sidecar) projection field.
///
/// Returns `None` for sidecar names, cross-task fields (`dependencies`,
/// `resolved_dependencies`, `relations`), the crew enrichments a host resolves
/// (`resolved_crew`, `crew_model`, `crew_unresolved`), and unknown names, so
/// callers can keep those on their existing fetch paths.
pub fn task_show_record_field_json(task: &Task, field: &str) -> Option<Value> {
    match field {
        "id" => Some(json!(task.id)),
        "parent_id" => Some(json!(task.parent_id())),
        "title" => Some(json!(task.title)),
        "description" => Some(json!(task.description)),
        "acceptance_criteria" => Some(json!(task.acceptance_criteria)),
        "tags" => Some(json!(task.tags)),
        "required_tools" => Some(json!(task.required_tools)),
        "plan" => Some(json!(task.plan)),
        "execution_summary" => Some(json!(task.execution_summary)),
        "context_files" => Some(json!(task.context_files)),
        "created_by" => Some(json!(task.created_by)),
        "planned_by" => Some(json!(task.planned_by)),
        "implemented_by" => Some(json!(task.implemented_by)),
        "type" => Some(json!(task.task_type.to_string())),
        "status" => Some(json!(task.status.to_string())),
        "terminal" => Some(json!(refuses_implementer_writes(task.status))),
        "priority" => Some(json!(task.priority.to_string())),
        "complexity" => Some(json!(task.complexity.map(|value| value.to_string()))),
        "pr_status" => Some(json!(task.pr_status)),
        "external_refs" => Some(json!(task.external_refs)),
        "source_task_id" => Some(json!(task.source_task_id())),
        "job_run_id" => Some(json!(task.job_run_id)),
        "job_run_machine" => Some(json!(task.job_run_machine)),
        "crew" => Some(json!(task.crew)),
        "os_requirement" => Some(task_os_requirement_json(task).unwrap_or(Value::Null)),
        "readiness" => Some(task_readiness_json(task).unwrap_or(Value::Null)),
        "orchestrator" => Some(json!(task.orchestrator)),
        "created_at" => Some(json!(task.created_at.to_rfc3339())),
        "updated_at" => Some(json!(task.updated_at.to_rfc3339())),
        _ => None,
    }
}
