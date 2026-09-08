//! Routine definition schema (v1) — the durable, git-versioned unit of
//! scheduled work. One YAML file under `.orbit/routines/` in a routine-source
//! workspace describes a cron trigger, a catalog target, host pinning, and a
//! retry/overlap policy. See `docs/design/routines/2_design.md` [ORB-10021].
//!
//! Parsing is fail-closed: an invalid file is an error, never a routine that
//! fires with defaults. Targets are catalog references only — there is no
//! inline command form (ADR-0206 posture).

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::error::WorkflowError;

/// Routine YAML schema version this binary reads and writes.
pub const ROUTINE_SCHEMA_VERSION: u32 = 1;

/// Prefix for job catalog target references (`job:<name>`).
pub const ROUTINE_JOB_TARGET_PREFIX: &str = "job:";
/// Prefix reserved for activity references. Not dispatchable in v1: run
/// dispatch is job-shaped (`submit_pipeline_run` resolves jobs by name),
/// so activities are fired by wrapping them in a one-step job in the same
/// source workspace.
pub const ROUTINE_ACTIVITY_TARGET_PREFIX: &str = "activity:";

/// A parsed, validated routine definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutineDefinition {
    /// Schema version marker (`schemaVersion: 1`).
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    /// Unique routine name across all routine sources on a host.
    pub name: String,
    /// Human description of what the routine does.
    #[serde(default)]
    pub description: String,
    /// Versioned global kill-switch. Absent means enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Explicit host pinning — the routine fires only on hosts whose
    /// `host_id` appears here. There is no "any host" value in v1.
    ///
    /// A *committed* definition (`.orbit/routines/*.yaml`) must list at least
    /// one host (see [`RoutineDefinition::validate_committed`]). A *local*
    /// definition (`.orbit/routines/local/*.yaml`) may omit it — it is
    /// implicitly pinned to the loading host — and, if present, may name only
    /// that host (see [`RoutineDefinition::validate_local`] and
    /// [`parse_local_routine_yaml`]).
    #[serde(default)]
    pub hosts: Vec<String>,
    /// When the routine is due.
    pub trigger: RoutineTrigger,
    /// What fires: a catalog reference (`job:<name>`).
    pub target: RoutineTarget,
    /// Timeout, retry, and overlap handling applied by the dispatcher.
    #[serde(default)]
    pub policy: RoutinePolicy,
}

/// Cron trigger plus the policy for fires missed while the host was asleep
/// or powered off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutineTrigger {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<super::automation::members::StateTrigger>,
    /// Standard 5-field cron expression, evaluated in host-local time.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cron: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deliveries_landed: Option<super::automation::DeliveryTrigger>,
    /// What to do about scheduled slots that fell in a gap. Defaults to
    /// `skip` (wait for the next natural slot).
    #[serde(default)]
    pub missed_run: MissedRunPolicy,
}

/// Missed-fire policy for gaps (sleep, downtime) between sweeps.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissedRunPolicy {
    /// Fire a single make-up run on the next sweep, no matter how many
    /// slots were missed.
    CatchUpOnce,
    /// Ignore missed slots; wait for the next natural one.
    #[default]
    Skip,
}

/// What a routine fires: a reference into the existing catalog, resolved at
/// load time. v1 dispatches `job:<name>` only — see
/// [`ROUTINE_ACTIVITY_TARGET_PREFIX`] for why `activity:` is reserved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutineTarget {
    /// A v2 job resolved by name through the job catalog.
    Job(String),
}

impl RoutineTarget {
    /// The catalog job name this target dispatches.
    pub fn job_name(&self) -> &str {
        match self {
            Self::Job(name) => name,
        }
    }

    /// Canonical string form (`job:<name>`).
    pub fn as_ref_string(&self) -> String {
        match self {
            Self::Job(name) => format!("{ROUTINE_JOB_TARGET_PREFIX}{name}"),
        }
    }

    fn parse(raw: &str) -> Result<Self, String> {
        let value = raw.trim();
        if let Some(name) = value.strip_prefix(ROUTINE_JOB_TARGET_PREFIX) {
            let name = name.trim();
            if name.is_empty() {
                return Err("target 'job:' is missing a job name".to_string());
            }
            return Ok(Self::Job(name.to_string()));
        }
        if let Some(name) = value.strip_prefix(ROUTINE_ACTIVITY_TARGET_PREFIX) {
            return Err(format!(
                "target 'activity:{}' is not dispatchable in routines v1; wrap the \
                 activity in a one-step job in the same workspace and reference it \
                 as 'job:<name>'",
                name.trim()
            ));
        }
        // ADR-0194: source provenance for rejecting inline routine commands.
        Err(format!(
            "target '{value}' must be a catalog reference of the form 'job:<name>'; \
             inline commands are not supported"
        ))
    }
}

impl Serialize for RoutineTarget {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.as_ref_string())
    }
}

impl<'de> Deserialize<'de> for RoutineTarget {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

/// Dispatcher policy applied around each fire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutinePolicy {
    /// Ceiling on how long one fire may stay in flight before it stops
    /// blocking `overlap: forbid` (the staleness horizon) and is recorded
    /// as timed out.
    #[serde(default = "default_timeout_minutes")]
    pub timeout_minutes: u64,
    /// Bounded retries with fixed backoff for failed fires.
    #[serde(default)]
    pub retries: RoutineRetries,
    /// Whether a due fire may dispatch while a previous fire of the same
    /// routine is still in flight.
    #[serde(default)]
    pub overlap: OverlapPolicy,
}

impl Default for RoutinePolicy {
    fn default() -> Self {
        Self {
            timeout_minutes: default_timeout_minutes(),
            retries: RoutineRetries::default(),
            overlap: OverlapPolicy::default(),
        }
    }
}

