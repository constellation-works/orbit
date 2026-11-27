//! The notice registry flows: translate canonical tool input into the
//! notice use cases. Every decision is the application layer's.

use orbit_common::governance::notice::{NoticeVerb, notice_list_limit};
use orbit_common::governance::operation::OperationInput;
use orbit_common::{NoticeRefusalCode, OrbitError};
use orbit_types::record::{AuthorClass, NoticeEvidence, NoticeKind, NoticeScope};
use orbit_types::tool::McpCapability;
use serde_json::Value;

use super::{
    NoticeCaller, NoticeListFilter, NoticePostRawFields, NoticePostRequest, NoticeSelector,
    NoticeStatusFilter, close, post, read,
};
use crate::OrbitRuntime;
use crate::app::tool_call::Caller;
use crate::runtime::authorization::resolved_caller_capabilities;

fn invalid(message: impl Into<String>) -> OrbitError {
    OrbitError::notice_refused(NoticeRefusalCode::InvalidField, message)
}

fn to_json<T: serde::Serialize>(value: &T) -> Result<Value, OrbitError> {
    serde_json::to_value(value)
        .map_err(|error| OrbitError::Execution(format!("serialize notice: {error}")))
}

/// Run one notice verb. The tool entry checked `input` against the verb's
/// spec; `caller` is the session and task scope it resolved.
pub(super) fn dispatch(
    runtime: &OrbitRuntime,
    verb: NoticeVerb,
    input: &OperationInput,
    caller: &Caller,
) -> Result<Value, OrbitError> {
    let session_context = &caller.session;
    let notice_caller = || -> Result<NoticeCaller, OrbitError> {
        Ok(NoticeCaller {
            session: session_context.clone(),
            scope_task_id: caller.task_id.clone(),
            scope_run_id: caller.run_id.clone(),
            orchestrator: input.optional_string("orchestrator")?,
            model: input
                .optional_string("model")?
                .or_else(|| caller.model.clone()),
        })
    };
    match verb {
        NoticeVerb::Post => to_json(&post::post_notice(
            runtime,
            &notice_caller()?,
            post_request(input)?,
        )?),
        NoticeVerb::Close => {
            let selector = match (
                input.optional_string("id")?,
                input.optional_string("dedupe_key")?,
            ) {
                (Some(id), None) => NoticeSelector::Id(id),
                (None, Some(key)) => NoticeSelector::DedupeKey(key),
                _ => return Err(invalid("pass exactly one of `id` or `dedupe_key`")),
            };
            to_json(&close::close_notice(
                runtime,
                &notice_caller()?,
                selector,
                &input.optional_raw_string("reason")?.unwrap_or_default(),
            )?)
        }
        NoticeVerb::List => to_json(&read::list_notices(runtime, &list_filter(input)?)?),
        NoticeVerb::Show => {
            let operator = session_context.worker_invocation.is_none()
                && resolved_caller_capabilities(runtime.env(), session_context)
                    .contains(&McpCapability::Operator);
            to_json(&read::show_notice(
                runtime,
                &input.required_string("id")?,
                operator,
            )?)
        }
    }
}

fn parse_kind(value: &str) -> Result<NoticeKind, OrbitError> {
    NoticeKind::parse(value).ok_or_else(|| invalid(format!("`kind` must be {}", NoticeKind::NAMES)))
}

fn parse_scope(value: &str) -> Result<NoticeScope, OrbitError> {
    NoticeScope::parse(value)
        .ok_or_else(|| invalid(format!("`scope` must be {}", NoticeScope::NAMES)))
}

fn post_request(input: &OperationInput) -> Result<NoticePostRequest, OrbitError> {
    let evidence = match input.as_value().get("evidence") {
        None | Some(Value::Null) => Vec::new(),
        Some(value) => serde_json::from_value::<Vec<NoticeEvidence>>(value.clone())
            .map_err(|_| invalid("`evidence` is a list of {\"type\", \"value\"} objects"))?,
    };
    NoticePostRequest::parse_raw(NoticePostRawFields {
        kind: Some(input.required_string("kind")?),
        title: input.required_string("title")?,
        body: input.optional_raw_string("body")?,
        scope: input.optional_string("scope")?,
        dedupe_key: input.optional_string("dedupe_key")?,
        linked_task: input.optional_string("linked_task")?,
        close_on_linked_done: input.optional_bool("close_on_linked_done")?,
        evidence,
        hosts: input.optional_string_list("hosts")?.unwrap_or_default(),
        expires_in_hours: input
            .optional_count("expires_in_hours")?
            .map(|hours| u64::try_from(hours).unwrap_or(u64::MAX)),
        expires_at: input.optional_string("expires_at")?,
        blank_means_absent: false,
    })
}

fn list_filter(input: &OperationInput) -> Result<NoticeListFilter, OrbitError> {
    let defaults = NoticeListFilter::default();
    let all_workspaces = input.optional_bool("all")?.unwrap_or(false);
    // An owner-local aggregate read (`all: true`) without a limit returns
    // every match; the aggregating caller limits once after the merge.
    let limit = match notice_list_limit(input)? {
        Some(limit) => Some(limit),
        None if all_workspaces => None,
        None => defaults.limit,
    };
    Ok(NoticeListFilter {
        status: match input.optional_string("status")? {
            Some(status) => NoticeStatusFilter::parse(&status)
                .ok_or_else(|| invalid("`status` must be open, closed, expired or all"))?,
            None => defaults.status,
        },
        include_global: input
            .optional_bool("include_global")?
            .unwrap_or(defaults.include_global),
        all_workspaces,
        scope: input
            .optional_string("scope")?
            .map(|scope| parse_scope(&scope))
            .transpose()?,
        kinds: input
            .optional_string_list("kind")?
            .unwrap_or_default()
            .iter()
            .map(|kind| parse_kind(kind))
            .collect::<Result<_, _>>()?,
        linked_task: input.optional_string("linked_task")?,
        author_class: input
            .optional_string("author_class")?
            .map(|class| {
                AuthorClass::parse(&class)
                    .filter(|class| *class != AuthorClass::System)
                    .ok_or_else(|| {
                        invalid("`author_class` must be operator, orchestrator or agent")
                    })
            })
            .transpose()?,
        host: input.optional_string("host")?,
        ended_since: None,
        limit,
    })
}
