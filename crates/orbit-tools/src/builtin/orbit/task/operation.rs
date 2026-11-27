//! The task noun's MCP surface table.
//!
//! The migrated task tools are served by the generic operation tool from the
//! registry in `orbit_common::governance::task`. What stays here is what the
//! spec does not hold: `show` and `list` have a dashboard bounded view, and
//! `show` a versioned snapshot, that other host actions serve under their own
//! checks; `add` and `update` have a guarded write mode with its own input
//! contract and schema conditionals, and `update` refuses an implementation
//! activity's `blocked` status before any field check. The operator review
//! and recovery verbs refuse a managed run before any field check, except a
//! resident cycle’s brokered recovery actions, which its cycle guard decides.
//! `resume_delivery` also checks its declared input shape before refusing a
//! call without that broker.

use orbit_common::OrbitError;
use orbit_common::governance::operation::OperationInput;
use orbit_common::governance::task::{
    TASK_CLI_OPTIONS_PARAM, TASK_ID_WIRE_ALIAS, TaskVerb, artifact_listing_hint,
};
use orbit_common::protocol::tool_input::{
    optional_csv_or_string_list_alias, reject_unknown_tool_fields, required_string,
};
use serde_json::Value;

use super::super::domain_control::{bounded, bounded_read, mode_field_error};
use super::super::operation::GuardedModes;
use super::super::{apply_session_orchestrator_default, resolve_workspace_argument};
use super::guarded;
use crate::{OrbitBuiltinAction, ToolContext};

const SNAPSHOT_KEYS: &[&str] = &[
    "workspace",
    "id",
    TASK_ID_WIRE_ALIAS,
    "snapshot",
    "model",
    "fields",
    "field",
    "limit",
];
const BOUNDED_SHOW_KEYS: &[&str] = &[
    "workspace",
    "view",
    "id",
    TASK_ID_WIRE_ALIAS,
    "limit",
    "comments_offset",
    "history_offset",
    "artifacts_offset",
    "model",
];
const BOUNDED_LIST_KEYS: &[&str] = &[
    "workspace",
    "view",
    "offset",
    "limit",
    "search",
    "status",
    "priority",
    "model",
];

/// Top-level fields of a `snapshot: true` response, plus the routing
/// `workspace` echo. Mirrors the serialized desktop task snapshot.
const SNAPSHOT_RESPONSE_FIELDS: &[&str] = &[
    "schema_version",
    "observed_at",
    "revision",
    "task",
    "actions",
    "comments",
    "history",
    "artifacts",
    "comments_total",
    "history_total",
    "artifacts_total",
    "reviewed_head",
    "reviewed_head_reason",
    "truncated_fields",
    "content_truncated",
    "review",
    "review_reason",
    "workspace",
];

/// Every top-level field a guarded review can answer with across all its
/// outcomes (applied or replayed, revision conflict, refusal, accepted with a
/// refresh error). A field absent from one outcome is still a valid selector,
/// so validity never depends on what the write does.
pub(super) const GUARDED_REVIEW_RESPONSE_FIELDS: &[&str] = &[
    "snapshot",
    "replayed",
    "conflict",
    "mutation_applied",
    "refusal",
    "accepted",
    "task_id",
    "refresh_error",
    "workspace",
];

