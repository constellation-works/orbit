use std::borrow::Cow;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;

use crate::OrbitRuntime;
use crate::skill_catalog::{LoadedSkill, SkillCatalogDoctorStatus};

use super::managed_assets::{
    ManagedAssetLayout, ManagedAssetReconciliation, reconcile_managed_assets,
};

/// The client skill-discovery directories under `base_root`.
pub(crate) fn skill_link_roots(base_root: &Path) -> Vec<PathBuf> {
    [".agents", ".claude"]
        .into_iter()
        .map(|dir| base_root.join(dir).join("skills"))
        .collect()
}

/// Every shipped skill file, keyed by its path relative to the skills root.
///
/// Skills are directory trees rather than single documents, so their managed
/// manifest keys on the relative path ([`ManagedAssetLayout::RelativePath`])
/// instead of a bare definition name.
///
/// Routers separate everyday task work, backlog orchestration and machine setup.
/// References load on demand and may link across the bundled skill trees. The
/// ordering below groups each skill's files under its router, in the order its
/// reference table presents them.
pub(crate) const DEFAULT_SKILL_FILES: &[(&str, &str)] = &[
    // Everyday task work: the router, its references in table order, then its evaluation cases.
    (
        "orbit/SKILL.md",
        include_str!("../../assets/skills/orbit/SKILL.md"),
    ),
    (
        "orbit/references/task-execution-steps.md",
        include_str!("../../assets/skills/orbit/references/task-execution-steps.md"),
    ),
    (
        "orbit/references/task-execution-handoff-cases.md",
        include_str!("../../assets/skills/orbit/references/task-execution-handoff-cases.md"),
    ),
    (
        "orbit/references/task-execution-post-deploy.md",
        include_str!("../../assets/skills/orbit/references/task-execution-post-deploy.md"),
    ),
    (
        "orbit/references/task-execution-pr-sections.md",
        include_str!("../../assets/skills/orbit/references/task-execution-pr-sections.md"),
    ),
    (
        "orbit/references/task-execution-ui-checks.md",
        include_str!("../../assets/skills/orbit/references/task-execution-ui-checks.md"),
    ),
    (
        "orbit/references/task-fields-projections.md",
        include_str!("../../assets/skills/orbit/references/task-fields-projections.md"),
    ),
    (
        "orbit/references/task-fields-relations.md",
        include_str!("../../assets/skills/orbit/references/task-fields-relations.md"),
    ),
    (
        "orbit/references/task-fields-tools.md",
        include_str!("../../assets/skills/orbit/references/task-fields-tools.md"),
    ),
    (
        "orbit/references/task-fields-placement.md",
        include_str!("../../assets/skills/orbit/references/task-fields-placement.md"),
    ),
    (
        "orbit/references/task-fields-review.md",
        include_str!("../../assets/skills/orbit/references/task-fields-review.md"),
    ),
    (
        "orbit/references/task-fields-validation.md",
        include_str!("../../assets/skills/orbit/references/task-fields-validation.md"),
    ),
    (
        "orbit/references/task-authoring-contract.md",
        include_str!("../../assets/skills/orbit/references/task-authoring-contract.md"),
    ),
    (
        "orbit/references/task-authoring-create.md",
        include_str!("../../assets/skills/orbit/references/task-authoring-create.md"),
    ),
    (
        "orbit/references/task-authoring-footprint.md",
        include_str!("../../assets/skills/orbit/references/task-authoring-footprint.md"),
    ),
    (
        "orbit/references/task-authoring-new-files.md",
        include_str!("../../assets/skills/orbit/references/task-authoring-new-files.md"),
    ),
    (
        "orbit/references/task-authoring-tags.md",
        include_str!("../../assets/skills/orbit/references/task-authoring-tags.md"),
    ),
    (
        "orbit/references/task-review.md",
        include_str!("../../assets/skills/orbit/references/task-review.md"),
    ),
    (
        "orbit/references/search.md",
        include_str!("../../assets/skills/orbit/references/search.md"),
    ),
    (
        "orbit/references/concepts.md",
        include_str!("../../assets/skills/orbit/references/concepts.md"),
    ),
    (
        "orbit/references/friction.md",
        include_str!("../../assets/skills/orbit/references/friction.md"),
    ),
    (
        "orbit/references/tool-surface-routing.md",
        include_str!("../../assets/skills/orbit/references/tool-surface-routing.md"),
    ),
    (
        "orbit/references/tool-surface-capability-map.md",
        include_str!("../../assets/skills/orbit/references/tool-surface-capability-map.md"),
    ),
    (
        "orbit/references/tool-surface-registry.md",
        include_str!("../../assets/skills/orbit/references/tool-surface-registry.md"),
    ),
    (
        "orbit/references/tool-surface-mcp-serve.md",
        include_str!("../../assets/skills/orbit/references/tool-surface-mcp-serve.md"),
    ),
    (
        "orbit/references/tool-surface-evolution.md",
        include_str!("../../assets/skills/orbit/references/tool-surface-evolution.md"),
    ),
    (
        "orbit/references/tool-surface-codebase.md",
        include_str!("../../assets/skills/orbit/references/tool-surface-codebase.md"),
    ),
    (
        "orbit/references/tool-surface-pilot.md",
        include_str!("../../assets/skills/orbit/references/tool-surface-pilot.md"),
    ),
    (
        "orbit/references/tool-surface-decisions.md",
        include_str!("../../assets/skills/orbit/references/tool-surface-decisions.md"),
    ),
    (
        "orbit/references/tool-surface-scorecards.md",
        include_str!("../../assets/skills/orbit/references/tool-surface-scorecards.md"),
    ),
    (
        "orbit/references/tool-surface-notices.md",
        include_str!("../../assets/skills/orbit/references/tool-surface-notices.md"),
    ),
    (
        "orbit/references/attention-reading.md",
        include_str!("../../assets/skills/orbit/references/attention-reading.md"),
    ),
    (
        "orbit/references/attention-kinds.md",
        include_str!("../../assets/skills/orbit/references/attention-kinds.md"),
    ),
    (
        "orbit/references/manager-mandates.md",
        include_str!("../../assets/skills/orbit/references/manager-mandates.md"),
    ),
    (
        "orbit/references/manager-oversight.md",
        include_str!("../../assets/skills/orbit/references/manager-oversight.md"),
    ),
    (
        "orbit/references/resident-cycle-act.md",
        include_str!("../../assets/skills/orbit/references/resident-cycle-act.md"),
    ),
    (
        "orbit/references/resident-cycle-escalate.md",
        include_str!("../../assets/skills/orbit/references/resident-cycle-escalate.md"),
    ),
    (
        "orbit/references/setup/distributed-drain-setup.md",
        include_str!("../../assets/skills/orbit/references/setup/distributed-drain-setup.md"),
    ),
    (
        "orbit/references/setup/distributed-drain-follower.md",
        include_str!("../../assets/skills/orbit/references/setup/distributed-drain-follower.md"),
    ),
    (
        "orbit/references/setup/distributed-drain-owner-surface.md",
        include_str!(
            "../../assets/skills/orbit/references/setup/distributed-drain-owner-surface.md"
        ),
    ),
    (
        "orbit/references/setup/distributed-drain-recovery.md",
        include_str!("../../assets/skills/orbit/references/setup/distributed-drain-recovery.md"),
    ),
    (
        "orbit/references/desktop-navigation.md",
        include_str!("../../assets/skills/orbit/references/desktop-navigation.md"),
    ),
    (
        "orbit/evals/evals.json",
        include_str!("../../assets/skills/orbit/evals/evals.json"),
    ),
    // The orchestrator's operating loop, layered on the primitives above.
    (
        "orbit-orchestrate/SKILL.md",
        include_str!("../../assets/skills/orbit-orchestrate/SKILL.md"),
    ),
    (
        "orbit-orchestrate/evals/evals.json",
        include_str!("../../assets/skills/orbit-orchestrate/evals/evals.json"),
    ),
    (
        "orbit-orchestrate/references/authorization.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/authorization.md"),
    ),
    (
        "orbit-orchestrate/references/common-failures.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/common-failures.md"),
    ),
    (
        "orbit-orchestrate/references/loop-author.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/loop-author.md"),
    ),
    (
        "orbit-orchestrate/references/loop-crew.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/loop-crew.md"),
    ),
    (
        "orbit-orchestrate/references/loop-inspect.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/loop-inspect.md"),
    ),
    (
        "orbit-orchestrate/references/loop-promote.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/loop-promote.md"),
    ),
    (
        "orbit-orchestrate/references/loop-stalled-prs.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/loop-stalled-prs.md"),
    ),
    (
        "orbit-orchestrate/references/mid-run-steering.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/mid-run-steering.md"),
    ),
    (
        "orbit-orchestrate/references/model-ranking.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/model-ranking.md"),
    ),
    (
        "orbit-orchestrate/references/orchestration-analytics-windows.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/orchestration-analytics-windows.md"
        ),
    ),
    (
        "orbit-orchestrate/references/orchestration-auto-fix.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/orchestration-auto-fix.md"),
    ),
    (
        "orbit-orchestrate/references/orchestration-backlog-forecast.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/orchestration-backlog-forecast.md"
        ),
    ),
    (
        "orbit-orchestrate/references/orchestration-completion.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/orchestration-completion.md"
        ),
    ),
    (
        "orbit-orchestrate/references/orchestration-dispatch.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/orchestration-dispatch.md"),
    ),
    (
        "orbit-orchestrate/references/orchestration-drain-control.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/orchestration-drain-control.md"
        ),
    ),
    (
        "orbit-orchestrate/references/orchestration-drain-keeper.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/orchestration-drain-keeper.md"
        ),
    ),
    (
        "orbit-orchestrate/references/orchestration-drain-routing.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/orchestration-drain-routing.md"
        ),
    ),
    (
        "orbit-orchestrate/references/orchestration-failed-runs.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/orchestration-failed-runs.md"
        ),
    ),
    (
        "orbit-orchestrate/references/orchestration-per-run-review.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/orchestration-per-run-review.md"
        ),
    ),
    (
        "orbit-orchestrate/references/orchestration-pilot-admission.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/orchestration-pilot-admission.md"
        ),
    ),
    (
        "orbit-orchestrate/references/orchestration-pilot-apply.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/orchestration-pilot-apply.md"
        ),
    ),
    (
        "orbit-orchestrate/references/orchestration-proposed-approval.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/orchestration-proposed-approval.md"
        ),
    ),
    (
        "orbit-orchestrate/references/orchestration-readiness.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/orchestration-readiness.md"),
    ),
    (
        "orbit-orchestrate/references/orchestration-reservations.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/orchestration-reservations.md"
        ),
    ),
    (
        "orbit-orchestrate/references/orchestration-resource-forecast.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/orchestration-resource-forecast.md"
        ),
    ),
    (
        "orbit-orchestrate/references/orchestration-retry-landing.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/orchestration-retry-landing.md"
        ),
    ),
    (
        "orbit-orchestrate/references/orchestration-unattended.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/orchestration-unattended.md"
        ),
    ),
    (
        "orbit-orchestrate/references/recovery-blocked-run.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/recovery-blocked-run.md"),
    ),
    (
        "orbit-orchestrate/references/recovery-ci-findings.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/recovery-ci-findings.md"),
    ),
    (
        "orbit-orchestrate/references/recovery-deployment.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/recovery-deployment.md"),
    ),
    (
        "orbit-orchestrate/references/recovery-pr-completion.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/recovery-pr-completion.md"),
    ),
    (
        "orbit-orchestrate/references/recovery-repair-authority.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/recovery-repair-authority.md"
        ),
    ),
    (
        "orbit-orchestrate/references/recovery-salvage-candidate.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/recovery-salvage-candidate.md"
        ),
    ),
    (
        "orbit-orchestrate/references/recovery-settle-rescued-task.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/recovery-settle-rescued-task.md"
        ),
    ),
    (
        "orbit-orchestrate/references/recovery-source-repair.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/recovery-source-repair.md"),
    ),
    (
        "orbit-orchestrate/references/run-debugging-agent-processes.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/run-debugging-agent-processes.md"
        ),
    ),
    (
        "orbit-orchestrate/references/run-debugging-audit-output.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/run-debugging-audit-output.md"
        ),
    ),
    (
        "orbit-orchestrate/references/run-debugging-audit-trail.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/run-debugging-audit-trail.md"
        ),
    ),
    (
        "orbit-orchestrate/references/run-debugging-decisions.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/run-debugging-decisions.md"),
    ),
    (
        "orbit-orchestrate/references/run-debugging-delivery-holds.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/run-debugging-delivery-holds.md"
        ),
    ),
    (
        "orbit-orchestrate/references/run-debugging-git-state.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/run-debugging-git-state.md"),
    ),
    (
        "orbit-orchestrate/references/run-debugging-history.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/run-debugging-history.md"),
    ),
    (
        "orbit-orchestrate/references/run-debugging-hosted-checks.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/run-debugging-hosted-checks.md"
        ),
    ),
    (
        "orbit-orchestrate/references/run-debugging-live-processes.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/run-debugging-live-processes.md"
        ),
    ),
    (
        "orbit-orchestrate/references/run-debugging-no-edit-rerun.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/run-debugging-no-edit-rerun.md"
        ),
    ),
    (
        "orbit-orchestrate/references/run-debugging-recovery-evidence.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/run-debugging-recovery-evidence.md"
        ),
    ),
    (
        "orbit-orchestrate/references/run-debugging-report.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/run-debugging-report.md"),
    ),
    (
        "orbit-orchestrate/references/run-debugging-response-correction.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/run-debugging-response-correction.md"
        ),
    ),
    (
        "orbit-orchestrate/references/run-debugging-review-delivery.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/run-debugging-review-delivery.md"
        ),
    ),
    (
        "orbit-orchestrate/references/run-debugging-task-state.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/run-debugging-task-state.md"
        ),
    ),
    (
        "orbit-orchestrate/references/run-debugging-triage.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/run-debugging-triage.md"),
    ),
    (
        "orbit-orchestrate/references/run-debugging-upgrade-refusals.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/run-debugging-upgrade-refusals.md"
        ),
    ),
    (
        "orbit-orchestrate/references/run-debugging-validation-replay.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/run-debugging-validation-replay.md"
        ),
    ),
    (
        "orbit-orchestrate/references/run-debugging-whole-run-replay.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/run-debugging-whole-run-replay.md"
        ),
    ),
    (
        "orbit-orchestrate/references/walkthroughs.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/walkthroughs.md"),
    ),
    (
        "orbit-orchestrate/references/workflows-auto-tasks.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/workflows-auto-tasks.md"),
    ),
    (
        "orbit-orchestrate/references/workflows-cancel-and-diagnose.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/workflows-cancel-and-diagnose.md"
        ),
    ),
    (
        "orbit-orchestrate/references/workflows-ci-log-evidence.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/workflows-ci-log-evidence.md"
        ),
    ),
    (
        "orbit-orchestrate/references/workflows-ci-reassessment.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/workflows-ci-reassessment.md"
        ),
    ),
    (
        "orbit-orchestrate/references/workflows-ci-sweep.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/workflows-ci-sweep.md"),
    ),
    (
        "orbit-orchestrate/references/workflows-custom-jobs.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/workflows-custom-jobs.md"),
    ),
    (
        "orbit-orchestrate/references/workflows-delivery-inputs.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/workflows-delivery-inputs.md"
        ),
    ),
    (
        "orbit-orchestrate/references/workflows-pilot-holds.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/workflows-pilot-holds.md"),
    ),
    (
        "orbit-orchestrate/references/workflows-preserved-candidate.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/workflows-preserved-candidate.md"
        ),
    ),
    (
        "orbit-orchestrate/references/workflows-run-jobs.md",
        include_str!("../../assets/skills/orbit-orchestrate/references/workflows-run-jobs.md"),
    ),
    (
        "orbit-orchestrate/references/workflows-shipped-pipelines.md",
        include_str!(
            "../../assets/skills/orbit-orchestrate/references/workflows-shipped-pipelines.md"
        ),
    ),
    (
        "orbit-setup/SKILL.md",
        include_str!("../../assets/skills/orbit-setup/SKILL.md"),
    ),
    (
        "orbit-setup/references/first-run.md",
        include_str!("../../assets/skills/orbit-setup/references/first-run.md"),
    ),
    (
        "orbit-setup/references/providers.md",
        include_str!("../../assets/skills/orbit-setup/references/providers.md"),
    ),
    (
        "orbit-setup/references/workspace-tuning.md",
        include_str!("../../assets/skills/orbit-setup/references/workspace-tuning.md"),
    ),
    (
        "orbit-setup/references/configuration.md",
        include_str!("../../assets/skills/orbit-setup/references/configuration.md"),
    ),
    (
        "orbit-setup/references/linux-sandbox.md",
        include_str!("../../assets/skills/orbit-setup/references/linux-sandbox.md"),
    ),
    (
        "orbit-setup/references/windows-wsl2.md",
        include_str!("../../assets/skills/orbit-setup/references/windows-wsl2.md"),
    ),
    (
        "orbit-setup/references/macos.md",
        include_str!("../../assets/skills/orbit-setup/references/macos.md"),
    ),
    (
        "orbit-setup/references/browser-checks.md",
        include_str!("../../assets/skills/orbit-setup/references/browser-checks.md"),
    ),
    (
        "orbit-setup/references/automation.md",
        include_str!("../../assets/skills/orbit-setup/references/automation.md"),
    ),
    (
        "orbit-setup/references/auto-tasks.md",
        include_str!("../../assets/skills/orbit-setup/references/auto-tasks.md"),
    ),
    (
        "orbit-setup/references/plugins.md",
        include_str!("../../assets/skills/orbit-setup/references/plugins.md"),
    ),
    (
        "orbit-setup/references/applications.md",
        include_str!("../../assets/skills/orbit-setup/references/applications.md"),
    ),
    (
        "orbit-setup/references/remote-access.md",
        include_str!("../../assets/skills/orbit-setup/references/remote-access.md"),
    ),
    (
        "orbit-setup/references/multi-host.md",
        include_str!("../../assets/skills/orbit-setup/references/multi-host.md"),
    ),
    (
        "orbit-setup/references/publication.md",
        include_str!("../../assets/skills/orbit-setup/references/publication.md"),
    ),
    (
        "orbit-setup/references/maintenance.md",
        include_str!("../../assets/skills/orbit-setup/references/maintenance.md"),
    ),
    (
        "orbit-setup/references/operational-logs.md",
        include_str!("../../assets/skills/orbit-setup/references/operational-logs.md"),
    ),
];

