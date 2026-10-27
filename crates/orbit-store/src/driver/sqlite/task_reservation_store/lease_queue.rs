//! The hard-lease queue (`docs/design/path-leases`, §3).
//!
//! A blocked hard request is a lease row holding nothing (`files_json` is
//! empty) with its selectors in `queued_files_json` and `queue_kind =
//! 'request'`. A blocked expansion keeps what the lease holds in `files_json`
//! and waits for the rest under `queue_kind = 'expansion'`. No overlap reader
//! looks at `queued_files_json`, so a queued lease holds nothing.
//!
//! The queue orders by priority, then by `queued_at`; an entry waiting at
//! least `lease.aging_minutes` ranks one priority level higher. A request is
//! granted once nothing holds its paths and no entry ranked ahead of it that
//! still waits wants them. An expansion waits only on held paths, so the only
//! cycles are leases each waiting on another's held paths; the owner breaks
//! one by refusing the expansion ranked last in the cycle.

use std::cmp::Ordering;

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{TransactionBehavior, params};
use serde_json::json;

use super::leases::{LEASE_COLUMNS, lease_row, load_lease};
use super::notices::{self, LeaseOverlapNotice};
use super::rows::{LEASE_ROWS, expire_leases, find_reservation_conflicts, serialize_string_list};
use super::{reservation_scope_clause, unique_row_id};
use crate::contracts::{
    LeaseDeadlock, LeaseExpandParams, LeaseExpandResult, LeaseMode, LeasePriority, LeaseQueueGrant,
    LeaseQueueGrantParams, LeaseQueueGrantResult, LeaseQueueKind, LeaseRecord, TaskLockConflict,
    TaskReservationScope,
};
use crate::{Store, StoreTx};

/// One waiting request or expansion with its effective rank.
struct QueuedLease {
    record: LeaseRecord,
    kind: LeaseQueueKind,
    rank: u8,
    queued_at: DateTime<Utc>,
}

impl QueuedLease {
    fn queue_order(&self, other: &Self) -> Ordering {
        other
            .rank
            .cmp(&self.rank)
            .then(self.queued_at.cmp(&other.queued_at))
            .then(self.record.lease_id.cmp(&other.record.lease_id))
    }
}

fn priority_rank(priority: LeasePriority) -> u8 {
    match priority {
        LeasePriority::Low => 0,
        LeasePriority::Normal => 1,
        LeasePriority::High => 2,
    }
}

/// The priority rank, one level higher (at most `high`) once the entry has
/// waited `aging_minutes`.
fn effective_rank(
    priority: LeasePriority,
    queued_at: DateTime<Utc>,
    now: DateTime<Utc>,
    aging_minutes: u32,
) -> u8 {
    let rank = priority_rank(priority);
    if now - queued_at >= Duration::minutes(i64::from(aging_minutes)) {
        (rank + 1).min(priority_rank(LeasePriority::High))
    } else {
        rank
    }
}

/// Every active waiting request and expansion, in queue order.
fn load_queue(
    tx: &StoreTx<'_>,
    workspace_orbit_dir: &str,
    workspace_id: Option<&str>,
    aging_minutes: u32,
) -> Result<Vec<QueuedLease>, OrbitError> {
    let now_text = crate::now_string();
    let now = Utc::now();
    let sql = format!(
        "SELECT {LEASE_COLUMNS}
         FROM task_reservations
         WHERE {}
           AND {LEASE_ROWS}
           AND released_at IS NULL
           AND expires_at > ?3
           AND queue_kind IS NOT NULL
           AND queued_files_json IS NOT NULL",
        reservation_scope_clause(TaskReservationScope::Files),
    );
    let mut stmt = tx.tx.prepare(&sql).map_err(sqlite_store_error)?;
    let rows = stmt
        .query_map(
            params![workspace_id, workspace_orbit_dir, now_text],
            lease_row,
        )
        .map_err(sqlite_store_error)?;
    let mut queue = Vec::new();
    for row in rows {
        let record = row.map_err(sqlite_store_error)?.into_record()?;
        let Some(kind) = record.queue_kind else {
            continue;
        };
        if record.queued_selectors.is_empty() {
            continue;
        }
        // An unreadable queue time reads as now: the entry ranks youngest
        // rather than jumping the queue.
        let queued_at = record
            .queued_at
            .as_deref()
            .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
            .map_or(now, |at| at.with_timezone(&Utc));
        queue.push(QueuedLease {
            rank: effective_rank(record.priority, queued_at, now, aging_minutes),
            kind,
            queued_at,
            record,
        });
    }
    queue.sort_by(QueuedLease::queue_order);
    Ok(queue)
}

