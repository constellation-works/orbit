//! Task relation targets that no longer resolve, and definition-artifact health
//! for skills, jobs, activities, auto-tasks and routines.

use std::collections::BTreeSet;

use orbit_core::application::health::artifact::ArtifactCondition;

use crate::doctor::*;

/// Task relation/dependency targets that no longer resolve to a registered
/// task bundle — the "grandfathered" relations that make a generated task
/// index fail to rebuild against its relation validator, so task reads serve
/// from a bundle scan (ORB-10305). Audited from the canonical bundles and
/// scoped to the current workspace: an edge the generated index lost is the
/// one blocking its repair, so it is reported (and marked) rather than hidden.
pub(in crate::doctor) fn doctor_check_task_relations(
    runtime: &OrbitRuntime,
) -> WorkspaceDoctorResult {
    let workspace_id = match runtime.workspace_id() {
        Ok(id) => id,
        Err(error) => {
            return check(
                "task-relations",
                WorkspaceDoctorStatus::Warning,
                format!("cannot resolve workspace id to audit relations: {error}"),
            );
        }
    };
    match runtime.audit_dangling_relations(Some(&workspace_id)) {
        Err(error) => check(
            "task-relations",
            WorkspaceDoctorStatus::Warning,
            format!("cannot audit task relations: {error}"),
        ),
        Ok(dangling) if dangling.is_empty() => check(
            "task-relations",
            WorkspaceDoctorStatus::Ok,
            "no unresolved relation/dependency targets".to_string(),
        ),
        Ok(dangling) => {
            let detail = dangling
                .iter()
                .map(|target| {
                    format!(
                        "{} ({}) -> {}{}",
                        target.source_task_id,
                        target.relation_type,
                        target.target_task_id,
                        if target.indexed {
                            ""
                        } else {
                            " [missing from generated index]"
                        }
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            actionable_check(
                "task-relations",
                WorkspaceDoctorStatus::Warning,
                format!(
                    "{} unresolved relation/dependency target(s) block task-index rebuild, so \
                     task reads fall back to a bundle scan until fixed or removed: {detail}",
                    dangling.len()
                ),
                "Inspect each named source with `orbit task show <task-id>`, then drop the unresolved edge from its `relations` through `orbit.task.update` or restore the target task; reads repair the index automatically once either lands.".to_string(),
            )
        }
    }
}

/// Definition-artifact health — one row per artifact kind (skills, jobs,
/// activities, auto-tasks, routines) [ORB-10800].
///
/// Severity is deliberately asymmetric. A workspace-authored definition that
/// fails to parse is the operator's own in-progress edit, and classifying it
/// as `Error` would silently start failing `orbit doctor` in cron and CI for
/// workspaces that were passing yesterday. Only an *unloadable shipped
/// default* — a broken install: Orbit-written content that no longer parses,
/// or a primary shipped default that is simply gone — escalates to `Error`.
/// Everything else warns.
pub(in crate::doctor) fn doctor_check_definition_artifacts(
    runtime: &OrbitRuntime,
) -> Vec<WorkspaceDoctorResult> {
    let report = match runtime.inspect_definition_artifacts() {
        Ok(report) => report,
        Err(error) => {
            return vec![actionable_check(
                "artifacts",
                WorkspaceDoctorStatus::Warning,
                format!("cannot inspect definition artifacts: {error}"),
                "Resolve the store/runtime error, then rerun `orbit doctor`.".to_string(),
            )];
        }
    };

    report
        .into_iter()
        .map(|health| {
            let check_name = format!("artifacts-{}", health.kind.as_str());
            if health.findings.is_empty() {
                let message = if health.scanned == 0 {
                    format!("no {} on disk yet", health.kind.as_str())
                } else {
                    format!(
                        "{} {} loaded, none residual, stale, forked, deprecated, faulty, or missing",
                        health.scanned,
                        health.kind.as_str()
                    )
                };
                let status = if health.scanned == 0 {
                    WorkspaceDoctorStatus::Skipped
                } else {
                    WorkspaceDoctorStatus::Ok
                };
                return check(&check_name, status, message);
            }

            let status = if health
                .findings
                .iter()
                .any(ArtifactFinding::is_unloadable_shipped_default)
            {
                WorkspaceDoctorStatus::Error
            } else {
                WorkspaceDoctorStatus::Warning
            };
            let detail = health
                .findings
                .iter()
                .map(|finding| format!("{}: {}", finding.condition.as_str(), finding.detail))
                .collect::<Vec<_>>()
                .join("; ");
            let attention_count = health
                .findings
                .iter()
                .map(|finding| finding.name.as_str())
                .collect::<BTreeSet<_>>()
                .len();
            let missing_count = health
                .findings
                .iter()
                .filter(|finding| finding.condition == ArtifactCondition::Missing)
                .map(|finding| finding.name.as_str())
                .collect::<BTreeSet<_>>()
                .len();
            // Missing shipped defaults are absent from `scanned`; include
            // them in the population. Keep the ratio bounded if a catalog
            // probe reports multiple findings for one scanned entry.
            let population = health
                .scanned
                .saturating_add(missing_count)
                .max(attention_count);
            let breakdown = [
                ArtifactCondition::Missing,
                ArtifactCondition::Stale,
                ArtifactCondition::Forked,
                ArtifactCondition::Faulty,
                ArtifactCondition::Residual,
                ArtifactCondition::Deprecated,
                ArtifactCondition::DanglingLink,
            ]
            .into_iter()
            .filter_map(|condition| {
                let count = health
                    .findings
                    .iter()
                    .filter(|finding| finding.condition == condition)
                    .map(|finding| finding.name.as_str())
                    .collect::<BTreeSet<_>>()
                    .len();
                (count > 0).then(|| format!("{count} {}", condition.as_str()))
            })
            .collect::<Vec<_>>()
            .join(", ");
            // Every finding carries its own exact repair command; dedupe so a
            // kind with five stale copies names one command, not five.
            let mut remediations: Vec<&str> = Vec::new();
            for finding in &health.findings {
                let remediation = finding.remediation.as_str();
                if !remediations.contains(&remediation) {
                    remediations.push(remediation);
                }
            }
            actionable_check(
                &check_name,
                status,
                format!(
                    "{attention_count} of {population} {} need attention ({breakdown}) — \
                     {detail}",
                    health.kind.as_str(),
                ),
                remediations.join(" "),
            )
        })
        .collect()
}
