//! Task relation validation against the coordinator registry: target
//! existence, cross-workspace dangling targets, and cycle detection for
//! replacement sets.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::task::{
    CYCLIC_RELATION_TYPES, TaskEnvelopeV2, TaskRelation, TaskRelationEdge, TaskRelationType,
    is_valid_orb_task_id, task_id_prefix, validate_orb_task_id, validate_task_relations_for_source,
};
use rusqlite::{Connection, params, params_from_iter};

use super::allocator::{
    active_task_prefix, known_task_prefixes, registered_task_ids, task_prefix_is_registered,
};
use super::partition_id::validate_partition_id;
use super::queries::workspace_by_id;
use super::store::TaskRegistryStore;
use super::util::{parse_relation_type_name, relation_type_name};
use crate::contracts::DanglingRelationTarget;

impl TaskRegistryStore {
    /// Validate task relations against every workspace in the coordination
    /// registry without mutating allocator, bundle, or index state.
    pub fn validate_task_relations(
        &self,
        partition_id: &str,
        source_task_id: &str,
        relations: &[TaskRelation],
    ) -> Result<(), OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        validate_orb_task_id(source_task_id)?;
        let conn = self.read()?;
        validate_relations_in_registry(
            &conn,
            &partition_id,
            source_task_id,
            relations,
            &[source_task_id.to_string()],
            &[],
        )
    }

    /// Preflight relation targets for a task whose globally allocated source ID
    /// does not exist yet. This runs before allocation so a missing target
    /// cannot consume an ID or write a partial bundle.
    pub fn validate_new_task_relation_targets(
        &self,
        partition_id: &str,
        relations: &[TaskRelation],
    ) -> Result<(), OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        let conn = self.read()?;
        validate_relation_targets_exist(&conn, &partition_id, None, relations)
    }

    /// Audit the coordination registry for relation edges whose target is a
    /// valid `ORB-` task id with no registered task bundle — the "grandfathered"
    /// relations that make [`validate_relation_targets_exist`] reject an index
    /// rebuild (ORB-10305). Scans indexed relation rows across the whole
    /// registry, or a single workspace when `partition_id` is set, so these
    /// targets can be surfaced (and cleaned) proactively instead of only when a
    /// rebuild trips over them.
    ///
    /// Mirrors the validator's resolution semantics: only `ORB-` targets can be
    /// unresolved; friction / ADR targets that `produces`/`resolves`
    /// edges legitimately allow to dangle are excluded.
    pub fn dangling_relation_targets(
        &self,
        partition_id: Option<&str>,
    ) -> Result<Vec<DanglingRelationTarget>, OrbitError> {
        let partition_id = partition_id.map(validate_partition_id).transpose()?;
        let conn = self.read()?;

        let mut sql = String::from(
            "SELECT r.workspace_id, r.source_task_id, r.relation_type, r.target_task_id
             FROM task_bundle_relations r
             LEFT JOIN task_bundle_bindings b ON b.task_id = r.target_task_id
             WHERE b.task_id IS NULL",
        );
        let mut values: Vec<String> = Vec::new();
        if let Some(partition_id) = &partition_id {
            sql.push_str(" AND r.workspace_id = ?1");
            values.push(partition_id.clone());
        }
        sql.push_str(
            " ORDER BY r.workspace_id, r.source_task_id, r.relation_type, r.target_task_id",
        );

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let rows = stmt
            .query_map(params_from_iter(values.iter()), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        let mut dangling = Vec::new();
        let known_prefixes = known_task_prefixes(&conn)?;
        for row in rows {
            let (partition_id, source_task_id, relation_type, target_task_id) =
                row.map_err(|e| OrbitError::Store(e.to_string()))?;
            // Non-task artifact targets and foreign-prefix task references are
            // both allowed to remain unresolved here. Only a locally known
            // prefix can be a dangling relation in this registry.
            if !is_valid_orb_task_id(&target_task_id) {
                continue;
            }
            let Some(prefix) = task_id_prefix(&target_task_id) else {
                continue;
            };
            if !known_prefixes.contains(prefix) {
                continue;
            }
            dangling.push(DanglingRelationTarget {
                partition_id,
                source_task_id,
                relation_type,
                target_task_id,
            });
        }
        Ok(dangling)
    }

    pub fn indexed_relation_targets(
        &self,
        partition_id: &str,
        source_task_id: &str,
        relation_type: TaskRelationType,
    ) -> Result<Vec<String>, OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        validate_orb_task_id(source_task_id)?;
        let conn = self.read()?;
        let mut stmt = conn
            .prepare_cached(
                "SELECT target_task_id FROM task_bundle_relations
                 WHERE workspace_id = ?1 AND source_task_id = ?2 AND relation_type = ?3
                 ORDER BY target_task_id ASC",
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let rows = stmt
            .query_map(
                params![
                    partition_id,
                    source_task_id,
                    relation_type_name(relation_type)
                ],
                |row| row.get::<_, String>(0),
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| OrbitError::Store(e.to_string()))
    }

    pub fn indexed_relation_sources(
        &self,
        partition_id: &str,
        target_task_id: &str,
        relation_type: TaskRelationType,
    ) -> Result<Vec<String>, OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        validate_orb_task_id(target_task_id)?;
        let conn = self.read()?;
        let mut stmt = conn
            .prepare_cached(
                "SELECT source_task_id FROM task_bundle_relations
                 WHERE workspace_id = ?1 AND target_task_id = ?2 AND relation_type = ?3
                 ORDER BY source_task_id ASC",
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let rows = stmt
            .query_map(
                params![
                    partition_id,
                    target_task_id,
                    relation_type_name(relation_type)
                ],
                |row| row.get(0),
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| OrbitError::Store(e.to_string()))
    }
}

