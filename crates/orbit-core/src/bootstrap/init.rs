use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_store::compose::{global_executor_def_store, global_policy_def_store};
use orbit_store::friction_store;
use orbit_types::workspace::WorkspacePaths;

use crate::OrbitRuntime;
use crate::application::MANAGED_ASSET_MANIFEST_FILE;
use crate::application::executor::seed_default_executors;
use crate::application::job::seed_default_jobs;
use crate::application::routine::RoutineSeedIdentity;
use crate::application::skill::{
    default_skill_ids, is_default_skill_file_for_root, seed_default_skills,
};
use crate::application::workspace_sync::{
    ManagedArtifactOutcome, reconcile_workspace_managed_artifacts,
};
use crate::bootstrap::activity::seed_default_activities;
use crate::bootstrap::global_defaults::{
    global_defaults_are_current, record_global_defaults_reconciled,
};
use crate::bootstrap::policy::seed_default_policies;
use orbit_common::fs::io::{create_dir_symlink, remove_path_if_exists};

use crate::runtime::{is_global_orbit_root, resolve_global_root};
use orbit_config::{ConfigRoots, ConfigSeed, ResolvedConfig, seed_default_config};

const LEGACY_WORKSPACE_SEEDED_SKILL_IDS: [&str; 2] = ["orbit-approve-task", "orbit-pr"];

#[derive(Debug, Clone)]
pub struct InitResult {
    pub refreshed_skill_files: usize,
    pub created_skills_symlink: bool,
    pub created_config: bool,
    pub refreshed_default_activities: usize,
    pub retired_default_activities: usize,
    pub refreshed_default_jobs: usize,
    pub retired_default_jobs: usize,
    pub managed_asset_warnings: Vec<String>,
    pub refreshed_default_executors: usize,
    pub refreshed_default_policies: usize,
    pub refreshed_default_routines: usize,
    pub seeded_default_auto_tasks: usize,
}

#[derive(Debug, Clone, Default)]
pub struct InitOptions {
    pub force: bool,
    /// When true, always overwrite default skill files even if
    /// they already exist.  Explicit `orbit init` sets this; implicit
    /// bootstrap from other commands does not.
    pub refresh_defaults: bool,
    /// When true, seed only the globally scoped resource sets and skip
    /// workspace-local layout concerns like skills, tasks, and state.
    pub global_only: bool,
    /// Explicit global root to seed when preparing a workspace root.
    pub global_root_override: Option<PathBuf>,
    /// Host and registered workspace name to materialize newly seeded
    /// workspace routines against. Higher-level composition owns both halves
    /// of that identity and supplies them explicitly; `None` skips routine
    /// seeding entirely.
    pub routine_seed_identity: Option<RoutineSeedIdentity>,
    /// When true, create/update user-level skill symlinks for global skills.
    pub link_global_skills: bool,
    /// Explicit inputs for seeding a fresh `config.toml`: which provider
    /// families this host can dispatch to, plus any crew assignments the init
    /// prompt collected. Core never probes the host for these — the CLI init
    /// adapter owns detection and prompting and passes the result down.
    ///
    /// `None` seeds the static template alone, so config loading falls back to
    /// the built-in crew registry. Ignored when config.toml already exists —
    /// init remains idempotent.
    pub config_seed: Option<ConfigSeed>,
}

impl OrbitRuntime {
    pub fn init_workspace_with_options(
        &self,
        options: InitOptions,
    ) -> Result<InitResult, OrbitError> {
        init_workspace_at_root(&self.data_root(), options)
    }
}

/// Ensures both global and workspace roots are bootstrapped.
/// Global root gets config plus all globally scoped resource defaults.
/// Workspace root gets only workspace-local layout and runtime state dirs.
///
/// This runs on every runtime open, so it deliberately supplies no
/// [`InitOptions::config_seed`]: implicit bootstrap has no operator present to
/// detect a host for, and probing `PATH` here would cost every runtime open a
/// filesystem scan. A config seeded this way carries no `[crews]` table, so it
/// resolves to the built-in crew registry until an explicit `orbit init`
/// freezes the host's detected families. [ORB-10885]
///
/// Seeding the global defaults renders, hashes, and re-reads every managed
/// asset, which is worth paying for exactly once per asset set. A warm open
/// therefore skips it entirely once the root carries this binary's stamp; see
/// [`crate::bootstrap::global_defaults`] for what that stamp does and does not
/// claim. The workspace side stays unconditional: it only creates directories
/// and reaps skill trees that older releases seeded into a workspace root.
pub(crate) fn ensure_orbit_root_initialized(
    global_root: &Path,
    workspace_root: &Path,
) -> Result<(), OrbitError> {
    if !global_defaults_are_current(global_root) {
        let global_init = init_workspace_at_root(
            global_root,
            InitOptions {
                global_only: true,
                ..Default::default()
            },
        );
        ignore_denied_implicit_bootstrap_write("global defaults", global_root, global_init)?;
    }

    let workspace_layout = prepare_workspace_root_layout(workspace_root, global_root);
    ignore_denied_implicit_bootstrap_write("workspace layout", workspace_root, workspace_layout)?;
    if ResolvedConfig::load(&ConfigRoots::global_only(global_root))?.scoring_enabled {
        let scoreboard = seed_scoreboard_templates(workspace_root);
        ignore_denied_implicit_bootstrap_write("scoreboard templates", workspace_root, scoreboard)?;
    }
    Ok(())
}

