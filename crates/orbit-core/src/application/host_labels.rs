//! Label placement: which hosts may run a task by its `requires_labels`.
//!
//! A host carries its `machine.labels` plus its OS as an implicit label. A
//! task's `requires_labels` must all be among them (all-of), on top of its
//! `os:` tags (any-of). This host knows its own set; every other executor's
//! is its latest executor-census declaration in this workspace, which a
//! follower sends on each pull. Placement reads the two the same way at write
//! time (refuse what no host could run), at admission (skip what this host
//! cannot run) and on read surfaces (name the wait while no host that could
//! run it is pulling).

use std::collections::BTreeSet;

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_types::task::{
    HostOs, REQUIRES_LABELS_FIELD, Task, TaskOsRequirement, effective_host_labels,
    host_labels_wait, missing_host_labels, quoted_labels,
};

use crate::OrbitRuntime;

/// Days a census declaration counts as a host serving this workspace, the
/// same window claim evidence uses for creation-time crew draws.
const SERVING_LABEL_EVIDENCE_DAYS: i64 = 7;

/// The name of this host when the runtime has no machine identity.
const LOCAL_HOST: &str = "this host";

/// One host's effective labels, as placement reads them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FleetLabelHost {
    /// The machine id, or `this host` for a runtime with no machine identity.
    pub machine: String,
    /// Whether this is the runtime's own host.
    pub local: bool,
    pub os: Option<HostOs>,
    /// Its `machine.labels` plus its OS.
    pub labels: BTreeSet<String>,
    /// Whether it is pulling or draining now: this host always is; another
    /// executor while its census row is live.
    pub live: bool,
    /// When another executor last declared these labels.
    pub seen_at: Option<DateTime<Utc>>,
}

impl FleetLabelHost {
    /// Whether this host may start a task with `tags` and `requires_labels`.
    #[must_use]
    pub fn runs(&self, tags: &[String], requires_labels: &[String]) -> bool {
        TaskOsRequirement::from_tags(tags).satisfied_by(self.os)
            && missing_host_labels(requires_labels, &self.labels).is_empty()
    }

    fn describe(&self) -> String {
        let name = if self.local && self.machine != LOCAL_HOST {
            format!("{} (this host)", self.machine)
        } else {
            self.machine.clone()
        };
        let labels = self.labels.iter().cloned().collect::<Vec<_>>();
        format!("{name}: {}", quoted_labels(&labels))
    }
}

