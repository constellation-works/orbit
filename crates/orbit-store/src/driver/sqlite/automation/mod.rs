//! Automation records in the existing host SQLite database.

use crate::Store;
use crate::contracts::AutomationStoreBackend;
use crate::driver::sqlite::migration::FeatureMigration;
use orbit_common::OrbitError;
use orbit_types::workflow::automation::recovery::RecoveryRecord;
use orbit_types::workflow::automation::{AcceptedCoverage, AutomationState, Delivery};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

pub(crate) fn initialize(store: &Store) -> Result<(), OrbitError> {
    store.apply_feature_migrations(
        "automation",
        &[
            FeatureMigration::new(1, "consumer_checkpoints_and_coverage", |conn| {
                conn.execute_batch("CREATE TABLE automation_consumers (consumer TEXT PRIMARY KEY, generation INTEGER NOT NULL, state_json TEXT NOT NULL);
            CREATE TABLE automation_coverage (batch_id TEXT PRIMARY KEY, consumer TEXT NOT NULL, batch_json TEXT NOT NULL, receipt_json TEXT NOT NULL, accepted_at TEXT NOT NULL);
            CREATE INDEX automation_coverage_consumer ON automation_coverage(consumer, accepted_at);
            CREATE TABLE automation_delivery_intents (record_id TEXT PRIMARY KEY, repository TEXT NOT NULL, branch TEXT NOT NULL, delivery_json TEXT NOT NULL);
            CREATE TABLE automation_delivery_members (repository TEXT NOT NULL, branch TEXT NOT NULL, commit_id TEXT NOT NULL, record_id TEXT NOT NULL, PRIMARY KEY(repository,branch,commit_id,record_id));
            CREATE TABLE automation_waivers (batch_id TEXT PRIMARY KEY, consumer TEXT NOT NULL, batch_json TEXT NOT NULL, waiver_json TEXT NOT NULL);
            CREATE TABLE automation_job_keys (workspace_id TEXT NOT NULL, action_key TEXT NOT NULL, run_id TEXT NOT NULL, PRIMARY KEY(workspace_id,action_key));")
                    .map_err(|e| OrbitError::Store(e.to_string()))
            }),
            FeatureMigration::new(2, "retry_lineage_index", |conn| {
                conn.execute_batch(
                    "CREATE INDEX IF NOT EXISTS job_runs_retry_lineage ON job_runs(workspace_id,retry_source_run_id)",
                )
                .map_err(|error| OrbitError::Store(error.to_string()))
            }),
            FeatureMigration::new(3, "consumer_recovery_records", |conn| {
                conn.execute_batch(
                    "CREATE TABLE automation_recoveries (record_id TEXT PRIMARY KEY, consumer TEXT NOT NULL, recorded_at TEXT NOT NULL, record_json TEXT NOT NULL);
            CREATE INDEX automation_recoveries_consumer ON automation_recoveries(consumer, recorded_at);",
                )
                .map_err(|error| OrbitError::Store(error.to_string()))
            }),
        ],
    )
}

fn encode<T: serde::Serialize>(value: &T) -> Result<String, OrbitError> {
    serde_json::to_string(value).map_err(|e| OrbitError::Store(e.to_string()))
}

fn decode<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T, OrbitError> {
    serde_json::from_str(raw)
        .map_err(|e| OrbitError::Store(format!("invalid persisted automation record: {e}")))
}

impl AutomationStoreBackend for Store {
    fn automation_waive(
        &self,
        previous: &AutomationState,
        next: &AutomationState,
        waiver: &orbit_types::workflow::automation::BatchWaiver,
    ) -> Result<bool, OrbitError> {
        waivers::commit(self, previous, next, waiver)
    }

    fn automation_waivers(
        &self,
        consumer: &str,
        limit: usize,
    ) -> Result<Vec<orbit_types::workflow::automation::BatchWaiver>, OrbitError> {
        waivers::list(self, consumer, limit)
    }

    fn automation_recover(
        &self,
        previous: &AutomationState,
        next: &AutomationState,
        record: &RecoveryRecord,
    ) -> Result<bool, OrbitError> {
        recovery::commit(self, previous, next, record)
    }

    fn automation_recoveries(
        &self,
        consumer: &str,
        limit: usize,
    ) -> Result<Vec<RecoveryRecord>, OrbitError> {
        recovery::list(self, consumer, limit)
    }

    fn automation_receipt(
        &self,
        consumer: &str,
        batch: &str,
    ) -> Result<Option<AcceptedCoverage>, OrbitError> {
        self.with_read_connection(|conn| {
            let raw: Option<String> = conn
                .query_row(
                    "SELECT receipt_json FROM automation_coverage WHERE consumer=?1 AND batch_id=?2",
                    params![consumer, batch],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| OrbitError::Store(e.to_string()))?;
            raw.as_deref().map(decode).transpose()
        })
    }

    fn automation_record_delivery_intent(
        &self,
        delivery: &orbit_types::workflow::automation::Delivery,
    ) -> Result<(), OrbitError> {
        intents::record(self, delivery)
    }

    fn automation_delivery_intents(
        &self,
        repository: &str,
        branch: &str,
        commits: &[String],
    ) -> Result<Vec<orbit_types::workflow::automation::Delivery>, OrbitError> {
        intents::lookup(self, repository, branch, commits)
    }

    fn automation_state(&self, consumer: &str) -> Result<Option<AutomationState>, OrbitError> {
        self.with_read_connection(|conn| {
            let raw: Option<String> = conn
                .query_row(
                    "SELECT state_json FROM automation_consumers WHERE consumer=?1",
                    [consumer],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| OrbitError::Store(e.to_string()))?;
            raw.as_deref().map(decode).transpose()
        })
    }

    fn automation_initialize(&self, state: &AutomationState) -> Result<bool, OrbitError> {
        if state
            .members
            .as_ref()
            .is_some_and(|members| members != &Default::default())
            || state.generation != 0
            || state.baseline != state.observed
            || state.baseline != state.covered
            || state.active.is_some()
            || !state.pending.is_empty()
            || !state.waived.is_empty()
            || !state.pending_commits.is_empty()
            || !state.unresolved.is_empty()
        {
            return Err(OrbitError::InvalidInput(
                "invalid automation baseline".into(),
            ));
        }
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            tx.connection()
                .execute(
                    "INSERT OR IGNORE INTO automation_consumers VALUES (?1,0,?2)",
                    params![state.consumer, encode(state)?],
                )
                .map(|n| n == 1)
                .map_err(|e| OrbitError::Store(e.to_string()))
        })
    }

    fn automation_commit(
        &self,
        previous: &AutomationState,
        next: &AutomationState,
        receipt: Option<&AcceptedCoverage>,
    ) -> Result<bool, OrbitError> {
        validate_transition(previous, next, receipt)?;
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let conn = tx.connection();

            // The generation and the exact prior state both fence the write, so a
            // concurrent evaluation that already moved the consumer changes nothing.
            let changed = conn
                .execute(
                    "UPDATE automation_consumers SET generation=?1,state_json=?2 WHERE consumer=?3 AND generation=?4 AND state_json=?5",
                    params![
                        next.generation,
                        encode(next)?,
                        previous.consumer,
                        previous.generation,
                        encode(previous)?
                    ],
                )
                .map_err(|e| OrbitError::Store(e.to_string()))?;

            if changed == 0 {
                return Ok(false);
            }

            if let Some(receipt) = receipt {
                // Preserve shipped delivery receipt bytes; member records carry
                // only their frozen action, never the unrelated consumer inventory.
                let batch_json = if let Some(members) = &previous.members {
                    encode(&serde_json::json!({"state_member": members.active}))?
                } else {
                    encode(&previous.active)?
                };

                conn.execute(
                    "INSERT INTO automation_coverage VALUES (?1,?2,?3,?4,?5)",
                    params![
                        receipt.batch_id,
                        previous.consumer,
                        batch_json,
                        encode(receipt)?,
                        receipt.accepted_at.to_rfc3339()
                    ],
                )
                .map_err(|e| OrbitError::Store(e.to_string()))?;
            }

            Ok(true)
        })
    }

    fn automation_states(
        &self,
        prefix: &str,
        limit: usize,
    ) -> Result<Vec<AutomationState>, OrbitError> {
        self.with_read_connection(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT state_json FROM automation_consumers WHERE consumer LIKE ?1 ESCAPE '\\' ORDER BY consumer LIMIT ?2",
                )
                .map_err(|e| OrbitError::Store(e.to_string()))?;
            let pattern = format!("{}%", prefix.replace('%', "\\%").replace('_', "\\_"));
            let rows = stmt
                .query_map(params![pattern, limit.min(100)], |row| row.get::<_, String>(0))
                .map_err(|e| OrbitError::Store(e.to_string()))?;
            rows.map(|row| {
                row.map_err(|e| OrbitError::Store(e.to_string()))
                    .and_then(|raw| decode(&raw))
            })
            .collect()
        })
    }

    fn automation_receipts(
        &self,
        consumer: &str,
        limit: usize,
    ) -> Result<Vec<AcceptedCoverage>, OrbitError> {
        self.with_read_connection(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT receipt_json FROM automation_coverage WHERE consumer=?1 ORDER BY accepted_at DESC,batch_id DESC LIMIT ?2",
                )
                .map_err(|e| OrbitError::Store(e.to_string()))?;

            let rows = stmt
                .query_map(params![consumer, limit.min(100)], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(|e| OrbitError::Store(e.to_string()))?;

            rows.map(|row| decode(&row.map_err(|e| OrbitError::Store(e.to_string()))?))
                .collect()
        })
    }
}

