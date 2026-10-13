use std::collections::BTreeSet;

use orbit_common::OrbitError;
use orbit_common::governance::auto_task::AutoTaskVerb;
use orbit_common::governance::drain::DrainVerb;
use orbit_common::governance::friction::FrictionVerb;
use orbit_common::governance::operation::{OperationVerb, ParamSpec, ParamType, Redaction};
use orbit_common::governance::task::{TASK_CLI_OPTIONS_PARAM, TaskVerb};
use orbit_common::governance::workspace_claim::WorkspaceClaimVerb;
use orbit_common::security::redaction::{
    is_high_confidence_single_token_credential, redact_all, redact_home_dir,
    redact_sensitive_env_text,
};
use orbit_tools::OrbitBuiltinAction;
use orbit_types::identity::normalize_optional_attribution_label;
use orbit_types::telemetry::AuditEventStatus;
use serde_json::{Map, Value, json};

use crate::OrbitRuntime;
use crate::runtime::audit::system::{
    AuditRunAttribution, SystemAuditEvent, record_system_audit_event,
};

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
    fn redactions_applied(&self) -> bool {
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
    for param in policy.declared.iter().flat_map(|group| group.iter()) {
        let mode = match param.redact {
            Redaction::None => continue,
            Redaction::FreeText => TextMode::Free,
            Redaction::Path => TextMode::PathOnly,
        };
        // The tool entry renames aliases before dispatch; an alias takes its
        // parameter's class all the same.
        for field in std::iter::once(&param.name).chain(param.aliases) {
            match param.param_type {
                ParamType::StringList | ParamType::EnumList(_) => {
                    sanitize_string_array_field(&mut object, field, field, mode, &mut report)?
                }
                ParamType::String
                | ParamType::Integer
                | ParamType::UnsignedInteger
                | ParamType::Boolean
                | ParamType::Switch
                | ParamType::Enum(_) => {
                    sanitize_string_field(&mut object, field, field, mode, &mut report)?
                }
                // An object's free text is the action policy's `nested_objects`.
                ParamType::Object | ParamType::Array => {}
            }
        }
    }
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
    report: &mut ArtifactRedactionReport,
    persisted_task_id: Option<&str>,
    agent: Option<&str>,
    model: Option<&str>,
) -> Result<(), OrbitError> {
    if !is_covered_mutating_action(action) {
        return Ok(());
    }
    if action == OrbitBuiltinAction::TaskReconcileReview {
        sanitize_response_value(response, "response", report);
    }
    if let Some(object) = response.as_object_mut() {
        object.insert(
            "redactions_applied".to_string(),
            Value::Bool(report.redactions_applied()),
        );
        object.insert("redactions".to_string(), report.response_details());
    }
    if report.redactions_applied() {
        emit_audit_events(
            runtime,
            action,
            response,
            report,
            persisted_task_id,
            agent,
            model,
        )?;
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
    /// A registry operation's parameter groups; each parameter is redacted
    /// per its declared [`Redaction`] class, in declaration order.
    declared: &'static [&'static [ParamSpec]],
    free_text_fields: &'static [&'static str],
    free_text_arrays: &'static [&'static str],
    path_fields: &'static [&'static str],
    path_arrays: &'static [&'static str],
    nested_arrays: &'static [NestedArrayPolicy],
    nested_objects: &'static [NestedObjectPolicy],
}

const NO_REDACTION: ActionPolicy = ActionPolicy {
    declared: &[],
    free_text_fields: &[],
    free_text_arrays: &[],
    path_fields: &[],
    path_arrays: &[],
    nested_arrays: &[],
    nested_objects: &[],
};

/// The free text among `orbit task add`'s CLI options.
const TASK_ADD_CLI_OPTIONS: &[NestedObjectPolicy] = &[NestedObjectPolicy {
    object_key: TASK_CLI_OPTIONS_PARAM,
    free_text_fields: &["plan"],
    free_text_arrays: &[],
}];

/// A template replacement and a field-scoped template edit carry the same
/// free-text fields.
const AUTO_TASK_TEMPLATE: &[NestedObjectPolicy] = &[
    NestedObjectPolicy {
        object_key: "template",
        free_text_fields: &["title", "description"],
        free_text_arrays: &["acceptance_criteria"],
    },
    NestedObjectPolicy {
        object_key: "template_patch",
        free_text_fields: &["title", "description"],
        free_text_arrays: &["acceptance_criteria"],
    },
];