impl OrbitRuntime {
    /// This host, then every other executor whose census row in this
    /// workspace was declared within the last seven days, each with its
    /// effective labels. A census that cannot be read leaves this host
    /// alone, so placement never admits more than this host can show.
    pub(crate) fn fleet_label_hosts(&self) -> Vec<FleetLabelHost> {
        let own = self.automation_machine_identity().map(str::to_string);
        let mut hosts = vec![FleetLabelHost {
            machine: own.clone().unwrap_or_else(|| LOCAL_HOST.to_string()),
            local: true,
            os: self.host_os(),
            labels: self.host_labels(),
            live: true,
            seen_at: None,
        }];
        let now = Utc::now();
        let recorded = match self.stores().tasks().recorded_executor_census() {
            Ok(recorded) => recorded,
            Err(error) => {
                tracing::warn!(
                    target: "orbit.core.placement",
                    %error,
                    "could not read the executor census; label placement sees only this host"
                );
                return hosts;
            }
        };
        let live = self
            .stores()
            .tasks()
            .live_executor_census(now)
            .map(|live| {
                live.into_iter()
                    .map(|entry| entry.machine_id)
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        let cutoff = now - Duration::days(SERVING_LABEL_EVIDENCE_DAYS);
        for entry in recorded {
            if own.as_deref() == Some(entry.machine_id.as_str()) || entry.seen_at < cutoff {
                continue;
            }
            hosts.push(FleetLabelHost {
                local: false,
                live: live.contains(&entry.machine_id),
                labels: effective_host_labels(&entry.capability.labels, entry.capability.os),
                os: entry.capability.os,
                seen_at: Some(entry.seen_at),
                machine: entry.machine_id,
            });
        }
        hosts
    }

    /// Refuse a `requires_labels` no registered serving host carries
    /// together with the task's `os:` tags, naming the labels no host
    /// carries and every known label set. The task is not written.
    pub(crate) fn refuse_unservable_labels(
        &self,
        tags: &[String],
        requires_labels: &[String],
    ) -> Result<(), OrbitError> {
        if requires_labels.is_empty() {
            return Ok(());
        }
        let hosts = self.fleet_label_hosts();
        if hosts.iter().any(|host| host.runs(tags, requires_labels)) {
            return Ok(());
        }
        let carried = hosts
            .iter()
            .flat_map(|host| host.labels.iter().cloned())
            .collect::<BTreeSet<_>>();
        let missing = missing_host_labels(requires_labels, &carried);
        let gap = if missing.is_empty() {
            "no single host carries all of them together with the task's `os:` tags".to_string()
        } else {
            format!("no host carries {}", quoted_labels(&missing))
        };
        let known = hosts
            .iter()
            .map(FleetLabelHost::describe)
            .collect::<Vec<_>>()
            .join("; ");
        Err(OrbitError::InvalidInput(format!(
            "{REQUIRES_LABELS_FIELD} {} cannot be satisfied by any registered serving host: \
             {gap}. Known label sets: {known}. Declare the labels in `machine.labels` on the \
             host that has the capability; a follower is known here once it has pulled from \
             this workspace",
            quoted_labels(requires_labels)
        )))
    }

    /// Why this host may not start `task` for its labels, or `None` when it
    /// carries every one: `waits for a host labelled `gpu` (requires_labels);
    /// this host lacks `gpu``.
    pub fn task_host_labels_wait(&self, task: &Task) -> Option<String> {
        if task.requires_labels.is_empty() {
            return None;
        }
        let missing = missing_host_labels(&task.requires_labels, &self.host_labels());
        (!missing.is_empty()).then(|| {
            format!(
                "{}; this host lacks {}",
                host_labels_wait(&task.requires_labels),
                quoted_labels(&missing)
            )
        })
    }

    /// The named wait for `task` while no live host can run it for its
    /// labels, reading the fleet once.
    pub fn task_placement_wait(&self, task: &Task) -> Option<String> {
        if task.requires_labels.is_empty() {
            return None;
        }
        label_placement_wait(task, &self.fleet_label_hosts())
    }

    /// The OS set of the known hosts that carry `requires_labels`, which a
    /// creation-time crew draw narrows a task's `os:` requirement to.
    /// `None` when the task requires no label.
    pub(crate) fn label_carrier_oses(
        &self,
        requires_labels: &[String],
    ) -> Option<BTreeSet<HostOs>> {
        if requires_labels.is_empty() {
            return None;
        }
        Some(
            self.fleet_label_hosts()
                .into_iter()
                .filter(|host| missing_host_labels(requires_labels, &host.labels).is_empty())
                .filter_map(|host| host.os)
                .collect(),
        )
    }
}

/// The named wait for a task no live host can run for its labels, read
/// against `hosts` (see [`OrbitRuntime::fleet_label_hosts`]), or `None` when
/// the task requires no label or a live host carries them all.
#[must_use]
pub(crate) fn label_placement_wait(task: &Task, hosts: &[FleetLabelHost]) -> Option<String> {
    if task.requires_labels.is_empty() {
        return None;
    }
    let carriers = hosts
        .iter()
        .filter(|host| host.runs(&task.tags, &task.requires_labels))
        .collect::<Vec<_>>();
    if carriers.iter().any(|host| host.live) {
        return None;
    }
    let wait = host_labels_wait(&task.requires_labels);
    if carriers.is_empty() {
        return Some(format!("{wait}: no registered serving host carries them"));
    }
    let now = Utc::now();
    let last = carriers
        .iter()
        .map(|host| match host.seen_at {
            Some(seen) => format!(
                "{} last pulled {} min ago",
                host.machine,
                now.signed_duration_since(seen).num_minutes().max(0)
            ),
            None => format!("{} is not pulling", host.machine),
        })
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "{wait}: no host carrying them is pulling now ({last})"
    ))
}
