//! Private seal ledger. Completion time is not a column on the assistance row.
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use orbit_common::{OrbitError, fs::io};
use orbit_types::assistance;
use serde::{Deserialize, Serialize};

use crate::OrbitRuntime;

fn digest_file_name(digest: &str) -> &str {
    digest.strip_prefix("sha256:").unwrap_or(digest)
}

pub(super) fn ledger_json_error(_: serde_json::Error) -> OrbitError {
    OrbitError::Store("precedent decision receipt encoding is invalid".into())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PrecedentDecisionReceipt {
    pub schema_version: u32,
    pub request_id: String,
    pub binding_key: String,
    pub category: String,
    pub raised_at: DateTime<Utc>,
    pub policy_epoch: String,
    pub source_digest: String,
    pub work_id: Option<String>,
    pub sealed_at: Option<DateTime<Utc>>,
    pub admission_gap: Option<String>,
}

pub(super) fn receipt_path(
    runtime: &OrbitRuntime,
    request_id: &str,
) -> Result<PathBuf, OrbitError> {
    let workspace = assistance::assistance_digest(runtime.workspace_id()?.as_bytes());
    let subject = assistance::assistance_digest(request_id.as_bytes());
    Ok(receipt_dir(runtime, &workspace)?.join(format!("{}.json", digest_file_name(&subject))))
}

pub(super) fn receipt_dir(
    runtime: &OrbitRuntime,
    workspace_digest: &str,
) -> Result<PathBuf, OrbitError> {
    let workspace = digest_file_name(workspace_digest);
    Ok(runtime
        .global_root()
        .join("state/precedent-decision/v1")
        .join(workspace))
}

pub(super) fn read_receipt(path: &Path) -> Result<Option<PrecedentDecisionReceipt>, OrbitError> {
    let file = match io::open_read_only_no_follow(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if file.metadata()?.len() > 8 * 1024 * 1024 {
        return Err(OrbitError::Store(
            "precedent decision receipt exceeds its bound".into(),
        ));
    }
    let receipt: PrecedentDecisionReceipt =
        serde_json::from_reader(file).map_err(ledger_json_error)?;
    if receipt.schema_version != 1 {
        return Err(OrbitError::Store(
            "unsupported precedent decision receipt".into(),
        ));
    }
    Ok(Some(receipt))
}

pub(super) fn write_receipt(
    path: &Path,
    receipt: &PrecedentDecisionReceipt,
) -> Result<(), OrbitError> {
    io::atomic_write_text(
        path,
        &serde_json::to_string(receipt).map_err(ledger_json_error)?,
    )?;
    Ok(())
}

pub(super) fn with_receipt<T>(
    path: &Path,
    body: impl FnOnce() -> Result<T, OrbitError>,
) -> Result<T, OrbitError> {
    io::with_exclusive_file_lock_options(
        path,
        "precedent decision",
        io::FileLockOptions {
            timeout: std::time::Duration::from_millis(50),
            ..Default::default()
        },
        body,
    )
}