/// The waiting entries a new request at `priority` would queue behind that
/// want any of its `selectors`. Every waiting entry is older than a new
/// request, so an equal rank is ahead of it.
pub(super) fn queued_ahead_of_request(
    tx: &StoreTx<'_>,
    workspace_orbit_dir: &str,
    workspace_id: Option<&str>,
    aging_minutes: u32,
    priority: LeasePriority,
    selectors: &[String],
) -> Result<Vec<String>, OrbitError> {
    let rank = priority_rank(priority);
    let lock_modes = tx.lock_modes.clone();
    Ok(
        load_queue(tx, workspace_orbit_dir, workspace_id, aging_minutes)?
            .into_iter()
            .filter(|entry| {
                entry.rank >= rank
                    && lock_modes.footprints_block(selectors, &entry.record.queued_selectors)
            })
            .map(|entry| entry.record.lease_id)
            .collect(),
    )
}

/// A waiting lease's 1-based position in the queue.
pub(super) fn queue_position(
    tx: &StoreTx<'_>,
    workspace_orbit_dir: &str,
    workspace_id: Option<&str>,
    aging_minutes: u32,
    lease_id: &str,
) -> Result<Option<u32>, OrbitError> {
    Ok(
        load_queue(tx, workspace_orbit_dir, workspace_id, aging_minutes)?
            .iter()
            .position(|entry| entry.record.lease_id == lease_id)
            .and_then(|index| u32::try_from(index + 1).ok()),
    )
}

/// Task reservations and hard leases, other than `lease_id` itself, holding
/// any of `selectors`.
fn holder_conflicts(
    tx: &mut StoreTx<'_>,
    workspace_orbit_dir: &str,
    workspace_id: Option<&str>,
    selectors: &[String],
    lease_id: &str,
) -> Result<Vec<TaskLockConflict>, OrbitError> {
    let now = crate::now_string();
    Ok(
        find_reservation_conflicts(tx, workspace_orbit_dir, workspace_id, &now, selectors)?
            .into_iter()
            .filter(|conflict| conflict.held_by_id != lease_id)
            .collect(),
    )
}

/// Write a lease's held and waiting selectors. An empty `queued` clears the
/// queue columns; otherwise `kind` and `queued_at` say what waits since when.
fn store_lease_surface(
    tx: &StoreTx<'_>,
    lease_id: &str,
    held: &[String],
    queued: &[String],
    kind: LeaseQueueKind,
    queued_at: &str,
) -> Result<(), OrbitError> {
    let (queued, queued_at, kind) = if queued.is_empty() {
        (None, None, None)
    } else {
        (
            Some(serialize_string_list(queued)?),
            Some(queued_at),
            Some(kind.as_str()),
        )
    };
    tx.tx
        .execute(
            "UPDATE task_reservations
             SET files_json = ?2,
                 queued_files_json = ?3,
                 queued_at = ?4,
                 queue_kind = ?5
             WHERE reservation_id = ?1
               AND kind = 'lease'
               AND released_at IS NULL",
            params![
                lease_id,
                serialize_string_list(held)?,
                queued,
                queued_at,
                kind
            ],
        )
        .map_err(sqlite_store_error)?;
    Ok(())
}

/// The `granted` notice and the `overlap` notices other leases get when
/// `lease` gains `selectors`.
fn notify_grant(
    tx: &StoreTx<'_>,
    workspace_orbit_dir: &str,
    workspace_id: Option<&str>,
    lease: &LeaseRecord,
    selectors: &[String],
    kind: LeaseQueueKind,
    still_queued: &[String],
) -> Result<(), OrbitError> {
    let lease_id = lease.lease_id.as_str();
    let (payload, event_key, overlap_key) = match kind {
        LeaseQueueKind::Request => (
            json!({
                "mode": lease.mode, "principal": lease.principal, "selectors": selectors,
                "from_queue": true,
            }),
            "granted".to_string(),
            format!("lease:{lease_id}"),
        ),
        LeaseQueueKind::Expansion => {
            let step = unique_row_id("");
            (
                json!({
                    "mode": lease.mode, "principal": lease.principal, "selectors": selectors,
                    "expansion": true, "still_queued": still_queued,
                }),
                format!("granted:expansion:{step}"),
                format!("lease:{lease_id}:expansion:{step}"),
            )
        }
    };
    notices::append_lease_notice(tx, lease_id, "granted", &payload, &event_key)?;
    notices::append_lease_overlaps(
        tx,
        &LeaseOverlapNotice {
            orbit_dir: workspace_orbit_dir,
            workspace_id,
            holder: lease_id,
            paths: selectors,
            kind: "overlap",
            payload: json!({"lease_id": lease_id, "mode": lease.mode, "principal": lease.principal}),
            event_key: &overlap_key,
            landing: None,
        },
    )
}

/// `held` plus `added`, deduplicated and ordered.
fn union(held: &[String], added: &[String]) -> Vec<String> {
    let mut all = held.iter().chain(added).cloned().collect::<Vec<_>>();
    all.sort();
    all.dedup();
    all
}

