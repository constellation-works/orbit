//! Crew memory's run-bound flows: translate tool input and the caller's
//! trusted run binding, and delegate to the runtime's crew memory
//! operations, which own the gate, the write rules and which runs may call
//! each verb.

use orbit_common::OrbitError;
use orbit_common::governance::memory::MemoryVerb;
use orbit_common::governance::operation::OperationInput;
use serde_json::Value;

use super::{MemoryFile, MemoryToolCaller, MemoryWrite, agent};
use crate::OrbitRuntime;
use crate::app::tool_call::Caller;
use crate::runtime::run_input::managed_run_context_task_id_from_env;

/// Who is calling, from the session's run binding: a claimed leaf's worker
/// binding, or the run this tool context names with the task its managed
/// environment names. Never from input.
fn memory_tool_caller(process_env: &orbit_common::Env, caller: &Caller) -> MemoryToolCaller {
    let worker = caller.session.worker_invocation.as_ref();
    let scope_run_id = caller.run_id.clone();
    let scope_task_id = caller.task_id.clone().or_else(|| {
        scope_run_id
            .as_ref()
            .and_then(|_| managed_run_context_task_id_from_env(process_env))
    });
    MemoryToolCaller {
        worker_task_id: worker.map(|binding| binding.task_id.clone()),
        worker_run_id: worker.map(|binding| binding.bound_run_id.clone()),
        scope_task_id,
        scope_run_id,
        model: caller.model.clone(),
    }
}

/// Run one memory verb. The tool entry checked `input` against the verb's
/// spec; `caller` is the session and run binding it resolved.
pub(super) fn dispatch(
    runtime: &OrbitRuntime,
    verb: MemoryVerb,
    input: &OperationInput,
    caller: &Caller,
) -> Result<Value, OrbitError> {
    let reason = || input.optional_string("reason");
    let memory_caller = || memory_tool_caller(runtime.env(), caller);
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
            agent::crew_memory_tool_show(
                runtime,
                &memory_caller(),
                input.optional_string("key")?.as_deref(),
                revision,
            )
        }
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
            agent::crew_memory_tool_write(
                runtime,
                &memory_caller(),
                &input.required_string("key")?,
                write,
                reason()?,
            )
        }
        MemoryVerb::Propose => agent::crew_memory_tool_propose(
            runtime,
            &memory_caller(),
            input.optional_raw_string("text")?.unwrap_or_default(),
            input.optional_string_list("evidence")?.unwrap_or_default(),
        ),
    }
}
