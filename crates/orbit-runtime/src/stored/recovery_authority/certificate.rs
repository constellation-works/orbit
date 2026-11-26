//! Recovery attempt reservations and immutable certificates.

use std::path::Path;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_engine::RebaseRecoveryAttemptScope;
use orbit_store::recovery_authority::{
    RecoveryAttemptRecord, RecoveryAuthorityStore, RecoveryCertificateRecord,
};
use serde_json::Value;

use super::root::{
    AUTHORITY_DB, authority_root, refuse_symlinked_authority_files, restrict_permissions,
};

/// The checkpoint field carrying the host-assigned recovery attempt.
const RECOVERY_ATTEMPT_FIELD: &str = "recovery_attempt";

/// The durable record a host writes when it completes a rebase recovery, and
/// the only evidence a later resume will accept.
#[derive(Debug)]
pub struct RecoveryAuthority {
    // Certificate and worker-binding persistence share this authority repository.
    pub(super) store: RecoveryAuthorityStore,
}

/// The identity a certificate binds. Every field is re-derived from the
/// candidate checkpoint at verification time, so a payload that was edited,
/// moved to another run, step or attempt, or pointed at another workspace no
/// longer matches the record the host wrote.
#[derive(Debug, PartialEq, Eq)]
struct CertificateBinding {
    run_id: String,
    step_id: String,
    /// The host-assigned recovery attempt; `None` only for a payload written
    /// before attempts existed, which the legacy table alone can vouch for.
    attempt: Option<i64>,
    workspace_path: String,
    head_sha: String,
    base_sha: String,
    payload_digest: String,
}

impl RecoveryAuthority {
    /// Open (creating on first use) the authority for `global_root`.
    pub fn open(global_root: &Path) -> Result<Self, OrbitError> {
        let root = authority_root(global_root)?;
        refuse_symlinked_authority_files(&root)?;

        let store = RecoveryAuthorityStore::open(&root.join(AUTHORITY_DB))?;
        restrict_permissions(&root)?;

        Ok(Self { store })
    }

    /// Reserve the next recovery attempt of `step_id` in `run_id`.
    ///
    /// The host calls this once per admitted conflict recovery, before the
    /// provider runs, and carries the returned number in memory to the
    /// checkpoint it later issues. The number is assigned here, never chosen
    /// by the caller, and each reservation supersedes every earlier one for
    /// the same run and step.
    pub fn begin_attempt(
        &self,
        run_id: &str,
        step_id: &str,
        scope: &RebaseRecoveryAttemptScope,
    ) -> Result<u64, OrbitError> {
        if [
            run_id,
            step_id,
            &scope.workspace_path,
            &scope.head_sha_before,
            &scope.target_base_sha,
        ]
        .iter()
        .any(|value| value.is_empty())
        {
            return Err(OrbitError::InvalidInput(
                "rebase recovery attempt needs a run, step, workspace, original HEAD and pinned base"
                    .to_string(),
            ));
        }
        let record = RecoveryAttemptRecord {
            workspace_path: scope.workspace_path.clone(),
            head_sha_before: scope.head_sha_before.clone(),
            target_base_sha: scope.target_base_sha.clone(),
        };
        self.store.write("reserve rebase recovery attempt", |tx| {
            let attempt = tx.latest_reservation(run_id, step_id)?.unwrap_or(0) + 1;
            tx.insert_reservation(run_id, step_id, attempt, &record, &Utc::now().to_rfc3339())?;
            u64::try_from(attempt).map_err(|_| {
                OrbitError::Execution("rebase recovery attempt is out of range".to_string())
            })
        })
    }

    /// Record the host's completion of one reserved attempt of `step_id` in
    /// `run_id`; the attempt is the payload's `recovery_attempt`.
    ///
    /// Certificates are immutable: re-issuing the identical payload succeeds so
    /// a retried write is harmless, while different evidence for an attempt
    /// already certified is refused rather than replacing the accepted record.
    /// An attempt the host never reserved, one a later reservation superseded,
    /// and evidence for another workspace, original HEAD or pinned base than
    /// the reservation named are refused before anything is written.
    pub fn issue(&self, run_id: &str, step_id: &str, checkpoint: &Value) -> Result<(), OrbitError> {
        let binding = CertificateBinding::derive(run_id, step_id, checkpoint)?;
        let Some(attempt) = binding.attempt else {
            return Err(OrbitError::Execution(format!(
                "rebase recovery evidence for run {run_id} step `{step_id}` names no host-reserved \
                 attempt"
            )));
        };
        let record = binding.into_record();

        self.store
            .write("record rebase recovery certificate", |tx| {
                let Some(scope) = tx.reservation(run_id, step_id, attempt)? else {
                    return Err(OrbitError::PolicyDenied(format!(
                        "rebase recovery attempt {attempt} for run {run_id} step `{step_id}` was \
                         not reserved by this host"
                    )));
                };
                if tx.latest_reservation(run_id, step_id)? != Some(attempt) {
                    return Err(OrbitError::PolicyDenied(format!(
                        "rebase recovery attempt {attempt} for run {run_id} step `{step_id}` was \
                         superseded by a later attempt"
                    )));
                }
                if scope.workspace_path != record.workspace_path
                    || bound_field(checkpoint, "head_sha_before")? != scope.head_sha_before
                    || bound_field(checkpoint, "target_base_sha")? != scope.target_base_sha
                {
                    return Err(OrbitError::PolicyDenied(format!(
                        "rebase recovery evidence for run {run_id} step `{step_id}` attempt \
                         {attempt} does not describe the reserved workspace, original HEAD and \
                         pinned base"
                    )));
                }

                if let Some(recorded) = tx.certificate(run_id, step_id, attempt)? {
                    if recorded == record {
                        return Ok(());
                    }
                    return Err(OrbitError::Execution(format!(
                        "rebase recovery for run {run_id} step `{step_id}` attempt {attempt} is \
                         already certified with different evidence; the accepted certificate is \
                         immutable"
                    )));
                }

                tx.insert_certificate(&record, attempt, &Utc::now().to_rfc3339())
            })
    }

