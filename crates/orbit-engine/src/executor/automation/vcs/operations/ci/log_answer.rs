//! The private `ci.job_log` answer, typed once where the bounded log read ends.
//!
//! The producer builds it from the collector's `RunLogRead`; the operations
//! readers (prerequisite evidence, retry diagnostics) take that value, and the
//! landing transport serializes it through the private operation seam and
//! decodes it once on the other side. Fields keep missing, null and malformed
//! observations distinct from their known values, so each reader applies its
//! own completeness policy to the same answer.

use orbit_common::ProviderReadThrottle;
use orbit_tools::github_cli::{self, SOURCE_JOB_API_LOG, SOURCE_RUN_LOG};
use serde::{Serialize, Serializer};
use serde_json::{Map, Value};

use super::jobs::CiJobObservation;

/// Where the log bytes came from. An answer without a recognizable source is
/// a legacy shape, not an unknown source: `None` on the answer is absent,
/// [`CiLogSource::Unknown`] is a source name no reader trusts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::executor::automation::vcs) enum CiLogSource {
    RunLog,
    JobApiLog,
    Unknown(String),
}

impl CiLogSource {
    fn decode(source: &str) -> Self {
        match source {
            SOURCE_RUN_LOG => Self::RunLog,
            SOURCE_JOB_API_LOG => Self::JobApiLog,
            other => Self::Unknown(other.to_string()),
        }
    }

    fn name(&self) -> &str {
        match self {
            Self::RunLog => SOURCE_RUN_LOG,
            Self::JobApiLog => SOURCE_JOB_API_LOG,
            Self::Unknown(name) => name,
        }
    }
}

impl Serialize for CiLogSource {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.name())
    }
}

/// Identity of the job whose log a job-API fallback read. The wire object is
/// kept for serialization (it embeds `url` and any later field unchanged).
#[derive(Clone, Debug)]
pub(in crate::executor::automation::vcs) struct CiLogSourceJob {
    pub(in crate::executor::automation::vcs) job_id: CiJobObservation<u64>,
    pub(in crate::executor::automation::vcs) name: CiJobObservation<String>,
    pub(in crate::executor::automation::vcs) status: CiJobObservation<String>,
    pub(in crate::executor::automation::vcs) conclusion: CiJobObservation<String>,
    wire: Value,
}

impl Serialize for CiLogSourceJob {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.wire.serialize(serializer)
    }
}

impl CiLogSourceJob {
    fn decode(wire: Value) -> Self {
        let field = |key: &str| wire.get(key).cloned();
        Self {
            job_id: CiJobObservation::decode(field("job_id"), Value::as_u64),
            name: CiJobObservation::decode(field("name"), |v| v.as_str().map(str::to_string)),
            status: CiJobObservation::decode(field("status"), |v| v.as_str().map(str::to_string)),
            conclusion: CiJobObservation::decode(field("conclusion"), |v| {
                v.as_str().map(str::to_string)
            }),
            wire,
        }
    }
}

/// The collector's selected failure regions with the proof flags a reader
/// binds on. Every other field stays in the wire object, uninterpreted.
#[derive(Clone, Debug)]
pub(in crate::executor::automation::vcs) struct CiLogFailureRegions {
    kind: Option<String>,
    command_complete: Option<bool>,
    selection_complete: Option<bool>,
    pub(in crate::executor::automation::vcs) text: Option<String>,
    wire: Map<String, Value>,
}

impl CiLogFailureRegions {
    fn decode(wire: Map<String, Value>) -> Self {
        let flag = |key: &str| wire.get(key).and_then(Value::as_bool);
        Self {
            kind: wire.get("kind").and_then(Value::as_str).map(str::to_string),
            command_complete: flag("command_complete"),
            selection_complete: flag("selection_complete"),
            text: wire.get("text").and_then(Value::as_str).map(str::to_string),
            wire,
        }
    }

    /// The collector proved the whole command and the whole selection.
    pub(in crate::executor::automation::vcs) fn selection_proven(&self) -> bool {
        self.kind.as_deref() == Some("runner_failure_regions")
            && self.command_complete == Some(true)
            && self.selection_complete == Some(true)
    }

    /// The audit projection of the regions: everything but the selected text.
    pub(in crate::executor::automation::vcs) fn audit_wire(&self) -> Value {
        let mut wire = self.wire.clone();
        wire.remove("text");
        Value::Object(wire)
    }
}

impl Serialize for CiLogFailureRegions {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.wire.serialize(serializer)
    }
}