fn validate_transition(
    previous: &AutomationState,
    next: &AutomationState,
    receipt: Option<&AcceptedCoverage>,
) -> Result<(), OrbitError> {
    let invalid = || OrbitError::InvalidInput("invalid automation checkpoint transition".into());

    if previous.consumer != next.consumer
        || previous.epoch != next.epoch
        || previous.trigger != next.trigger
        || previous.repository != next.repository
        || previous.branch != next.branch
        || previous.baseline != next.baseline
        || previous.generation.checked_add(1) != Some(next.generation)
        || next.pending.len() > 1000
        || next.pending_commits.len() > 5000
    {
        return Err(invalid());
    }

    if previous.members.is_some() || next.members.is_some() {
        return members::validate(previous, next, receipt);
    }

    // An attempt may gain retries but never swap the batch it was frozen against,
    // and it may only disappear when a receipt retires it.
    if let Some(old) = &previous.active {
        if let Some(new) = &next.active {
            if old.batch != new.batch
                || old.input_digest != new.input_digest
                || old.reissue != new.reissue
                || new.attempt < old.attempt
            {
                return Err(invalid());
            }
        } else if receipt.is_none() {
            return Err(invalid());
        }
    }

    if let Some(receipt) = receipt {
        let active = previous.active.as_ref().ok_or_else(invalid)?;
        if active.batch.id != receipt.batch_id
            || active.action_id.as_deref() != Some(&receipt.action_id)
            || active.input_digest != receipt.input_digest
            || next.active.is_some()
            || next.covered != active.batch.through_inclusive
            || previous.covered != active.batch.from_exclusive
            || receipt.evidence.is_empty()
            || receipt.submitted_by.is_empty()
        {
            return Err(invalid());
        }

        use sha2::{Digest, Sha256};

        if receipt.evidence_digest != format!("{:x}", Sha256::digest(&receipt.evidence)) {
            return Err(invalid());
        }

        // Accepting the batch retires exactly the deliveries it fully covered;
        // anything it only partly covered has to survive the checkpoint.
        let uncovered = |deliveries: &[Delivery]| {
            deliveries
                .iter()
                .filter(|delivery| {
                    !delivery
                        .commits
                        .iter()
                        .all(|sha| active.batch.commits.contains(sha))
                })
                .cloned()
                .collect::<Vec<_>>()
        };

        if next.waived != uncovered(&previous.waived) {
            return Err(invalid());
        }

        if next.pending != uncovered(&previous.pending) {
            return Err(invalid());
        }

        let retained_exclusions = previous
            .excluded
            .iter()
            .filter(|excluded| {
                !excluded
                    .delivery
                    .commits
                    .iter()
                    .all(|sha| active.batch.commits.contains(sha))
            })
            .cloned()
            .collect::<Vec<_>>();
        if next.excluded != retained_exclusions {
            return Err(invalid());
        }

        if !previous.pending_commits.starts_with(&active.batch.commits)
            || next.pending_commits != previous.pending_commits[active.batch.commits.len()..]
        {
            return Err(invalid());
        }
    } else if previous.covered != next.covered {
        // A proven-covered prefix may advance the scheduling cursor without a
        // consumer examination receipt. Observation remains append-only.
        validate_excluded_prefix_retirement(previous, next)?;
    } else {
        // Without a receipt the covered cursor stands still: observation may only
        // append commits and retain every delivery already pending.
        if previous.waived != next.waived
            || !next.pending_commits.starts_with(&previous.pending_commits)
            || previous
                .pending
                .iter()
                .any(|delivery| !next.pending.contains(delivery))
            || previous
                .excluded
                .iter()
                .any(|excluded| !next.excluded.contains(excluded))
        {
            return Err(invalid());
        }

        // A newly claimed batch must be an exact prefix of what is already pending.
        if previous.active.is_none()
            && let Some(active) = &next.active
        {
            let batch = &active.batch;
            if batch.consumer != next.consumer
                || batch.epoch != next.epoch
                || batch.repository != next.repository
                || batch.branch != next.branch
                || batch.from_exclusive != next.covered
                || batch.commits.is_empty()
                || !next.pending_commits.starts_with(&batch.commits)
                || batch.commits.last() != Some(&batch.through_inclusive.commit)
                || batch
                    .deliveries
                    .iter()
                    .any(|delivery| !next.pending.contains(delivery))
            {
                return Err(invalid());
            }
        }
    }

    Ok(())
}

