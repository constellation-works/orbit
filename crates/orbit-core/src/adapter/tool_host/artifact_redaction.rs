use std::collections::BTreeSet;

use orbit_common::OrbitError;
use orbit_common::governance::friction::FrictionVerb;
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_common::security::redaction::{
    is_high_confidence_single_token_credential, redact_all, redact_home_dir,
    redact_sensitive_env_text,
};
use orbit_tools::OrbitBuiltinAction;
use orbit_types::identity::normalize_optional_attribution_label;
use orbit_types::telemetry::AuditEventStatus;
use serde_json::{Map, Value, json};

use crate::{AuditEventInsertParams, OrbitRuntime};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum ArtifactRedactionKind {
    Env,
    Pattern,
    HomeDir,
}

impl ArtifactRedactionKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Env => "env",
            Self::Pattern => "pattern",
            Self::HomeDir => "home_dir",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ArtifactRedactionField {
    pub field_path: String,
    pub kinds: BTreeSet<ArtifactRedactionKind>,
    pub classes: BTreeSet<&'static str>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ArtifactRedactionReport {
    fields: Vec<ArtifactRedactionField>,
}

impl ArtifactRedactionReport {
    pub(super) fn redactions_applied(&self) -> bool {
        !self.fields.is_empty()
    }

    fn push(
        &mut self,
        field_path: String,
        kinds: BTreeSet<ArtifactRedactionKind>,
        classes: BTreeSet<&'static str>,
    ) {
        if !kinds.is_empty() {
            self.fields.push(ArtifactRedactionField {
                field_path,
                kinds,
                classes,
            });
        }
    }

    fn response_details(&self) -> Value {
        Value::Array(
            self.fields
                .iter()
                .map(|field| {
                    json!({
                        "field_path": field.field_path,
                        "redaction_kinds": field
                            .kinds
                            .iter()
                            .map(|kind| kind.as_str())
                            .collect::<Vec<_>>(),
                        "redaction_classes": field.classes,
                    })
                })
                .collect(),
        )
    }
}

pub(super) fn sanitize_tool_input(
    action: OrbitBuiltinAction,
    input: Value,
) -> Result<(Value, ArtifactRedactionReport), OrbitError> {
    let policy = policy_for_action(action);
    let Value::Object(mut object) = input else {
        return Ok((input, ArtifactRedactionReport::default()));
    };

    let mut report = ArtifactRedactionReport::default();
    for field in policy.free_text_fields {
        sanitize_string_field(&mut object, field, field, TextMode::Free, &mut report)?;
    }
    for field in policy.free_text_arrays {
        sanitize_string_array_field(&mut object, field, field, TextMode::Free, &mut report)?;
    }
    for field in policy.path_fields {
        sanitize_string_field(&mut object, field, field, TextMode::PathOnly, &mut report)?;
    }
    for field in policy.path_arrays {
        sanitize_string_array_field(&mut object, field, field, TextMode::PathOnly, &mut report)?;
    }
    for nested in policy.nested_arrays {
        sanitize_nested_string_array_field(&mut object, nested, &mut report)?;
    }
    for nested in policy.nested_objects {
        sanitize_nested_object_fields(&mut object, nested, &mut report)?;
    }
    Ok((Value::Object(object), report))
}

pub(super) fn finish_tool_response(
    runtime: &OrbitRuntime,
    action: OrbitBuiltinAction,
    response: &mut Value,
    report: &ArtifactRedactionReport,
    agent: Option<&str>,
    model: Option<&str>,
) -> Result<(), OrbitError> {
    if !is_covered_mutating_action(action) {
        return Ok(());
    }
    if let Some(object) = response.as_object_mut() {
        object.insert(
            "redactions_applied".to_string(),
            Value::Bool(report.redactions_applied()),
        );
        object.insert("redactions".to_string(), report.response_details());
    }
    if report.redactions_applied() {
        emit_audit_events(runtime, action, response, report, agent, model)?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum TextMode {
    Free,
    PathOnly,
}

#[derive(Clone, Copy)]
struct NestedArrayPolicy {
    array_key: &'static str,
    field_key: &'static str,
    field_alias: Option<&'static str>,
    mode: TextMode,
}

#[derive(Clone, Copy)]
struct NestedObjectPolicy {
    object_key: &'static str,
    free_text_fields: &'static [&'static str],
    free_text_arrays: &'static [&'static str],
}