impl GuardedModes for TaskVerb {
    fn preempt(self, ctx: &ToolContext, input: &Value) -> Option<Result<Value, OrbitError>> {
        // Task calls have always required an object; the shared spec also
        // accepts null as empty input for other nouns.
        if !input.is_object() {
            return Some(Err(OrbitError::InvalidInput(
                "task tool arguments must be an object".into(),
            )));
        }
        if matches!(self, TaskVerb::Add | TaskVerb::Update)
            && ctx.session_context.origin_session_id.is_some()
            && input.get(TASK_CLI_OPTIONS_PARAM).is_some()
        {
            return Some(Err(OrbitError::InvalidInput(format!(
                "unknown field '{TASK_CLI_OPTIONS_PARAM}'"
            ))));
        }
        if matches!(self, TaskVerb::Show)
            && input.get("snapshot") == Some(&Value::Bool(true))
            && input.get("view").is_none()
            && let Err(error) = reject_unknown_tool_fields(input, SNAPSHOT_KEYS)
        {
            return Some(Err(mode_field_error(error, "snapshot: true")));
        }
        if matches!(self, TaskVerb::Show | TaskVerb::List)
            && input.get("view").and_then(Value::as_str) == Some("bounded")
        {
            let keys = if matches!(self, TaskVerb::Show) {
                BOUNDED_SHOW_KEYS
            } else {
                BOUNDED_LIST_KEYS
            };
            if let Err(error) = reject_unknown_tool_fields(input, keys) {
                return Some(Err(mode_field_error(error, "view: bounded")));
            }
        }
        match self {
            TaskVerb::ArtifactPut => super::artifact_put::preempt(ctx, input),
            TaskVerb::Add => {
                guarded::is_guarded(input).then(|| guarded::write(ctx, input.clone(), true))
            }
            TaskVerb::Update => {
                // [ORB-14269] Before both the guarded edit and the ordinary
                // update. An implementation activity used to set `blocked`
                // mid-run; the executor never re-reads status, so later
                // steps still ran.
                if implementation_activity_sets_blocked(ctx, input) {
                    return Some(Err(OrbitError::InvalidInput(
                        "an implementation activity cannot set status to blocked; return result.blocker as {kind, evidence} so the step boundary records the blocker and stops the run"
                            .to_string(),
                    )));
                }
                if guarded::is_guarded(input) && input.get("repilot") == Some(&Value::Bool(true)) {
                    return Some(Err(OrbitError::InvalidInput("repilot cannot accompany a guarded update; request orbit.task.recover action repilot after the guarded write completes".into())));
                }
                if guarded::is_guarded(input) {
                    return Some(guarded::write(ctx, input.clone(), false));
                }
                // The spec reads JSON null as absent; an ordinary update
                // refuses a null crew rather than keeping the current one.
                (input.get("crew") == Some(&Value::Null)).then(|| {
                    Err(OrbitError::InvalidInput(
                        "`crew` must be a string; omit it to keep the current crew or use an empty string to redraw"
                            .to_string(),
                    ))
                })
            }
            TaskVerb::ReconcileReview => {
                managed_run_refusal(ctx, "managed runs cannot reconcile reviews")
            }
            TaskVerb::RetryHandoff => {
                managed_run_refusal(ctx, "managed runs cannot retry owner handoffs")
            }
            TaskVerb::RecoverClaim => {
                managed_run_refusal(ctx, "managed runs cannot recover claims")
            }
            TaskVerb::RevokeHandoff => {
                managed_run_refusal(ctx, "managed runs cannot revoke handoff authority")
            }
            TaskVerb::ReviewReset => {
                managed_run_refusal(ctx, "managed runs cannot reset review budgets")
            }
            TaskVerb::Repilot => {
                managed_run_refusal(ctx, "managed runs cannot request another task pilot")
            }
            TaskVerb::Requeue => {
                managed_run_refusal(ctx, "managed agents cannot requeue blocked tasks")
            }
            TaskVerb::ResumeDelivery => Some(Err(OrbitError::CapabilityDenied(
                "delivery recovery requires an authenticated resident cycle".into(),
            ))),
            TaskVerb::Show
            | TaskVerb::List
            | TaskVerb::Eligible
            | TaskVerb::Lint
            | TaskVerb::ArtifactGet
            | TaskVerb::LocksList
            | TaskVerb::LocksReserve
            | TaskVerb::LocksRelease
            | TaskVerb::BlockTriage
            | TaskVerb::Reject
            | TaskVerb::Delete => None,
        }
    }

    fn preempt_after_input_check(self) -> bool {
        matches!(self, TaskVerb::ResumeDelivery)
    }

    fn normalize_input(self, input: &mut Value) {
        let boolean_fields: &[&str] = match self {
            TaskVerb::Add => &["allow_missing_context"],
            TaskVerb::Update => &["allow_missing_context", "allow_drop_system_tags", "repilot"],
            TaskVerb::Delete => &["force"],
            TaskVerb::Show
            | TaskVerb::List
            | TaskVerb::Eligible
            | TaskVerb::Lint
            | TaskVerb::ArtifactGet
            | TaskVerb::ArtifactPut
            | TaskVerb::LocksList
            | TaskVerb::LocksReserve
            | TaskVerb::LocksRelease
            | TaskVerb::ReviewReset
            | TaskVerb::Repilot
            | TaskVerb::Requeue
            | TaskVerb::ResumeDelivery
            | TaskVerb::BlockTriage
            | TaskVerb::ReconcileReview
            | TaskVerb::RetryHandoff
            | TaskVerb::RecoverClaim
            | TaskVerb::RevokeHandoff
            | TaskVerb::Reject => return,
        };
        let Some(object) = input.as_object_mut() else {
            return;
        };
        // These ordinary write fields had permissive domain parsers before
        // the migration. Leave null, malformed values and every other field
        // unchanged for the existing spec and domain checks.
        if let Some(value @ Value::Object(_)) = object.get_mut("relations") {
            *value = Value::Array(vec![value.take()]);
        }
        for field in boolean_fields {
            if let Some(value @ Value::String(_)) = object.get_mut(*field) {
                let normalized = value.as_str().map(str::trim);
                if normalized.is_some_and(|raw| raw.eq_ignore_ascii_case("true")) {
                    *value = Value::Bool(true);
                } else if normalized.is_some_and(|raw| raw.eq_ignore_ascii_case("false")) {
                    *value = Value::Bool(false);
                }
            }
        }
    }

