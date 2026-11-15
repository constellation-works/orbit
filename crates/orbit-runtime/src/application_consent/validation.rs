//! Consent-time validation of application consent schema v2: scope names,
//! rate limits, the per-operation budget, the expiry cap and the closed
//! constraint set. Admission of constrained scopes is a later slice; this
//! module only decides what an operator may record.

use std::collections::{BTreeMap, BTreeSet};

use crate::RuntimeKernel;
use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_common::governance::authorization::{
    APPLICATION_BACKLOG_TRANSITION_SOURCES, APPLICATION_DIRECT_TASK_STATUSES,
    APPLICATION_EXACT_TASK_STATUSES, APPLICATION_TASK_SHOW_FIELDS,
    APPLICATION_TRANSITION_TASK_STATUSES, application_operation_ceiling,
    application_operator_floored,
};
use orbit_types::tool::{
    APPLICATION_CONSENT_SCHEMA_V2, ApplicationConsentRequest, ApplicationConstraint,
    ApplicationOperation, ApplicationScope, ApplicationTaskPredicate,
};
use serde_json::Value;
use std::path::Path;

/// Largest serialized operation a schema v2 consent may carry.
const OPERATION_BUDGET_BYTES: usize = 8 * 1024;
/// Largest rate-limit window, one day.
const RATE_LIMIT_MAX_WINDOW_SECONDS: u32 = 86_400;
/// Most admissions a rate limit may allow per window.
const RATE_LIMIT_MAX_ADMISSIONS: u32 = 10_000;
/// Every task status a predicate or transition may name.
const TASK_STATUSES: &[&str] = &[
    "proposed",
    "backlog",
    "in-progress",
    "review",
    "done",
    "blocked",
    "archived",
    "rejected",
    "someday",
];
/// Fields that name the object a call acts on. They bind to state, never to
/// free text.
const IDENTITY_FIELDS: &[&str] = &["id", "name", "path", "draft"];
/// Built-in fields that may carry bounded free text.
const BUILTIN_TEXT_FIELDS: &[&str] = &["comment", "note", "plan", "execution_summary"];

/// Where an operation sits in its scope.
#[derive(Clone, Copy)]
pub(crate) enum OperationRole<'a> {
    Entry,
    Callback { entry: &'a ApplicationOperation },
}

/// Longest lifetime, in days, of an application consent with constrained,
/// named or rate-limited scopes, so a stolen credential has a bounded window
/// and the operator re-reviews the profile at least monthly.
///
/// The former `machine.application_consent_max_days` default, fixed when the key was removed [ORB-22808].
pub(crate) const APPLICATION_CONSENT_MAX_DAYS: u32 = 30;

/// Request-level schema v2 rules. A v1 request (every scope exact, unnamed
/// and unlimited) passes untouched and keeps its 4 KiB request budget.
pub(crate) fn validate_consent_request_v2(
    request: &ApplicationConsentRequest,
    max_days: u32,
    now: DateTime<Utc>,
) -> Result<(), OrbitError> {
    if request.schema_version() != APPLICATION_CONSENT_SCHEMA_V2 {
        return Ok(());
    }
    if request.expires_at > now + Duration::days(i64::from(max_days)) {
        return Err(OrbitError::InvalidInput(format!(
            "application consent with constrained, named or rate-limited scopes must expire \
             within {max_days} days"
        )));
    }
    let mut names = BTreeSet::new();
    for scope in &request.scopes {
        validate_scope_header(scope, &mut names)?;
        for operation in std::iter::once(&scope.operation).chain(&scope.callbacks) {
            let bytes = serde_json::to_vec(operation)
                .map_err(|_| OrbitError::InvalidInput("invalid application operation".into()))?;
            if bytes.len() > OPERATION_BUDGET_BYTES {
                return Err(OrbitError::InvalidInput(format!(
                    "application operation `{}` exceeds the 8 KiB per-operation budget",
                    operation.tool
                )));
            }
        }
    }
    Ok(())
}