struct ActionPolicy {
    free_text_fields: &'static [&'static str],
    free_text_arrays: &'static [&'static str],
    path_fields: &'static [&'static str],
    path_arrays: &'static [&'static str],
    nested_arrays: &'static [NestedArrayPolicy],
    nested_objects: &'static [NestedObjectPolicy],
}

const NO_REDACTION: ActionPolicy = ActionPolicy {
    free_text_fields: &[],
    free_text_arrays: &[],
    path_fields: &[],
    path_arrays: &[],
    nested_arrays: &[],
    nested_objects: &[],
};

const TASK_ADD_NESTED: &[NestedArrayPolicy] = &[
    NestedArrayPolicy {
        array_key: "external_refs",
        field_key: "url",
        field_alias: None,
        mode: TextMode::PathOnly,
    },
    NestedArrayPolicy {
        array_key: "externalRefs",
        field_key: "url",
        field_alias: None,
        mode: TextMode::PathOnly,
    },
    NestedArrayPolicy {
        array_key: "external-refs",
        field_key: "url",
        field_alias: None,
        mode: TextMode::PathOnly,
    },
];

const AUTO_TASK_TEMPLATE: &[NestedObjectPolicy] = &[NestedObjectPolicy {
    object_key: "template",
    free_text_fields: &["title", "description"],
    free_text_arrays: &["acceptance_criteria"],
}];