fn ignore_denied_implicit_bootstrap_write<T>(
    component: &str,
    root: &Path,
    result: Result<T, OrbitError>,
) -> Result<Option<T>, OrbitError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.is_readonly_or_access_failure() => {
            tracing::warn!(
                target: "orbit.core.bootstrap",
                component,
                root = %root.display(),
                error = %error,
                "skipped incidental runtime bootstrap persistence"
            );
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// Initialize the global `~/.orbit/` root. Always targets `~/.orbit/`
/// regardless of cwd, unless `--root` override is provided.
pub fn init_global(
    root_override: Option<&Path>,
    options: InitOptions,
) -> Result<InitResult, OrbitError> {
    let global_root = match root_override {
        Some(root) => root.to_path_buf(),
        None => resolve_global_root()?,
    };
    init_workspace_at_root(
        &global_root,
        InitOptions {
            global_only: true,
            link_global_skills: true,
            ..options
        },
    )
}

pub fn init_workspace_at_root(
    orbit_root: &Path,
    options: InitOptions,
) -> Result<InitResult, OrbitError> {
    let init_target = resolve_init_target_from_root(orbit_root);
    let orbit_root = init_target.orbit_root.clone();

    if options.force {
        remove_path_if_exists(&orbit_root)?;
    }
    // The workspace branch needs its global root before laying out the
    // workspace: skill reaping must know which catalog is the live global one.
    let workspace_global_root = if options.global_only {
        None
    } else {
        Some(
            options
                .global_root_override
                .clone()
                .map_or_else(resolve_global_root, Ok::<PathBuf, OrbitError>)?,
        )
    };
    let layout = match workspace_global_root.as_deref() {
        None => prepare_global_root_layout(&orbit_root)?,
        Some(global_root) => prepare_workspace_root_layout(&orbit_root, global_root)?,
    };
    let skills_root = if options.global_only {
        global_skills_dir(&orbit_root)
    } else {
        layout.skills_dir.clone()
    };

    let overwrite = options.force || options.refresh_defaults;
    let mut skill_asset_warnings: Vec<String> = Vec::new();
    let mut refreshed_skill_files = if options.global_only {
        let reconciliation = seed_default_skills(&skills_root, &orbit_root, overwrite)?;
        skill_asset_warnings = reconciliation.warnings;
        reconciliation.refreshed
    } else {
        0
    };
    let created_config = if options.global_only {
        let config_path = orbit_root.join("config.toml");
        seed_default_config(&config_path, options.config_seed.as_ref())?
    } else {
        false
    };

    let skill_ids = default_skill_ids();
    let mut created_skills_symlink = false;
    // Home-scoped skill link directories (`~/.agents/skills`, `~/.claude/skills`)
    // are only ever touched for the true global Orbit root. A non-global root
    // (a validation root, an alternate `--root`, a workspace root) must leave
    // them exactly as found — no removal, no re-creation, no replacement.
    if options.global_only && options.link_global_skills && is_global_orbit_root(&orbit_root) {
        for skills_links_root in &init_target.skills_links_roots {
            created_skills_symlink |=
                ensure_skill_links(&skills_root, &skill_ids, skills_links_root, options.force)?;
        }
    }

    let mut refreshed_default_routines = 0usize;
    let mut seeded_default_auto_tasks = 0usize;
    let (
        refreshed_default_activities,
        retired_default_activities,
        refreshed_default_jobs,
        retired_default_jobs,
        managed_asset_warnings,
        refreshed_default_executors,
        refreshed_default_policies,
        scoring_enabled,
    ) = match workspace_global_root {
        None => {
            let executor_store = global_executor_def_store(layout.executors_dir.clone());
            let policy_store = global_policy_def_store(layout.policies_dir.clone());
            // Skills/activities/jobs refresh on ordinary `orbit init`.
            // Executors do not: `refresh_defaults` overwrite would restore
            // shipped sandbox and wipe an operator `spec.sandbox: off`.
            // `--force` still resets them by deleting the root first.
            let refreshed_default_executors =
                seed_default_executors(executor_store.as_ref(), options.force)?;
            let refreshed_default_policies =
                seed_default_policies(policy_store.as_ref(), overwrite)?;
            let activity_reconciliation =
                seed_default_activities(&layout.activities_dir, overwrite)?;
            let job_reconciliation = seed_default_jobs(&layout.jobs_dir, overwrite)?;
            let mut managed_asset_warnings = std::mem::take(&mut skill_asset_warnings);
            managed_asset_warnings.extend(activity_reconciliation.warnings);
            managed_asset_warnings.extend(job_reconciliation.warnings);
            (
                activity_reconciliation.refreshed,
                activity_reconciliation.retired,
                job_reconciliation.refreshed,
                job_reconciliation.retired,
                managed_asset_warnings,
                refreshed_default_executors,
                refreshed_default_policies,
                false,
            )
        }
        Some(global_root) => {
            let global_result = init_workspace_at_root(
                &global_root,
                InitOptions {
                    refresh_defaults: options.refresh_defaults,
                    global_only: true,
                    link_global_skills: options.link_global_skills || options.refresh_defaults,
                    config_seed: options.config_seed.clone(),
                    ..Default::default()
                },
            )?;
            refreshed_skill_files = global_result.refreshed_skill_files;
            created_skills_symlink = global_result.created_skills_symlink;
            // Routines and auto-tasks are workspace-scoped, so their managed-asset
            // reconciliation happens here rather than in the global branch; fold
            // their warnings in alongside the global (skill/activity/job) ones.
            let mut managed_asset_warnings = global_result.managed_asset_warnings;
            // Routines are workspace-authored (`.orbit/routines/`, no global
            // directory), so defaults seed here rather than in the global branch.
            // Host identity is owned by higher-level composition and injected;
            // Core never opens host.toml or falls back to an OS hostname.
            {
                let reconciliation = reconcile_workspace_managed_artifacts(
                    &global_root,
                    &orbit_root,
                    options.routine_seed_identity.as_ref(),
                    false,
                )?;
                refreshed_default_routines = reconciliation
                    .actions
                    .iter()
                    .filter(|action| {
                        action.kind == "routine"
                            && matches!(
                                action.outcome,
                                ManagedArtifactOutcome::Created | ManagedArtifactOutcome::Refreshed
                            )
                    })
                    .count();
                seeded_default_auto_tasks = reconciliation
                    .actions
                    .iter()
                    .filter(|action| {
                        action.kind == "auto_task"
                            && matches!(
                                action.outcome,
                                ManagedArtifactOutcome::Created | ManagedArtifactOutcome::Refreshed
                            )
                    })
                    .count();
                managed_asset_warnings.extend(
                    reconciliation
                        .actions
                        .into_iter()
                        .filter(|action| action.outcome == ManagedArtifactOutcome::Preserved)
                        .filter_map(|action| action.detail),
                );
            }
            (
                global_result.refreshed_default_activities,
                global_result.retired_default_activities,
                global_result.refreshed_default_jobs,
                global_result.retired_default_jobs,
                managed_asset_warnings,
                global_result.refreshed_default_executors,
                global_result.refreshed_default_policies,
                ResolvedConfig::load(&ConfigRoots::new(&global_root, &orbit_root))?.scoring_enabled,
            )
        }
    };

    if scoring_enabled {
        seed_scoreboard_templates(&orbit_root)?;
    }
    if !options.global_only {
        friction_store::ensure_default_tag_taxonomy(&orbit_root.join("frictions"))?;
    }
    // Every globally scoped default has now landed, so later runtime opens may
    // skip the reconciliation pass until this binary's asset set changes. The
    // stamp is bookkeeping, not a default: an immutable global root that needs
    // no writes must still complete init, so a denied write only costs the next
    // open another reconciliation.
    if options.global_only {
        let stamp = record_global_defaults_reconciled(&orbit_root);
        ignore_denied_implicit_bootstrap_write("global defaults stamp", &orbit_root, stamp)?;
    }

    Ok(InitResult {
        refreshed_skill_files,
        created_skills_symlink,
        created_config,
        refreshed_default_activities,
        retired_default_activities,
        refreshed_default_jobs,
        retired_default_jobs,
        managed_asset_warnings,
        refreshed_default_executors,
        refreshed_default_policies,
        refreshed_default_routines,
        seeded_default_auto_tasks,
    })
}

pub(crate) fn global_skills_dir(global_root: &Path) -> PathBuf {
    global_root.join("skills")
}

#[derive(Debug, Clone)]
struct InitTarget {
    orbit_root: PathBuf,
    skills_links_roots: Vec<PathBuf>,
}

fn resolve_init_target_from_root(orbit_root: &Path) -> InitTarget {
    let orbit_root = orbit_root.to_path_buf();
    let skills_links_base = crate::paths::home_dir()
        .or_else(|| find_git_repo_root(&orbit_root))
        .unwrap_or_else(|| {
            orbit_root
                .parent()
                .unwrap_or(orbit_root.as_path())
                .to_path_buf()
        });
    let skills_links_roots = skill_link_roots(&skills_links_base);

    InitTarget {
        orbit_root,
        skills_links_roots,
    }
}

pub(crate) fn skill_link_roots(base_root: &Path) -> Vec<PathBuf> {
    [".agents", ".claude"]
        .into_iter()
        .map(|dir| base_root.join(dir).join("skills"))
        .collect()
}

fn find_git_repo_root(start: &Path) -> Option<PathBuf> {
    crate::paths::find_git_repo_root(start)
}

fn seed_scoreboard_templates(orbit_root: &Path) -> Result<(), OrbitError> {
    let scoreboard_dir = orbit_layout_paths(orbit_root).scoreboard_dir;
    fs::create_dir_all(&scoreboard_dir).map_err(|e| OrbitError::Io(e.to_string()))?;

    let pr_path = scoreboard_dir.join("pr.json");
    if !pr_path.exists() {
        fs::write(&pr_path, "{}\n").map_err(|e| OrbitError::Io(e.to_string()))?;
    }

    let task_review_path = scoreboard_dir.join("task_review.json");
    if !task_review_path.exists() {
        fs::write(&task_review_path, "{}\n").map_err(|e| OrbitError::Io(e.to_string()))?;
    }

    Ok(())
}

fn prepare_workspace_root_layout(
    orbit_root: &Path,
    global_root: &Path,
) -> Result<WorkspacePaths, OrbitError> {
    fs::create_dir_all(orbit_root).map_err(|e| OrbitError::Io(e.to_string()))?;
    let layout = orbit_layout_paths(orbit_root);
    ensure_workspace_dirs(&layout)?;
    remove_workspace_seeded_default_skills(orbit_root, &layout, global_root)?;
    Ok(layout)
}

fn orbit_layout_paths(orbit_root: &Path) -> WorkspacePaths {
    let repo_root = orbit_root.parent().unwrap_or(orbit_root).to_path_buf();
    WorkspacePaths::new(
        repo_root,
        orbit_root.to_path_buf(),
        orbit_root.to_path_buf(),
    )
}

fn prepare_global_root_layout(orbit_root: &Path) -> Result<WorkspacePaths, OrbitError> {
    fs::create_dir_all(orbit_root).map_err(|e| OrbitError::Io(e.to_string()))?;
    let layout = orbit_layout_paths(orbit_root);
    ensure_global_dirs(&layout)?;
    Ok(layout)
}

fn ensure_workspace_dirs(paths: &WorkspacePaths) -> Result<(), OrbitError> {
    for dir in [
        &paths.resources_dir,
        &paths.state_dir,
        &paths.audit_dir,
        &paths.job_runs_dir,
        &paths.logs_dir,
        &paths.diagnostics_dir,
        &paths.scoreboard_dir,
        &paths.worktrees_dir,
        &paths.knowledge_dir,
    ] {
        fs::create_dir_all(dir).map_err(|e| OrbitError::Io(e.to_string()))?;
    }
    Ok(())
}

/// Reap skill trees left behind under a workspace root by older versions that
/// seeded them there.
///
/// `global_root` names the root that owns the live catalog. It is normally a
/// different path from `orbit_root`, but a `--root` scratch root makes one
/// directory serve as both, and then `<root>/skills` *is* the global catalog
/// seeded moments earlier in the same runtime open. Reaping it there deleted
/// every shipped skill on the first command after init, so the live catalog is
/// excluded from the candidate list. [ORB-10926]
fn remove_workspace_seeded_default_skills(
    orbit_root: &Path,
    paths: &WorkspacePaths,
    global_root: &Path,
) -> Result<(), OrbitError> {
    let live_catalog = global_skills_dir(global_root);
    for skills_dir in [paths.skills_dir.clone(), orbit_root.join("skills")] {
        let skills_dir = skills_dir.as_path();
        if !skills_dir.exists() || is_same_dir(skills_dir, &live_catalog) {
            continue;
        }

        for skill_id in default_skill_ids() {
            let skill_dir = skills_dir.join(skill_id);
            let skill_file = skill_dir.join("SKILL.md");
            if is_default_skill_file_for_root(skill_id, &skill_file, orbit_root)? {
                remove_path_if_exists(&skill_dir)?;
            }
        }
        for skill_id in LEGACY_WORKSPACE_SEEDED_SKILL_IDS {
            remove_path_if_exists(&skills_dir.join(skill_id))?;
        }

        // Skills are never seeded into a workspace-only skills directory, so a
        // managed manifest here describes skill trees that were just removed.
        // Drop it once nothing else remains, otherwise it would keep an
        // otherwise-empty legacy workspace skills directory alive forever.
        if directory_holds_only(skills_dir, MANAGED_ASSET_MANIFEST_FILE)? {
            remove_path_if_exists(&skills_dir.join(MANAGED_ASSET_MANIFEST_FILE))?;
        }
        remove_empty_dir(skills_dir)?;
    }
    Ok(())
}

/// Whether two paths name the same existing directory. Falls back to a literal
/// comparison when either side cannot be canonicalized.
fn is_same_dir(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

/// Whether `dir` contains exactly one entry, named `file_name`.
fn directory_holds_only(dir: &Path, file_name: &str) -> Result<bool, OrbitError> {
    if !dir.is_dir() {
        return Ok(false);
    }
    let mut entries = fs::read_dir(dir).map_err(|e| OrbitError::Io(e.to_string()))?;
    let Some(first) = entries.next() else {
        return Ok(false);
    };
    let first = first.map_err(|e| OrbitError::Io(e.to_string()))?;
    Ok(first.file_name() == file_name && entries.next().is_none())
}

fn remove_empty_dir(dir: &Path) -> Result<(), OrbitError> {
    if !dir.is_dir() {
        return Ok(());
    }
    let mut entries = fs::read_dir(dir).map_err(|e| OrbitError::Io(e.to_string()))?;
    if entries.next().is_none() {
        fs::remove_dir(dir).map_err(|e| OrbitError::Io(e.to_string()))?;
    }
    Ok(())
}

fn ensure_global_dirs(paths: &WorkspacePaths) -> Result<(), OrbitError> {
    for dir in [
        &paths.resources_dir,
        &paths.activities_dir,
        &paths.jobs_dir,
        &paths.executors_dir,
        &paths.policies_dir,
        &global_skills_dir(&paths.orbit_dir),
    ] {
        fs::create_dir_all(dir).map_err(|e| OrbitError::Io(e.to_string()))?;
    }
    Ok(())
}

/// Add or repair links for `skill_ids`, then reap Orbit-owned dangling
/// leftovers whose IDs left the current default set.
fn ensure_skill_links(
    skills_root: &Path,
    skill_ids: &[&str],
    skills_links_dir: &Path,
    force: bool,
) -> Result<bool, OrbitError> {
    if let Some(parent) = skills_links_dir.parent() {
        fs::create_dir_all(parent).map_err(|e| OrbitError::Io(e.to_string()))?;
    }

    if let Ok(metadata) = fs::symlink_metadata(skills_links_dir)
        && !metadata.file_type().is_dir()
    {
        if force {
            remove_path_if_exists(skills_links_dir)?;
        } else {
            return Err(OrbitError::InvalidInput(format!(
                "expected '{}' to be a directory for skill links; found non-directory path",
                skills_links_dir.display()
            )));
        }
    }

    if !skills_links_dir.exists() {
        fs::create_dir_all(skills_links_dir).map_err(|e| OrbitError::Io(e.to_string()))?;
    } else if !skills_links_dir.is_dir() {
        if force {
            remove_path_if_exists(skills_links_dir)?;
            fs::create_dir_all(skills_links_dir).map_err(|e| OrbitError::Io(e.to_string()))?;
        } else {
            return Err(OrbitError::InvalidInput(format!(
                "expected '{}' to be a directory for skill links; found non-directory path",
                skills_links_dir.display()
            )));
        }
    }
    let canonical_skills_root = skills_root
        .canonicalize()
        .map_err(|e| OrbitError::Io(e.to_string()))?;

    let mut changed = false;
    for skill_id in skill_ids {
        let target = skills_root.join(skill_id);
        if !target.exists() {
            return Err(OrbitError::InvalidInput(format!(
                "skill target does not exist for link: {}",
                target.display()
            )));
        }
        let link_path = skills_links_dir.join(skill_id);

        if let Ok(link_meta) = fs::symlink_metadata(&link_path) {
            if link_meta.file_type().is_symlink() {
                let resolved_target = resolve_symlink_target(&link_path)?;
                let canonical_expected = canonical_skills_root.join(skill_id);
                if let Ok(canonical_existing) = resolved_target.canonicalize()
                    && canonical_existing == canonical_expected
                {
                    continue;
                }
                fs::remove_file(&link_path).map_err(|e| OrbitError::Io(e.to_string()))?;
                create_dir_symlink(&target, &link_path)?;
                changed = true;
                continue;
            }
            if force {
                remove_path_if_exists(&link_path)?;
                create_dir_symlink(&target, &link_path)?;
                changed = true;
                continue;
            }
            return Err(OrbitError::InvalidInput(format!(
                "expected '{}' to be a symlink to '{}'; found non-symlink path",
                link_path.display(),
                target.display()
            )));
        }

        create_dir_symlink(&target, &link_path)?;
        changed = true;
    }

    changed |= reap_stale_skill_links(
        skills_root,
        &canonical_skills_root,
        skill_ids,
        skills_links_dir,
    )?;

    Ok(changed)
}

/// Resolve a symlink to the path it names without requiring the target to exist.
fn resolve_symlink_target(link_path: &Path) -> Result<PathBuf, OrbitError> {
    let target_path = fs::read_link(link_path).map_err(|e| OrbitError::Io(e.to_string()))?;
    if target_path.is_absolute() {
        Ok(target_path)
    } else {
        Ok(link_path
            .parent()
            .unwrap_or(Path::new("."))
            .join(target_path))
    }
}

fn is_orbit_skill_link_target(
    resolved: &Path,
    skills_root: &Path,
    canonical_skills_root: &Path,
    skill_id: &str,
) -> bool {
    resolved == skills_root.join(skill_id) || resolved == canonical_skills_root.join(skill_id)
}

/// Drop client-dir symlinks for skill IDs that left `default_skill_ids()`
/// and whose Orbit-owned target has already been reaped.
///
/// A leftover is removed only when it is a symlink, its name is not a
/// current default, it points at `{skills_root}/{name}` (the path Orbit
/// itself writes), and that target no longer exists. User custom skill
/// directories and custom symlinks that still resolve — or that point
/// outside the Orbit skills root — are left alone.
fn reap_stale_skill_links(
    skills_root: &Path,
    canonical_skills_root: &Path,
    skill_ids: &[&str],
    skills_links_dir: &Path,
) -> Result<bool, OrbitError> {
    if !skills_links_dir.exists() {
        return Ok(false);
    }

    let current: BTreeSet<&str> = skill_ids.iter().copied().collect();
    let mut changed = false;
    let entries = fs::read_dir(skills_links_dir).map_err(|e| OrbitError::Io(e.to_string()))?;
    for entry in entries {
        let entry = entry.map_err(|e| OrbitError::Io(e.to_string()))?;
        let path = entry.path();
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if current.contains(name.as_str()) {
            continue;
        }
        let meta = fs::symlink_metadata(&path).map_err(|e| OrbitError::Io(e.to_string()))?;
        if !meta.file_type().is_symlink() {
            continue;
        }
        let resolved = resolve_symlink_target(&path)?;
        if !is_orbit_skill_link_target(&resolved, skills_root, canonical_skills_root, &name) {
            continue;
        }
        if path.exists() {
            continue;
        }
        fs::remove_file(&path).map_err(|e| OrbitError::Io(e.to_string()))?;
        changed = true;
    }
    Ok(changed)
}

// --- Public link/unlink API ---

#[derive(Debug, Clone)]
pub struct LinkResult {
    pub linked_count: usize,
    pub roots: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct UnlinkResult {
    pub removed_count: usize,
    pub cleaned_dirs: Vec<PathBuf>,
}

/// Re-create skill symlinks in `~/.agents/skills/` and `~/.claude/skills/`.
pub fn link_skills(global_root: &Path) -> Result<LinkResult, OrbitError> {
    let init_target = resolve_init_target_from_root(global_root);
    let skills_root = global_skills_dir(&init_target.orbit_root);

    if !skills_root.exists() {
        return Err(OrbitError::InvalidInput(format!(
            "skills root does not exist: {}",
            skills_root.display()
        )));
    }

    let skill_ids = default_skill_ids();
    let mut linked_count = 0usize;
    let mut roots = Vec::new();

    for skills_links_root in &init_target.skills_links_roots {
        let changed = ensure_skill_links(&skills_root, &skill_ids, skills_links_root, false)?;
        if changed {
            linked_count += skill_ids.len();
        }
        roots.push(skills_links_root.clone());
    }

    Ok(LinkResult {
        linked_count,
        roots,
    })
}

/// Remove skill symlinks from `~/.agents/skills/` and `~/.claude/skills/`.
/// Only removes symlinks — regular files and directories are left intact.
pub fn unlink_skills(global_root: &Path) -> Result<UnlinkResult, OrbitError> {
    let init_target = resolve_init_target_from_root(global_root);
    let mut removed_count = 0usize;
    let mut cleaned_dirs = Vec::new();

    for skills_links_dir in &init_target.skills_links_roots {
        if !skills_links_dir.exists() {
            continue;
        }

        let entries = fs::read_dir(skills_links_dir).map_err(|e| OrbitError::Io(e.to_string()))?;

        for entry in entries {
            let entry = entry.map_err(|e| OrbitError::Io(e.to_string()))?;
            let meta =
                fs::symlink_metadata(entry.path()).map_err(|e| OrbitError::Io(e.to_string()))?;
            if meta.file_type().is_symlink() {
                fs::remove_file(entry.path()).map_err(|e| OrbitError::Io(e.to_string()))?;
                removed_count += 1;
            }
        }

        // Clean up empty skills dir, then empty parent (.agents/ or .claude/)
        if skills_links_dir.exists() && dir_is_empty(skills_links_dir)? {
            fs::remove_dir(skills_links_dir).map_err(|e| OrbitError::Io(e.to_string()))?;
            cleaned_dirs.push(skills_links_dir.clone());

            if let Some(parent) = skills_links_dir.parent()
                && parent.exists()
                && dir_is_empty(parent)?
            {
                fs::remove_dir(parent).map_err(|e| OrbitError::Io(e.to_string()))?;
                cleaned_dirs.push(parent.to_path_buf());
            }
        }
    }

    Ok(UnlinkResult {
        removed_count,
        cleaned_dirs,
    })
}

fn dir_is_empty(path: &Path) -> Result<bool, OrbitError> {
    let mut entries = fs::read_dir(path).map_err(|e| OrbitError::Io(e.to_string()))?;
    Ok(entries.next().is_none())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tempfile::tempdir;

    use super::*;

    /// Make global-init routing entirely fixture-owned even when the test
    /// binary was launched by a managed Orbit run. This is one scoped guard so
    /// its process-wide lock also restores the parent environment on drop.
    fn global_init_env(home: &Path) -> orbit_common::test_env::ScopedEnv {
        orbit_common::test_env::scoped(
            orbit_common::test_env::INHERITED_AUTHORITY_ENV
                .iter()
                .copied()
                .map(|name| (name, None))
                .chain(std::iter::once(("HOME", home.to_str()))),
        )
    }

    #[test]
    fn fresh_workspace_init_seeds_disabled_worktree_gc_routine() {
        let temp = tempdir().expect("tempdir");
        let global_root = temp.path().join("global");
        let orbit_root = temp.path().join("repo/.orbit");
        let result = init_workspace_at_root(
            &orbit_root,
            InitOptions {
                global_root_override: Some(global_root.clone()),
                refresh_defaults: true,
                routine_seed_identity: Some(
                    RoutineSeedIdentity::new("host-a", "repo").expect("routine seed identity"),
                ),
                ..Default::default()
            },
        )
        .expect("initialize fresh workspace");

        assert_eq!(
            result.refreshed_default_routines,
            crate::application::routine::DEFAULT_ROUTINE_FILES.len()
        );
        let yaml = fs::read_to_string(orbit_root.join("routines/worktree_gc.yaml"))
            .expect("read seeded worktree GC routine");
        assert!(!yaml.contains("__ORBIT_"));
        let routine = orbit_common::protocol::yaml::parse_routine_yaml(&yaml)
            .expect("seeded worktree GC routine parses");
        assert!(!routine.enabled);
        assert_eq!(routine.hosts, vec!["host-a".to_string()]);
        assert_eq!(
            routine.target,
            orbit_types::workflow::RoutineTarget::Job("worktree_gc_pipeline".to_string())
        );
        assert_eq!(
            routine.policy.overlap,
            orbit_types::workflow::OverlapPolicy::Forbid
        );

        let routine_path = orbit_root.join("routines/worktree_gc.yaml");
        fs::write(&routine_path, "operator edited").expect("hand edit routine");
        init_workspace_at_root(
            &orbit_root,
            InitOptions {
                global_root_override: Some(global_root.clone()),
                refresh_defaults: true,
                routine_seed_identity: Some(
                    RoutineSeedIdentity::new("host-a", "repo").expect("routine seed identity"),
                ),
                ..Default::default()
            },
        )
        .expect("plain re-init");
        assert_eq!(
            fs::read_to_string(&routine_path).expect("read preserved routine"),
            "operator edited",
            "plain re-init preserves a hand-edited routine"
        );

        init_workspace_at_root(
            &orbit_root,
            InitOptions {
                global_root_override: Some(global_root),
                force: true,
                refresh_defaults: true,
                routine_seed_identity: Some(
                    RoutineSeedIdentity::new("host-b", "repo").expect("routine seed identity"),
                ),
                ..Default::default()
            },
        )
        .expect("forced re-init");
        let forced =
            fs::read_to_string(&routine_path).expect("read force-overwritten worktree GC routine");
        assert!(!forced.contains("operator edited"));
        let forced = orbit_common::protocol::yaml::parse_routine_yaml(&forced)
            .expect("force-overwritten routine parses");
        assert_eq!(forced.hosts, vec!["host-b".to_string()]);
        assert!(!forced.enabled);
    }

    #[test]
    fn workspace_init_seeds_inert_defaults_without_clobbering_edits() {
        let temp = tempdir().expect("tempdir");
        let global_root = temp.path().join("global");
        let orbit_root = temp.path().join("repo/.orbit");
        let options = InitOptions {
            global_root_override: Some(global_root),
            config_seed: Some(ConfigSeed::default()),
            ..Default::default()
        };

        let initial = init_workspace_at_root(&orbit_root, options.clone())
            .expect("initialize fresh workspace");
        assert_eq!(
            initial.seeded_default_auto_tasks,
            crate::application::auto_tasks::DEFAULT_AUTO_TASK_FILES.len()
        );
        let friction_path = orbit_root.join("auto_tasks/friction-curation.yaml");
        let qa_path = orbit_root.join("auto_tasks/qa-sweep.yaml");
        let security_path = orbit_root.join("auto_tasks/security-review.yaml");
        let code_review_path = orbit_root.join("auto_tasks/code-review.yaml");
        let friction = fs::read_to_string(&friction_path).expect("read seeded friction definition");
        let friction_definition = orbit_common::protocol::yaml::parse_auto_task_yaml(&friction)
            .expect("seeded friction definition parses through loader schema");
        assert!(!friction_definition.enabled);
        // [ORB-10877] Shipped recurring work names the portable system lane,
        // not a family-specific crew.
        assert_eq!(friction_definition.template.crew.as_deref(), Some("system"));
        assert!(
            friction.contains("\n  crew: system"),
            "seeded friction default must name the system crew"
        );
        assert!(matches!(
            friction_definition.dedupe,
            orbit_types::workflow::DedupePolicy::SkipIfOpen
        ));
        let qa = fs::read_to_string(&qa_path).expect("read seeded QA definition");
        let qa_definition = orbit_common::protocol::yaml::parse_auto_task_yaml(&qa)
            .expect("seeded QA definition parses through loader schema");
        assert!(!qa_definition.enabled);
        assert_eq!(qa_definition.template.crew.as_deref(), Some("system"));
        assert!(
            qa.contains("\n  crew: system"),
            "seeded QA default must name the system crew"
        );
        assert!(matches!(
            qa_definition.dedupe,
            orbit_types::workflow::DedupePolicy::SkipIfOpen
        ));
        let security =
            fs::read_to_string(&security_path).expect("read seeded security-review definition");
        let security_definition = orbit_common::protocol::yaml::parse_auto_task_yaml(&security)
            .expect("seeded security-review definition parses through loader schema");
        assert!(!security_definition.enabled);
        assert_eq!(security_definition.template.crew.as_deref(), Some("system"));
        assert!(
            security.contains("\n  crew: system"),
            "seeded security-review default must name the system crew"
        );
        assert!(matches!(
            security_definition.dedupe,
            orbit_types::workflow::DedupePolicy::SkipIfOpen
        ));
        let code_review =
            fs::read_to_string(&code_review_path).expect("read seeded code-review definition");
        let code_review_definition =
            orbit_common::protocol::yaml::parse_auto_task_yaml(&code_review)
                .expect("seeded code-review definition parses through loader schema");
        assert!(!code_review_definition.enabled);
        assert_eq!(
            code_review_definition.template.crew.as_deref(),
            Some("system")
        );
        assert!(
            code_review.contains("\n  crew: system"),
            "seeded code-review default must name the system crew"
        );
        assert!(matches!(
            code_review_definition.dedupe,
            orbit_types::workflow::DedupePolicy::SkipIfOpen
        ));
        assert!(!orbit_root.join("state/auto-tasks.json").exists());
        let loaded = crate::application::auto_tasks::collect_auto_tasks(&orbit_root);
        assert!(
            loaded.errors.is_empty(),
            "seeded definition must load cleanly"
        );
        assert_eq!(
            loaded.definitions.len(),
            crate::application::auto_tasks::DEFAULT_AUTO_TASK_FILES.len()
        );
        assert!(
            loaded
                .definitions
                .iter()
                .any(|loaded| loaded.definition.name == "friction-curation")
        );
        assert!(
            loaded
                .definitions
                .iter()
                .any(|loaded| loaded.definition.name == "qa-sweep")
        );
        assert!(
            loaded
                .definitions
                .iter()
                .any(|loaded| loaded.definition.name == "security-review")
        );
        assert!(
            loaded
                .definitions
                .iter()
                .any(|loaded| loaded.definition.name == "code-review")
        );

        let authored_friction = "operator-authored friction definition\n";
        let authored_qa = "operator-authored QA definition\n";
        let authored_security = "operator-authored security-review definition\n";
        let authored_code_review = "operator-authored code-review definition\n";
        fs::write(&friction_path, authored_friction).expect("write friction edit");
        fs::write(&qa_path, authored_qa).expect("write QA edit");
        fs::write(&security_path, authored_security).expect("write security-review edit");
        fs::write(&code_review_path, authored_code_review).expect("write code-review edit");
        let repeated =
            init_workspace_at_root(&orbit_root, options).expect("reinitialize workspace");
        assert_eq!(repeated.seeded_default_auto_tasks, 0);
        assert_eq!(
            fs::read_to_string(friction_path).expect("read preserved friction definition"),
            authored_friction
        );
        assert_eq!(
            fs::read_to_string(qa_path).expect("read preserved QA definition"),
            authored_qa
        );
        assert_eq!(
            fs::read_to_string(security_path).expect("read preserved security-review definition"),
            authored_security
        );
        assert_eq!(
            fs::read_to_string(code_review_path).expect("read preserved code-review definition"),
            authored_code_review
        );
    }

    #[test]
    fn global_init_seeds_skills_and_home_level_links() {
        let home = tempdir().expect("home tempdir");
        let _env = global_init_env(home.path());

        let result = init_global(
            None,
            InitOptions {
                refresh_defaults: true,
                config_seed: Some(ConfigSeed::default()),
                ..Default::default()
            },
        );

        let result = result.expect("init global");
        // Skills are now reconciled per managed file rather than per skill
        // directory, so a refresh counts every SKILL.md *and* reference file.
        assert_eq!(
            result.refreshed_skill_files,
            crate::application::skill::DEFAULT_SKILL_FILES.len()
        );
        assert!(result.created_skills_symlink);
        assert!(
            home.path()
                .join(".orbit")
                .join("skills")
                .join("orbit")
                .join("SKILL.md")
                .exists()
        );
        // Reference files seed as a nested tree, not just the router document.
        assert!(
            home.path()
                .join(".orbit")
                .join("skills")
                .join("orbit")
                .join("references")
                .join("run-debugging.md")
                .exists()
        );
        assert!(
            !home
                .path()
                .join(".orbit")
                .join("resources")
                .join("skills")
                .join("orbit")
                .join("SKILL.md")
                .exists()
        );
        assert_skill_link_exists(home.path().join(".agents").join("skills").join("orbit"));
        assert_skill_link_exists(home.path().join(".claude").join("skills").join("orbit"));
    }

    #[test]
    fn workspace_init_leaves_repo_skills_unseeded() {
        let home = tempdir().expect("home tempdir");
        let workspace = tempdir().expect("workspace tempdir");
        // The shared guard serializes every HOME mutation in this test binary,
        // including the runtime resolve tests; a module-local lock did not.
        let _env = orbit_common::test_env::scoped([("HOME", home.path().to_str())]);

        let orbit_root = workspace.path().join(".orbit");
        seed_default_skills(
            &orbit_root.join("resources").join("skills"),
            &orbit_root,
            true,
        )
        .expect("seed legacy workspace resource skills");
        seed_default_skills(&orbit_root.join("skills"), &orbit_root, true)
            .expect("seed legacy workspace skills");
        let custom_skill = orbit_root.join("resources").join("skills").join("custom");
        fs::create_dir_all(&custom_skill).expect("create custom skill");
        fs::write(
            custom_skill.join("SKILL.md"),
            "# Custom\n\n## Purpose\n\nKeep me.\n",
        )
        .expect("write custom skill");
        let legacy_skill = orbit_root.join("resources").join("skills").join("orbit-pr");
        fs::create_dir_all(&legacy_skill).expect("create legacy skill");
        fs::write(
            legacy_skill.join("SKILL.md"),
            "---\nname: orbit-pr\n---\n\n# Orbit PR\n",
        )
        .expect("write legacy skill");

        let result = init_workspace_at_root(
            &orbit_root,
            InitOptions {
                refresh_defaults: true,
                global_root_override: Some(home.path().join(".orbit")),
                config_seed: Some(ConfigSeed::default()),
                ..Default::default()
            },
        );

        let result = result.expect("init workspace");
        // Skills are now reconciled per managed file rather than per skill
        // directory, so a refresh counts every SKILL.md *and* reference file.
        assert_eq!(
            result.refreshed_skill_files,
            crate::application::skill::DEFAULT_SKILL_FILES.len()
        );
        assert!(result.created_skills_symlink);
        assert!(
            !orbit_root
                .join("resources")
                .join("skills")
                .join("orbit")
                .join("SKILL.md")
                .exists()
        );
        assert!(!orbit_root.join("skills").exists());
        assert!(orbit_root.join("state").join("logs").exists());
        assert!(custom_skill.join("SKILL.md").exists());
        assert!(!legacy_skill.exists());
        assert!(
            home.path()
                .join(".orbit")
                .join("skills")
                .join("orbit")
                .join("SKILL.md")
                .exists()
        );
        assert_skill_link_exists(home.path().join(".claude").join("skills").join("orbit"));
    }

    /// A `--root` scratch root makes one directory serve as both the global and
    /// the workspace root. Every runtime open seeds the global skill catalog and
    /// then reaps workspace-seeded leftovers; when the two roots are the same
    /// path, the reap used to delete the catalog it had just written.
    /// [ORB-10926]
    #[test]
    fn runtime_open_keeps_global_skills_when_root_doubles_as_workspace() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path().join("scratch-root");
        let router = global_skills_dir(&root).join("orbit").join("SKILL.md");

        ensure_orbit_root_initialized(&root, &root).expect("first runtime open");
        assert!(
            router.exists(),
            "init must seed the global skill catalog at {}",
            router.display()
        );

        // Legacy workspace-seeded skills still live in the resources tree of the
        // same root, and must still be reaped.
        let legacy_skills = root.join("resources").join("skills");
        seed_default_skills(&legacy_skills, &root, true).expect("seed legacy workspace skills");
        assert!(legacy_skills.join("orbit").join("SKILL.md").exists());

        ensure_orbit_root_initialized(&root, &root).expect("second runtime open");

        assert!(
            router.exists(),
            "a runtime open must not delete the global skill catalog it seeded"
        );
        assert!(
            global_skills_dir(&root)
                .join("orbit")
                .join("references")
                .join("run-debugging.md")
                .exists(),
            "reference files under the global catalog must survive too"
        );
        assert!(
            !legacy_skills.exists(),
            "legacy workspace-seeded skills must still be reaped"
        );
    }

    #[test]
    fn global_init_writes_crew_settings_as_custom_crew_to_config_toml() {
        let home = tempdir().expect("home tempdir");
        let _env = global_init_env(home.path());

        let settings = BTreeMap::from([(
            "custom".to_string(),
            orbit_config::CrewSeed {
                provider: Some("codex".into()),
                model: Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.into()),
            },
        )]);

        let result = init_global(
            None,
            InitOptions {
                refresh_defaults: true,
                config_seed: Some(ConfigSeed::default().with_crews(settings)),
                ..Default::default()
            },
        );

        let result = result.expect("init global with crew settings");
        assert!(result.created_config);

        let config_path = home.path().join(".orbit").join("config.toml");
        let contents = fs::read_to_string(&config_path).expect("read config");
        assert!(!contents.contains("[agent.reviewer]"));
        assert!(contents.contains("default_crew = \"custom\""));
        assert!(contents.contains("provider = \"codex\""));
        assert!(contents.contains(&format!(
            "model = \"{}\"",
            orbit_common::test_fixtures::TEST_CODEX_MODEL
        )));

        // Round-trips through toml: custom crew is one flat assignment.
        let parsed: toml::Value = toml::from_str(&contents).expect("parse");
        let custom = parsed
            .get("crews")
            .and_then(|v| v.as_table())
            .and_then(|v| v.get("custom"))
            .and_then(|v| v.as_table())
            .expect("custom crew table");
        assert_eq!(custom.len(), 2);
        assert_eq!(
            custom.get("provider").and_then(|v| v.as_str()),
            Some("codex")
        );
        assert_eq!(
            custom.get("model").and_then(|v| v.as_str()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL)
        );
    }

    #[test]
    fn global_init_with_existing_config_does_not_overwrite_crew_settings() {
        let home = tempdir().expect("home tempdir");
        let _env = global_init_env(home.path());

        // Pre-seed config.toml with user content.
        let orbit_root = home.path().join(".orbit");
        fs::create_dir_all(&orbit_root).expect("mkdir .orbit");
        let config_path = orbit_root.join("config.toml");
        let user_content = "# pre-existing user config\n";
        fs::write(&config_path, user_content).expect("preseed");

        let settings = BTreeMap::from([(
            "custom".to_string(),
            orbit_config::CrewSeed {
                provider: Some("claude".into()),
                model: None,
            },
        )]);

        let result = init_global(
            None,
            InitOptions {
                refresh_defaults: true,
                config_seed: Some(ConfigSeed::default().with_crews(settings)),
                ..Default::default()
            },
        );

        let result = result.expect("init global");
        assert!(!result.created_config);
        let final_contents = fs::read_to_string(&config_path).expect("read config");
        assert_eq!(final_contents, user_content);
    }

    #[test]
    fn global_init_without_crew_settings_writes_clean_template() {
        let home = tempdir().expect("home tempdir");
        let _env = global_init_env(home.path());

        let result = init_global(
            None,
            InitOptions {
                refresh_defaults: true,
                config_seed: Some(ConfigSeed::default()),
                ..Default::default()
            },
        );

        let result = result.expect("init global");
        assert!(result.created_config);
        let config_path = home.path().join(".orbit").join("config.toml");
        let contents = fs::read_to_string(&config_path).expect("read config");
        for line in contents.lines() {
            assert!(
                !line.trim_start().starts_with("[agent."),
                "unexpected uncommented agent section: {line}",
            );
        }
        assert!(!contents.contains("[crews."));
        assert!(!contents.contains("default_crew"));
    }

    fn retired_skill_ids() -> [&'static str; 5] {
        [
            "orbit-knowledge",
            "orbit-search",
            "orbit-task",
            "orbit-task-pilot",
            "orbit-workflow",
        ]
    }

    fn write_skill_dir(dir: &Path) {
        fs::create_dir_all(dir).expect("create skill dir");
        fs::write(dir.join("SKILL.md"), "# skill\n").expect("write SKILL.md");
    }

    fn seed_orbit_owned_link(skills_root: &Path, links_dir: &Path, skill_id: &str) -> PathBuf {
        let target = skills_root.join(skill_id);
        let link = links_dir.join(skill_id);
        fs::create_dir_all(links_dir).expect("create links dir");
        create_dir_symlink(&target, &link).expect("create orbit-owned skill link");
        link
    }

    #[test]
    fn ensure_skill_links_reaps_dangling_retired_orbit_owned_links() {
        let temp = tempdir().expect("tempdir");
        let skills_root = temp.path().join("skills");
        let links_dir = temp.path().join("client").join("skills");
        write_skill_dir(&skills_root.join("orbit"));

        let mut retired_links = Vec::new();
        for id in retired_skill_ids() {
            retired_links.push(seed_orbit_owned_link(&skills_root, &links_dir, id));
        }
        seed_orbit_owned_link(&skills_root, &links_dir, "orbit");

        let changed = ensure_skill_links(&skills_root, &["orbit"], &links_dir, false)
            .expect("reconcile skill links");
        assert!(changed, "reaping retired dangling links is a change");

        assert_skill_link_exists(links_dir.join("orbit"));
        for link in retired_links {
            assert!(
                fs::symlink_metadata(&link).is_err(),
                "retired dangling Orbit link must be reaped: {}",
                link.display()
            );
        }
    }

    #[test]
    fn ensure_skill_links_leaves_live_custom_and_non_orbit_entries() {
        let temp = tempdir().expect("tempdir");
        let skills_root = temp.path().join("skills");
        let links_dir = temp.path().join("client").join("skills");
        write_skill_dir(&skills_root.join("orbit"));

        let custom_target = temp.path().join("custom-skill");
        write_skill_dir(&custom_target);
        let custom_link = links_dir.join("my-custom");
        fs::create_dir_all(&links_dir).expect("create links dir");
        create_dir_symlink(&custom_target, &custom_link).expect("create custom skill link");

        let live_retired_target = skills_root.join("orbit-task");
        write_skill_dir(&live_retired_target);
        let live_retired_link = seed_orbit_owned_link(&skills_root, &links_dir, "orbit-task");

        let foreign_dangling = links_dir.join("foreign-broken");
        create_dir_symlink(&temp.path().join("does-not-exist"), &foreign_dangling)
            .expect("create foreign dangling link");

        let real_dir = links_dir.join("operator-dir");
        write_skill_dir(&real_dir);

        ensure_skill_links(&skills_root, &["orbit"], &links_dir, false)
            .expect("reconcile skill links");

        assert_skill_link_exists(links_dir.join("orbit"));
        assert_eq!(
            fs::read_link(&custom_link).expect("read custom link"),
            custom_target
        );
        assert!(
            custom_link.join("SKILL.md").exists(),
            "live custom skill symlink must be left untouched"
        );
        assert_eq!(
            fs::read_link(&live_retired_link).expect("read live retired link"),
            live_retired_target,
            "Orbit-owned link whose target still exists must be left"
        );
        assert!(
            fs::symlink_metadata(&foreign_dangling)
                .expect("foreign dangling metadata")
                .file_type()
                .is_symlink(),
            "dangling symlink that does not point at the Orbit skills root stays"
        );
        assert!(
            real_dir.join("SKILL.md").exists(),
            "operator-owned skill directory must be left untouched"
        );
    }

    fn assert_skill_link_exists(path: PathBuf) {
        let metadata = fs::symlink_metadata(&path).expect("link metadata");
        assert!(
            metadata.file_type().is_symlink(),
            "expected {} to be a symlink",
            path.display()
        );
        assert!(path.join("SKILL.md").exists());
    }
}
