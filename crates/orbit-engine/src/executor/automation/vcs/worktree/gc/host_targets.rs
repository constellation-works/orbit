//! Host validation output of run checkouts.
//!
//! Configured required validation builds into host-owned output outside the
//! checkout, keyed by the checkout's canonical path
//! (`build_output::host_validation_target`).
//! Removing the checkout leaves that target behind. GC attributes a target to
//! runs only by re-deriving each recorded run's worktree path with the same
//! key rule; a directory name no run record derives is never inferred to
//! belong to anyone and is retained.
//!
//! Detached scratch checkouts (base reruns, host evidence, recovery) are
//! keyed by paths no run record names. Their targets are attributed only by a
//! durable record of what built them (`scratch_target`), and reclaimed only
//! while holding the lock every user of the target holds. A target neither a
//! run record nor a scratch record derives stays
//! `skipped:unattributed_host_target`.

use super::super::super::baseline::recorded_base_runs;
use super::super::super::build_output::{
    HOST_VALIDATION_TARGETS_DIR, host_validation_source, host_validation_target_key,
};
use super::super::super::scratch_target::{LEASE_LABEL, leased_target};
use super::held::{HeldDirectory, Identity, entry_name};
use super::leftovers::worker_may_be_alive;
use super::paths::{attributed_task_ids, run_worktree_paths};
use super::reclaim::reclaim_patterns;
use super::sweep::{WorktreeGcOptions, WorktreeGcReport};
use orbit_common::OrbitError;
use orbit_common::fs::file_lock::{read_file_lock_holder, try_acquire_exclusive_file_lock};
use orbit_types::workflow::JobRun;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{CStr, CString, OsStr};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

const UNATTRIBUTED_DETAIL: &str = "Neither a recorded run's checkout nor a base-rerun, \
     host-evidence or recovery record derives this host validation target. GC retains it.";
const BASE_RUN_LABEL: &str = "base run cache";

/// The runs whose checkout one host target was built for.
struct Attribution<'a> {
    /// The run's worktree path, as the sweep reports it.
    worktree: PathBuf,
    /// The canonical checkout path the target's name hashes.
    source: PathBuf,
    runs: Vec<&'a JobRun>,
}

/// The scratch checkout one host target was built for, and the lock its
/// users hold while they build there.
struct ScratchOwner {
    /// `base-rerun`, `host-evidence` or `recovery`.
    kind: &'static str,
    /// The canonical checkout path the target's name hashes.
    source: PathBuf,
    lock: PathBuf,
    lock_label: &'static str,
    /// A lease's lineage record, retired with the target.
    record: Option<PathBuf>,
}

/// The host validation targets directory `repo_root`'s runs build into, held
/// open, when it exists.
struct TargetsRoot {
    primary: PathBuf,
    path: PathBuf,
    held: HeldDirectory,
}

/// One entry of the targets directory, addressed through the held root.
struct Target {
    name: CString,
    path: PathBuf,
}

/// Open the targets directory of `repo_root` from `/` without following any
/// component: a redirect anywhere is refused rather than followed, and every
/// later check and change goes through the descriptor, not the path.
fn targets_root(repo_root: &Path) -> Result<Option<TargetsRoot>, OrbitError> {
    let Some(primary) = targets_primary(repo_root) else {
        return Ok(None);
    };
    let mut held = HeldDirectory::open(&primary)?;
    let mut path = primary.clone();
    for component in HOST_VALIDATION_TARGETS_DIR {
        path.push(component);
        held = match held.open_child(&entry_name(OsStr::new(component))?) {
            Ok(child) => child,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) if matches!(error.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) => {
                return Err(OrbitError::PolicyDenied(format!(
                    "host validation target component '{}' must be a directory, not a redirect",
                    path.display()
                )));
            }
            Err(error) => return Err(error.into()),
        };
    }
    Ok(Some(TargetsRoot {
        primary,
        path,
        held,
    }))
}

