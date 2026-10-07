//! Which job delivers a shipped task, and which jobs hold a task's delivery
//! slot (design `docs/design/plugins/1_scope.md` §4.5).
//!
//! A job opts in with `spec.task_delivery`. A task opts in with one
//! `delivery:<job>` tag. Without the tag a shipped task goes through
//! `task_<mode>_pipeline`, unchanged; with it, the named job must be active and
//! deliver the ship mode, or shipping refuses. Nothing here falls back. The
//! one cross-mode selection is `delivery:task_local_pipeline` in a PR ship:
//! local delivery needs nothing from a forge, so a task may always choose it.
//!
//! The PR pipeline opens its pull request through a forge, so admission
//! refuses that route when no Git remote of the checkout names a network host
//! ([`PrForgeCheck`]) — before any run, worktree or crew exists.

use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_common::fs::git::run_git;
use orbit_types::task::{DELIVERY_JOB_TAG_PREFIX, Task, delivery_job_selection};
use orbit_types::workflow::{JobKind, JobV2, ShipMode};
use orbit_types::workspace::{git_remote_network_host, redact_git_remote};

use super::catalog::{DEFAULT_JOB_FILES, is_default_job_name, shipped_job_spec};
use crate::OrbitRuntime;
use crate::application::review::LOCAL_ROUTE_JOB;

/// The job that opens a pull request: the one route a forge remote gates.
const PR_ROUTE_JOB: &str = "task_pr_pipeline";

/// The job a gate dispatches for one bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeliveryRoute {
    pub(crate) job_name: String,
    /// A `delivery:<job>` tag chose the job; `false` is the default pipeline.
    pub(crate) selected: bool,
    /// The plugin contributing the selected job, when it is a plugin job.
    pub(crate) plugin: Option<String>,
}

impl DeliveryRoute {
    /// The route opens a pull request, so it needs a forge remote.
    pub(crate) fn opens_pull_request(&self) -> bool {
        self.job_name == PR_ROUTE_JOB
    }

    /// The route merges into the local base without a pull request.
    pub(crate) fn delivers_locally(&self) -> bool {
        self.job_name == LOCAL_ROUTE_JOB
    }
}

/// Whether a task with `tags` would ship through the PR pipeline in `mode`,
/// read from the tags alone so discovery need not hydrate the task. A
/// malformed selection is left to [`OrbitRuntime::resolve_delivery_route`].
pub(crate) fn tags_route_through_pr_pipeline(tags: &[String], mode: ShipMode) -> bool {
    mode == ShipMode::Pr && matches!(delivery_job_selection(tags), Ok(None | Some(PR_ROUTE_JOB)))
}

/// One admission pass's view of whether `pr_open` could find a forge. The
/// checkout's remotes are read at most once, and only if a route needs them.
#[derive(Default)]
pub(crate) struct PrForgeCheck(OnceCell<Option<String>>);

impl PrForgeCheck {
    /// Refuse with [`OrbitError::PrForgeRemoteMissing`] when no remote of the
    /// checkout names a network host. Remotes that cannot be read are not
    /// evidence of a missing forge: delivery then reports what it finds.
    pub(crate) fn require(&self, runtime: &OrbitRuntime) -> Result<(), OrbitError> {
        match self.0.get_or_init(|| runtime.missing_forge_remote()) {
            Some(remotes) => Err(OrbitError::PrForgeRemoteMissing {
                remotes: remotes.clone(),
            }),
            None => Ok(()),
        }
    }
}

impl OrbitRuntime {
    /// The refusal PR delivery from this checkout would meet at admission, or
    /// `None` when a remote names a network host (or the remotes cannot be
    /// read). Doctor reports the same verdict admission enforces.
    pub fn pr_forge_refusal(&self) -> Option<OrbitError> {
        PrForgeCheck::default().require(self).err()
    }

    /// Resolve the route for `tasks` and refuse a PR route `forge` finds no
    /// forge remote for.
    pub(crate) fn resolve_admitted_delivery_route(
        &self,
        tasks: &[Task],
        mode: ShipMode,
        forge: &PrForgeCheck,
    ) -> Result<DeliveryRoute, OrbitError> {
        let route = self.resolve_delivery_route(tasks, mode)?;
        if route.opens_pull_request() {
            forge.require(self)?;
        }
        Ok(route)
    }

    /// What the checkout has instead of a forge remote, or `None` when one of
    /// its remotes names a network host. `git remote -v` reports URLs after
    /// `insteadOf` rewriting, which is what `gh` sees too. A forge host is not
    /// further identified: GitHub Enterprise hosts and SSH host aliases are
    /// known only to `gh` and `ssh`, so any network host is given the benefit
    /// of the doubt and only a remote set that cannot reach a forge is refused.
    fn missing_forge_remote(&self) -> Option<String> {
        let repo_root = &self.paths().repo_root;
        let listed = match run_git(repo_root, &["remote", "-v"]) {
            Ok(output) if output.success => output.stdout,
            Ok(output) => {
                tracing::warn!(
                    repo_root = %repo_root.display(),
                    "cannot list Git remotes for PR admission: {}",
                    output.stderr.trim()
                );
                return None;
            }
            Err(error) => {
                tracing::warn!(
                    repo_root = %repo_root.display(),
                    "cannot list Git remotes for PR admission: {error}"
                );
                return None;
            }
        };
        let mut remotes: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for line in listed.lines() {
            let Some((name, rest)) = line.split_once('\t') else {
                continue;
            };
            let url = rest
                .rsplit_once(' ')
                .map_or(rest, |(url, _direction)| url)
                .trim();
            if git_remote_network_host(url).is_some() {
                return None;
            }
            remotes.entry(name).or_default().insert(url);
        }
        if remotes.is_empty() {
            return Some("this checkout has no Git remote".to_string());
        }
        let described = remotes
            .iter()
            .flat_map(|(name, urls)| {
                urls.iter().map(move |url| {
                    // A local path carries no credentials and is the fact the
                    // operator needs; anything else is shown redacted.
                    let shown = match redact_git_remote(url) {
                        local if local == "<local-path>" => (*url).to_string(),
                        shown => shown,
                    };
                    format!("{name} → {shown}")
                })
            })
            .collect::<Vec<_>>()
            .join(", ");
        Some(format!(
            "no remote of this checkout names a network host ({described})"
        ))
    }