    /// Whether `checkpoint` is exactly the current evidence this host
    /// certified for `run_id` / `step_id`.
    ///
    /// Current means no later attempt of the same step is certified: an
    /// earlier attempt's record stays intact, but it no longer vouches for
    /// anything once a newer recovery completed. A payload without an attempt
    /// predates attempts and is checked against the legacy record, under the
    /// same rule. An absent record means the checkpoint predates the authority
    /// boundary or was authored by something other than the host; both are
    /// unusable, and neither is blessed here.
    pub fn verify(
        &self,
        run_id: &str,
        step_id: &str,
        checkpoint: &Value,
    ) -> Result<bool, OrbitError> {
        let Ok(binding) = CertificateBinding::derive(run_id, step_id, checkpoint) else {
            return Ok(false);
        };
        let latest = self.store.latest_certified_attempt(run_id, step_id)?;
        let recorded = match binding.attempt {
            Some(attempt) if latest == Some(attempt) => {
                self.store.certificate(run_id, step_id, attempt)?
            }
            None if latest.is_none() => self.store.legacy_certificate(run_id, step_id)?,
            Some(_) | None => return Ok(false),
        };
        Ok(recorded == Some(binding.into_record()))
    }

    /// Every attempt reserved for `run_id` / `step_id`, oldest first.
    pub fn attempts(
        &self,
        run_id: &str,
        step_id: &str,
    ) -> Result<Vec<RebaseRecoveryAttemptScope>, OrbitError> {
        Ok(self
            .store
            .list_attempts(run_id, step_id)?
            .into_iter()
            .map(|row| RebaseRecoveryAttemptScope {
                workspace_path: row.workspace_path,
                head_sha_before: row.head_sha_before,
                target_base_sha: row.target_base_sha,
            })
            .collect())
    }
}

impl CertificateBinding {
    fn derive(run_id: &str, step_id: &str, checkpoint: &Value) -> Result<Self, OrbitError> {
        // The payload names its own run and step. Certifying a payload under a
        // different identity would leave the two disagreeing, so refuse it at
        // the point the record is derived instead of storing the mismatch.
        let payload_run_id = bound_field(checkpoint, "run_id")?;
        let payload_step_id = bound_field(checkpoint, "step_id")?;
        if payload_run_id != run_id || payload_step_id != step_id {
            return Err(OrbitError::Execution(format!(
                "rebase recovery evidence names run {payload_run_id} step `{payload_step_id}`, \
                 not run {run_id} step `{step_id}`"
            )));
        }

        Ok(Self {
            run_id: run_id.to_string(),
            step_id: step_id.to_string(),
            attempt: recovery_attempt(checkpoint)?,
            workspace_path: bound_field(checkpoint, "workspace_path")?,
            head_sha: bound_field(checkpoint, "head_sha")?,
            base_sha: bound_field(checkpoint, "base_sha")?,
            payload_digest: payload_digest(checkpoint),
        })
    }

    fn into_record(self) -> RecoveryCertificateRecord {
        RecoveryCertificateRecord {
            run_id: self.run_id,
            step_id: self.step_id,
            attempt: self.attempt,
            workspace_path: self.workspace_path,
            head_sha: self.head_sha,
            base_sha: self.base_sha,
            payload_digest: self.payload_digest,
        }
    }
}

/// The payload's `recovery_attempt`: absent for a pre-attempt payload, and
/// otherwise a positive integer the host reserved.
fn recovery_attempt(checkpoint: &Value) -> Result<Option<i64>, OrbitError> {
    let Some(value) = checkpoint.get(RECOVERY_ATTEMPT_FIELD) else {
        return Ok(None);
    };
    value
        .as_i64()
        .filter(|attempt| *attempt >= 1)
        .map(Some)
        .ok_or_else(|| {
            OrbitError::Execution(format!(
                "rebase recovery evidence has an invalid `{RECOVERY_ATTEMPT_FIELD}`"
            ))
        })
}

fn bound_field(checkpoint: &Value, field: &str) -> Result<String, OrbitError> {
    checkpoint
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            OrbitError::Execution(format!(
                "rebase recovery evidence is missing its `{field}` binding"
            ))
        })
}

/// BLAKE3 over a key-sorted rendering of the whole payload, so any edit to any
/// field — including ones the bound columns do not name, such as `task_ids` or
/// `rewritten` — changes the digest.
// Shared with the legacy-certificate security fixture to seed the persisted format.
pub(super) fn payload_digest(checkpoint: &Value) -> String {
    blake3::hash(orbit_common::protocol::sorted_keys_json_string(checkpoint).as_bytes())
        .to_hex()
        .to_string()
}
