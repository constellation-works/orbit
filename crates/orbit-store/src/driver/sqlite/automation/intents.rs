//! Indexed immutable delivery-owner intents; reachability is verified by Core.
use super::{decode, encode};
use crate::Store;
use orbit_common::OrbitError;
use orbit_types::workflow::automation::Delivery;
use rusqlite::{OptionalExtension, TransactionBehavior, params};
pub(super) fn record(store: &Store, delivery: &Delivery) -> Result<(), OrbitError> {
    if delivery.commits.is_empty() || delivery.commits.len() > 5000 {
        return Err(OrbitError::InvalidInput(
            "direct delivery membership must contain 1..=5000 commits".into(),
        ));
    }
    let id = format!("{}:{}", delivery.key, delivery.after.commit);
    store.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
        let conn = tx.connection();
        let existing: Option<String> = conn
            .query_row(
                "SELECT delivery_json FROM automation_delivery_intents WHERE record_id=?1",
                [&id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        if let Some(raw) = existing {
            let old: Delivery = decode(&raw)?;
            if old.before != delivery.before
                || old.after != delivery.after
                || old.commits != delivery.commits
            {
                return Err(OrbitError::InvalidInput(
                    "direct delivery identity changed".into(),
                ));
            }
            return Ok(());
        }
        conn.execute(
            "INSERT INTO automation_delivery_intents VALUES (?1,?2,?3,?4)",
            params![id, delivery.repository, delivery.branch, encode(delivery)?],
        )
        .map_err(|e| OrbitError::Store(e.to_string()))?;
        for sha in &delivery.commits {
            conn.execute(
                "INSERT INTO automation_delivery_members VALUES (?1,?2,?3,?4)",
                params![delivery.repository, delivery.branch, sha, id],
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        }
        Ok(())
    })
}
pub(super) fn lookup(
    store: &Store,
    repository: &str,
    branch: &str,
    commits: &[String],
) -> Result<Vec<Delivery>, OrbitError> {
    if commits.len() > 200 {
        return Err(OrbitError::InvalidInput(
            "delivery lookup exceeds page limit".into(),
        ));
    }
    store.with_read_connection(|conn| {
  let mut found=std::collections::BTreeMap::new();
  let mut stmt=conn.prepare("SELECT i.record_id,i.delivery_json FROM automation_delivery_members m JOIN automation_delivery_intents i ON i.record_id=m.record_id WHERE m.repository=?1 AND m.branch=?2 AND m.commit_id=?3 LIMIT 51").map_err(|e|OrbitError::Store(e.to_string()))?;
  for sha in commits {let rows=stmt.query_map(params![repository,branch,sha],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).map_err(|e|OrbitError::Store(e.to_string()))?;for row in rows {let (id,raw)=row.map_err(|e|OrbitError::Store(e.to_string()))?;found.insert(id,decode(&raw)?);if found.len()>50 {return Err(OrbitError::Store("delivery intent page exceeds limit".into()));}}}
  Ok(found.into_values().collect())
 })
}
