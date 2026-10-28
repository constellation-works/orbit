//! Request bodies and partial-update deserialization.

use crate::api::bad_request;
use axum::response::Response;
use orbit_core::{
    ExternalRef, TaskComplexity, TaskCreateStatus, TaskPriority, TaskStatus, TaskType,
};
use orbit_types::task::{TaskOrigin, TaskRelation};
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value};

#[derive(Deserialize, Default)]
pub(in crate::api) struct ApproveBody {
    #[serde(default)]
    pub(super) note: Option<String>,
    #[serde(default)]
    pub(super) comment: Option<String>,
}

#[derive(Deserialize)]
pub(in crate::api) struct RejectBody {
    pub(super) note: String,
    #[serde(default)]
    pub(super) comment: Option<String>,
}

/// Body for `POST /tasks/:id/comments`. `author` is the operator's own name;
/// it is sanitized to a human identity by [`super::writes::human_comment_author`], never taken
/// as-is when it names an agent family or a model.
#[derive(Deserialize, Default)]
pub(in crate::api) struct CommentBody {
    #[serde(default)]
    pub(super) message: String,
    #[serde(default)]
    pub(super) author: Option<String>,
}

#[derive(Deserialize)]
pub(in crate::api) struct CreateTaskBody {
    pub(super) title: String,
    pub(super) description: String,
    #[serde(default)]
    pub(super) acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub(super) dependencies: Vec<String>,
    #[serde(default)]
    pub(super) relations: Vec<TaskRelation>,
    #[serde(default)]
    pub(super) tags: Vec<String>,
    #[serde(default)]
    pub(super) required_tools: Vec<String>,
    #[serde(default)]
    pub(super) plan: String,
    #[serde(default)]
    pub(super) context_files: Vec<String>,
    /// Declares that missing `context_files` selectors name targets this task
    /// is about to create, mirroring `orbit task add --allow-missing-context`
    /// and the `orbit.task.add` tool's `allow_missing_context` input. Each
    /// missing selector is recorded as durable creation intent; see
    /// [`OrbitRuntime::add_task_screened`](orbit_core::OrbitRuntime::add_task_screened).
    #[serde(default)]
    pub(super) allow_missing_context: bool,
    #[serde(default)]
    pub(super) external_refs: Vec<ExternalRef>,
    /// Trap field (ORB-00042): `workspace` is a *workspace selector* and this
    /// endpoint takes it as the `?workspace=<id>` query parameter, never as a
    /// body field. Historically bridge sent `{"workspace": <path>}` here and
    /// serde silently dropped the unknown key, so every task landed in the
    /// default workspace. Deserializing the key just to reject it makes the
    /// mistake a loud 400. A `#[serde(deny_unknown_fields)]` would break every
    /// tolerant caller that sends extra keys.
    #[serde(default)]
    pub(super) workspace: Option<String>,
    #[serde(default = "default_priority")]
    pub(super) priority: TaskPriority,
    pub(super) complexity: TaskComplexity,
    #[serde(default)]
    pub(super) task_type: Option<TaskType>,
    #[serde(default)]
    pub(super) status: Option<TaskCreateStatus>,
    #[serde(default)]
    pub(super) parent_id: Option<String>,
    #[serde(default)]
    pub(super) source_task_id: Option<String>,
    #[serde(default)]
    pub(super) crew: Option<String>,
    #[serde(default)]
    pub(super) orchestrator: Option<String>,
    /// The orchestrator's model when it differs from the orchestrator crew's
    /// configured model; omitted, creation records the crew's model.
    #[serde(default)]
    pub(super) orchestrator_model: Option<String>,
    /// Why the task was filed; omitted, it stays unset.
    #[serde(default)]
    pub(super) origin: Option<TaskOrigin>,
    /// Caller-supplied provenance, forwarded as the write's model identity the
    /// same way `POST /api/frictions` takes it. Before
    /// ORB-10648 this key was undeclared, so serde dropped it and the task was
    /// attributed to the ambient identity while the caller was told `model` had
    /// been applied.
    #[serde(default)]
    pub(super) model: Option<String>,
    /// Retired create input, declared so it stays *knowingly* tolerated rather
    /// than falling into [`CreateTaskBody::unsupported`]. `comment` is one of
    /// `orbit.task.add`'s retired fields: the native tool rejects it with
    /// `invalid_input`.
    /// This HTTP body still accepts the key so existing dashboard clients are
    /// not broken; comment on a task with `POST /tasks/:id/comments`.
    #[serde(default)]
    pub(super) comment: Option<String>,
    /// Trap field (ORB-10648): attribution was consolidated to `model`-only, so
    /// an `agent` key is a caller bug rather than a usable input. The native
    /// tool surface rejects it outright (`reject_agent_field`); this body does
    /// the same instead of dropping it.
    #[serde(default)]
    pub(super) agent: Option<String>,
    /// Every key this endpoint does not declare, captured rather than dropped.
    /// See [`reject_unsupported_task_body_fields`].
    #[serde(flatten)]
    pub(super) unsupported: Map<String, Value>,
}