fn task_relation_edges(envelope: &TaskEnvelopeV2) -> Vec<TaskRelationEdge> {
    envelope
        .relations
        .iter()
        .filter(|relation| is_valid_orb_task_id(&relation.target))
        .map(|relation| TaskRelationEdge {
            source: envelope.id.clone(),
            relation_type: relation.relation_type,
            target: relation.target.clone(),
        })
        .collect()
}

/// Validate a replacement set as a unit: its members' currently indexed edges
/// are ignored in favour of the edges it is about to write, so the set's own
/// cross-references resolve regardless of the order rows land in.
pub(super) fn validate_replacement_relations(
    conn: &Connection,
    partition_id: &str,
    envelopes: &[TaskEnvelopeV2],
) -> Result<(), OrbitError> {
    if envelopes.is_empty() {
        return Ok(());
    }
    if workspace_by_id(conn, partition_id)?.is_none() {
        return Err(OrbitError::not_found(
            NotFoundKind::Workspace,
            partition_id.to_string(),
        ));
    }

    let replacement_edges = envelopes
        .iter()
        .flat_map(task_relation_edges)
        .collect::<Vec<_>>();
    let replacement_sources = envelopes
        .iter()
        .map(|envelope| envelope.id.clone())
        .collect::<BTreeSet<_>>();

    validate_replacement_relation_targets(conn, partition_id, envelopes)?;

    let seeds = cycle_walk_seeds(&[], &replacement_edges);
    let mut validation_edges = reachable_cycle_family_edges(conn, &seeds)?
        .into_iter()
        .filter(|edge| {
            !replacement_sources.contains(&edge.source) && is_valid_orb_task_id(&edge.target)
        })
        .collect::<Vec<_>>();
    validation_edges.extend(replacement_edges);

    for envelope in envelopes {
        validate_task_relations_for_source(&envelope.id, &envelope.relations, &validation_edges)
            .map_err(OrbitError::from)?;
    }
    Ok(())
}

