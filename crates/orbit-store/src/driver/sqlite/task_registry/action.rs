//! Permanent action-key mapping at the task allocation authority.
use super::TaskRegistryStore;
use orbit_common::OrbitError;
use rusqlite::{OptionalExtension, params};
impl TaskRegistryStore {
    pub(crate) fn reserve_task_action(
        &self,
        workspace: &str,
        key: &str,
        digest: &str,
    ) -> Result<String, OrbitError> {
        if key.is_empty() || key.len() > 256 {
            return Err(OrbitError::InvalidInput("invalid task action key".into()));
        }
        // Allocation can leave a harmless hole if another process wins; it can
        // never reissue an ID. The winning key mapping commits before bundle I/O.
        if let Some(id) = self.task_action(workspace, key, digest)? {
            return Ok(id);
        }
        let candidate = self.allocate_task_id(workspace)?;
        let conn = self
            .conn
            .lock()
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        conn.execute(
            "INSERT OR IGNORE INTO task_action_keys VALUES (?1,?2,?3,?4)",
            params![workspace, key, candidate, digest],
        )
        .map_err(|e| OrbitError::Store(e.to_string()))?;
        drop(conn);
        self.task_action(workspace, key, digest)?
            .ok_or_else(|| OrbitError::Store("task action reservation disappeared".into()))
    }
    fn task_action(
        &self,
        workspace: &str,
        key: &str,
        digest: &str,
    ) -> Result<Option<String>, OrbitError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let row:Option<(String,String)>=conn.query_row("SELECT task_id,input_digest FROM task_action_keys WHERE workspace_id=?1 AND action_key=?2",params![workspace,key],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(|e|OrbitError::Store(e.to_string()))?;
        row.map(|(id, stored)| {
            if stored == digest {
                Ok(id)
            } else {
                Err(OrbitError::InvalidInput("action key input changed".into()))
            }
        })
        .transpose()
    }
}