fn default_priority() -> TaskPriority {
    TaskPriority::Medium
}

/// Reject a task body carrying keys the endpoint would otherwise discard.
///
/// ORB-10648: both task bodies derived a plain `Deserialize`, so any undeclared
/// key (`priority` on update, an `agent` typo, a field only the native
/// `orbit.task.update` tool declares) was silently dropped and the handler
/// still answered `200` with the task JSON. A caller that reports per-field
/// application — bridge's `orbit_task_update` write confirmation — then
/// affirmatively reports a field as applied that was never persisted, and a
/// false "applied" is worse than an error because nothing prompts a read-back.
///
/// The contract is all-or-nothing: an unsupported key fails the whole request,
/// so no write lands partially. The keys are captured through
/// `#[serde(flatten)]` rather than `#[serde(deny_unknown_fields)]` for the same
/// reason [`CreateTaskBody::workspace`] is a declared trap field (ORB-00042):
/// the diagnostic stays Orbit's own, naming every offending key and pointing at
/// the surface that does accept it, instead of serde's opaque message.
pub(super) fn reject_unsupported_task_body_fields(
    agent: Option<&String>,
    unsupported: &Map<String, Value>,
    endpoint: &str,
) -> Option<Response> {
    if agent.is_some() {
        return Some(bad_request(format!(
            "{endpoint} no longer accepts `agent`; use `model` with the agent \
             family (codex, claude, gemini, grok, or muse) for attribution"
        )));
    }
    if unsupported.is_empty() {
        return None;
    }
    let mut names = unsupported.keys().cloned().collect::<Vec<_>>();
    names.sort();
    let names = names
        .iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ");
    Some(bad_request(format!(
        "unsupported body field(s) {names}: {endpoint} does not apply them, so the \
         request is rejected rather than reporting a write that would not land"
    )))
}

