use chrono::{SecondsFormat, Utc};
use orbit_common::OrbitError;
use serde_json::{Value, json};

use super::roles;
use crate::OrbitRuntime;

impl OrbitRuntime {
    /// Rebuild this workspace's crew-performance snapshot from a fresh scan
    /// over `workflow.crew_performance_window_days`, replacing the stored
    /// one atomically. With `workflow.crew_performance` off this does not
    /// scan and writes no snapshot in workspaces that do not use the
    /// feature (the hourly routine still records a job run).
    pub fn refresh_crew_performance_snapshot(&self) -> Result<Value, OrbitError> {
        if !self.crew_performance().is_on() {
            return Ok(json!({ "skipped": "crew_performance_off" }));
        }
        let window_days = self.crew_performance_window_days();
        let types = self.crew_performance_types();
        let facts = self.agent_scan()?;
        let since = Utc::now() - chrono::Duration::days(i64::from(window_days));
        let cells = roles::implementer_by_type_cells(&facts, since, types)
            .iter()
            .map(crew_performance_cell)
            .collect::<Result<Vec<_>, OrbitError>>()?;
        let snapshot = orbit_store::contracts::CrewPerformanceSnapshot {
            workspace_id: self.workspace_id()?,
            computed_at: facts.computed_at,
            window_days,
            types_hash: orbit_config::crew_performance_types_hash(types),
            types_json: serde_json::to_string(types).map_err(|error| {
                OrbitError::Execution(format!("serialize crew performance types: {error}"))
            })?,
            cells,
        };
        let cell_count = snapshot.cells.len();
        let computed_at = snapshot
            .computed_at
            .to_rfc3339_opts(SecondsFormat::Secs, true);
        let types_hash = snapshot.types_hash.clone();
        use orbit_store::contracts::CrewPerformanceStoreBackend as _;
        self.sqlite_store()?
            .replace_crew_performance_snapshot(&snapshot)?;
        Ok(json!({
            "computed_at": computed_at,
            "window_days": window_days,
            "types_hash": types_hash,
            "cells": cell_count,
        }))
    }

    /// This workspace's stored crew-performance snapshot when a draw may
    /// use it (fresh, built from the current type list), else the typed
    /// reason it may not.
    pub fn usable_crew_performance_snapshot(
        &self,
    ) -> Result<
        Result<
            orbit_store::contracts::CrewPerformanceSnapshot,
            orbit_store::contracts::CrewPerformanceSnapshotUnusable,
        >,
        OrbitError,
    > {
        use orbit_store::contracts::CrewPerformanceStoreBackend as _;
        self.sqlite_store()?.usable_crew_performance_snapshot(
            &self.workspace_id()?,
            Utc::now(),
            &orbit_config::crew_performance_types_hash(self.crew_performance_types()),
        )
    }

    /// The readiness view of the snapshot: its metadata when one is stored,
    /// plus the unusable reason when a draw would ignore it. Advisory, so a
    /// store that cannot be read reports `unreadable` beside the error
    /// rather than failing readiness, so it is not confused with `missing`.
    pub fn crew_performance_readiness(&self) -> Value {
        let mode = self.crew_performance().as_str();
        let stored = (|runtime: &Self| -> Result<_, OrbitError> {
            use orbit_store::contracts::CrewPerformanceStoreBackend as _;
            runtime
                .sqlite_store()?
                .crew_performance_snapshot(&runtime.workspace_id()?)
        })(self);
        match stored {
            Ok(snapshot) => {
                let types_hash =
                    orbit_config::crew_performance_types_hash(self.crew_performance_types());
                let unusable = orbit_store::contracts::usable_crew_performance_snapshot(
                    snapshot.clone(),
                    Utc::now(),
                    &types_hash,
                )
                .err();
                json!({
                    "mode": mode,
                    "snapshot": snapshot.map(|snapshot| json!({
                        "computed_at": snapshot.computed_at
                            .to_rfc3339_opts(SecondsFormat::Secs, true),
                        "window_days": snapshot.window_days,
                        "types": serde_json::from_str::<Value>(&snapshot.types_json)
                            .unwrap_or(Value::Null),
                        "types_hash": snapshot.types_hash,
                        "cells": snapshot.cells.len(),
                    })),
                    "unusable_reason": unusable.map(|reason| reason.as_str()),
                })
            }
            Err(error) => json!({
                "mode": mode,
                "snapshot": Value::Null,
                "unusable_reason": "unreadable",
                "error": error.to_string(),
            }),
        }
    }
}

/// One by-type cell JSON object as the persisted snapshot cell, so a stored
/// row is byte-for-byte the scorecard's cell for the same window and types.
pub(super) fn crew_performance_cell(
    cell: &Value,
) -> Result<orbit_store::contracts::CrewPerformanceCell, OrbitError> {
    let text = |key: &str| {
        cell[key].as_str().map(str::to_string).ok_or_else(|| {
            OrbitError::Execution(format!("crew performance cell missing `{key}`: {cell}"))
        })
    };
    let number = |key: &str| {
        cell[key].as_f64().ok_or_else(|| {
            OrbitError::Execution(format!("crew performance cell missing `{key}`: {cell}"))
        })
    };
    Ok(orbit_store::contracts::CrewPerformanceCell {
        crew: text("crew")?,
        model: text("model")?,
        tier: text("tier")?,
        task_type: text("type")?,
        n: cell["n"].as_u64().ok_or_else(|| {
            OrbitError::Execution(format!("crew performance cell missing `n`: {cell}"))
        })?,
        raw: number("raw")?,
        crew_mean: number("crew_mean")?,
        score: number("score")?,
    })
}