/// The provider's throttling answer beside a failed fallback read. A throttle
/// that cannot be read is no wait evidence, but is still carried verbatim.
#[derive(Clone, Debug)]
pub(in crate::executor::automation::vcs) enum CiLogReadThrottle {
    Absent,
    Known(ProviderReadThrottle),
    Unreadable(Value),
}

impl CiLogReadThrottle {
    fn decode(wire: Option<Value>) -> Self {
        match wire {
            None | Some(Value::Null) => Self::Absent,
            Some(wire) => match serde_json::from_value::<ProviderReadThrottle>(wire.clone()) {
                Ok(throttle) => Self::Known(throttle),
                Err(_) => Self::Unreadable(wire),
            },
        }
    }

    fn is_absent(&self) -> bool {
        matches!(self, Self::Absent)
    }
}

impl Serialize for CiLogReadThrottle {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Absent => serializer.serialize_none(),
            Self::Known(throttle) => throttle.serialize(serializer),
            Self::Unreadable(wire) => wire.serialize(serializer),
        }
    }
}

/// One failed job's bounded log read. `None` on a flag or text field is "not
/// observed": a reader's policy decides whether that is a refusal.
#[derive(Clone, Debug, Serialize)]
pub(in crate::executor::automation::vcs) struct CiJobLogAnswer {
    pub(in crate::executor::automation::vcs) log: Option<String>,
    pub(in crate::executor::automation::vcs) truncated: Option<bool>,
    pub(in crate::executor::automation::vcs) source_complete: Option<bool>,
    pub(in crate::executor::automation::vcs) diagnostic: Option<String>,
    pub(in crate::executor::automation::vcs) diagnostic_command: Option<String>,
    pub(in crate::executor::automation::vcs) failure_regions: Option<CiLogFailureRegions>,
    pub(in crate::executor::automation::vcs) source_bytes: Option<u64>,
    pub(in crate::executor::automation::vcs) source_jobs: Vec<CiLogSourceJob>,
    pub(in crate::executor::automation::vcs) source: Option<CiLogSource>,
    pub(in crate::executor::automation::vcs) fallback_error: Option<String>,
    // Present only beside `fallback_error`, so readers that refuse an
    // unreadable log keep refusing it.
    #[serde(skip_serializing_if = "CiLogReadThrottle::is_absent")]
    pub(in crate::executor::automation::vcs) read_throttle: CiLogReadThrottle,
}

impl CiJobLogAnswer {
    /// The producer: the collector's typed read, field for field.
    pub(super) fn from_read(read: github_cli::RunLogRead) -> Self {
        let github_cli::RunLogRead {
            log,
            source,
            source_jobs,
            fallback_error,
            fallback_throttle,
            ..
        } = read;
        Self {
            log: Some(log.text),
            truncated: Some(log.truncated),
            source_complete: Some(log.source_complete),
            diagnostic: log.diagnostic,
            diagnostic_command: log.diagnostic_command,
            failure_regions: match log.failure_regions {
                Some(Value::Object(regions)) => Some(CiLogFailureRegions::decode(regions)),
                _ => None,
            },
            source_bytes: Some(log.total_bytes as u64),
            source_jobs: source_jobs
                .into_iter()
                .map(CiLogSourceJob::decode)
                .collect(),
            source: Some(CiLogSource::decode(source)),
            fallback_error,
            read_throttle: fallback_throttle
                .map_or(CiLogReadThrottle::Absent, CiLogReadThrottle::Known),
        }
    }

    /// The transport consumer: decode the serialized answer once. Absent or
    /// malformed fields stay unobserved; nothing is defaulted to a pass.
    pub(in crate::executor::automation::vcs) fn decode_response(response: Value) -> Self {
        let text = |key: &str| {
            response
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        let flag = |key: &str| response.get(key).and_then(Value::as_bool);
        Self {
            log: text("log"),
            truncated: flag("truncated"),
            source_complete: flag("source_complete"),
            diagnostic: text("diagnostic"),
            diagnostic_command: text("diagnostic_command"),
            failure_regions: match response.get("failure_regions") {
                Some(Value::Object(regions)) => Some(CiLogFailureRegions::decode(regions.clone())),
                _ => None,
            },
            source_bytes: response.get("source_bytes").and_then(Value::as_u64),
            source_jobs: response
                .get("source_jobs")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .cloned()
                .map(CiLogSourceJob::decode)
                .collect(),
            source: response
                .get("source")
                .and_then(Value::as_str)
                .map(CiLogSource::decode),
            fallback_error: text("fallback_error"),
            read_throttle: CiLogReadThrottle::decode(response.get("read_throttle").cloned()),
        }
    }
}