/// Every builtin action must make an explicit redaction decision here.
///
/// Keeping this match exhaustive makes an added action a compile failure until
/// its persisted input fields have been reviewed. `NO_REDACTION` is deliberate
/// for read-only actions and mutations that only persist structural values.
fn policy_for_action(action: OrbitBuiltinAction) -> ActionPolicy {
    match action {
        OrbitBuiltinAction::AdrAdd
        | OrbitBuiltinAction::AdrRestore
        | OrbitBuiltinAction::AdrUpdate => ActionPolicy {
            free_text_fields: &["title", "body"],
            free_text_arrays: &[],
            path_fields: &[],
            path_arrays: &[],
            nested_arrays: &[],
            nested_objects: &[],
        },
        OrbitBuiltinAction::TaskAdd => ActionPolicy {
            free_text_fields: &["title", "description", "plan", "comment"],
            free_text_arrays: &["acceptance_criteria"],
            path_fields: &[],
            path_arrays: &["context_files", "context"],
            nested_arrays: TASK_ADD_NESTED,
            nested_objects: &[],
        },
        OrbitBuiltinAction::TaskUpdate => ActionPolicy {
            free_text_fields: &[
                "title",
                "description",
                "plan",
                "execution_summary",
                "comment",
            ],
            free_text_arrays: &["acceptance_criteria"],
            path_fields: &[],
            path_arrays: &["context_files", "context"],
            nested_arrays: &[],
            nested_objects: &[],
        },
        OrbitBuiltinAction::TaskReject => ActionPolicy {
            free_text_fields: &["note", "comment"],
            free_text_arrays: &[],
            path_fields: &[],
            path_arrays: &[],
            nested_arrays: &[],
            nested_objects: &[],
        },
        OrbitBuiltinAction::Friction(FrictionVerb::Add) => ActionPolicy {
            free_text_fields: &["body", "description"],
            free_text_arrays: &[],
            path_fields: &[],
            path_arrays: &[],
            nested_arrays: &[],
            nested_objects: &[],
        },
        OrbitBuiltinAction::Friction(FrictionVerb::Update) => ActionPolicy {
            free_text_fields: &["body"],
            free_text_arrays: &[],
            path_fields: &[],
            path_arrays: &[],
            nested_arrays: &[],
            nested_objects: &[],
        },
        OrbitBuiltinAction::AutoTaskAdd | OrbitBuiltinAction::AutoTaskUpdate => ActionPolicy {
            free_text_fields: &["description"],
            free_text_arrays: &[],
            path_fields: &[],
            path_arrays: &[],
            nested_arrays: &[],
            nested_objects: AUTO_TASK_TEMPLATE,
        },
        OrbitBuiltinAction::AdrSupersede => ActionPolicy {
            free_text_fields: &[],
            free_text_arrays: &[],
            path_fields: &[],
            path_arrays: &[],
            nested_arrays: &[],
            nested_objects: &[],
        },
        OrbitBuiltinAction::AdrShow
        | OrbitBuiltinAction::AdrList
        | OrbitBuiltinAction::AutoTaskList
        | OrbitBuiltinAction::AutoTaskMint
        | OrbitBuiltinAction::AutoTaskShow
        | OrbitBuiltinAction::AutoTaskToggle
        | OrbitBuiltinAction::CommandExec
        | OrbitBuiltinAction::Friction(FrictionVerb::List)
        | OrbitBuiltinAction::Friction(FrictionVerb::Show)
        | OrbitBuiltinAction::Friction(FrictionVerb::Stats)
        | OrbitBuiltinAction::Friction(FrictionVerb::Tags)
        | OrbitBuiltinAction::Friction(FrictionVerb::Resolve)
        // Operation-mode verbs persist grant structure (task ids, rights,
        // windows, a short operator reason); no free-text artifact is written.
        | OrbitBuiltinAction::OperationMode(_)
        | OrbitBuiltinAction::DrainClaims
        | OrbitBuiltinAction::DrainProbe
        | OrbitBuiltinAction::DrainReceiptLookup
        | OrbitBuiltinAction::PipelineInvoke
        | OrbitBuiltinAction::PipelineWait
        | OrbitBuiltinAction::Search
        | OrbitBuiltinAction::StateGet
        | OrbitBuiltinAction::StateSet
        | OrbitBuiltinAction::TaskArtifactGet
        | OrbitBuiltinAction::TaskDelete
        | OrbitBuiltinAction::TaskLint
        | OrbitBuiltinAction::TaskList
        | OrbitBuiltinAction::TaskLocks
        | OrbitBuiltinAction::TaskLocksRelease
        | OrbitBuiltinAction::TaskLocksReserve
        | OrbitBuiltinAction::TaskShow
        | OrbitBuiltinAction::WorkflowRunList
        | OrbitBuiltinAction::WorkflowRunResume
        | OrbitBuiltinAction::WorkflowRunShow
        | OrbitBuiltinAction::WorkflowRunWorkers
        | OrbitBuiltinAction::WorkflowShip
        | OrbitBuiltinAction::WorkspaceClaimAcquire
        | OrbitBuiltinAction::WorkspaceClaimRelease
        | OrbitBuiltinAction::WorkspaceClaimShow
        // [ORB-11354] The invocation prompt is free text and *is* persisted on
        // the run record, but redacting it would change the instruction the
        // agent receives — an operator debugging an auth failure needs to name
        // the thing they are debugging. It stays verbatim; the run record is
        // already operator-only, and the provider argv/stdout paths keep their
        // own redactors.
        | OrbitBuiltinAction::AgentInvoke => NO_REDACTION,
    }
}

fn is_covered_mutating_action(action: OrbitBuiltinAction) -> bool {
    matches!(
        action,
        OrbitBuiltinAction::AdrAdd
            | OrbitBuiltinAction::AdrRestore
            | OrbitBuiltinAction::AdrUpdate
            | OrbitBuiltinAction::AdrSupersede
            | OrbitBuiltinAction::TaskAdd
            | OrbitBuiltinAction::TaskUpdate
            | OrbitBuiltinAction::TaskReject
            | OrbitBuiltinAction::AutoTaskAdd
            | OrbitBuiltinAction::AutoTaskUpdate
            | OrbitBuiltinAction::Friction(FrictionVerb::Add | FrictionVerb::Update)
    )
}