/// The canonical primary checkout whose targets directory holds the output of
/// `repo_root`'s runs. The builder places a checkout's target under the
/// primary that owns its Git common directory
/// (`build_output::host_validation_source`), so a workspace that is itself a
/// linked worktree, as a replica checkout can be, has its runs' targets under
/// that primary rather than its own root [ORB-16398].
fn targets_primary(repo_root: &Path) -> Option<PathBuf> {
    let checkout = fs::canonicalize(repo_root).ok()?;
    if fs::symlink_metadata(checkout.join(".git")).is_ok_and(|meta| meta.is_dir()) {
        return Some(checkout);
    }
    let (primary, _) = host_validation_source(&checkout).ok()?;
    fs::canonicalize(primary).ok()
}

/// The size of a retained target, measured through the held root.
fn retained_bytes(root: &TargetsRoot, name: &CStr, options: &WorktreeGcOptions) -> Option<u64> {
    if !(options.delete || options.estimate_bytes) {
        return None;
    }
    let entry = root
        .held
        .entry(name)
        .ok()
        .filter(|entry| entry.is_directory)?;
    measure(root, name, entry.identity).ok()
}

fn measure(root: &TargetsRoot, name: &CStr, identity: Identity) -> std::io::Result<u64> {
    root.held.open_entry(name, identity)?.contents_bytes()
}

/// The canonical checkout path a run's worktree had, derived without the
/// worktree itself existing: the managed worktree root resolves, the
/// worktree's own name is one plain component.
fn canonical_source(worktree: &Path) -> Option<PathBuf> {
    let parent = fs::canonicalize(worktree.parent()?).ok()?;
    Some(parent.join(worktree.file_name()?))
}

fn attributions<'a>(repo_root: &Path, runs: &'a [JobRun]) -> BTreeMap<String, Attribution<'a>> {
    let mut index = BTreeMap::<String, Attribution<'a>>::new();
    for run in runs {
        for worktree in run_worktree_paths(repo_root, run).unwrap_or_default() {
            let Some(source) = canonical_source(&worktree) else {
                continue;
            };
            index
                .entry(host_validation_target_key(&source))
                .or_insert_with(|| Attribution {
                    worktree,
                    source,
                    runs: Vec::new(),
                })
                .runs
                .push(run);
        }
    }
    index
}

/// Base runs' scratch checkouts by the target key their path derives.
fn base_run_owners(repo_root: &Path) -> BTreeMap<String, ScratchOwner> {
    recorded_base_runs(repo_root)
        .into_iter()
        .filter_map(|run| {
            let source = canonical_source(&run.worktree)?;
            Some((
                host_validation_target_key(&source),
                ScratchOwner {
                    kind: "base-rerun",
                    source,
                    lock: run.lock,
                    lock_label: BASE_RUN_LABEL,
                    record: None,
                },
            ))
        })
        .collect()
}

fn scratch_owner(
    root: &TargetsRoot,
    base_runs: &mut BTreeMap<String, ScratchOwner>,
    key: &str,
) -> Option<ScratchOwner> {
    base_runs.remove(key).or_else(|| {
        leased_target(&root.primary, key).map(|leased| ScratchOwner {
            kind: leased.record.kind.label(),
            source: leased.record.source,
            lock: leased.lock,
            lock_label: LEASE_LABEL,
            record: Some(leased.record_path),
        })
    })
}

/// Whether a run's checkout has a host validation target. A prefilter only:
/// collection still applies every run, worker and containment gate.
pub(super) fn run_has_host_target(repo_root: &Path, run: &JobRun) -> bool {
    let Ok(Some(root)) = targets_root(repo_root) else {
        return false;
    };
    run_worktree_paths(repo_root, run)
        .unwrap_or_default()
        .iter()
        .filter_map(|worktree| canonical_source(worktree))
        .any(|source| {
            entry_name(OsStr::new(&host_validation_target_key(&source)))
                .and_then(|name| root.held.entry(&name))
                .is_ok_and(|entry| entry.is_directory)
        })
}

