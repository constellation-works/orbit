use orbit_common::OrbitError;
use orbit_common::governance::operation::{ParamType, Redaction};
use orbit_common::governance::resident::ResidentVerb;
use orbit_common::governance::task::TaskVerb;
use orbit_tools::OrbitBuiltinAction;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::resident;

use super::policy::{self, TextMode};
use super::{ArtifactRedactionReport, audit, sanitize};

pub(in super::super) fn sanitize_tool_input(
    action: OrbitBuiltinAction,
    input: Value,
) -> Result<(Value, ArtifactRedactionReport), OrbitError> {
    if let Some(comment) = input.get("comment").and_then(Value::as_str) {
        resident::ensure_resident_comment_redaction_safe(comment)?;
    }
    if matches!(
        action,
        OrbitBuiltinAction::Resident(ResidentVerb::Checkpoint)
    ) && let Some(record) = input.get("record")
    {
        resident::ensure_resident_comment_redaction_safe(&record.to_string())?;
    }
    let policy = policy::policy_for_action(action);
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
                    sanitize::sanitize_string_array_field(
                        &mut object,
                        field,
                        field,
                        mode,
                        &mut report,
                    )?
                }
                ParamType::String
                | ParamType::RawString
                | ParamType::Path
                | ParamType::Integer
                | ParamType::UnsignedInteger
                | ParamType::Boolean
                | ParamType::Switch
                | ParamType::Enum(_) => {
                    sanitize::sanitize_string_field(&mut object, field, field, mode, &mut report)?
                }
                // An object's free text is the action policy's `nested_objects`.
                ParamType::Object | ParamType::Array => {}
            }
        }
    }
    for field in policy.free_text_fields {
        sanitize::sanitize_string_field(&mut object, field, field, TextMode::Free, &mut report)?;
    }
    for field in policy.free_text_arrays {
        sanitize::sanitize_string_array_field(
            &mut object,
            field,
            field,
            TextMode::Free,
            &mut report,
        )?;
    }
    for field in policy.path_fields {
        sanitize::sanitize_string_field(
            &mut object,
            field,
            field,
            TextMode::PathOnly,
            &mut report,
        )?;
    }
    for field in policy.path_arrays {
        sanitize::sanitize_string_array_field(
            &mut object,
            field,
            field,
            TextMode::PathOnly,
            &mut report,
        )?;
    }
    for nested in policy.nested_arrays {
        sanitize::sanitize_nested_string_array_field(&mut object, nested, &mut report)?;
    }
    for nested in policy.nested_objects {
        sanitize::sanitize_nested_object_fields(&mut object, nested, &mut report)?;
    }
    Ok((Value::Object(object), report))
}

pub(in super::super) fn finish_tool_response(
    runtime: &OrbitRuntime,
    action: OrbitBuiltinAction,
    response: &mut Value,
    report: &mut ArtifactRedactionReport,
    persisted_task_id: Option<&str>,
    agent: Option<&str>,
    model: Option<&str>,
) -> Result<(), OrbitError> {
    if !policy::is_covered_mutating_action(action) {
        return Ok(());
    }
    if action == OrbitBuiltinAction::Task(TaskVerb::ReconcileReview) {
        sanitize::sanitize_response_value(response, "response", report);
    }
    if let Some(object) = response.as_object_mut() {
        object.insert(
            "redactions_applied".to_string(),
            Value::Bool(report.redactions_applied()),
        );
        object.insert("redactions".to_string(), report.response_details());
    }
    if report.redactions_applied() {
        audit::emit_audit_events(
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
