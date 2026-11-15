//! Workspace removal composes the workspace catalog with task-store ownership
//! and checkout-local cleanup. Both transactions keep target resolution and
//! mutation under the registry lock; teardown saves deregistration last so a
//! failed deletion remains retryable with the same selector.

use std::path::{Path, PathBuf};

use orbit_core::OrbitError;
use orbit_registry::workspace_registry;
use orbit_types::workspace::{Workspace, WorkspaceCheckout, WorkspaceRegistry};

use crate::task_store::{self, UnclaimedPartition};

/// Catalog removal preserves local data and reports any populated partition.
#[derive(Debug, Clone)]
pub struct WorkspaceRemovalResult {
    pub workspace: String,
    pub id: String,
    pub left_task_store: Option<UnclaimedPartition>,
}

/// Confirmation supplied by the invoking surface.
#[derive(Debug, Clone, Copy)]
pub struct WorkspaceTeardownOptions {
    pub confirm: bool,
}

/// The resolved teardown plan and its ordered cleanup results.
#[derive(Debug, Clone)]
pub struct WorkspaceTeardownResult {
    pub workspace: String,
    pub id: String,
    pub checkout: PathBuf,
    pub plan: String,
    pub removed: Vec<String>,
}

/// Deregister a workspace while retaining checkout evidence for leftover data.
pub fn remove_workspace(
    global_root: &Path,
    selector: &str,
) -> Result<WorkspaceRemovalResult, OrbitError> {
    let registry_path = workspace_registry::registry_path_for(global_root);
    workspace_registry::with_registry_lock(&registry_path, || {
        let mut registry = workspace_registry::load_registry_from(&registry_path)?;
        let workspace_id =
            workspace_id_for_selector(&registry, selector)?.unwrap_or_else(|| selector.to_string());
        let checkout = registry
            .checkouts
            .iter()
            .find(|checkout| checkout.workspace_id == workspace_id)
            .cloned();
        let slug = workspace_registry::find_workspace_by_id(&registry, &workspace_id)
            .map(|workspace| workspace.name.clone())
            .unwrap_or_else(|| workspace_id.clone());

        // Retain checkout evidence before the catalog rows disappear so doctor
        // can still classify a leftover populated partition [ORB-12223].
        let leftover = task_store::retain_task_store_on_catalog_remove(
            global_root,
            &workspace_id,
            &slug,
            checkout.as_ref(),
        )?;

        let removed = workspace_registry::remove_workspace(&mut registry, &workspace_id)?;
        workspace_registry::save_registry_to(&registry, &registry_path)?;
        Ok(WorkspaceRemovalResult {
            workspace: removed.name,
            id: removed.id,
            left_task_store: leftover.filter(|partition| partition.task_bundles > 0),
        })
    })
}