/// Resolve every replacement relation target with one primary-key query, then
/// use one materialized prefix set for any target that query did not find.
fn validate_replacement_relation_targets(
    conn: &Connection,
    source_workspace_id: &str,
    envelopes: &[TaskEnvelopeV2],
) -> Result<(), OrbitError> {
    let candidates = envelopes
        .iter()
        .flat_map(|envelope| {
            envelope
                .relations
                .iter()
                .filter(|relation| {
                    is_valid_orb_task_id(&relation.target) && relation.target != envelope.id
                })
                .map(|relation| relation.target.clone())
        })
        .collect::<BTreeSet<_>>();
    if candidates.is_empty() {
        return Ok(());
    }

    let registered = registered_task_ids(conn, &candidates)?;
    if registered.len() == candidates.len() {
        return Ok(());
    }
    let known_prefixes = known_task_prefixes(conn)?;
    for envelope in envelopes {
        for relation in &envelope.relations {
            if !candidates.contains(&relation.target) || registered.contains(&relation.target) {
                continue;
            }
            let Some(prefix) = task_id_prefix(&relation.target) else {
                continue;
            };
            if !known_prefixes.contains(prefix) {
                continue;
            }
            return Err(OrbitError::InvalidInput(format!(
                "task relation target '{}' from workspace '{}' does not resolve in the coordination registry",
                relation.target, source_workspace_id
            )));
        }
    }
    Ok(())
}

fn validate_relations_in_registry(
    conn: &Connection,
    source_workspace_id: &str,
    source_task_id: &str,
    relations: &[TaskRelation],
    replaced_sources: &[String],
    replacement_edges: &[TaskRelationEdge],
) -> Result<(), OrbitError> {
    validate_relation_targets_exist(conn, source_workspace_id, Some(source_task_id), relations)?;

    let replaced_sources = replaced_sources.iter().collect::<BTreeSet<_>>();
    let seeds = cycle_walk_seeds(relations, replacement_edges);
    let mut existing_edges = reachable_cycle_family_edges(conn, &seeds)?
        .into_iter()
        .filter(|edge| {
            !replaced_sources.contains(&edge.source) && is_valid_orb_task_id(&edge.target)
        })
        .collect::<Vec<_>>();
    existing_edges.extend(
        replacement_edges
            .iter()
            .filter(|edge| edge.source != source_task_id)
            .cloned(),
    );
    validate_task_relations_for_source(source_task_id, relations, &existing_edges)
        .map_err(Into::into)
}

/// Where the cycle check can start walking, and therefore what the registry
/// subgraph must be closed over.
///
/// The validator only probes reachability forward from a new relation's
/// target, so those targets are the primary seeds. A replacement edge is not
/// in the registry yet, so any path crossing one resumes at its target —
/// seeding those as well keeps the fetched subgraph closed over the batch's
/// own unwritten edges. Only cycle-family targets matter; the other relation
/// types are queryable metadata the walk never follows.
fn cycle_walk_seeds(
    relations: &[TaskRelation],
    replacement_edges: &[TaskRelationEdge],
) -> BTreeSet<String> {
    let relation_targets = relations
        .iter()
        .filter(|relation| CYCLIC_RELATION_TYPES.contains(&relation.relation_type))
        .map(|relation| relation.target.clone());
    let replacement_targets = replacement_edges
        .iter()
        .filter(|edge| CYCLIC_RELATION_TYPES.contains(&edge.relation_type))
        .map(|edge| edge.target.clone());
    relation_targets
        .chain(replacement_targets)
        .filter(|target| is_valid_orb_task_id(target))
        .collect()
}

/// The subgraph walk from [`reachable_cycle_family_edges`], as SQL over
/// `seed_count` bound seed ids.
///
/// Separate from its caller so `relation_subgraph_query_stays_indexed` can put
/// it through `EXPLAIN QUERY PLAN`; both of its joins have to resolve as index
/// searches.
pub(super) fn reachable_cycle_family_sql(seed_count: usize) -> String {
    let seed_rows = (1..=seed_count)
        .map(|index| format!("SELECT ?{index}"))
        .collect::<Vec<_>>()
        .join(" UNION ");
    let families = CYCLIC_RELATION_TYPES
        .iter()
        .map(|relation_type| format!("'{}'", relation_type_name(*relation_type)))
        .collect::<Vec<_>>()
        .join(", ");
    // `UNION` (not `UNION ALL`) is what terminates the walk on an existing
    // cycle: the registry is not guaranteed acyclic from this query's side.
    //
    // The collecting select uses `CROSS JOIN` purely to pin the join order.
    // SQLite has no cardinality estimate for a recursive CTE, and its choice
    // here is not stable: with a plain join the same statement plans as a
    // `SEARCH` against this registry's schema but as a full `SCAN` of
    // `task_bundle_relations` against a reduced one. A scan is exactly the
    // cost this query exists to avoid, so the order is not left to the
    // planner. `relation_subgraph_query_stays_indexed` checks the result.
    format!(
        "WITH RECURSIVE reachable(task_id) AS (
             {seed_rows}
             UNION
             SELECT edge.target_task_id
             FROM task_bundle_relations AS edge
             JOIN reachable ON edge.source_task_id = reachable.task_id
             WHERE edge.relation_type IN ({families})
         )
         SELECT edge.source_task_id, edge.relation_type, edge.target_task_id
         FROM reachable
         CROSS JOIN task_bundle_relations AS edge
             ON edge.source_task_id = reachable.task_id
         WHERE edge.relation_type IN ({families})
         ORDER BY edge.source_task_id, edge.relation_type, edge.target_task_id"
    )
}