/// Every builtin action must make an explicit redaction decision here.
///
/// Keeping this match exhaustive makes an added action a compile failure until
/// its persisted input fields have been reviewed. `NO_REDACTION` is deliberate
/// for read-only actions and mutations that only persist structural values.
fn policy_for_action(action: OrbitBuiltinAction) -> ActionPolicy {
    match action {
        // An artifact payload is stored as given; only its path is
        // structural, and the store screens it.
        OrbitBuiltinAction::TaskArtifactPut => NO_REDACTION,
        OrbitBuiltinAction::TaskReconcileReview => ActionPolicy {
            // `command` selects an already-authorized stored command. Preserve
            // it byte-for-byte for exact evidence matching, then redact only
            // serialized reports at the response boundary.
            declared: &[],
            free_text_fields: &["reason"],
            free_text_arrays: &[], path_fields: &[], path_arrays: &[], nested_arrays: &[], nested_objects: &[],
        },
        OrbitBuiltinAction::TaskReviewReset => ActionPolicy {
            declared: &[],
            free_text_fields: &["reason"],
            free_text_arrays: &[], path_fields: &[], path_arrays: &[], nested_arrays: &[], nested_objects: &[],
        },
        OrbitBuiltinAction::TaskReject => ActionPolicy {
            declared: &[],
            free_text_fields: &["note", "comment"],
            free_text_arrays: &[],
            path_fields: &[],
            path_arrays: &[],
            nested_arrays: &[],
            nested_objects: &[],
        },
        // Claim-lifecycle payloads are compared against durable replay records;
        // all parameters are structural and must stay byte-for-byte intact.
        OrbitBuiltinAction::Drain(verb) => match verb {
            DrainVerb::Probe
            | DrainVerb::ReceiptLookup
            | DrainVerb::Pull
            | DrainVerb::ClaimBind
            | DrainVerb::ClaimSettle
            | DrainVerb::Claims => ActionPolicy {
                declared: verb.spec().params,
                ..NO_REDACTION
            },
        },
        OrbitBuiltinAction::WorkspaceClaim(verb) => match verb {
            WorkspaceClaimVerb::Acquire
            | WorkspaceClaimVerb::Release
            | WorkspaceClaimVerb::Show => ActionPolicy {
                declared: verb.spec().params,
                ..NO_REDACTION
            },
        },
        OrbitBuiltinAction::Friction(verb) => ActionPolicy {
            declared: verb.spec().params,
            ..NO_REDACTION
        },
        OrbitBuiltinAction::Task(verb) => match verb {
            // The plan `orbit task add` carries among its CLI options is free
            // text too.
            TaskVerb::Add => ActionPolicy {
                declared: verb.spec().params,
                nested_objects: TASK_ADD_CLI_OPTIONS,
                ..NO_REDACTION
            },
            // Each parameter redacts per its declared class; the task reads
            // persist nothing, and lint writes only recorded selectors.
            TaskVerb::Update
            | TaskVerb::List
            | TaskVerb::Eligible
            | TaskVerb::Show
            | TaskVerb::Lint => ActionPolicy {
                declared: verb.spec().params,
                ..NO_REDACTION
            },
        },
        OrbitBuiltinAction::AutoTask(verb) => match verb {
            AutoTaskVerb::Add | AutoTaskVerb::Update => ActionPolicy {
                declared: verb.spec().params,
                nested_objects: AUTO_TASK_TEMPLATE,
                ..NO_REDACTION
            },
            // Delete, recover and reset reasons are operator audit notes kept
            // verbatim; the other verbs persist only structural values.
            AutoTaskVerb::List
            | AutoTaskVerb::Show
            | AutoTaskVerb::Toggle
            | AutoTaskVerb::Delete
            | AutoTaskVerb::Restore
            | AutoTaskVerb::Mint
            | AutoTaskVerb::Recover
            | AutoTaskVerb::Reset => NO_REDACTION,
        },
        OrbitBuiltinAction::DesktopRead
        | OrbitBuiltinAction::DesktopDrain
        | OrbitBuiltinAction::DesktopAutomation
        | OrbitBuiltinAction::DesktopTaskSnapshot
        // Desktop writes redact at their transactional persistence boundary.
        | OrbitBuiltinAction::DesktopTaskWrite
        | OrbitBuiltinAction::CommandExec
        | OrbitBuiltinAction::PipelineInvoke
        | OrbitBuiltinAction::PipelineWait
        | OrbitBuiltinAction::Search
        | OrbitBuiltinAction::TaskArtifactGet
        | OrbitBuiltinAction::TaskDelete
        | OrbitBuiltinAction::TaskLocks
        | OrbitBuiltinAction::TaskLocksRelease
        | OrbitBuiltinAction::TaskLocksReserve
        | OrbitBuiltinAction::WorkflowRunList
        | OrbitBuiltinAction::WorkflowRunResume
        | OrbitBuiltinAction::WorkflowRunShow
        | OrbitBuiltinAction::WorkflowShip
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
    match action {
        // A registry operation reports redactions when it declares a redacted
        // parameter.
        OrbitBuiltinAction::Friction(verb) => verb
            .spec()
            .params()
            .any(|param| param.redact != Redaction::None),
        OrbitBuiltinAction::AutoTask(verb) => {
            !policy_for_action(OrbitBuiltinAction::AutoTask(verb))
                .nested_objects
                .is_empty()
        }
        OrbitBuiltinAction::Drain(verb) => match verb {
            DrainVerb::Probe
            | DrainVerb::ReceiptLookup
            | DrainVerb::Pull
            | DrainVerb::ClaimBind
            | DrainVerb::ClaimSettle
            | DrainVerb::Claims => false,
        },
        OrbitBuiltinAction::WorkspaceClaim(verb) => match verb {
            WorkspaceClaimVerb::Acquire
            | WorkspaceClaimVerb::Release
            | WorkspaceClaimVerb::Show => false,
        },
        OrbitBuiltinAction::Task(verb) => match verb {
            TaskVerb::Add | TaskVerb::Update => true,
            TaskVerb::List | TaskVerb::Eligible | TaskVerb::Show | TaskVerb::Lint => false,
        },
        action => matches!(
            action,
            OrbitBuiltinAction::TaskArtifactPut
                | OrbitBuiltinAction::TaskReject
                | OrbitBuiltinAction::TaskReconcileReview
                | OrbitBuiltinAction::TaskReviewReset
        ),
    }
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

fn sanitize_response_value(value: &mut Value, path: &str, report: &mut ArtifactRedactionReport) {
    match value {
        Value::String(raw) => {
            let env_scrubbed = redact_sensitive_env_text(raw);
            let pattern_scrubbed = redact_all(&env_scrubbed);
            let sanitized = redact_home_dir(&pattern_scrubbed);
            let mut kinds = BTreeSet::new();
            let mut classes = BTreeSet::new();
            if env_scrubbed != *raw {
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
            if sanitized != *raw {
                *raw = sanitized;
                report.push(path.to_string(), kinds, classes);
            }
        }
        Value::Array(values) => {
            for (index, value) in values.iter_mut().enumerate() {
                sanitize_response_value(value, &format!("{path}[{index}]"), report);
            }
        }
        Value::Object(fields) => {
            for (key, value) in fields {
                sanitize_response_value(value, &format!("{path}.{key}"), report);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn emit_audit_events(
    runtime: &OrbitRuntime,
    action: OrbitBuiltinAction,
    response: &Value,
    report: &ArtifactRedactionReport,
    persisted_task_id: Option<&str>,
    agent: Option<&str>,
    model: Option<&str>,
) -> Result<(), OrbitError> {
    let tool_name = tool_name(action);
    let artifact = artifact_target(action, response, persisted_task_id)?;
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
        record_system_audit_event(
            runtime,
            SystemAuditEvent {
                execution_id_prefix: "audit-artifact-redaction",
                command: "artifact_redaction",
                subcommand: Some("field"),
                tool_name: Some(tool_name),
                target_type: artifact.artifact_type,
                target_id: Some(artifact.artifact_id),
                role: &actor,
                status: AuditEventStatus::Success,
                arguments_json: payload.to_string(),
                workspace_id: None,
                task_id: artifact.task_id,
                attribution: AuditRunAttribution::Environment { run_id: None },
            },
        )?;
    }
    Ok(())
}

#[derive(Debug)]
pub(super) struct ArtifactTarget<'a> {
    artifact_type: &'static str,
    artifact_id: &'a str,
    task_id: Option<&'a str>,
}

fn artifact_target<'a>(
    action: OrbitBuiltinAction,
    response: &'a Value,
    persisted_task_id: Option<&'a str>,
) -> Result<ArtifactTarget<'a>, OrbitError> {
    match action {
        OrbitBuiltinAction::Task(TaskVerb::Add | TaskVerb::Update)
        | OrbitBuiltinAction::TaskArtifactPut
        | OrbitBuiltinAction::TaskReject
        | OrbitBuiltinAction::TaskReconcileReview
        | OrbitBuiltinAction::TaskReviewReset => {
            let id = persisted_task_id.ok_or_else(|| {
                OrbitError::Execution("redaction audit missing persisted task id".to_string())
            })?;
            Ok(ArtifactTarget {
                artifact_type: "task",
                artifact_id: id,
                task_id: Some(id),
            })
        }
        // Every friction verb that redacts writes the record its response
        // names.
        OrbitBuiltinAction::Friction(
            FrictionVerb::Add
            | FrictionVerb::List
            | FrictionVerb::Show
            | FrictionVerb::Stats
            | FrictionVerb::Tags
            | FrictionVerb::Update
            | FrictionVerb::Resolve
            | FrictionVerb::Rehome,
        ) => Ok(ArtifactTarget {
            artifact_type: "friction",
            artifact_id: response_string(response, "id")?,
            task_id: None,
        }),
        OrbitBuiltinAction::AutoTask(verb) => match verb {
            AutoTaskVerb::Add | AutoTaskVerb::Update => Ok(ArtifactTarget {
                artifact_type: "auto_task",
                artifact_id: response_string(response, "name")?,
                task_id: None,
            }),
            AutoTaskVerb::List
            | AutoTaskVerb::Show
            | AutoTaskVerb::Toggle
            | AutoTaskVerb::Delete
            | AutoTaskVerb::Restore
            | AutoTaskVerb::Mint
            | AutoTaskVerb::Recover
            | AutoTaskVerb::Reset => Err(OrbitError::Execution(format!(
                "unsupported redaction audit action: {action:?}"
            ))),
        },
        OrbitBuiltinAction::Drain(verb) => match verb {
            DrainVerb::Probe
            | DrainVerb::ReceiptLookup
            | DrainVerb::Pull
            | DrainVerb::ClaimBind
            | DrainVerb::ClaimSettle
            | DrainVerb::Claims => Err(OrbitError::Execution(format!(
                "unsupported redaction audit action: {action:?}"
            ))),
        },
        OrbitBuiltinAction::WorkspaceClaim(verb) => match verb {
            WorkspaceClaimVerb::Acquire
            | WorkspaceClaimVerb::Release
            | WorkspaceClaimVerb::Show => Err(OrbitError::Execution(format!(
                "unsupported redaction audit action: {action:?}"
            ))),
        },
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
        OrbitBuiltinAction::TaskArtifactPut => "orbit.task.artifact.put",
        OrbitBuiltinAction::TaskReject => "orbit.task.reject",
        OrbitBuiltinAction::TaskReconcileReview => "orbit.task.reconcile_review",
        OrbitBuiltinAction::TaskReviewReset => "orbit.task.review_reset",
        OrbitBuiltinAction::Task(verb) => verb.tool_name(),
        OrbitBuiltinAction::Friction(verb) => verb.tool_name(),
        OrbitBuiltinAction::AutoTask(verb) => verb.tool_name(),
        OrbitBuiltinAction::Drain(verb) => match verb {
            DrainVerb::Probe
            | DrainVerb::ReceiptLookup
            | DrainVerb::Pull
            | DrainVerb::ClaimBind
            | DrainVerb::ClaimSettle
            | DrainVerb::Claims => verb.tool_name(),
        },
        OrbitBuiltinAction::WorkspaceClaim(verb) => match verb {
            WorkspaceClaimVerb::Acquire
            | WorkspaceClaimVerb::Release
            | WorkspaceClaimVerb::Show => verb.tool_name(),
        },
        _ => "orbit.unknown",
    }
}