fn validate_scope_header<'a>(
    scope: &'a ApplicationScope,
    names: &mut BTreeSet<&'a str>,
) -> Result<(), OrbitError> {
    let constrained = scope.operation.is_constrained()
        || scope
            .callbacks
            .iter()
            .any(ApplicationOperation::is_constrained);
    match scope.name.as_deref() {
        Some(name) => {
            if name.is_empty()
                || name.len() > 48
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            {
                return Err(OrbitError::InvalidInput(
                    "application scope names match [a-z0-9-]{1,48}".into(),
                ));
            }
            if !names.insert(name) {
                return Err(OrbitError::InvalidInput(format!(
                    "application scope name `{name}` is not unique"
                )));
            }
        }
        None if constrained => {
            return Err(OrbitError::InvalidInput(
                "a constrained application scope needs a `name`".into(),
            ));
        }
        None => {}
    }
    if let Some(limit) = &scope.rate_limit
        && (!(1..=RATE_LIMIT_MAX_ADMISSIONS).contains(&limit.admissions)
            || !(1..=RATE_LIMIT_MAX_WINDOW_SECONDS).contains(&limit.window_seconds))
    {
        return Err(OrbitError::InvalidInput(format!(
            "application scope rate limit needs 1..={RATE_LIMIT_MAX_ADMISSIONS} admissions per \
             1..={RATE_LIMIT_MAX_WINDOW_SECONDS} window seconds"
        )));
    }
    Ok(())
}

/// Validate a constrained operation's `fields` map. `builtin_fields` is the
/// ceiling row's field list for a built-in, `None` for a plugin tool, whose
/// `plugin_fields` are its declared input parameters.
pub(crate) fn validate_constrained_fields(
    operation: &ApplicationOperation,
    fields: &BTreeMap<String, ApplicationConstraint>,
    role: OperationRole<'_>,
    builtin_fields: Option<&[&str]>,
    plugin_fields: &[String],
) -> Result<(), OrbitError> {
    let tool = operation.tool.as_str();
    let refuse = |field: &str, reason: &str| {
        OrbitError::InvalidInput(format!(
            "application operation `{tool}` field `{field}`: {reason}"
        ))
    };
    if fields.len() > 32 {
        return Err(OrbitError::InvalidInput(format!(
            "application operation `{tool}` lists more than 32 fields"
        )));
    }
    for (field, constraint) in fields {
        if field.is_empty()
            || field.len() > 64
            || !field
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        {
            return Err(refuse(field, "invalid field name"));
        }
        match builtin_fields {
            Some(allowed) if !allowed.contains(&field.as_str()) => {
                return Err(refuse(field, "exceeds the application ceiling"));
            }
            None if !plugin_fields.iter().any(|declared| declared == field) => {
                return Err(refuse(field, "is not in the plugin tool's declared input"));
            }
            _ => {}
        }
        let required = constraint.required_form();
        if matches!(required, ApplicationConstraint::Optional(_)) {
            return Err(refuse(field, "`optional` cannot wrap `optional`"));
        }
        validate_constraint_shape(required, field, fields, role).map_err(|r| refuse(field, &r))?;
        let kind = required.kind();
        if IDENTITY_FIELDS.contains(&field.as_str()) {
            let identity_kinds = ["const", "one_of", "task", "bound", "token", "artifact_json"];
            if !identity_kinds.contains(&kind) {
                return Err(refuse(
                    field,
                    "an identity field binds to state: const, one_of, task, bound, token or artifact_json",
                ));
            }
            if kind == "token" && builtin_fields.is_some() {
                return Err(refuse(
                    field,
                    "`token` is only for plugin-internal ids Orbit cannot resolve",
                ));
            }
        }
        if kind == "text"
            && builtin_fields.is_some()
            && !BUILTIN_TEXT_FIELDS.contains(&field.as_str())
        {
            return Err(refuse(
                field,
                "`text` is allowed only on comment, note, plan and execution_summary",
            ));
        }
        if (kind == "workspace") != (field == "workspace")
            && !(field == "workspace" && kind == "const")
        {
            return Err(refuse(
                field,
                "`workspace` constrains only the `workspace` field, as `workspace` or `const`",
            ));
        }
        if kind == "transition" && !(tool == "orbit.task.update" && field == "status") {
            return Err(refuse(
                field,
                "`transition` constrains only orbit.task.update `status`",
            ));
        }
    }
    if builtin_fields.is_some_and(|allowed| allowed.contains(&"id")) && !fields.contains_key("id") {
        return Err(OrbitError::InvalidInput(format!(
            "application operation `{tool}` must constrain `id`"
        )));
    }
    validate_builtin_field_rules(tool, fields)
}

