use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_store::compose::{global_executor_def_store, global_policy_def_store};
use orbit_store::friction_store;
use orbit_types::workspace::{DEFAULT_BASE_BRANCH, WorkspacePaths};

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
use crate::bootstrap::product_profile::ProductProfile;
use orbit_common::fs::io::{create_dir_symlink, create_private_dir_all, remove_path_if_exists};

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
    /// The registered base branch the seeded delivery auto-tasks observe.
    /// Higher-level composition reads it from the workspace registry; `None`
    /// renders the registry default, which is also what an unregistered
    /// workspace record would carry.
    pub workspace_base_branch: Option<String>,
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
    ProductProfile::Orbit.validate_roots(&[global_root, workspace_root])?;
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
    ProductProfile::Orbit.validate_roots(&[&orbit_root])?;
    if let Some(global_root) = workspace_global_root.as_deref() {
        ProductProfile::Orbit.validate_roots(&[global_root])?;
    }
    if options.force {
        remove_path_if_exists(&orbit_root)?;
    }
    let claim = ProductProfile::Orbit.claim_root(&orbit_root);
    ignore_denied_implicit_bootstrap_write("product marker", &orbit_root, claim)?;
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
                    options
                        .workspace_base_branch
                        .as_deref()
                        .unwrap_or(DEFAULT_BASE_BRANCH),
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
    create_private_dir_all(&scoreboard_dir).map_err(|e| OrbitError::Io(e.to_string()))?;

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
    create_private_dir_all(orbit_root).map_err(|e| OrbitError::Io(e.to_string()))?;
    let layout = orbit_layout_paths(orbit_root);
    ensure_workspace_dirs(&layout)?;
    remove_workspace_seeded_default_skills(orbit_root, &layout, global_root)?;
    Ok(layout)
}

/// `pub(super)` so `bootstrap/tests/init.rs` can assert the seeded layout
/// against the same path derivation `init` itself uses.
pub(super) fn orbit_layout_paths(orbit_root: &Path) -> WorkspacePaths {
    let repo_root = orbit_root.parent().unwrap_or(orbit_root).to_path_buf();
    WorkspacePaths::new(
        repo_root,
        orbit_root.to_path_buf(),
        orbit_root.to_path_buf(),
    )
}

fn prepare_global_root_layout(orbit_root: &Path) -> Result<WorkspacePaths, OrbitError> {
    create_private_dir_all(orbit_root).map_err(|e| OrbitError::Io(e.to_string()))?;
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
        &paths.scoreboard_dir,
        &paths.worktrees_dir,
    ] {
        create_private_dir_all(dir).map_err(|e| OrbitError::Io(e.to_string()))?;
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
        create_private_dir_all(dir).map_err(|e| OrbitError::Io(e.to_string()))?;
    }
    Ok(())
}

/// Add or repair links for `skill_ids`, then reap Orbit-owned dangling
/// leftovers whose IDs left the current default set.
/// `pub(super)` so `bootstrap/tests/init.rs` can exercise link reconciliation
/// directly instead of inferring it from a full `init` run.
pub(super) fn ensure_skill_links(
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