/// The cycle-family edges forward-reachable from `seeds`, as one recursive
/// walk of the registry's relation rows.
///
/// This is exactly the subgraph the cycle check can observe, so fetching the
/// whole `task_bundle_relations` table on every task write only ever bought
/// rows the validator would ignore. The walk deliberately crosses workspace
/// boundaries — a relation may target a task in another workspace, and a cycle
/// through one is still a cycle — so it cannot be narrowed to a
/// `workspace_id = ?` filter.
///
/// Rows belonging to a replaced source are filtered by the caller rather than
/// here: traversing through a stale edge can only over-collect real edges, and
/// an over-collected edge whose only link into the graph was that stale edge
/// is unreachable from the seeds and cannot change the verdict.
fn reachable_cycle_family_edges(
    conn: &Connection,
    seeds: &BTreeSet<String>,
) -> Result<Vec<TaskRelationEdge>, OrbitError> {
    if seeds.is_empty() {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare(&reachable_cycle_family_sql(seeds.len()))
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    let rows = stmt
        .query_map(params_from_iter(seeds.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    let mut edges = Vec::new();
    for row in rows {
        let (source, relation_type, target) = row.map_err(|e| OrbitError::Store(e.to_string()))?;
        edges.push(TaskRelationEdge {
            source,
            relation_type: parse_relation_type_name(&relation_type).map_err(OrbitError::Store)?,
            target,
        });
    }
    Ok(edges)
}

fn validate_relation_targets_exist(
    conn: &Connection,
    source_workspace_id: &str,
    source_task_id: Option<&str>,
    relations: &[TaskRelation],
) -> Result<(), OrbitError> {
    if workspace_by_id(conn, source_workspace_id)?.is_none() {
        return Err(OrbitError::not_found(
            NotFoundKind::Workspace,
            source_workspace_id.to_string(),
        ));
    }
    let candidates = relations
        .iter()
        .filter(|relation| {
            is_valid_orb_task_id(&relation.target)
                && source_task_id != Some(relation.target.as_str())
        })
        .map(|relation| relation.target.clone())
        .collect::<BTreeSet<_>>();
    if candidates.is_empty() {
        return Ok(());
    }

    let registered = registered_task_ids(conn, &candidates)?;
    let active_prefix = active_task_prefix(conn)?;
    let mut probed: BTreeMap<&str, bool> = BTreeMap::new();
    for relation in relations {
        if !candidates.contains(&relation.target) || registered.contains(&relation.target) {
            continue;
        }
        let Some(prefix) = task_id_prefix(&relation.target) else {
            continue;
        };
        // An unresolvable target under a prefix this registry has never issued
        // is a foreign id, not a dangling edge; only a known prefix means the
        // task should have been here.
        let known = if prefix == active_prefix {
            true
        } else {
            match probed.get(prefix) {
                Some(known) => *known,
                None => {
                    let known = task_prefix_is_registered(conn, prefix)?;
                    probed.insert(prefix, known);
                    known
                }
            }
        };
        if !known {
            continue;
        }
        return Err(OrbitError::InvalidInput(format!(
            "task relation target '{}' from workspace '{}' does not resolve in the coordination registry",
            relation.target, source_workspace_id
        )));
    }
    Ok(())
}