/// The `SKILL.md` entry point of every shipped skill, as `(id, content)`.
/// A skill id is the first path component of its managed asset paths.
pub(crate) fn default_skill_files() -> Vec<(&'static str, &'static str)> {
    DEFAULT_SKILL_FILES
        .iter()
        .filter_map(|(relative, content)| {
            relative.strip_suffix("/SKILL.md").map(|id| (id, *content))
        })
        .collect()
}

use crate::paths::ORBIT_ROOT_TOKEN;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillDoctorStatus {
    Ok,
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub struct SkillDoctorResult {
    pub skill_name: String,
    pub status: SkillDoctorStatus,
    pub message: String,
}

pub(crate) fn default_skill_ids() -> Vec<&'static str> {
    default_skill_files()
        .into_iter()
        .map(|(id, _)| id)
        .collect()
}

/// Materialize the shipped skill trees under `skills_root`, recording the
/// digest Orbit wrote for each file so a skill (or a single reference file)
/// dropped from a later release can be retired by content provenance.
///
/// The digest covers the *rendered* document: `ORBIT_ROOT_TOKEN` resolves to
/// the absolute root before the write, so an unchanged release re-seeds as a
/// no-op on the same root.
// ADR-0366: skills are managed by relative path, so a single reference
// file can be retired independently of its SKILL.md.
pub(crate) fn seed_default_skills(
    skills_root: &Path,
    orbit_root: &Path,
    overwrite: bool,
) -> Result<ManagedAssetReconciliation, OrbitError> {
    reconcile_managed_assets(
        skills_root,
        "skill",
        ManagedAssetLayout::RelativePath,
        DEFAULT_SKILL_FILES,
        overwrite,
        |_, content| {
            Ok(Cow::Owned(inject_skill_template_tokens(
                content, orbit_root,
            )))
        },
    )
}