/// Partial-update body for `PATCH /tasks/:id`. Each field is `Option<...>`;
/// fields absent from the JSON body remain unchanged.
///
/// Every key the caller sends is either applied or refused: unknown keys land
/// in [`UpdateTaskBody::unsupported`] and fail the request
/// ([`reject_unsupported_task_body_fields`], ORB-10648). Nothing is dropped in
/// silence.
#[derive(Deserialize, Default)]
pub(in crate::api) struct UpdateTaskBody {
    #[serde(default)]
    pub(super) title: Option<String>,
    /// RFC 3339 schedule; an empty string clears, null or absence preserves.
    #[serde(default)]
    pub(super) not_before: Option<String>,
    #[serde(default)]
    pub(super) description: Option<String>,
    #[serde(default)]
    pub(super) acceptance_criteria: Option<Vec<String>>,
    #[serde(default)]
    pub(super) dependencies: Option<Vec<String>>,
    #[serde(default)]
    pub(super) relations: Option<Vec<TaskRelation>>,
    #[serde(default)]
    pub(super) tags: Option<Vec<String>>,
    #[serde(default)]
    pub(super) plan: Option<String>,
    #[serde(default)]
    pub(super) execution_summary: Option<String>,
    #[serde(default)]
    pub(super) comment: Option<String>,
    #[serde(default)]
    pub(super) status: Option<TaskStatus>,
    /// Human override of the lifecycle table for this write's `status`, the
    /// dashboard's equivalent of `orbit task update --force` (ORB-12445). Only
    /// meaningful with `status`; alone it is a 400, matching the CLI flag's
    /// `requires = "status"`. The registered `orbit.task.update` tool and the
    /// MCP surface still have no such field, so no agent can grant itself the
    /// override.
    #[serde(default)]
    pub(super) force: bool,
    /// Replacement dispatch priority (ORB-10648). Previously undeclared here
    /// even though the record layer could persist it, so an operator's
    /// re-prioritization was dropped while the response reported success.
    #[serde(default)]
    pub(super) priority: Option<TaskPriority>,
    #[serde(default)]
    pub(super) complexity: Option<TaskComplexity>,
    #[serde(default)]
    pub(super) task_type: Option<TaskType>,
    #[serde(default, deserialize_with = "deserialize_nullable_string_patch_field")]
    pub(super) pr_status: Option<Option<String>>,
    #[serde(default)]
    pub(super) context_files: Option<Vec<String>>,
    /// Escape for a `context_files` selector that names a target this task is
    /// about to create. See [`CreateTaskBody::allow_missing_context`].
    #[serde(default)]
    pub(super) allow_missing_context: bool,
    /// Permit replacing tags with a list that drops a system identity tag (`ci-failure:*`).
    #[serde(default)]
    pub(super) allow_drop_system_tags: bool,
    #[serde(default, deserialize_with = "deserialize_nullable_string_patch_field")]
    pub(super) crew: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_nullable_string_patch_field")]
    pub(super) orchestrator: Option<Option<String>>,
    /// Replacement origin; an origin can be corrected but not cleared.
    #[serde(default)]
    pub(super) origin: Option<TaskOrigin>,
    /// Caller-supplied provenance, forwarded as the write's model identity.
    /// See [`CreateTaskBody::model`].
    #[serde(default)]
    pub(super) model: Option<String>,
    /// Trap field. See [`CreateTaskBody::agent`].
    #[serde(default)]
    pub(super) agent: Option<String>,
    /// Every key this endpoint does not declare, captured rather than dropped.
    /// See [`reject_unsupported_task_body_fields`].
    #[serde(flatten)]
    pub(super) unsupported: Map<String, Value>,
}

fn deserialize_nullable_string_patch_field<'de, D>(
    deserializer: D,
) -> Result<Option<Option<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Some)
}
#[derive(Deserialize, Default)]
pub(in crate::api) struct RequeueBody {
    #[serde(default)]
    pub(super) reason: Option<String>,
    #[serde(default)]
    pub(super) discard_candidate: bool,
}

#[derive(Deserialize, Default)]
pub(in crate::api) struct PilotAdmissionBody {
    #[serde(default)]
    pub(super) verdict: Option<String>,
    #[serde(default)]
    pub(super) decision: Option<String>,
    #[serde(default)]
    pub(super) action: Option<String>,
    #[serde(default)]
    pub(super) evidence: Option<String>,
    #[serde(default)]
    pub(super) reason: Option<String>,
}

#[derive(Deserialize, Default)]
pub(in crate::api) struct PostDeployBody {
    #[serde(default)]
    pub(super) criterion: Option<usize>,
    #[serde(default)]
    pub(super) evidence: Option<String>,
    #[serde(default)]
    pub(super) reason: Option<String>,
}