    /// Jobs whose live runs hold the delivery slot of every task in their
    /// `input.task_ids`: those declaring `spec.task_delivery`.
    ///
    /// Shipped names are read from the binary's own assets, which is what
    /// name-based execution runs (L-0060) even where the workspace catalog was
    /// never seeded; every other name from the active catalog, plugin layer
    /// included.
    pub(crate) fn task_delivery_job_ids(&self) -> Result<BTreeSet<String>, OrbitError> {
        let mut ids = BTreeSet::new();
        for (name, _) in DEFAULT_JOB_FILES {
            if shipped_job_spec(name)?.holds_task_delivery() {
                ids.insert((*name).to_string());
            }
        }
        for (name, _, spec) in self.load_v2_job_assets()?.iter() {
            if !is_default_job_name(name) && spec.holds_task_delivery() {
                ids.insert(name.to_string());
            }
        }
        Ok(ids)
    }

    /// Resolve the job that delivers `tasks` together in `mode`.
    ///
    /// Refused, never defaulted: a malformed or conflicting selection, a job
    /// no active catalog layer provides (naming its plugin when one is
    /// installed but not serving), a subroutine, or a job whose
    /// `task_delivery.modes` does not include `mode` — except
    /// `task_local_pipeline` selected in a PR ship, which delivers locally.
    pub(crate) fn resolve_delivery_route(
        &self,
        tasks: &[Task],
        mode: ShipMode,
    ) -> Result<DeliveryRoute, OrbitError> {
        let Some((job, task_id)) = bundle_selection(tasks)? else {
            return Ok(DeliveryRoute {
                job_name: format!("task_{}_pipeline", mode.as_input_value()),
                selected: false,
                plugin: None,
            });
        };
        let refuse = |reason: String| {
            OrbitError::InvalidInput(format!(
                "task '{task_id}' selects delivery job '{job}' (tag \
                 `{DELIVERY_JOB_TAG_PREFIX}{job}`), which {reason}; it will not be shipped \
                 through the default pipeline instead"
            ))
        };
        let (spec, plugin) = match self.selected_delivery_spec(job) {
            Ok(found) => found,
            Err(OrbitError::NotFound { .. }) => {
                return Err(refuse(match self.plugin_load().inactive_job_owner(job) {
                    Some(owner) => format!(
                        "plugin '{}' ships, but that plugin {}; enable it or remove the tag",
                        owner.plugin, owner.state
                    ),
                    None => "no active plugin or workspace job provides and no installed \
                             plugin ships; install and enable its plugin or remove the tag"
                        .to_string(),
                }));
            }
            Err(error) => return Err(error),
        };
        if spec.kind != JobKind::Workflow {
            return Err(refuse(format!(
                "declares `kind: {}` and cannot be dispatched",
                spec.kind
            )));
        }
        // A local selection needs no forge, so it is honoured in a PR ship.
        let local_in_pr =
            mode == ShipMode::Pr && job == LOCAL_ROUTE_JOB && spec.delivers_mode(ShipMode::Local);
        if !spec.delivers_mode(mode) && !local_in_pr {
            return Err(refuse(format!(
                "does not declare `spec.task_delivery.modes` containing '{}'",
                mode.as_input_value()
            )));
        }
        Ok(DeliveryRoute {
            job_name: job.to_string(),
            selected: true,
            plugin,
        })
    }

    /// The spec a selected job name executes under, and the active plugin
    /// contributing it, if any.
    fn selected_delivery_spec(&self, job: &str) -> Result<(JobV2, Option<String>), OrbitError> {
        if is_default_job_name(job) {
            return Ok((self.resolved_job_spec(job)?, None));
        }
        let (path, spec) = self.load_v2_job_asset_by_name(job)?;
        let plugin = self
            .plugin_load()
            .active_job_owner(&path)
            .map(str::to_string);
        Ok((spec, plugin))
    }
}

/// The one job every task of a bundle selects, with a task naming it, or
/// `None` when no task selects one. Tasks that disagree — including a task
/// with no selection bundled beside one with a selection — are refused: the
/// gate dispatches a single job for the whole bundle.
fn bundle_selection(tasks: &[Task]) -> Result<Option<(&str, &str)>, OrbitError> {
    let mut selections = Vec::with_capacity(tasks.len());
    for task in tasks {
        let selection = task
            .delivery_job_selection()
            .map_err(|error| OrbitError::InvalidInput(format!("task '{}': {error}", task.id)))?;
        selections.push((task.id.as_str(), selection));
    }
    let Some(&(first_task, first)) = selections.first() else {
        return Ok(None);
    };
    if let Some(&(other_task, other)) = selections.iter().find(|(_, other)| *other != first) {
        let describe = |selection: Option<&str>| {
            selection.map_or_else(
                || "the default pipeline".to_string(),
                |job| format!("'{job}'"),
            )
        };
        return Err(OrbitError::InvalidInput(format!(
            "bundled tasks select different delivery jobs: task '{first_task}' selects {}, task \
             '{other_task}' selects {}; ship them separately",
            describe(first),
            describe(other)
        )));
    }
    Ok(first.map(|job| (job, first_task)))
}