/// The first cycle in the wait-for graph `edges` (adjacency by index).
fn find_cycle(edges: &[Vec<usize>]) -> Option<Vec<usize>> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        New,
        OnPath,
        Done,
    }
    fn visit(
        node: usize,
        edges: &[Vec<usize>],
        marks: &mut [Mark],
        path: &mut Vec<usize>,
    ) -> Option<Vec<usize>> {
        marks[node] = Mark::OnPath;
        path.push(node);
        for &next in &edges[node] {
            match marks[next] {
                Mark::OnPath => {
                    let start = path.iter().position(|&n| n == next)?;
                    return Some(path[start..].to_vec());
                }
                Mark::New => {
                    if let Some(cycle) = visit(next, edges, marks, path) {
                        return Some(cycle);
                    }
                }
                Mark::Done => {}
            }
        }
        path.pop();
        marks[node] = Mark::Done;
        None
    }
    let mut marks = vec![Mark::New; edges.len()];
    for node in 0..edges.len() {
        if marks[node] == Mark::New
            && let Some(cycle) = visit(node, edges, &mut marks, &mut Vec::new())
        {
            return Some(cycle);
        }
    }
    None
}

/// Refuse waiting expansions until no lease waits, through others, on
/// itself. Each cycle loses the expansion ranked last in queue order: the
/// lowest effective priority, then the youngest.
fn break_deadlocks(
    tx: &StoreTx<'_>,
    workspace_orbit_dir: &str,
    workspace_id: Option<&str>,
    aging_minutes: u32,
) -> Result<Vec<LeaseDeadlock>, OrbitError> {
    let lock_modes = tx.lock_modes.clone();
    let mut refused = Vec::new();
    loop {
        let expansions = load_queue(tx, workspace_orbit_dir, workspace_id, aging_minutes)?
            .into_iter()
            .filter(|entry| entry.kind == LeaseQueueKind::Expansion)
            .collect::<Vec<_>>();
        let edges = expansions
            .iter()
            .enumerate()
            .map(|(waiter, entry)| {
                expansions
                    .iter()
                    .enumerate()
                    .filter(|(holder, other)| {
                        *holder != waiter
                            && other.record.mode == LeaseMode::Hard
                            && lock_modes.footprints_block(
                                &entry.record.queued_selectors,
                                &other.record.selectors,
                            )
                    })
                    .map(|(holder, _)| holder)
                    .collect()
            })
            .collect::<Vec<Vec<usize>>>();
        let Some(cycle) = find_cycle(&edges) else {
            return Ok(refused);
        };
        // `expansions` is in queue order, so the highest index ranks last.
        let Some(&victim_index) = cycle.iter().max() else {
            return Ok(refused);
        };
        let victim = &expansions[victim_index].record;
        let members = cycle
            .iter()
            .map(|&index| expansions[index].record.lease_id.clone())
            .collect::<Vec<_>>();
        store_lease_surface(
            tx,
            &victim.lease_id,
            &victim.selectors,
            &[],
            LeaseQueueKind::Expansion,
            "",
        )?;
        notices::append_lease_notice(
            tx,
            &victim.lease_id,
            "deadlock",
            &json!({"selectors": victim.queued_selectors, "cycle": members}),
            &format!("deadlock:{}", unique_row_id("")),
        )?;
        refused.push(LeaseDeadlock {
            lease_id: victim.lease_id.clone(),
            selectors: victim.queued_selectors.clone(),
            cycle: members,
        });
    }
}

