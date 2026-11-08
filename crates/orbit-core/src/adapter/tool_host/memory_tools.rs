//! Crew memory tool handlers: translate tool input and delegate to the
//! runtime's crew memory operations, which own the gate and the write rules.

use std::path::PathBuf;

use crate::OrbitRuntime;
use crate::application::crew_memory::{MemoryFile, MemoryWrite};
use orbit_common::OrbitError;
use orbit_common::governance::memory::MemoryVerb;
use orbit_common::governance::operation::OperationVerb;
use serde_json::Value;

pub(super) fn dispatch(
    runtime: &OrbitRuntime,
    verb: MemoryVerb,
    input: Value,
) -> Result<Value, OrbitError> {
    let input = verb.spec().parse_input(input)?;
    let reason = || input.optional_string("reason");
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
            runtime.crew_memory_show(input.optional_string("key")?.as_deref(), revision)
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
            runtime.crew_memory_write(&input.required_string("key")?, write, reason()?)
        }
        MemoryVerb::Reset => runtime.crew_memory_reset(&input.required_string("key")?, reason()?),
        MemoryVerb::Copy => runtime.crew_memory_copy(
            &input.required_string("from")?,
            &input.required_string("to")?,
            reason()?,
        ),
    }
}