    fn guarded_mode(
        self,
        ctx: &ToolContext,
        input: &OperationInput,
    ) -> Option<Result<Value, OrbitError>> {
        let input = input.as_value();
        match self {
            TaskVerb::Show => match bounded(input) {
                Ok(true) => Some(bounded_read(
                    ctx,
                    input.clone(),
                    "task",
                    BOUNDED_SHOW_KEYS,
                    "omit `view` to use the default full task record",
                )),
                Ok(false) => (input.get("snapshot") == Some(&Value::Bool(true)))
                    .then(|| snapshot(ctx, input.clone())),
                Err(error) => Some(Err(error)),
            },
            TaskVerb::List => match bounded(input) {
                Ok(false) => None,
                Ok(true) => Some(bounded_read(
                    ctx,
                    input.clone(),
                    "tasks",
                    BOUNDED_LIST_KEYS,
                    "omit `view` to use the default task listing",
                )),
                Err(error) => Some(Err(error)),
            },
            TaskVerb::Add
            | TaskVerb::Update
            | TaskVerb::Eligible
            | TaskVerb::Lint
            | TaskVerb::ArtifactGet
            | TaskVerb::ArtifactPut
            | TaskVerb::LocksList
            | TaskVerb::LocksReserve
            | TaskVerb::LocksRelease
            | TaskVerb::ReviewReset
            | TaskVerb::Repilot
            | TaskVerb::Requeue
            | TaskVerb::ResumeDelivery
            | TaskVerb::BlockTriage
            | TaskVerb::ReconcileReview
            | TaskVerb::RetryHandoff
            | TaskVerb::RecoverClaim
            | TaskVerb::RevokeHandoff
            | TaskVerb::Reject
            | TaskVerb::Delete => None,
        }
    }

    fn shape_input(self, ctx: &ToolContext, input: &mut Value) -> Result<(), OrbitError> {
        match self {
            TaskVerb::ArtifactPut => {
                *input = crate::artifact_materialize::prepare_remote_payload(input.take(), ctx)?;
                Ok(())
            }
            TaskVerb::ArtifactGet => {
                required_string(input, &["id"], "id")?;
                required_string(input, &["path"], "path").map_err(|error| {
                    OrbitError::InvalidInput(format!(
                        "{error}; {}",
                        artifact_listing_hint(&ctx.session_context)
                    ))
                })?;
                Ok(())
            }
            TaskVerb::Add => {
                // The operator command leaves its text to the domain's own
                // checks, and its description defaults to empty; MCP callers
                // must title and describe the task.
                if input.get(TASK_CLI_OPTIONS_PARAM).is_none() {
                    required_string(input, &["title"], "title")?;
                    required_string(input, &["description"], "description")?;
                }
                required_string(input, &["complexity"], "complexity")?;
                resolve_workspace_argument(ctx, input, "orbit.task.add")?;
                apply_session_orchestrator_default(ctx, input);
                Ok(())
            }
            TaskVerb::Update
            | TaskVerb::Show
            | TaskVerb::List
            | TaskVerb::Eligible
            | TaskVerb::Lint
            | TaskVerb::LocksList
            | TaskVerb::LocksReserve
            | TaskVerb::LocksRelease
            | TaskVerb::ReviewReset
            | TaskVerb::Repilot
            | TaskVerb::Requeue
            | TaskVerb::ResumeDelivery
            | TaskVerb::BlockTriage
            | TaskVerb::ReconcileReview
            | TaskVerb::RetryHandoff
            | TaskVerb::RecoverClaim
            | TaskVerb::RevokeHandoff
            | TaskVerb::Reject
            | TaskVerb::Delete => Ok(()),
        }
    }

