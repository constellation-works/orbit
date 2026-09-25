//! Overlays of audit tool-call counts, friction counts and scoreboard metrics
//! onto per-agent summaries, keyed by normalized agent family.

use super::AgentSummary;
use super::types::FamilyScoreboard;
use crate::friction_store::FrictionReportedCount;
use crate::{AuditToolCallCountsByRole, AuditToolCallCountsBySurfaceAndRole};
use orbit_common::OrbitError;
use orbit_types::identity::{
    all_agent_families, infer_agent_family_from_model, normalize_attribution_label,
    normalize_optional_attribution_label,
};
use serde_json::Value;
use std::collections::BTreeMap;

pub(super) fn overlay_nested_metric(
    agents: &mut BTreeMap<String, AgentSummary>,
    scoreboard: &FamilyScoreboard,
    metric: &str,
    mut apply: impl FnMut(&mut AgentSummary, u64),
) {
    let Some(by_family) = scoreboard.get(metric) else {
        return;
    };

    for (family, value) in by_family {
        let summary = agents.entry(family_key(family)).or_default();
        apply(summary, *value);
    }
}

pub(super) fn overlay_audit_tool_calls_by_surface(
    agents: &mut BTreeMap<String, AgentSummary>,
    rows: &[AuditToolCallCountsBySurfaceAndRole],
) {
    for row in rows {
        let family = family_key(&row.role);
        if family.is_empty() {
            continue;
        }
        let summary = agents.entry(family).or_default();
        let entry = summary
            .tool_calls_by_surface
            .entry(row.surface.clone())
            .or_insert(0);
        *entry = entry.saturating_add(row.total);
    }
}

pub(super) fn overlay_audit_tool_calls(
    agents: &mut BTreeMap<String, AgentSummary>,
    audit_tool_calls: &[AuditToolCallCountsByRole],
) {
    let mut by_family: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for row in audit_tool_calls {
        let family = family_key(&row.role);
        if family.is_empty() {
            continue;
        }
        let entry = by_family.entry(family).or_default();
        entry.0 = entry.0.saturating_add(row.total);
        entry.1 = entry.1.saturating_add(row.failed);
    }

    for (family, (total, failed)) in by_family {
        let summary = agents.entry(family).or_default();
        // Total competes with token scoreboard data; failures only exist in audit rows.
        summary.tool_calls = summary.tool_calls.max(total);
        summary.failed_tool_calls = summary.failed_tool_calls.saturating_add(failed);
    }
}

/// Fold per-model friction counts into per-family agent rows. The caller has
/// already applied the window cutoff in SQL, so this only maps model labels to
/// families and sums collisions.
pub(super) fn overlay_friction_reported(
    agents: &mut BTreeMap<String, AgentSummary>,
    reported: &[FrictionReportedCount],
) {
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    for entry in reported {
        let family = {
            let normalized =
                normalize_optional_attribution_label(Some(&entry.model), None).unwrap_or_default();
            infer_agent_family_from_model(&normalized).unwrap_or(normalized)
        };
        *counts.entry(family).or_insert(0) += entry.count;
    }
    for (family, count) in counts {
        let summary = agents.entry(family).or_default();
        summary.friction.reported = count;
    }
}

pub(super) fn family_key(label: &str) -> String {
    let normalized = normalize_attribution_label(label, None);
    infer_agent_family_from_model(&normalized).unwrap_or(normalized)
}

pub(super) fn seed_known_family_agents(agents: &mut BTreeMap<String, AgentSummary>) {
    for family in all_agent_families() {
        agents.entry(family.to_string()).or_default();
    }
}

pub(super) fn normalize_model_scoreboard(parsed: Value) -> Result<FamilyScoreboard, OrbitError> {
    let mut normalized = FamilyScoreboard::new();
    let Value::Object(metrics) = parsed else {
        return Err(OrbitError::Io(
            "scoreboard json must be an object".to_string(),
        ));
    };

    for (metric, metric_value) in metrics {
        let Value::Object(entries) = metric_value else {
            continue;
        };
        let family_entries = normalized.entry(metric).or_default();
        for (first_key, first_value) in entries {
            match first_value {
                Value::Number(number) => {
                    let value = number.as_u64().ok_or_else(|| {
                        OrbitError::Io("scoreboard counter must be u64".to_string())
                    })?;
                    *family_entries.entry(family_key(&first_key)).or_insert(0) += value;
                }
                Value::Object(inner) => {
                    for (family, value) in inner {
                        let Value::Number(number) = value else {
                            continue;
                        };
                        let count = number.as_u64().ok_or_else(|| {
                            OrbitError::Io("scoreboard counter must be u64".to_string())
                        })?;
                        *family_entries.entry(family_key(&family)).or_insert(0) += count;
                    }
                }
                _ => {}
            }
        }
    }

    Ok(normalized)
}
