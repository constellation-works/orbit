//! Friction family and rate metrics for the scoreboard and stats projection.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Datelike, TimeZone, Utc};
use orbit_common::OrbitError;
use orbit_types::identity::{
    all_agent_families, infer_agent_family_from_model, normalize_optional_attribution_label,
};
use orbit_types::task::{Task, TaskStatus};
use serde_json::{Value, json};

use super::{FrictionReportedCount, FrictionStore, stats};

impl FrictionStore {
    /// Friction counts by reporting model over an optional window, for the
    /// scoreboard. Bounded by distinct model labels.
    pub fn reported_by_model(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<FrictionReportedCount>, OrbitError> {
        self.store
            .with_read_connection(|conn| stats::counts_by_model(conn, &self.workspace_id, since))
    }

    /// The `orbit.friction.stats` projection, computed entirely from SQL
    /// aggregates plus the caller's task attribution.
    pub fn stats(&self, tasks: &[Task]) -> Result<Value, OrbitError> {
        let now = Utc::now();
        let month_start = Utc
            .with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
            .single()
            .ok_or_else(|| OrbitError::Store("compute current friction month".to_string()))?;
        let (next_year, next_month) = if now.month() == 12 {
            (now.year() + 1, 1)
        } else {
            (now.year(), now.month() + 1)
        };
        let next_month_start = Utc
            .with_ymd_and_hms(next_year, next_month, 1, 0, 0, 0)
            .single()
            .ok_or_else(|| OrbitError::Store("compute next friction month".to_string()))?;

        let (counts, resolved_this_month, by_model, by_tag_model) =
            self.store.with_read_connection(|conn| {
                Ok((
                    stats::status_counts(conn, &self.workspace_id)?,
                    stats::resolved_in_window(
                        conn,
                        &self.workspace_id,
                        month_start,
                        next_month_start,
                    )?,
                    stats::counts_by_model(conn, &self.workspace_id, None)?,
                    stats::counts_by_tag_and_model(conn, &self.workspace_id)?,
                ))
            })?;

        let tasks_done = completed_tasks_by_family(tasks);
        let mut frictions_by_family: BTreeMap<String, u64> = BTreeMap::new();
        let mut families = BTreeSet::new();
        for entry in &by_model {
            let family = friction_family_key(&entry.model);
            families.insert(family.clone());
            *frictions_by_family.entry(family).or_insert(0) += entry.count;
        }
        let mut frictions_by_tag_family: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
        for (tag, model, count) in by_tag_model {
            let family = friction_family_key(&model);
            families.insert(family.clone());
            *frictions_by_tag_family
                .entry(tag)
                .or_default()
                .entry(family)
                .or_insert(0) += count;
        }
        families.extend(tasks_done.keys().cloned());
        families.extend(known_family_keys());

        let mut by_family = serde_json::Map::new();
        for family in &families {
            let frictions = frictions_by_family.get(family).copied().unwrap_or(0);
            let done = tasks_done.get(family).copied().unwrap_or(0);
            by_family.insert(family.clone(), rate_row(frictions, done));
        }

        let mut by_tag = serde_json::Map::new();
        for (tag, by_family_counts) in frictions_by_tag_family {
            let mut tag_map = serde_json::Map::new();
            for family in &families {
                let frictions = by_family_counts.get(family).copied().unwrap_or(0);
                let done = tasks_done.get(family).copied().unwrap_or(0);
                tag_map.insert(family.clone(), rate_row(frictions, done));
            }
            by_tag.insert(tag, Value::Object(tag_map));
        }

        Ok(json!({
            "total": counts.total(),
            "open": counts.get("open"),
            "triaged": counts.get("triaged"),
            "resolved": counts.get("resolved"),
            "resolved_this_month": resolved_this_month,
            "by_family": Value::Object(by_family),
            "by_tag": Value::Object(by_tag),
        }))
    }
}

fn completed_tasks_by_family(tasks: &[Task]) -> BTreeMap<String, u64> {
    let mut counts = BTreeMap::new();
    for task in tasks {
        if !matches!(task.status, TaskStatus::Done | TaskStatus::Archived) {
            continue;
        }
        let Some(model) = normalize_optional_attribution_label(
            task.implemented_by.as_deref(),
            task.implemented_by.as_deref(),
        ) else {
            continue;
        };
        *counts.entry(friction_family_key(&model)).or_insert(0) += 1;
    }
    counts
}

pub(crate) fn friction_family_key(value: &str) -> String {
    let normalized = normalize_optional_attribution_label(Some(value), None).unwrap_or_default();
    infer_agent_family_from_model(&normalized).unwrap_or(normalized)
}

fn known_family_keys() -> impl Iterator<Item = String> {
    all_agent_families()
        .into_iter()
        .map(|family| family.to_string())
}

fn rate_row(frictions: u64, tasks_done: u64) -> Value {
    let rate = if tasks_done == 0 {
        json!("n/a")
    } else {
        let raw = (frictions as f64) * 10.0 / (tasks_done as f64);
        json!((raw * 10.0).round() / 10.0)
    };
    json!({
        "frictions": frictions,
        "tasks_done": tasks_done,
        "frictions_per_10_tasks": rate,
    })
}