/// A legacy workspace skill may be removed only when every entry in its tree
/// is a shipped file with unchanged bytes. Missing shipped references are
/// allowed because older releases seeded smaller trees; unknown entries and
/// symlinks are treated as operator-owned content.
pub(crate) fn is_default_skill_tree_for_root(
    skill_id: &str,
    skill_dir: &Path,
    orbit_root: &Path,
) -> Result<bool, OrbitError> {
    let prefix = format!("{skill_id}/");
    let expected: Vec<_> = DEFAULT_SKILL_FILES
        .iter()
        .filter_map(|(path, content)| path.strip_prefix(&prefix).map(|path| (path, *content)))
        .collect();
    if expected.is_empty() {
        return Ok(false);
    }
    let metadata = match std::fs::symlink_metadata(skill_dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(OrbitError::Io(error.to_string())),
    };
    if !metadata.file_type().is_dir() {
        return Ok(false);
    }

    let mut found_router = false;
    let mut pending = vec![skill_dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).map_err(|e| OrbitError::Io(e.to_string()))? {
            let entry = entry.map_err(|e| OrbitError::Io(e.to_string()))?;
            let path = entry.path();
            let relative = path
                .strip_prefix(skill_dir)
                .map_err(|e| OrbitError::Io(e.to_string()))?;
            let file_type = entry
                .file_type()
                .map_err(|e| OrbitError::Io(e.to_string()))?;
            if file_type.is_dir() {
                if !expected
                    .iter()
                    .any(|(name, _)| Path::new(name).starts_with(relative))
                {
                    return Ok(false);
                }
                pending.push(path);
            } else if file_type.is_file() {
                let Some((_, content)) = expected
                    .iter()
                    .find(|(name, _)| Path::new(name) == relative)
                else {
                    return Ok(false);
                };
                let existing = std::fs::read(&path).map_err(|e| OrbitError::Io(e.to_string()))?;
                if existing != inject_skill_template_tokens(content, orbit_root).as_bytes() {
                    return Ok(false);
                }
                found_router |= relative == Path::new("SKILL.md");
            } else {
                return Ok(false);
            }
        }
    }
    Ok(found_router)
}