/// Classify, and with `options.delete` remove, the host validation targets
/// of recorded runs. `removed_worktrees` are the checkouts this sweep removed
/// or would remove. A failure is reported, never returned, so the rest of the
/// sweep's figures survive it.
pub(super) fn collect_host_targets(
    repo_root: &Path,
    runs: &[JobRun],
    options: &WorktreeGcOptions,
    removed_worktrees: &BTreeSet<PathBuf>,
) -> Vec<WorktreeGcReport> {
    let root = match targets_root(repo_root) {
        Ok(Some(root)) => root,
        Ok(None) => return Vec::new(),
        Err(error) => {
            let mut report = blank_report(
                repo_root
                    .join(HOST_VALIDATION_TARGETS_DIR.iter().collect::<PathBuf>())
                    .as_path(),
            );
            report.action = format!("failed:{error}");
            return vec![report];
        }
    };
    let names = match root.held.names() {
        Ok(names) => names,
        Err(error) => {
            let mut report = blank_report(&root.path);
            report.action = format!("failed:inventory host validation targets: {error}");
            return vec![report];
        }
    };
    let index = attributions(repo_root, runs);
    let mut base_runs = base_run_owners(repo_root);
    let mut reports = Vec::new();
    for name in names {
        let target = Target {
            path: root.path.join(OsStr::from_bytes(name.to_bytes())),
            name,
        };
        let attribution = target.name.to_str().ok().and_then(|key| index.get(key));
        let Some(attribution) = attribution else {
            if options.run_id.is_none() {
                let owner = target
                    .name
                    .to_str()
                    .ok()
                    .and_then(|key| scratch_owner(&root, &mut base_runs, key));
                reports.push(collect_scratch_target(
                    repo_root, &root, &target, owner, options,
                ));
            }
            continue;
        };
        let selected = match options.run_id.as_deref() {
            Some(wanted) => match attribution.runs.iter().find(|run| run.run_id == wanted) {
                Some(run) => *run,
                None => continue,
            },
            None => attribution.runs[0],
        };
        let mut report = blank_report(&target.path);
        report.run_id = Some(selected.run_id.clone());
        report.run_state = Some(selected.state);
        let task_ids = attributed_task_ids(selected);
        report.task_id = (!task_ids.is_empty()).then(|| task_ids.join(","));
        report.detail = Some(format!(
            "host validation output of checkout '{}'",
            attribution.source.display()
        ));
        if let Err(error) = classify(
            repo_root,
            &root,
            &target,
            attribution,
            options,
            removed_worktrees,
            &mut report,
        ) {
            tracing::warn!(path = %target.path.display(), %error,
                "worktree GC failed to reclaim a host validation target; continuing the sweep");
            report.bytes_reclaimed = 0;
            report.action = format!("failed:{error}");
        }
        reports.push(report);
    }
    reports
}