fn sanitize_string_field(
    object: &mut Map<String, Value>,
    key: &str,
    field_path: &str,
    mode: TextMode,
    report: &mut ArtifactRedactionReport,
) -> Result<(), OrbitError> {
    let Some(Value::String(raw)) = object.get(key) else {
        return Ok(());
    };
    let (sanitized, kinds, classes) = sanitize_string(raw, field_path, mode)?;
    if sanitized != *raw {
        object.insert(key.to_string(), Value::String(sanitized));
        report.push(field_path.to_string(), kinds, classes);
    }
    Ok(())
}

fn sanitize_string_array_field(
    object: &mut Map<String, Value>,
    key: &str,
    field_path: &str,
    mode: TextMode,
    report: &mut ArtifactRedactionReport,
) -> Result<(), OrbitError> {
    match object.get_mut(key) {
        Some(Value::String(raw)) => {
            let (sanitized, kinds, classes) = sanitize_string(raw, field_path, mode)?;
            if sanitized != *raw {
                *raw = sanitized;
                report.push(field_path.to_string(), kinds, classes);
            }
        }
        Some(Value::Array(items)) => {
            for (index, item) in items.iter_mut().enumerate() {
                let Value::String(raw) = item else {
                    continue;
                };
                let item_path = format!("{field_path}[{index}]");
                let (sanitized, kinds, classes) = sanitize_string(raw, &item_path, mode)?;
                if sanitized != *raw {
                    *raw = sanitized;
                    report.push(item_path, kinds, classes);
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn sanitize_nested_object_fields(
    object: &mut Map<String, Value>,
    policy: &NestedObjectPolicy,
    report: &mut ArtifactRedactionReport,
) -> Result<(), OrbitError> {
    let Some(Value::Object(nested)) = object.get_mut(policy.object_key) else {
        return Ok(());
    };
    for field in policy.free_text_fields {
        let field_path = format!("{}.{}", policy.object_key, field);
        sanitize_string_field(nested, field, &field_path, TextMode::Free, report)?;
    }
    for field in policy.free_text_arrays {
        let field_path = format!("{}.{}", policy.object_key, field);
        sanitize_string_array_field(nested, field, &field_path, TextMode::Free, report)?;
    }
    Ok(())
}

fn sanitize_nested_string_array_field(
    object: &mut Map<String, Value>,
    policy: &NestedArrayPolicy,
    report: &mut ArtifactRedactionReport,
) -> Result<(), OrbitError> {
    if policy.array_key == "scope" {
        let Some(Value::Object(scope)) = object.get_mut("scope") else {
            return Ok(());
        };
        return sanitize_string_array_field(
            scope,
            policy.field_key,
            &format!("scope.{}", policy.field_key),
            policy.mode,
            report,
        );
    }

    let Some(Value::Array(items)) = object.get_mut(policy.array_key) else {
        return Ok(());
    };
    for (index, item) in items.iter_mut().enumerate() {
        let Value::Object(entry) = item else {
            continue;
        };
        let Some(key) = entry
            .contains_key(policy.field_key)
            .then_some(policy.field_key)
            .or_else(|| {
                policy
                    .field_alias
                    .filter(|alias| entry.contains_key(*alias))
            })
        else {
            continue;
        };
        let Some(Value::String(raw)) = entry.get(key) else {
            continue;
        };
        let field_path = format!("{}[{index}].{key}", policy.array_key);
        let (sanitized, kinds, classes) = sanitize_string(raw, &field_path, policy.mode)?;
        if sanitized != *raw {
            entry.insert(key.to_string(), Value::String(sanitized));
            report.push(field_path, kinds, classes);
        }
    }
    Ok(())
}

fn sanitize_string(
    raw: &str,
    field_path: &str,
    mode: TextMode,
) -> Result<
    (
        String,
        BTreeSet<ArtifactRedactionKind>,
        BTreeSet<&'static str>,
    ),
    OrbitError,
> {
    match mode {
        TextMode::PathOnly => {
            let sanitized = redact_home_dir(raw);
            let mut kinds = BTreeSet::new();
            let mut classes = BTreeSet::new();
            if sanitized != raw {
                kinds.insert(ArtifactRedactionKind::HomeDir);
                classes.insert("home_directory");
            }
            Ok((sanitized, kinds, classes))
        }
        TextMode::Free => {
            if is_high_confidence_single_token_credential(raw) {
                return Err(OrbitError::SensitiveInput {
                    field: field_path.to_string(),
                    reason: "whole-token credentials must not be persisted in Orbit artifacts"
                        .to_string(),
                });
            }
            let env_scrubbed = redact_sensitive_env_text(raw);
            let pattern_scrubbed = redact_all(raw);
            let sanitized = redact_home_dir(&pattern_scrubbed);
            let mut kinds = BTreeSet::new();
            let mut classes = BTreeSet::new();
            if env_scrubbed != raw {
                kinds.insert(ArtifactRedactionKind::Env);
                classes.insert("sensitive_environment_value");
            }
            if pattern_scrubbed != env_scrubbed {
                kinds.insert(ArtifactRedactionKind::Pattern);
                classes.extend(pattern_redaction_classes(&env_scrubbed, &pattern_scrubbed));
            }
            if sanitized != pattern_scrubbed {
                kinds.insert(ArtifactRedactionKind::HomeDir);
                classes.insert("home_directory");
            }
            Ok((sanitized, kinds, classes))
        }
    }
}

fn pattern_redaction_classes(before: &str, after: &str) -> BTreeSet<&'static str> {
    [
        ("[REDACTED_AUTH]", "authorization"),
        ("[REDACTED_SECRET]", "credential"),
        ("[REDACTED_API_KEY]", "credential"),
        ("[REDACTED_SSH_FINGERPRINT]", "ssh_fingerprint"),
        ("[REDACTED_SSH_KEY_COMMENT]", "ssh_key_comment"),
        ("[REDACTED_SSH_HOST]", "ssh_host"),
    ]
    .into_iter()
    .filter_map(|(marker, class)| {
        (after.matches(marker).count() > before.matches(marker).count()).then_some(class)
    })
    .collect()
}

fn emit_audit_events(
    runtime: &OrbitRuntime,
    action: OrbitBuiltinAction,
    response: &Value,
    report: &ArtifactRedactionReport,
    agent: Option<&str>,
    model: Option<&str>,
) -> Result<(), OrbitError> {
    let tool_name = tool_name(action);
    let artifact = artifact_target(action, response)?;
    let actor = normalize_optional_attribution_label(model.or(agent), model)
        .unwrap_or_else(|| runtime.actor_label().to_string());

    for field in &report.fields {
        let redaction_kinds = field
            .kinds
            .iter()
            .map(|kind| kind.as_str())
            .collect::<Vec<_>>();
        let payload = json!({
            "artifact_type": artifact.artifact_type,
            "artifact_id": artifact.artifact_id,
            "field_path": field.field_path,
            "actor": actor,
            "tool_name": tool_name,
            "redaction_kinds": redaction_kinds,
            "redaction_classes": field.classes,
        });
        runtime.record_audit_event(&AuditEventInsertParams {
            execution_id: audit_execution_id("audit-artifact-redaction"),
            command: "artifact_redaction".to_string(),
            subcommand: Some("field".to_string()),
            tool_name: Some(tool_name.to_string()),
            target_type: Some(artifact.artifact_type.to_string()),
            target_id: Some(artifact.artifact_id.to_string()),
            role: actor.clone(),
            status: AuditEventStatus::Success,
            exit_code: 0,
            duration_ms: 0,
            working_directory: runtime.paths().repo_root.to_string_lossy().into_owned(),
            arguments_json: Some(payload.to_string()),
            stdout_truncated: None,
            stderr_truncated: None,
            error_message: None,
            host: std::env::var("HOSTNAME").ok(),
            pid: std::process::id(),
            session_id: None,
            workspace_id: None,
            caller_machine_id: None,
            caller_machine_name: None,
            process_machine_id: None,
            process_machine_name: None,
            transport: None,
            effective_capabilities: Default::default(),
            origin_session_id: None,
            mcp_call_id: None,
            lease_id: None,
            task_id: artifact.task_id.map(ToOwned::to_owned),
            job_run_id: std::env::var("ORBIT_RUN_ID").ok().filter(|s| !s.is_empty()),
            activity_id: std::env::var("ORBIT_ACTIVITY_ID")
                .ok()
                .filter(|s| !s.is_empty()),
            step_index: std::env::var("ORBIT_STEP_INDEX")
                .ok()
                .and_then(|s| s.parse().ok()),
        })?;
    }
    Ok(())
}

struct ArtifactTarget<'a> {
    artifact_type: &'static str,
    artifact_id: &'a str,
    task_id: Option<&'a str>,
}

fn artifact_target(
    action: OrbitBuiltinAction,
    response: &Value,
) -> Result<ArtifactTarget<'_>, OrbitError> {
    match action {
        OrbitBuiltinAction::AdrAdd
        | OrbitBuiltinAction::AdrRestore
        | OrbitBuiltinAction::AdrUpdate
        | OrbitBuiltinAction::AdrSupersede => Ok(ArtifactTarget {
            artifact_type: "adr",
            artifact_id: response_string(response, "id")?,
            task_id: None,
        }),
        OrbitBuiltinAction::TaskAdd
        | OrbitBuiltinAction::TaskUpdate
        | OrbitBuiltinAction::TaskReject => {
            let id = response_string(response, "id")?;
            Ok(ArtifactTarget {
                artifact_type: "task",
                artifact_id: id,
                task_id: Some(id),
            })
        }
        OrbitBuiltinAction::Friction(FrictionVerb::Add | FrictionVerb::Update) => {
            Ok(ArtifactTarget {
                artifact_type: "friction",
                artifact_id: response_string(response, "id")?,
                task_id: None,
            })
        }
        OrbitBuiltinAction::AutoTaskAdd | OrbitBuiltinAction::AutoTaskUpdate => {
            Ok(ArtifactTarget {
                artifact_type: "auto_task",
                artifact_id: response_string(response, "name")?,
                task_id: None,
            })
        }
        _ => Err(OrbitError::Execution(format!(
            "unsupported redaction audit action: {action:?}"
        ))),
    }
}

fn response_string<'a>(response: &'a Value, field: &str) -> Result<&'a str, OrbitError> {
    response.get(field).and_then(Value::as_str).ok_or_else(|| {
        OrbitError::Execution(format!("redaction audit response missing string `{field}`"))
    })
}

fn tool_name(action: OrbitBuiltinAction) -> &'static str {
    match action {
        OrbitBuiltinAction::AdrAdd => "orbit.adr.add",
        OrbitBuiltinAction::AdrRestore => "orbit.adr.restore",
        OrbitBuiltinAction::AdrUpdate => "orbit.adr.update",
        OrbitBuiltinAction::AdrSupersede => "orbit.adr.supersede",
        OrbitBuiltinAction::TaskAdd => "orbit.task.add",
        OrbitBuiltinAction::TaskUpdate => "orbit.task.update",
        OrbitBuiltinAction::TaskReject => "orbit.task.reject",
        OrbitBuiltinAction::Friction(FrictionVerb::Add) => "orbit.friction.add",
        OrbitBuiltinAction::Friction(FrictionVerb::Update) => "orbit.friction.update",
        OrbitBuiltinAction::AutoTaskAdd => "orbit.auto_task.add",
        OrbitBuiltinAction::AutoTaskUpdate => "orbit.auto_task.update",
        _ => "orbit.unknown",
    }
}