pub(crate) fn inject_skill_template_tokens(raw: &str, orbit_root: &Path) -> String {
    let orbit_root_value = orbit_root.to_string_lossy();
    raw.replace(ORBIT_ROOT_TOKEN, orbit_root_value.as_ref())
}

impl OrbitRuntime {
    pub fn list_file_skills(&self) -> Result<Vec<LoadedSkill>, OrbitError> {
        self.skill_catalog().list()
    }

    pub fn show_file_skill(&self, name: &str) -> Result<LoadedSkill, OrbitError> {
        self.skill_catalog().load(name)
    }

    pub fn doctor_file_skills(&self) -> Result<Vec<SkillDoctorResult>, OrbitError> {
        let rows = self.skill_catalog().doctor()?;
        let mut results: Vec<SkillDoctorResult> = rows
            .into_iter()
            .map(|row| SkillDoctorResult {
                skill_name: row.skill_id,
                status: match row.status {
                    SkillCatalogDoctorStatus::Ok => SkillDoctorStatus::Ok,
                    SkillCatalogDoctorStatus::Warning => SkillDoctorStatus::Warning,
                    SkillCatalogDoctorStatus::Error => SkillDoctorStatus::Error,
                },
                message: row.message,
            })
            .collect();
        let global_root = self.global_root();
        if let Some(discovery_base) = global_root.parent() {
            results.extend(doctor_client_skill_links(&skill_link_roots(
                discovery_base,
            ))?);
        }
        Ok(results)
    }
}