/// Validate and remove one checkout's local state, then persist deregistration.
pub fn teardown_workspace(
    global_root: &Path,
    selector: &str,
    cwd: &Path,
    options: WorkspaceTeardownOptions,
) -> Result<WorkspaceTeardownResult, OrbitError> {
    let registry_path = workspace_registry::registry_path_for(global_root);
    // Keep target validation, deregistration and local deletion together;
    // another initializer must not register this checkout midway through teardown.
    workspace_registry::with_registry_lock(&registry_path, || {
        let mut registry = workspace_registry::load_registry_from(&registry_path)?;
        let (workspace, checkout) = resolve_teardown_target(&registry, selector)?;
        refuse_if_cwd_belongs_to_another_checkout(&registry, &checkout, cwd)?;

        let orbit_dir = checkout.orbit_dir.clone();
        let repo_root = checkout.repo_root.clone();
        let workspace_id = workspace.id.clone();
        let workspace_name = workspace.name.clone();

        let orbit_canonical =
            std::fs::canonicalize(&orbit_dir).unwrap_or_else(|_| orbit_dir.clone());
        let global_canonical =
            std::fs::canonicalize(global_root).unwrap_or_else(|_| global_root.to_path_buf());
        if orbit_canonical == global_canonical {
            return Err(OrbitError::InvalidInput(
                "refusing to teardown the global ~/.orbit/ directory".to_string(),
            ));
        }
        if orbit_dir.file_name().and_then(|n| n.to_str()) != Some(".orbit") {
            return Err(OrbitError::InvalidInput(format!(
                "data root '{}' does not end with .orbit — aborting teardown",
                orbit_dir.display()
            )));
        }

        let partitions = task_store::checkout_task_store_partitions(
            global_root,
            &orbit_dir,
            Some(workspace_id.as_str()),
        )?;
        let plan = format_teardown_plan(&workspace, &checkout, &partitions);
        if !options.confirm {
            return Err(OrbitError::InvalidInput(format!(
                "teardown is destructive. Resolved target:\n{plan}\nPass --confirm to proceed."
            )));
        }

        let mut removed: Vec<String> = Vec::new();

        // Prepare deregistration in memory, but keep the saved checkout
        // binding until every deletion succeeds. Failures (or a crash
        // before the final save) must remain retryable with the same selector.
        let ws = workspace_registry::remove_workspace(&mut registry, &workspace_id)?;

        // 1. Delete the task-store partition this checkout's task state is
        //    bound to. The catalog id above is not that partition's name
        //    unless the two id spaces happen to coincide, so the task registry
        //    resolves it and retires its bindings [ORB-12119].
        for partition in
            task_store::remove_checkout_task_stores(global_root, &orbit_dir, Some(&workspace_id))?
        {
            removed.push(format_deleted_partition(&partition, &workspace_name));
        }

        // 2. Remove the legacy repo-local skill links workspace init wrote
        //    into .agents/skills/ and .claude/skills/. Only links into this
        //    checkout's .orbit/skills/ are Orbit-owned; must run before step 3
        //    deletes that target.
        for dir_name in LEGACY_SKILL_DISCOVERY_DIRS {
            let cleanup = remove_owned_skill_links(&repo_root, &orbit_dir, dir_name)?;
            if cleanup.removed_links > 0 {
                removed.push(format!(
                    "removed {} Orbit skill link{} from {dir_name}/skills/",
                    cleanup.removed_links,
                    if cleanup.removed_links == 1 { "" } else { "s" }
                ));
            }
            for dir in &cleanup.removed_dirs {
                removed.push(format!("removed empty {}", dir.display()));
            }
        }

        // 3. Delete .orbit/ directory
        if orbit_dir.is_dir() {
            std::fs::remove_dir_all(&orbit_dir).map_err(|e| OrbitError::Io(e.to_string()))?;
            removed.push(format!("deleted {}", orbit_dir.display()));
        }

        // 4. Persist deregistration only after local deletion is complete.
        workspace_registry::save_registry_to(&registry, &registry_path)?;
        removed.push(format!(
            "deregistered workspace '{}' from registry",
            ws.name
        ));

        Ok(WorkspaceTeardownResult {
            workspace: workspace_name,
            id: workspace_id,
            checkout: repo_root,
            plan,
            removed,
        })
    })
}

fn resolve_teardown_target(
    registry: &WorkspaceRegistry,
    selector: &str,
) -> Result<(Workspace, WorkspaceCheckout), OrbitError> {
    let workspace_id = workspace_id_for_selector(registry, selector)?
        .ok_or_else(|| unknown_teardown_selector(selector))?;
    let workspace = workspace_registry::find_workspace_by_id(registry, &workspace_id)
        .cloned()
        .ok_or_else(|| unknown_teardown_selector(selector))?;
    let checkout = workspace_registry::find_checkout_by_id(registry, &workspace.id)
        .cloned()
        .ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "workspace '{}' ({}) has no registered checkout on this machine",
                workspace.name, workspace.id
            ))
        })?;
    Ok((workspace, checkout))
}

fn unknown_teardown_selector(selector: &str) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "unknown workspace selector '{selector}'; pass a registered workspace name, a logical workspace ID, or an absolute local checkout path"
    ))
}

fn workspace_id_for_selector(
    registry: &WorkspaceRegistry,
    selector: &str,
) -> Result<Option<String>, OrbitError> {
    if let Some(workspace) = workspace_registry::find_workspace(registry, selector)? {
        return Ok(Some(workspace.id.clone()));
    }

    if Path::new(selector).is_absolute() {
        return Ok(
            workspace_registry::find_checkout_by_path(registry, Path::new(selector))
                .map(|checkout| checkout.workspace_id.clone()),
        );
    }

    Ok(None)
}

fn refuse_if_cwd_belongs_to_another_checkout(
    registry: &WorkspaceRegistry,
    selected: &WorkspaceCheckout,
    cwd: &Path,
) -> Result<(), OrbitError> {
    let Some(cwd_checkout) = workspace_registry::find_checkout_by_path(registry, cwd) else {
        return Ok(());
    };
    if cwd_checkout.workspace_id == selected.workspace_id {
        return Ok(());
    }

    let cwd_name = workspace_registry::find_workspace_by_id(registry, &cwd_checkout.workspace_id)
        .map(|workspace| workspace.name.as_str())
        .unwrap_or(cwd_checkout.workspace_id.as_str());
    Err(OrbitError::InvalidInput(format!(
        "workspace selector does not match the checkout containing the current directory ('{cwd_name}'); cd into the target checkout or pass that workspace's name, id, or absolute path"
    )))
}