    fn schema_extension(self) -> Option<fn(&mut Value)> {
        match self {
            TaskVerb::Add => Some(guarded::extend_create_schema),
            TaskVerb::Update => Some(guarded::extend_update_schema),
            TaskVerb::Show
            | TaskVerb::List
            | TaskVerb::Eligible
            | TaskVerb::Lint
            | TaskVerb::ArtifactGet
            | TaskVerb::ArtifactPut
            | TaskVerb::LocksList
            | TaskVerb::LocksReserve
            | TaskVerb::LocksRelease
            | TaskVerb::ReviewReset
            | TaskVerb::Repilot
            | TaskVerb::Requeue
            | TaskVerb::ResumeDelivery
            | TaskVerb::BlockTriage
            | TaskVerb::ReconcileReview
            | TaskVerb::RetryHandoff
            | TaskVerb::RecoverClaim
            | TaskVerb::RevokeHandoff
            | TaskVerb::Reject
            | TaskVerb::Delete => None,
        }
    }
}

/// The refusal of an operator decision a managed run attempts.
fn managed_run_refusal(
    ctx: &ToolContext,
    refusal: &'static str,
) -> Option<Result<Value, OrbitError>> {
    ctx.is_managed_run()
        .then(|| Err(OrbitError::CapabilityDenied(refusal.into())))
}

/// `implement_one` and a resolved `agent_implement` target. Deny mode stamps
/// the step id as the activity name; the catalog name is accepted too.
fn implementation_activity_sets_blocked(ctx: &ToolContext, input: &Value) -> bool {
    let Some(activity) = ctx
        .tool_deny_policy
        .as_ref()
        .map(|policy| policy.activity.as_str())
    else {
        return false;
    };
    if activity != "implement_one" && activity != "agent_implement" {
        return false;
    }
    input
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|status| status.eq_ignore_ascii_case("blocked"))
}

/// The bounded versioned snapshot: one task, an opaque revision and the
/// actions available, for an explicit workspace.
fn snapshot(ctx: &ToolContext, mut input: Value) -> Result<Value, OrbitError> {
    reject_unknown_tool_fields(&input, SNAPSHOT_KEYS)
        .map_err(|error| mode_field_error(error, "snapshot: true"))?;
    required_string(&input, &["workspace"], "workspace")?;
    let projection = response_projection(&input, SNAPSHOT_RESPONSE_FIELDS)?;
    let ignored_limit = input.get("limit").is_some();
    if let Some(object) = input.as_object_mut() {
        for key in ["snapshot", "fields", "field", "limit"] {
            object.remove(key);
        }
    }
    let result =
        super::super::execute_host_action(ctx, input, OrbitBuiltinAction::DesktopTaskSnapshot)?;
    let mut result = project_response(result, projection);
    if ignored_limit {
        if !result.is_object() {
            result = serde_json::json!({"value": result});
        }
        result["note"] = serde_json::json!("limit was ignored for snapshot: true");
    }
    Ok(result)
}

/// Parses the response projection and refuses names outside `allowed` before
/// any host action, so a refused request has no effect.
pub(super) fn response_projection(
    input: &Value,
    allowed: &[&str],
) -> Result<Option<(Vec<String>, bool)>, OrbitError> {
    let Some(fields) = optional_csv_or_string_list_alias(input, &["fields", "field"])? else {
        return Ok(None);
    };
    let unknown = fields
        .iter()
        .filter(|field| !allowed.contains(&field.as_str()))
        .map(|field| (field.clone(), Value::Null))
        .collect::<serde_json::Map<_, _>>();
    if !unknown.is_empty() {
        let hint = match reject_unknown_tool_fields(&Value::Object(unknown.clone()), allowed) {
            Err(error) => error
                .did_you_mean()
                .map(<[String]>::to_vec)
                .unwrap_or_default(),
            Ok(()) => Vec::new(),
        };
        let names = unknown.keys().cloned().collect::<Vec<_>>();
        let suggestion = hint
            .first()
            .map(|name| format!(" (did you mean '{name}'?)"))
            .unwrap_or_default();
        return Err(OrbitError::invalid_input_with_suggestions(
            format!(
                "unknown response field{} '{}'{suggestion}; valid values: {}",
                if names.len() == 1 { "" } else { "s" },
                names.join("', '"),
                allowed.join(", ")
            ),
            hint,
        ));
    }
    Ok(Some((fields, input.get("fields").is_some())))
}

pub(super) fn project_response(result: Value, projection: Option<(Vec<String>, bool)>) -> Value {
    let Some((fields, keyed)) = projection else {
        return result;
    };
    let Value::Object(mut object) = result else {
        return result;
    };
    if !keyed && fields.len() == 1 {
        return object.remove(&fields[0]).unwrap_or(Value::Null);
    }
    Value::Object(orbit_common::governance::task::select_fields(
        object, &fields,
    ))
}