/// Retry bounds for failed fires: up to `max` re-dispatches, each waiting at
/// least `backoff_minutes` after the previous failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutineRetries {
    /// Maximum re-dispatches after the initial fire (0 = no retries).
    #[serde(default)]
    pub max: u32,
    /// Fixed delay between a failure and its retry.
    #[serde(default = "default_backoff_minutes")]
    pub backoff_minutes: u64,
}

impl Default for RoutineRetries {
    fn default() -> Self {
        Self {
            max: 0,
            backoff_minutes: default_backoff_minutes(),
        }
    }
}

/// Overlap handling when a fire comes due while another is in flight.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlapPolicy {
    /// Skip the fire while one is in flight (the default).
    #[default]
    Forbid,
    /// Dispatch anyway.
    Allow,
}

const fn default_true() -> bool {
    true
}

const fn default_timeout_minutes() -> u64 {
    60
}

const fn default_backoff_minutes() -> u64 {
    2
}

/// Maximum supported timeout or retry backoff. A week is long enough for
/// unattended maintenance while keeping routine policy mistakes bounded.
const MAX_DURATION_MINUTES: u64 = 7 * 24 * 60;

impl RoutineDefinition {
    /// Origin-agnostic semantic checks beyond serde shape: name charset, no
    /// blank host entries, non-empty cron, positive timeout. Full cron parsing
    /// happens in the scheduler (orbit-core), which owns the cron dependency.
    /// Host-count and host-identity rules are origin-specific — see
    /// [`Self::validate_committed`] and [`Self::validate_local`].
    pub fn validate_common(&self) -> Result<(), WorkflowError> {
        if !is_valid_routine_name(&self.name) {
            return Err(WorkflowError::Invalid(format!(
                "routine name '{}' must be non-empty, lowercase alphanumeric \
                 with '-' or '_' separators, and start alphanumeric",
                self.name
            )));
        }
        if self.hosts.iter().any(|host| host.trim().is_empty()) {
            return Err(WorkflowError::Invalid(format!(
                "routine '{}' hosts must not contain empty entries",
                self.name
            )));
        }
        if let Some(state) = &self.trigger.state {
            state.validate()?;
            if !self.trigger.cron.is_empty()
                || self.trigger.deliveries_landed.is_some()
                || self.hosts.len() > 1
                || self.policy.overlap != OverlapPolicy::Forbid
                || self.target.job_name() != state.job_name()
            {
                return Err(WorkflowError::Invalid("state routines require their pilot/triage target, one owner, overlap forbid and exactly one trigger".into()));
            }
        }
        if let Some(delivery) = &self.trigger.deliveries_landed {
            delivery.validate()?;
            if !self.trigger.cron.is_empty()
                || self.hosts.len() > 1
                || self.policy.overlap != OverlapPolicy::Forbid
            {
                return Err(WorkflowError::Invalid(
                    "delivery routines require one owner, overlap forbid, and exactly one trigger"
                        .into(),
                ));
            }
        }
        if self.trigger.cron.trim().is_empty()
            && self.trigger.deliveries_landed.is_none()
            && self.trigger.state.is_none()
        {
            return Err(WorkflowError::Invalid(format!(
                "routine '{}' trigger.cron must not be empty",
                self.name
            )));
        }
        if self.policy.timeout_minutes == 0 || self.policy.timeout_minutes > MAX_DURATION_MINUTES {
            return Err(WorkflowError::Invalid(format!(
                "routine '{}' policy.timeout_minutes must be between 1 and {MAX_DURATION_MINUTES}",
                self.name
            )));
        }
        if self.policy.retries.backoff_minutes > MAX_DURATION_MINUTES {
            return Err(WorkflowError::Invalid(format!(
                "routine '{}' policy.retries.backoff_minutes must not exceed {MAX_DURATION_MINUTES}",
                self.name
            )));
        }
        Ok(())
    }

    /// Committed-origin validation (`.orbit/routines/*.yaml`, excluding
    /// `local/`): the origin-agnostic checks plus a mandatory, non-empty host
    /// pin. A committed definition with no `hosts:` is a fail-closed load error
    /// — never "any host" — because an unpinned routine checked out on N
    /// source machines is N independent schedules (host-registry design §6).
    pub fn validate_committed(&self) -> Result<(), WorkflowError> {
        self.validate_common()?;
        if self.hosts.is_empty() {
            return Err(WorkflowError::Invalid(format!(
                "committed routine '{}' must pin at least one host (there is no \
                 \"any host\" in v1; move it to .orbit/routines/local/ to run \
                 only on the local host)",
                self.name
            )));
        }
        Ok(())
    }

    /// Local-origin validation (`.orbit/routines/local/`): the origin-agnostic
    /// checks; `hosts:` may be omitted (an implicit pin to the loading host)
    /// but any entry must name only `local_host_id`. A local definition that
    /// names another host — a remote pin — is rejected rather than becoming a
    /// hidden cross-machine schedule (host-registry design §6).
    pub fn validate_local(&self, local_host_id: &str) -> Result<(), WorkflowError> {
        self.validate_common()?;
        for host in &self.hosts {
            if host.trim() != local_host_id {
                return Err(WorkflowError::Invalid(format!(
                    "local routine '{}' pins host '{}', but a definition under \
                     .orbit/routines/local/ is implicit to the loading host '{}' and \
                     may not name another host; move it to a committed \
                     .orbit/routines/ definition to target another machine",
                    self.name,
                    host.trim(),
                    local_host_id
                )));
            }
        }
        Ok(())
    }
}

fn is_valid_routine_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return false;
    }
    chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_')
}
