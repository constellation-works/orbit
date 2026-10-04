//! Prepare, settle and list task commit journal rows.

use orbit_common::OrbitError;
use rusqlite::{TransactionBehavior, params};

use crate::Store;
use crate::contracts::{TaskCommitJournalRecord, TaskCommitJournalState};

impl Store {
    /// A durable boundary must reopen the same file-backed decision journal.
    pub(crate) fn task_commit_database_path(&self) -> Result<std::path::PathBuf, OrbitError> {
        let conn = self.read()?;
        let path: String = conn
            .query_row(
                "SELECT file FROM pragma_database_list WHERE name = 'main'",
                [],
                |row| row.get(0),
            )
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        if path.is_empty() {
            return Err(OrbitError::Store(
                "task coordination requires a file-backed journal".into(),
            ));
        }
        std::fs::canonicalize(path).map_err(OrbitError::from)
    }

    /// Record an undecided commit intent durably, before anything else moves.
    ///
    /// A `prepared` row is the evidence that lets recovery tell "this process
    /// died before deciding" from "this process decided and died before
    /// applying". Its own transaction commits, so the row survives the crash
    /// that the next step might not.
    pub(crate) fn prepare_task_commit_journal(
        &self,
        journal_id: &str,
        workspace_id: &str,
        task_id: &str,
        intent_json: &str,
    ) -> Result<(), OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            tx.tx
                .execute(
                    "INSERT INTO task_commit_journal(
                        journal_id, workspace_id, task_id, state, intent_json,
                        created_at, committed_at, applied_at, reservation_id
                     ) VALUES (?1, ?2, ?3, 'prepared', ?4, ?5, NULL, NULL, NULL)",
                    params![
                        journal_id,
                        workspace_id,
                        task_id,
                        intent_json,
                        crate::now_string()
                    ],
                )
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            Ok(())
        })
    }

    /// Mark a committed decision fully applied to the task bundle.
    pub(crate) fn finish_task_commit_journal(&self, journal_id: &str) -> Result<(), OrbitError> {
        self.settle_task_commit_journal(journal_id, TaskCommitJournalState::Applied, "committed")
    }

    /// Mark an undecided intent abandoned after its pre-commit state was
    /// restored.
    pub(crate) fn abort_task_commit_journal(&self, journal_id: &str) -> Result<(), OrbitError> {
        self.settle_task_commit_journal(journal_id, TaskCommitJournalState::Aborted, "prepared")
    }

    fn settle_task_commit_journal(
        &self,
        journal_id: &str,
        state: TaskCommitJournalState,
        from_state: &str,
    ) -> Result<(), OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let affected = tx
                .tx
                .execute(
                    "UPDATE task_commit_journal
                     SET state = ?2,
                         applied_at = CASE WHEN ?2 = 'applied' THEN ?3 ELSE applied_at END
                     WHERE journal_id = ?1 AND state = ?4",
                    params![journal_id, state.as_str(), crate::now_string(), from_state],
                )
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            if affected != 1 {
                return Err(OrbitError::Store(format!(
                    "task commit journal '{journal_id}' is no longer '{from_state}'; \
                     refusing to record '{}'",
                    state.as_str()
                )));
            }
            Ok(())
        })
    }

    /// Every unsettled decision for a workspace, oldest first. Recovery
    /// replays exactly this list.
    pub(crate) fn unsettled_task_commit_journal(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<TaskCommitJournalRecord>, OrbitError> {
        self.with_read_connection(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT journal_id, workspace_id, task_id, state, intent_json, reservation_id
                     FROM task_commit_journal
                     WHERE workspace_id = ?1 AND state IN ('prepared', 'committed')
                     ORDER BY created_at ASC, journal_id ASC",
                )
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            let rows = stmt
                .query_map(params![workspace_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<String>>(5)?,
                    ))
                })
                .map_err(|error| OrbitError::Store(error.to_string()))?;

            let mut records = Vec::new();
            for row in rows {
                let (journal_id, workspace_id, task_id, state, intent_json, reservation_id) =
                    row.map_err(|error| OrbitError::Store(error.to_string()))?;
                let state = TaskCommitJournalState::parse(&state).ok_or_else(|| {
                    OrbitError::Store(format!(
                        "task commit journal '{journal_id}' has unknown state '{state}'"
                    ))
                })?;
                records.push(TaskCommitJournalRecord {
                    journal_id,
                    workspace_id,
                    task_id,
                    state,
                    intent_json,
                    reservation_id,
                });
            }
            Ok(records)
        })
    }
}