fn format_teardown_plan(
    workspace: &Workspace,
    checkout: &WorkspaceCheckout,
    partitions: &[PathBuf],
) -> String {
    let mut lines = vec![
        format!("workspace: {} ({})", workspace.name, workspace.id),
        format!("checkout: {}", checkout.repo_root.display()),
    ];
    if partitions.is_empty() {
        lines.push("task-store partition: (none)".to_string());
    } else {
        for path in partitions {
            let bundles = partition_bundle_count(path);
            lines.push(format!(
                "task-store partition: {} ({bundles} bundle{})",
                path.display(),
                if bundles == 1 { "" } else { "s" }
            ));
        }
    }
    lines.join("\n")
}

fn format_deleted_partition(path: &Path, workspace_name: &str) -> String {
    let id = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("?");
    format!("deleted task store partition {id} (workspace '{workspace_name}')")
}

fn partition_bundle_count(path: &Path) -> usize {
    std::fs::read_dir(path)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.path().is_dir())
                .count()
        })
        .unwrap_or(0)
}

/// Repo-local discovery directories a legacy workspace init linked skills into.
const LEGACY_SKILL_DISCOVERY_DIRS: [&str; 2] = [".agents", ".claude"];

/// What legacy skill-link cleanup did in one discovery directory.
struct SkillLinkCleanup {
    removed_links: usize,
    removed_dirs: Vec<PathBuf>,
}

/// Remove the legacy skill links a workspace init wrote into
/// `<repo_root>/<dir_name>/skills`.
///
/// A link is Orbit-owned only when its name matches the skill it targets and
/// that target is `<orbit_dir>/skills/<name>` (or the same path through the
/// canonical `orbit_dir`), the layout workspace init linked. Unrelated,
/// dangling unrelated and plugin links, regular files, and directories stay.
///
/// Neither `<dir_name>` nor its `skills` child is followed when it is a
/// symlink: that directory belongs to whatever tree it points at, not to the
/// checkout being torn down. When this removes a link and so empties the real
/// `skills` directory, that directory and then an emptied real parent go too.
fn remove_owned_skill_links(
    repo_root: &Path,
    orbit_dir: &Path,
    dir_name: &str,
) -> Result<SkillLinkCleanup, OrbitError> {
    let mut cleanup = SkillLinkCleanup {
        removed_links: 0,
        removed_dirs: Vec::new(),
    };
    let parent = repo_root.join(dir_name);
    let skills_dir = parent.join("skills");
    if !is_real_dir(&parent)? || !is_real_dir(&skills_dir)? {
        return Ok(cleanup);
    }

    let skills_root = orbit_dir.join("skills");
    let canonical_skills_root = std::fs::canonicalize(orbit_dir)
        .map(|dir| dir.join("skills"))
        .unwrap_or_else(|_| skills_root.clone());
    let entries = std::fs::read_dir(&skills_dir).map_err(|e| OrbitError::Io(e.to_string()))?;
    for entry in entries {
        let entry = entry.map_err(|e| OrbitError::Io(e.to_string()))?;
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path).map_err(|e| OrbitError::Io(e.to_string()))?;
        if !meta.file_type().is_symlink() {
            continue;
        }
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        let target = std::fs::read_link(&path).map_err(|e| OrbitError::Io(e.to_string()))?;
        let resolved = if target.is_absolute() {
            target
        } else {
            skills_dir.join(target)
        };
        if resolved != skills_root.join(name) && resolved != canonical_skills_root.join(name) {
            continue;
        }
        std::fs::remove_file(&path).map_err(|e| OrbitError::Io(e.to_string()))?;
        cleanup.removed_links += 1;
    }

    // Only a directory this cleanup emptied is Orbit's to remove.
    if cleanup.removed_links == 0 {
        return Ok(cleanup);
    }
    for dir in [skills_dir, parent] {
        if !is_dir_empty(&dir) {
            break;
        }
        std::fs::remove_dir(&dir).map_err(|e| OrbitError::Io(e.to_string()))?;
        cleanup.removed_dirs.push(dir);
    }
    Ok(cleanup)
}

/// Whether `path` is a directory itself rather than a symlink to one.
fn is_real_dir(path: &Path) -> Result<bool, OrbitError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => Ok(meta.file_type().is_dir()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(OrbitError::Io(error.to_string())),
    }
}

/// Check if a directory is empty.
fn is_dir_empty(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(false)
}