/// Report dangling/orphaned client skill symlinks under the agent
/// discovery directories beside the selected global root.
///
/// Catalog doctor only walks seeded skill trees. Client CLIs discover
/// skills through these link dirs, so a leftover after a default-set
/// shrink is invisible unless this pass inspects them.
pub(crate) fn doctor_client_skill_links(
    skills_links_dirs: &[PathBuf],
) -> Result<Vec<SkillDoctorResult>, OrbitError> {
    Ok(dangling_client_skill_links(skills_links_dirs)?
        .into_iter()
        .map(|path| SkillDoctorResult {
            skill_name: path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("?")
                .to_string(),
            status: SkillDoctorStatus::Warning,
            message: format!(
                "dangling skill link at {} (target missing). {}",
                path.display(),
                skill_link_remediation(&path),
            ),
        })
        .collect())
}

pub(crate) fn skill_link_remediation(path: &Path) -> String {
    format!(
        "Restore the skill target or manually remove the dangling symlink at `{}`.",
        path.display(),
    )
}

/// Inspect only immediate symlinks, without modifying discovery entries or targets.
pub(crate) fn dangling_client_skill_links(
    skills_links_dirs: &[PathBuf],
) -> Result<Vec<PathBuf>, OrbitError> {
    let mut links = Vec::new();
    for dir in skills_links_dirs {
        if !dir.exists() {
            continue;
        }
        let mut paths = Vec::new();
        for entry in std::fs::read_dir(dir).map_err(|e| OrbitError::Io(e.to_string()))? {
            paths.push(entry.map_err(|e| OrbitError::Io(e.to_string()))?.path());
        }
        paths.sort();
        for path in paths {
            let meta =
                std::fs::symlink_metadata(&path).map_err(|e| OrbitError::Io(e.to_string()))?;
            if !meta.file_type().is_symlink() {
                continue;
            }
            if path.exists() {
                continue;
            }
            links.push(path);
        }
    }
    Ok(links)
}