fn validate_excluded_prefix_retirement(
    previous: &AutomationState,
    next: &AutomationState,
) -> Result<(), OrbitError> {
    let invalid = || OrbitError::InvalidInput("invalid automation checkpoint transition".into());

    if previous.active.is_some()
        || next.active.is_some()
        || previous.observed != next.observed
        || previous.pending != next.pending
        || previous.waived != next.waived
        || previous.pending_commits.len() <= next.pending_commits.len()
        || !previous.pending_commits.ends_with(&next.pending_commits)
    {
        return Err(invalid());
    }

    let end = previous.pending_commits.len() - next.pending_commits.len();
    let prefix = &previous.pending_commits[..end];
    if prefix.last() != Some(&next.covered.commit) {
        return Err(invalid());
    }

    let pending: std::collections::HashSet<&str> = previous
        .pending
        .iter()
        .flat_map(|delivery| delivery.commits.iter().map(String::as_str))
        .collect();
    if prefix
        .iter()
        .any(|sha| previous.unresolved.contains_key(sha) || pending.contains(sha.as_str()))
    {
        return Err(invalid());
    }

    let closed = previous.excluded.iter().any(|excluded| {
        excluded.delivery.after == next.covered
            && excluded
                .delivery
                .commits
                .iter()
                .all(|sha| prefix.contains(sha))
    });
    if !closed {
        return Err(invalid());
    }

    if previous.excluded.iter().any(|excluded| {
        let hits = excluded
            .delivery
            .commits
            .iter()
            .any(|sha| prefix.contains(sha));
        let whole = excluded
            .delivery
            .commits
            .iter()
            .all(|sha| prefix.contains(sha));
        hits && !whole
    }) || prefix.iter().any(|sha| {
        !previous
            .excluded
            .iter()
            .any(|excluded| excluded.delivery.commits.iter().any(|commit| commit == sha))
    }) {
        return Err(invalid());
    }

    let retained = previous
        .excluded
        .iter()
        .filter(|excluded| {
            !excluded
                .delivery
                .commits
                .iter()
                .all(|sha| prefix.contains(sha))
        })
        .cloned()
        .collect::<Vec<_>>();
    if next.excluded != retained {
        return Err(invalid());
    }

    let mut unresolved = previous.unresolved.clone();
    let mut associations = previous.associations.clone();
    for sha in prefix {
        unresolved.remove(sha);
        associations.remove(sha);
    }
    if next.unresolved != unresolved || next.associations != associations {
        return Err(invalid());
    }

    Ok(())
}

mod intents;
mod recovery;
#[cfg(test)]
mod tests;
mod waivers;

mod members;
