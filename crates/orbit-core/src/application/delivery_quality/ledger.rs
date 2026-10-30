//! Small per-unit receipts. Raw evidence and replies use service retention.
use super::contract;
use super::contract::{DeliveryQualityIdentity, DeliveryQualityProvenance};
use crate::OrbitRuntime;
use chrono::{DateTime, Utc};
use orbit_common::{OrbitError, fs::io};
use orbit_types::assistance::{AssistanceReason, AssistanceRequest};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct QualityReceipt {
    pub schema_version: u32,
    pub id: String,
    pub unit_id: String,
    pub identity: DeliveryQualityIdentity,
    pub provenance: DeliveryQualityProvenance,
    pub policy_epoch: String,
    pub grader_adapter: String,
    pub frozen_at: DateTime<Utc>,
    pub evidence_digest: String,
    pub source_digest: String,
    pub gaps: Vec<String>,
    pub non_code: bool,
    pub work_id: Option<String>,
    pub admission_gap: Option<AssistanceReason>,
}

pub(super) fn quality_root(runtime: &OrbitRuntime) -> Result<PathBuf, OrbitError> {
    Ok(runtime
        .global_root()
        .join("state/delivery-quality/v1")
        .join(&contract::quality_digest(&runtime.workspace_id()?)?[7..]))
}
pub(super) fn quality_receipt_path(
    runtime: &OrbitRuntime,
    id: &str,
) -> Result<PathBuf, OrbitError> {
    Ok(quality_root(runtime)?.join(format!("{}.json", &contract::quality_digest(&id)?[7..])))
}
pub(super) fn quality_read<T: serde::de::DeserializeOwned>(
    path: &Path,
) -> Result<Option<T>, OrbitError> {
    let file = match io::open_read_only_no_follow(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if file.metadata()?.len() > 4 * 1024 * 1024 {
        return Err(OrbitError::Store(
            "delivery quality ledger exceeds bound".into(),
        ));
    }
    serde_json::from_reader(file)
        .map(Some)
        .map_err(|_| OrbitError::Store("invalid delivery quality ledger".into()))
}
pub(super) fn quality_write<T: Serialize>(path: &Path, value: &T) -> Result<(), OrbitError> {
    let bytes = contract::quality_encode(value)?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(OrbitError::Store(
            "delivery quality ledger exceeds bound".into(),
        ));
    }
    io::atomic_write_text(
        path,
        &String::from_utf8(bytes)
            .map_err(|_| OrbitError::Store("invalid quality encoding".into()))?,
    )?;
    Ok(())
}
pub(super) fn quality_locked<T>(
    path: &Path,
    body: impl FnOnce() -> Result<T, OrbitError>,
) -> Result<T, OrbitError> {
    io::with_exclusive_file_lock_options(
        path,
        "delivery quality",
        io::FileLockOptions {
            timeout: std::time::Duration::from_millis(50),
            ..Default::default()
        },
        body,
    )
}

pub(super) fn quality_prepared_request(
    request: AssistanceRequest,
) -> Result<(AssistanceRequest, bool), OrbitError> {
    let before = contract::quality_encode(&request)?;
    let prepared = crate::application::assistance::prepare_assistance_evidence(request, &[])
        .map_err(|_| OrbitError::InvalidInput("delivery evidence redaction failed".into()))?;
    let changed = before != contract::quality_encode(&prepared)?;
    Ok((prepared, changed))
}