/// The structural rules of one constraint kind. Returns the refusal reason.
fn validate_constraint_shape(
    constraint: &ApplicationConstraint,
    field: &str,
    fields: &BTreeMap<String, ApplicationConstraint>,
    role: OperationRole<'_>,
) -> Result<(), String> {
    match constraint {
        ApplicationConstraint::Const(_)
        | ApplicationConstraint::Sha256(_)
        | ApplicationConstraint::Workspace(_) => Ok(()),
        ApplicationConstraint::OneOf(values) => in_range(values.len(), 1, 16, "`one_of` values"),
        ApplicationConstraint::Text(text) => {
            in_range(text.max_chars as usize, 1, 4096, "`text` max_chars")
        }
        ApplicationConstraint::Token(token) => {
            in_range(token.max_len as usize, 1, 128, "`token` max_len")
        }
        ApplicationConstraint::Task(predicate) => {
            validate_task_predicate(predicate, field, fields, role)
        }
        ApplicationConstraint::Transition(transition) => {
            in_range(transition.from.len(), 1, 16, "`transition` from statuses")?;
            for status in transition.from.iter().chain([&transition.to]) {
                known_status(status)?;
            }
            if !fields.contains_key("id") {
                return Err("`transition` needs the call's `id` field".into());
            }
            Ok(())
        }
        ApplicationConstraint::Bound(entry_field) => {
            let OperationRole::Callback { entry } = role else {
                return Err("`bound` is allowed only on a callback".into());
            };
            if !entry
                .fields
                .as_ref()
                .is_some_and(|fields| fields.contains_key(entry_field))
            {
                return Err(format!(
                    "`bound` names `{entry_field}`, which the constrained entry does not list"
                ));
            }
            Ok(())
        }
        ApplicationConstraint::ArtifactJson(artifact) => {
            let OperationRole::Callback { entry } = role else {
                return Err("`artifact_json` is allowed only on a callback".into());
            };
            artifact_path(&artifact.path)?;
            if !is_rfc6901_pointer(&artifact.pointer) {
                return Err("`artifact_json` pointer must be an RFC 6901 pointer".into());
            }
            let entry_binds_path = entry.fields.as_ref().is_some_and(|fields| {
                fields.values().any(|c| {
                    matches!(c.required_form(), ApplicationConstraint::Task(p)
                        if p.artifact_sha256.as_ref().is_some_and(|d| d.path == artifact.path))
                })
            });
            if !entry_binds_path {
                return Err(
                    "`artifact_json` needs an entry `task` predicate binding that artifact's digest"
                        .into(),
                );
            }
            Ok(())
        }
        ApplicationConstraint::SubsetOf(values) => {
            in_range(values.len(), 1, 32, "`subset_of` values")?;
            let distinct: BTreeSet<_> = values.iter().collect();
            if distinct.len() != values.len() || values.iter().any(String::is_empty) {
                return Err("`subset_of` values must be distinct, non-empty strings".into());
            }
            Ok(())
        }
        ApplicationConstraint::Optional(_) => Err("`optional` cannot wrap `optional`".into()),
    }
}