/// Decide `report`'s action, removing the target when the sweep deletes.
fn classify(
    repo_root: &Path,
    root: &TargetsRoot,
    target: &Target,
    attribution: &Attribution<'_>,
    options: &WorktreeGcOptions,
    removed_worktrees: &BTreeSet<PathBuf>,
    report: &mut WorktreeGcReport,
) -> Result<(), OrbitError> {
    let observed = match root.held.entry(&target.name) {
        Ok(entry) if entry.is_directory => entry.identity,
        _ => {
            report.action = "skipped:not_a_real_directory".to_string();
            return Ok(());
        }
    };
    // Every run whose checkout derives this target must be finished: a
    // shared path stays live while any run mapping it is active.
    if let Some(run) = attribution.runs.iter().find(|run| !run.state.is_terminal()) {
        report.run_id = Some(run.run_id.clone());
        report.run_state = Some(run.state);
        report.action = "skipped:run_not_terminal".to_string();
        return Ok(());
    }
    if let Some(run) = attribution.runs.iter().find(|run| worker_may_be_alive(run)) {
        report.run_id = Some(run.run_id.clone());
        report.run_state = Some(run.state);
        report.action = "skipped:worker_alive".to_string();
        return Ok(());
    }
    // Like declared output in a kept worktree, a sweep that reclaims kept
    // output is not held back by the whole-worktree age floor.
    if !options.reclaim_kept
        && options.older_than.is_some_and(|cutoff| {
            attribution
                .runs
                .iter()
                .any(|run| run.finished_at.unwrap_or(run.created_at) > cutoff)
        })
    {
        report.action = "skipped:too_recent".to_string();
        report.retained_bytes = retained_bytes(root, &target.name, options);
        return Ok(());
    }
    // Output of a checkout that still exists is reclaimed with the
    // checkout's declared output, and never when that is disabled.
    let source_present = fs::symlink_metadata(&attribution.source).is_ok();
    if source_present
        && !removed_worktrees.contains(&attribution.worktree)
        && (!(options.target_only || options.reclaim_kept) || reclaim_patterns(options).is_empty())
    {
        report.action = "skipped:source_retained".to_string();
        report.retained_bytes = retained_bytes(root, &target.name, options);
        return Ok(());
    }
    reclaim(repo_root, root, target, observed, options, report)?;
    if report.action == "reclaimed" {
        tracing::info!(path = %target.path.display(), bytes = report.bytes_reclaimed,
            "reclaimed a terminal run's host validation output");
    }
    Ok(())
}

/// Report a target no run record derives: retained unless a scratch record
/// names it, and reclaimed only while holding the lock its users hold.
fn collect_scratch_target(
    repo_root: &Path,
    root: &TargetsRoot,
    target: &Target,
    owner: Option<ScratchOwner>,
    options: &WorktreeGcOptions,
) -> WorktreeGcReport {
    let mut report = blank_report(&target.path);
    let Some(owner) = owner else {
        report.action = "skipped:unattributed_host_target".to_string();
        report.detail = Some(UNATTRIBUTED_DETAIL.to_string());
        report.retained_bytes = retained_bytes(root, &target.name, options);
        return report;
    };
    report.detail = Some(format!(
        "host validation output of {} checkout '{}'",
        owner.kind,
        owner.source.display()
    ));
    if let Err(error) = classify_scratch(repo_root, root, target, &owner, options, &mut report) {
        tracing::warn!(path = %target.path.display(), %error,
            "worktree GC failed to reclaim a scratch host validation target; continuing the sweep");
        report.bytes_reclaimed = 0;
        report.action = format!("failed:{error}");
    }
    report
}

fn classify_scratch(
    repo_root: &Path,
    root: &TargetsRoot,
    target: &Target,
    owner: &ScratchOwner,
    options: &WorktreeGcOptions,
    report: &mut WorktreeGcReport,
) -> Result<(), OrbitError> {
    let observed = match root.held.entry(&target.name) {
        Ok(entry) if entry.is_directory => entry.identity,
        _ => {
            report.action = "skipped:not_a_real_directory".to_string();
            return Ok(());
        }
    };
    // Every acquisition and release rewrites the lock's holder record, so its
    // modification time is the last use. An unreadable one counts as recent.
    if !options.reclaim_kept
        && options.older_than.is_some_and(|cutoff| {
            fs::metadata(&owner.lock)
                .and_then(|meta| meta.modified())
                .map_or(true, |used| {
                    chrono::DateTime::<chrono::Utc>::from(used) > cutoff
                })
        })
    {
        report.action = "skipped:too_recent".to_string();
        report.retained_bytes = retained_bytes(root, &target.name, options);
        return Ok(());
    }
    // A dry run takes no lock: a recorded holder is a user. Deletion holds
    // the lock itself until the target is gone, so no user can start.
    let _guard = if options.delete {
        match try_acquire_exclusive_file_lock(&owner.lock, owner.lock_label)? {
            Some(guard) => Some(guard),
            None => {
                report.action = "skipped:host_target_in_use".to_string();
                return Ok(());
            }
        }
    } else if read_file_lock_holder(&owner.lock).is_some() {
        report.action = "skipped:host_target_in_use".to_string();
        return Ok(());
    } else {
        None
    };
    reclaim(repo_root, root, target, observed, options, report)?;
    if report.action == "reclaimed" {
        if let Some(record) = &owner.record
            && let Err(error) = fs::remove_file(record)
        {
            tracing::warn!(path = %record.display(), %error,
                "worktree GC could not retire a reclaimed host validation target's lease record");
        }
        tracing::info!(path = %target.path.display(), bytes = report.bytes_reclaimed,
            kind = owner.kind, "reclaimed a scratch checkout's host validation output");
    }
    Ok(())
}

