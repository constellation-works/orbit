//! The dashboard's automation reads: one consumer's persisted state and the
//! evidence it accepted for a batch.

use orbit_common::OrbitError;
use serde_json::Value;

use super::Consumer;
use crate::OrbitRuntime;
use crate::app::tool_fields::serialize_error;
use crate::application::automation::consumer_key;

pub(super) fn state(
    runtime: &OrbitRuntime,
    consumer: &Consumer,
) -> Result<Option<Value>, OrbitError> {
    let key = consumer_key(runtime, &consumer.kind, &consumer.name)?;
    runtime
        .sqlite_store()?
        .read()
        .and_then(|snapshot| snapshot.automation().state(&key))?
        .map(|state| serde_json::to_value(state).map_err(serialize_error("automation state")))
        .transpose()
}

pub(super) fn evidence(
    runtime: &OrbitRuntime,
    consumer: &Consumer,
    batch: &str,
) -> Result<Option<Vec<u8>>, OrbitError> {
    let key = consumer_key(runtime, &consumer.kind, &consumer.name)?;
    Ok(runtime
        .sqlite_store()?
        .read()
        .and_then(|snapshot| snapshot.automation().receipt(&key, batch))?
        .map(|receipt| receipt.evidence))
}