fn validate_task_predicate(
    predicate: &ApplicationTaskPredicate,
    field: &str,
    fields: &BTreeMap<String, ApplicationConstraint>,
    role: OperationRole<'_>,
) -> Result<(), String> {
    if predicate.tags_all.is_empty() && predicate.tags_any.is_empty() {
        return Err("a `task` predicate needs `tags_all` or `tags_any`".into());
    }
    for tags in [&predicate.tags_all, &predicate.tags_any] {
        in_range(tags.len(), 0, 16, "`task` tags")?;
        if tags.iter().any(|tag| tag.is_empty() || tag.len() > 64) {
            return Err("`task` tags must be 1..=64 characters".into());
        }
    }
    in_range(predicate.status.len(), 1, 16, "`task` statuses")?;
    for status in &predicate.status {
        known_status(status)?;
    }
    if let Some(digest) = &predicate.artifact_sha256 {
        if matches!(role, OperationRole::Callback { .. }) {
            return Err("`artifact_sha256` is allowed only on an entry".into());
        }
        artifact_path(&digest.path)?;
        if !matches!(
            fields.get(&digest.field),
            Some(ApplicationConstraint::Sha256(_))
        ) {
            return Err(format!(
                "`artifact_sha256` field `{}` must itself be constrained `sha256`",
                digest.field
            ));
        }
    }
    if predicate.pin && (matches!(role, OperationRole::Entry) || field != "id") {
        return Err("`pin` is allowed only on a callback `id`".into());
    }
    Ok(())
}

