//! Crew memory tool handlers: translate tool input and the caller's trusted
//! run binding, and delegate to the runtime's crew memory operations, which
//! own the gate, the write rules and which callers may write which key.

use std::path::PathBuf;

use crate::OrbitRuntime;
use crate::application::crew_memory::{MemoryFile, MemoryToolCaller, MemoryWrite};
use crate::runtime::authorization::resolved_caller_capabilities;
use crate::runtime::run_input::managed_run_context_task_id_from_env;
use orbit_common::OrbitError;
use orbit_common::governance::memory::MemoryVerb;
use orbit_common::governance::operation::OperationVerb;
use orbit_tools::OrbitTaskScope;
use orbit_types::tool::{McpCapability, ToolSessionContext};
use serde_json::Value;

/// Who is calling, from the session's capabilities and its run binding:
/// a claimed leaf's worker binding, or the run this tool context names with
/// the task its managed environment names. Never from input.
fn memory_tool_caller(
    task_scope: &OrbitTaskScope,
    session: &ToolSessionContext,
    model: Option<&str>,
) -> MemoryToolCaller {
    let capabilities = resolved_caller_capabilities(session);
    let worker = session.worker_invocation.as_ref();
    let scope_run_id = task_scope.run_id.clone();
    let scope_task_id = task_scope.task_id.clone().or_else(|| {
        scope_run_id
            .as_ref()
            .and_then(|_| managed_run_context_task_id_from_env())
    });
    MemoryToolCaller {
        operator: worker.is_none() && capabilities.contains(&McpCapability::Operator),
        agent: capabilities.contains(&McpCapability::Agent),
        worker_task_id: worker.map(|binding| binding.task_id.clone()),
        worker_run_id: worker.map(|binding| binding.bound_run_id.clone()),
        scope_task_id,
        scope_run_id,
        model: model.map(ToOwned::to_owned),
    }
}

pub(super) fn dispatch(
    runtime: &OrbitRuntime,
    task_scope: &OrbitTaskScope,
    session: &ToolSessionContext,
    verb: MemoryVerb,
    input: Value,
    model: Option<&str>,
) -> Result<Value, OrbitError> {
    let input = verb.spec().parse_input(input)?;
    let reason = || input.optional_string("reason");
    let caller = || memory_tool_caller(task_scope, session, model);
    match verb {
        MemoryVerb::Show => {
            let revision = input
                .optional_count("revision")?
                .map(|revision| {
                    u32::try_from(revision).map_err(|_| {
                        OrbitError::InvalidInput(format!("revision {revision} is out of range"))
                    })
                })
                .transpose()?;
            runtime.crew_memory_tool_show(
                &caller(),
                input.optional_string("key")?.as_deref(),
                revision,
            )
        }
        MemoryVerb::History => runtime.crew_memory_history(&input.required_string("key")?),
        MemoryVerb::Export => runtime.crew_memory_export(
            input
                .optional_raw_string("dir")?
                .map(PathBuf::from)
                .as_deref(),
        ),
        MemoryVerb::Write => {
            let files = match input.as_value().get("files") {
                None | Some(Value::Null) => Vec::new(),
                Some(files) => {
                    serde_json::from_value::<Vec<MemoryFile>>(files.clone()).map_err(|error| {
                        OrbitError::InvalidInput(format!(
                            "`files` must be an array of {{path, text}} objects: {error}"
                        ))
                    })?
                }
            };
            let write = MemoryWrite {
                files,
                evidence: input.optional_string_list("evidence")?.unwrap_or_default(),
            };
            runtime.crew_memory_tool_write(
                &caller(),
                &input.required_string("key")?,
                write,
                reason()?,
            )
        }
        MemoryVerb::Reset => runtime.crew_memory_reset(&input.required_string("key")?, reason()?),
        MemoryVerb::Copy => runtime.crew_memory_copy(
            &input.required_string("from")?,
            &input.required_string("to")?,
            reason()?,
        ),
        MemoryVerb::Propose => runtime.crew_memory_tool_propose(
            &caller(),
            input.optional_raw_string("text")?.unwrap_or_default(),
            input.optional_string_list("evidence")?.unwrap_or_default(),
        ),
    }
}