/// Measure the classified target and, when the sweep deletes, remove it
/// through the held root after re-establishing containment.
fn reclaim(
    repo_root: &Path,
    root: &TargetsRoot,
    target: &Target,
    observed: Identity,
    options: &WorktreeGcOptions,
    report: &mut WorktreeGcReport,
) -> Result<(), OrbitError> {
    let measure_bytes = options.delete || options.estimate_bytes || options.target_only;
    if measure_bytes {
        report.bytes_reclaimed = measure(root, &target.name, observed).map_err(|error| {
            OrbitError::Execution(format!(
                "failed to measure host validation target '{}': {error}",
                target.path.display()
            ))
        })?;
    }
    if !options.delete {
        report.action = "would_reclaim".to_string();
        return Ok(());
    }
    // Re-establish containment immediately before deletion: the same real
    // root still at its path, holding the directory that was classified,
    // never a substitute.
    let unchanged = targets_root(repo_root)
        .ok()
        .flatten()
        .is_some_and(|current| {
            current.path == root.path && current.held.identity() == root.held.identity()
        })
        && root
            .held
            .entry(&target.name)
            .is_ok_and(|entry| entry.is_directory && entry.identity == observed);
    if !unchanged {
        report.bytes_reclaimed = 0;
        report.action = "skipped:not_a_real_directory".to_string();
        return Ok(());
    }
    #[cfg(feature = "test-support")]
    removal_fixture::before_removal(&target.path);
    // The removal goes through the held root and the classified identity, so
    // a component replaced after the check above redirects nothing.
    root.held
        .remove_directory(&target.name, observed)
        .map_err(|error| {
            OrbitError::Execution(format!(
                "failed to remove host validation target '{}': {error}",
                target.path.display()
            ))
        })?;
    report.action = "reclaimed".to_string();
    Ok(())
}

fn blank_report(path: &Path) -> WorktreeGcReport {
    WorktreeGcReport {
        path: path.to_path_buf(),
        run_id: None,
        run_state: None,
        task_id: None,
        task_status: None,
        pr_status: None,
        action: String::new(),
        bytes_reclaimed: 0,
        detail: None,
        reclaim: Vec::new(),
        recovery: None,
        retained_bytes: None,
    }
}

// Replacing a component between the last check and the removal is a race in
// production. Fixtures make it deterministic by acting at exactly that point.
#[cfg(feature = "test-support")]
mod removal_fixture {
    use std::{cell::RefCell, path::Path, rc::Rc};

    type Hook = Rc<dyn Fn(&Path)>;
    thread_local! {
        static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }

    /// Run `hook` with each host validation target's path after GC's last
    /// check of it and before its removal, on this thread, for one fixture
    /// operation.
    pub fn with_host_target_removal_hook<T>(
        hook: impl Fn(&Path) + 'static,
        operation: impl FnOnce() -> T,
    ) -> T {
        struct Restore(Option<Hook>);
        impl Drop for Restore {
            fn drop(&mut self) {
                HOOK.with(|slot| slot.replace(self.0.take()));
            }
        }
        let _restore = Restore(HOOK.with(|slot| slot.replace(Some(Rc::new(hook)))));
        operation()
    }

    pub(super) fn before_removal(path: &Path) {
        let hook = HOOK.with(|slot| slot.borrow().clone());
        if let Some(hook) = hook {
            hook(path);
        }
    }
}

#[cfg(feature = "test-support")]
pub use removal_fixture::with_host_target_removal_hook;