/// Tool-specific ceilings beyond the field list.
fn validate_builtin_field_rules(
    tool: &str,
    fields: &BTreeMap<String, ApplicationConstraint>,
) -> Result<(), OrbitError> {
    let refuse = |field: &str, reason: String| {
        OrbitError::InvalidInput(format!(
            "application operation `{tool}` field `{field}`: {reason}"
        ))
    };
    match tool {
        "orbit.task.update" => {
            let Some(status) = fields.get("status") else {
                return Ok(());
            };
            match status.required_form() {
                ApplicationConstraint::Transition(transition) => {
                    if !APPLICATION_TRANSITION_TASK_STATUSES.contains(&transition.to.as_str()) {
                        return Err(refuse(
                            "status",
                            format!("`{}` exceeds the application status ceiling", transition.to),
                        ));
                    }
                    if transition.to == "backlog"
                        && transition.from.iter().any(|from| {
                            !APPLICATION_BACKLOG_TRANSITION_SOURCES.contains(&from.as_str())
                        })
                    {
                        return Err(refuse(
                            "status",
                            "`backlog` is reachable only from review or in-progress; approving \
                             work for dispatch is never application authority"
                                .into(),
                        ));
                    }
                    Ok(())
                }
                ApplicationConstraint::Const(value) => {
                    direct_status(value).map_err(|reason| refuse("status", reason))
                }
                ApplicationConstraint::OneOf(values) => values
                    .iter()
                    .try_for_each(direct_status)
                    .map_err(|reason| refuse("status", reason)),
                other => Err(refuse(
                    "status",
                    format!("`{}` cannot constrain a status", other.kind()),
                )),
            }
        }
        "orbit.task.show" => match fields
            .get("fields")
            .map(ApplicationConstraint::required_form)
        {
            None => Ok(()),
            Some(ApplicationConstraint::SubsetOf(values))
                if values
                    .iter()
                    .all(|value| APPLICATION_TASK_SHOW_FIELDS.contains(&value.as_str())) =>
            {
                Ok(())
            }
            Some(_) => Err(refuse(
                "fields",
                "must be `subset_of` the application task.show fields".into(),
            )),
        },
        "orbit.task.artifact.get" => match fields.get("path") {
            Some(ApplicationConstraint::Const(Value::String(path))) if !path.is_empty() => Ok(()),
            _ => Err(refuse("path", "must be a non-empty `const` string".into())),
        },
        "orbit.auto_task.add" => {
            for required in ["name", "schedule", "template"] {
                if !matches!(
                    fields.get(required),
                    Some(ApplicationConstraint::ArtifactJson(_))
                ) {
                    return Err(refuse(
                        required,
                        "must be `artifact_json` bound to the approved artifact".into(),
                    ));
                }
            }
            for (field, constraint) in fields {
                if field == "workspace" {
                    continue;
                }
                if !matches!(constraint, ApplicationConstraint::ArtifactJson(_)) {
                    return Err(refuse(
                        field,
                        "must be `artifact_json`; an add callback binds the artifact, not a caller value"
                            .into(),
                    ));
                }
            }
            Ok(())
        }
        "orbit.auto_task.update" => {
            if !matches!(
                fields.get("name"),
                Some(ApplicationConstraint::ArtifactJson(_))
            ) {
                return Err(refuse(
                    "name",
                    "must be `artifact_json` bound to the approved definition".into(),
                ));
            }
            if let Some(enabled) = fields.get("enabled") {
                let boolean = match enabled.required_form() {
                    ApplicationConstraint::Const(Value::Bool(_)) => true,
                    ApplicationConstraint::OneOf(values)
                        if !values.is_empty() && values.iter().all(Value::is_boolean) =>
                    {
                        true
                    }
                    _ => false,
                };
                if !boolean {
                    return Err(refuse(
                        "enabled",
                        "must be a boolean `const` or `one_of`".into(),
                    ));
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn direct_status(value: &Value) -> Result<(), String> {
    match value.as_str() {
        Some(status) if APPLICATION_DIRECT_TASK_STATUSES.contains(&status) => Ok(()),
        _ => Err(
            "`const` and `one_of` statuses may name only done, rejected and archived; other \
             targets need a `transition`"
                .into(),
        ),
    }
}

fn known_status(status: &str) -> Result<(), String> {
    if TASK_STATUSES.contains(&status) {
        Ok(())
    } else {
        Err(format!("`{status}` is not a task status"))
    }
}

fn artifact_path(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.len() > 256
        || path.starts_with('/')
        || path
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err("artifact paths are relative, normalized and at most 256 bytes".into());
    }
    Ok(())
}

/// RFC 6901 syntax: the empty root pointer, or `/`-separated reference tokens
/// in which every `~` begins `~0` or `~1`. Only the syntax is checked; the
/// pointer is never evaluated against artifact bytes here.
fn is_rfc6901_pointer(pointer: &str) -> bool {
    pointer.is_empty()
        || pointer.strip_prefix('/').is_some_and(|tokens| {
            tokens.split('/').all(|token| {
                token
                    .split('~')
                    .skip(1)
                    .all(|escape| escape.starts_with(['0', '1']))
            })
        })
}

fn in_range(len: usize, min: usize, max: usize, what: &str) -> Result<(), String> {
    if (min..=max).contains(&len) {
        Ok(())
    } else {
        Err(format!("{what} must number {min}..={max}"))
    }
}

impl RuntimeKernel {
    /// Check one consented operation against the live plugin install or the
    /// built-in ceiling. Consent and every admission run it, so a stale
    /// plugin digest refuses later calls.
    pub(super) fn validate_application_operation(
        &self,
        operation: &ApplicationOperation,
        role: OperationRole<'_>,
    ) -> Result<(), OrbitError> {
        let keys: Vec<&str> = match (&operation.input, &operation.fields) {
            (Some(input), None) => input
                .as_object()
                .ok_or_else(|| {
                    OrbitError::InvalidInput(
                        "application operation input must be an exact JSON object".into(),
                    )
                })?
                .keys()
                .map(String::as_str)
                .collect(),
            (None, Some(fields)) => fields.keys().map(String::as_str).collect(),
            _ => {
                return Err(OrbitError::InvalidInput(
                    "application operation needs exactly one of `input` or `fields`".into(),
                ));
            }
        };
        match self.tool_registry().plugin_binding(&operation.tool) {
            Some(binding) => {
                let expected = operation.plugin.as_ref().ok_or_else(|| {
                    OrbitError::InvalidInput(
                        "plugin consent must name its installed manifest digest".into(),
                    )
                })?;
                if binding.provenance.name != expected.name
                    || binding.provenance.manifest_digest != expected.manifest_digest
                {
                    return Err(OrbitError::CapabilityDenied(
                        "plugin digest consent is stale".into(),
                    ));
                }
                let installed = self
                    .stores()
                    .plugins()
                    .get_plugin(&expected.name)?
                    .ok_or_else(|| {
                        OrbitError::CapabilityDenied("consented plugin is not installed".into())
                    })?;
                crate::plugin::grants::verify_install_path(&self.global_root(), &installed)
                    .map_err(OrbitError::CapabilityDenied)?;
                let loaded =
                    orbit_tools::plugin::load_plugin_dir(Path::new(&installed.install_path))
                        .map_err(|_| {
                            OrbitError::CapabilityDenied(
                                "consented plugin manifest cannot be verified".into(),
                            )
                        })?;
                if !installed.enabled || loaded.manifest_digest != expected.manifest_digest {
                    return Err(OrbitError::CapabilityDenied(
                        "plugin digest consent is stale or plugin disabled".into(),
                    ));
                }
                if let Some(fields) = &operation.fields {
                    let declared: Vec<String> = self
                        .tool_registry()
                        .get_schema(&operation.tool)
                        .map(|schema| schema.parameters.into_iter().map(|p| p.name).collect())
                        .unwrap_or_default();
                    validate_constrained_fields(operation, fields, role, None, &declared)?;
                }
            }
            None if operation.plugin.is_none() => {
                // The catalogue's Operator floors outrank any consent.
                if application_operator_floored(&operation.tool, keys.iter().copied()) {
                    return Err(OrbitError::CapabilityDenied(format!(
                        "`{}` with these inputs requires a host operator and is never application authority",
                        operation.tool
                    )));
                }
                let ceiling = application_operation_ceiling(&operation.tool).ok_or_else(|| {
                    OrbitError::CapabilityDenied(
                        "this built-in operation does not support application consent".into(),
                    )
                })?;
                match (&operation.input, &operation.fields) {
                    (Some(input), _) => {
                        if ceiling.exact_fields.is_empty() {
                            return Err(OrbitError::InvalidInput(format!(
                                "`{}` admits only constrained application operations",
                                operation.tool
                            )));
                        }
                        if keys.iter().any(|key| !ceiling.exact_fields.contains(key))
                            || input
                                .get("id")
                                .and_then(Value::as_str)
                                .is_none_or(str::is_empty)
                        {
                            return Err(OrbitError::InvalidInput("application task decisions admit only id, status/answer, note, comment and workspace".into()));
                        }
                        if operation.tool == "orbit.task.update"
                            && input.get("status").is_some_and(|status| {
                                !status
                                    .as_str()
                                    .is_some_and(|s| APPLICATION_EXACT_TASK_STATUSES.contains(&s))
                            })
                        {
                            return Err(OrbitError::InvalidInput("application task updates support approval, completion or dismissal statuses only".into()));
                        }
                    }
                    (None, Some(fields)) => {
                        let admitted = match role {
                            OperationRole::Entry => ceiling.constrained_entry,
                            OperationRole::Callback { .. } => ceiling.constrained_callback,
                        };
                        if !admitted {
                            return Err(OrbitError::InvalidInput(format!(
                                "`{}` exceeds the application ceiling as a constrained {}",
                                operation.tool,
                                match role {
                                    OperationRole::Entry => "entry",
                                    OperationRole::Callback { .. } => "callback",
                                }
                            )));
                        }
                        validate_constrained_fields(
                            operation,
                            fields,
                            role,
                            Some(ceiling.constrained_fields),
                            &[],
                        )?;
                    }
                    (None, None) => {
                        return Err(OrbitError::InvalidInput(
                            "application operation needs exactly one of `input` or `fields`".into(),
                        ));
                    }
                }
            }
            _ => {
                return Err(OrbitError::CapabilityDenied(
                    "this built-in operation does not support application consent".into(),
                ));
            }
        }
        Ok(())
    }
}