impl Store {
    /// Grant, in queue order, every waiting request whose paths are now free
    /// and every free selector of a waiting expansion. Entries without an
    /// entry in `task_conflicts` stay queued and still rank ahead.
    pub fn grant_queued_leases(
        &self,
        params: &LeaseQueueGrantParams,
    ) -> Result<LeaseQueueGrantResult, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let dir = params.workspace_orbit_dir.as_str();
            let workspace_id = params.workspace_id.as_deref();
            let expired_leases = expire_leases(tx, dir, workspace_id, &crate::now_string())?;
            let lock_modes = tx.lock_modes.clone();
            let mut waiting: Vec<Vec<String>> = Vec::new();
            let mut granted = Vec::new();
            for entry in load_queue(tx, dir, workspace_id, params.aging_minutes)? {
                let lease = &entry.record;
                let wanted = &lease.queued_selectors;
                let Some(tasks) = params.task_conflicts.get(&lease.lease_id) else {
                    waiting.push(wanted.clone());
                    continue;
                };
                let holders = holder_conflicts(tx, dir, workspace_id, wanted, &lease.lease_id)?;
                let (free, rest): (Vec<String>, Vec<String>) = match entry.kind {
                    LeaseQueueKind::Request => {
                        let blocked = !tasks.is_empty()
                            || !holders.is_empty()
                            || waiting
                                .iter()
                                .any(|ahead| lock_modes.footprints_block(wanted, ahead));
                        if blocked {
                            (Vec::new(), wanted.clone())
                        } else {
                            (wanted.clone(), Vec::new())
                        }
                    }
                    LeaseQueueKind::Expansion => wanted.iter().cloned().partition(|selector| {
                        !tasks
                            .iter()
                            .chain(&holders)
                            .any(|conflict| &conflict.file == selector)
                    }),
                };
                if !free.is_empty() {
                    let queued_at = lease.queued_at.clone().unwrap_or_default();
                    store_lease_surface(
                        tx,
                        &lease.lease_id,
                        &union(&lease.selectors, &free),
                        &rest,
                        entry.kind,
                        &queued_at,
                    )?;
                    notify_grant(tx, dir, workspace_id, lease, &free, entry.kind, &rest)?;
                    granted.push(LeaseQueueGrant {
                        lease_id: lease.lease_id.clone(),
                        kind: entry.kind,
                        selectors: free,
                        still_queued: rest.clone(),
                    });
                }
                if !rest.is_empty() {
                    waiting.push(rest);
                }
            }
            Ok(LeaseQueueGrantResult {
                granted,
                expired_leases,
            })
        })
    }

    /// Add selectors to a granted lease. An advisory lease takes them all; a
    /// hard lease takes the free ones and queues the rest as an expansion,
    /// keeping what it holds, then any deadlock this closes is broken.
    pub fn expand_lease(
        &self,
        params: &LeaseExpandParams,
    ) -> Result<LeaseExpandResult, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let dir = params.workspace_orbit_dir.as_str();
            let workspace_id = params.workspace_id.as_deref();
            let now = crate::now_string();
            let expired_leases = expire_leases(tx, dir, workspace_id, &now)?;
            let mut result = LeaseExpandResult {
                lease: None,
                added: Vec::new(),
                queued: Vec::new(),
                conflicts: Vec::new(),
                queue_position: None,
                deadlocks: Vec::new(),
                expired_leases,
            };
            let Some(lease) =
                load_lease(tx, dir, workspace_id, &params.lease_id)?.filter(|lease| {
                    lease.released_at.is_none()
                        && lease.queue_kind != Some(LeaseQueueKind::Request)
                        && lease.expires_at > now
                })
            else {
                return Ok(result);
            };
            let selectors = params
                .selectors
                .iter()
                .filter(|selector| {
                    !lease.selectors.contains(selector)
                        && !lease.queued_selectors.contains(selector)
                })
                .cloned()
                .collect::<Vec<_>>();
            let (added, queued, conflicts) = if lease.mode == LeaseMode::Hard {
                let holders = holder_conflicts(tx, dir, workspace_id, &selectors, &lease.lease_id)?;
                let conflicts = params
                    .task_conflicts
                    .iter()
                    .chain(&holders)
                    .filter(|conflict| selectors.contains(&conflict.file))
                    .cloned()
                    .collect::<Vec<_>>();
                let (added, queued): (Vec<String>, Vec<String>) = selectors
                    .into_iter()
                    .partition(|selector| !conflicts.iter().any(|c| &c.file == selector));
                (added, queued, conflicts)
            } else {
                (selectors, Vec::new(), Vec::new())
            };
            let waiting = union(&lease.queued_selectors, &queued);
            let queued_at = lease.queued_at.clone().unwrap_or_else(|| now.clone());
            store_lease_surface(
                tx,
                &lease.lease_id,
                &union(&lease.selectors, &added),
                &waiting,
                LeaseQueueKind::Expansion,
                &queued_at,
            )?;
            if !added.is_empty() {
                notify_grant(
                    tx,
                    dir,
                    workspace_id,
                    &lease,
                    &added,
                    LeaseQueueKind::Expansion,
                    &waiting,
                )?;
            }
            if !queued.is_empty() {
                let position =
                    queue_position(tx, dir, workspace_id, params.aging_minutes, &lease.lease_id)?;
                notices::append_lease_notice(
                    tx,
                    &lease.lease_id,
                    "queued",
                    &json!({
                        "selectors": queued,
                        "expansion": true,
                        "holders": conflicts,
                        "position": position,
                    }),
                    &format!("queued:expansion:{}", unique_row_id("")),
                )?;
                result.deadlocks = break_deadlocks(tx, dir, workspace_id, params.aging_minutes)?;
            }
            result.queue_position =
                queue_position(tx, dir, workspace_id, params.aging_minutes, &lease.lease_id)?;
            result.lease = load_lease(tx, dir, workspace_id, &lease.lease_id)?;
            result.added = added;
            result.queued = queued;
            result.conflicts = conflicts;
            Ok(result)
        })
    }
}
