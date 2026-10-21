//! What a drain start or a ship submission reports while it still proceeds.
//!
//! None of these refuses the submission. Each one names a condition the run
//! would otherwise hit without a sign: a host throttle holding its waves
//! [ORB-13901], a validation environment that may not find the user's
//! toolchain [ORB-13987], backlog only a host of another OS can start, an
//! `execution.env.pass` variable or `[crews.<name>] env` source the launching
//! environment does not hold [ORB-14777], a workspace whose task pilot is not
//! enabled on this machine, or a host that runs no required validation. CLI,
//! MCP and the dashboard all render the same [`SubmissionNotices`].

use orbit_types::workflow::ResourceThrottle;
use serde_json::{Value, json};

use crate::OrbitRuntime;

/// The submission the notices describe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmissionKind {
    /// A local backlog drain (`orbit run auto`).
    Drain,
    /// A replica's pull drain (`orbit run auto --pull`). The owner orders
    /// its work, so this host's backlog is not reported.
    PullDrain,
    /// A ship run. `explicit` when it names its tasks: discovery is refused
    /// while the host is throttled, so only an explicit selection warns.
    Ship { explicit: bool },
}

impl SubmissionKind {
    const fn is_drain(self) -> bool {
        matches!(self, Self::Drain | Self::PullDrain)
    }
}

/// The warnings and notes one submission carries.
#[derive(Debug, Clone, PartialEq)]
pub struct SubmissionNotices {
    kind: SubmissionKind,
    /// Host pressure holding new admissions.
    pub resource_throttle: Option<ResourceThrottle>,
    /// Why required validation may not find the user's toolchain.
    pub validation_env_warning: Option<String>,
    /// Backlog tasks that need a host of another OS. Local drains only.
    pub host_os_backlog_warning: Option<String>,
    /// The `execution.env.pass` names this environment does not hold.
    pub unset_env_pass_warning: Option<String>,
    /// Crews whose `[crews.<name>] env` sources this environment does not
    /// hold; drains exclude them and their activities are refused.
    pub unset_crew_env_warning: Option<String>,
    /// Claude crews a drain here excludes for want of a worker credential
    /// [ORB-16663]. Drains only.
    pub claude_worker_credential_warning: Option<String>,
    /// That no task pilot prepares this workspace's tasks on this machine.
    /// Local drains only: a pull drain's owner prepares its work.
    pub task_pilot_warning: Option<String>,
    /// That this host declares no required validation commands. Drains only.
    pub required_validation_note: Option<String>,
}

impl SubmissionNotices {
    /// Every warning, one sentence each, in display order.
    #[must_use]
    pub fn warnings(&self) -> Vec<String> {
        self.resource_throttle
            .as_ref()
            .map(ResourceThrottle::hold_reason)
            .into_iter()
            .chain(self.validation_env_warning.clone())
            .chain(self.host_os_backlog_warning.clone())
            .chain(self.unset_env_pass_warning.clone())
            .chain(self.unset_crew_env_warning.clone())
            .chain(self.claude_worker_credential_warning.clone())
            .chain(self.task_pilot_warning.clone())
            .collect()
    }

    /// Informational notes; they never describe a problem.
    #[must_use]
    pub fn notes(&self) -> Vec<String> {
        self.required_validation_note.clone().into_iter().collect()
    }

    /// Add the notices to a JSON submission receipt: `warning` and `note`
    /// (one per line), plus the structured `validation_env_warning` and, for
    /// a drain, `resource_throttle`. Absent notices add no key.
    pub fn insert_into(&self, receipt: &mut Value) {
        if self.kind.is_drain()
            && let Some(throttle) = &self.resource_throttle
        {
            receipt["resource_throttle"] = json!(throttle);
        }
        if let Some(warning) = &self.validation_env_warning {
            receipt["validation_env_warning"] = json!(warning);
        }
        let warnings = self.warnings();
        if !warnings.is_empty() {
            receipt["warning"] = json!(warnings.join("\n"));
        }
        let notes = self.notes();
        if !notes.is_empty() {
            receipt["note"] = json!(notes.join("\n"));
        }
    }
}

impl OrbitRuntime {
    /// The notices for a submission of `kind` this runtime just accepted.
    pub fn submission_notices(&self, kind: SubmissionKind) -> SubmissionNotices {
        let throttled = match kind {
            SubmissionKind::Ship { explicit } => explicit,
            SubmissionKind::Drain | SubmissionKind::PullDrain => true,
        };
        SubmissionNotices {
            kind,
            resource_throttle: throttled
                .then(|| self.admission_resource_throttle().throttle)
                .flatten(),
            validation_env_warning: self.validation_env_preflight_warning(),
            host_os_backlog_warning: (kind == SubmissionKind::Drain)
                .then(|| self.host_os_backlog_warning())
                .flatten(),
            unset_env_pass_warning: self.unset_env_pass_warning(),
            unset_crew_env_warning: self.unset_crew_env_warning(),
            claude_worker_credential_warning: kind
                .is_drain()
                .then(|| self.claude_worker_credential_warning())
                .flatten(),
            task_pilot_warning: (kind == SubmissionKind::Drain)
                .then(|| self.task_pilot_warning())
                .flatten(),
            required_validation_note: kind
                .is_drain()
                .then(|| self.required_validation_note())
                .flatten(),
        }
    }
}
