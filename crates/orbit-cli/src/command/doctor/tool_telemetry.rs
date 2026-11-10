//! Tool-telemetry capability of an executor definition, for
//! `orbit doctor providers`.
//!
//! Whether a provider CLI is installed says nothing about whether its output
//! carries tool-call evidence. Orbit records a run's tool calls, and so the
//! first-edit analytics built on them, only from a transport that emits tool
//! frames. This classifies the *effective* executor arguments, never the
//! binary, and never rewrites them.

use orbit_types::workflow::ExecutorDef;
use serde_json::{Value, json};

/// Whether an executor's effective arguments yield tool-call traces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ToolTelemetryStatus {
    /// The configured transport carries tool frames Orbit parses.
    Supported,
    /// The configured transport is valid for answers and usage but carries no
    /// tool frames, so no tool calls or first-edit samples can be recorded.
    Unavailable,
    /// Orbit does not recognise this configuration as supplying traces.
    Unknown,
    /// The executor launches no provider CLI.
    NotApplicable,
}

impl ToolTelemetryStatus {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::Unavailable => "unavailable",
            Self::Unknown => "unknown",
            Self::NotApplicable => "not_applicable",
        }
    }
}

/// The classification of one executor definition.
pub(super) struct ToolTelemetry {
    pub(super) status: ToolTelemetryStatus,
    /// The `--output-format` the effective arguments select, when one does.
    pub(super) output_format: Option<String>,
    pub(super) cause: Option<String>,
    pub(super) remediation: Option<String>,
}

impl ToolTelemetry {
    /// The short text-table label.
    pub(super) fn label(&self) -> String {
        match (self.status, self.output_format.as_deref()) {
            (ToolTelemetryStatus::NotApplicable, _) => "-".to_string(),
            (status, Some(format)) if status != ToolTelemetryStatus::Unknown => {
                format!("{} ({format})", status.as_str())
            }
            (status, _) => status.as_str().to_string(),
        }
    }

    pub(super) fn to_json(&self) -> Value {
        json!({
            "status": self.status.as_str(),
            "output_format": self.output_format,
            "cause": self.cause,
            "remediation": self.remediation,
        })
    }
}

/// Classify `def` by its effective `args`.
pub(super) fn classify(def: &ExecutorDef) -> ToolTelemetry {
    let Some(command) = def.command.as_deref() else {
        return ToolTelemetry {
            status: ToolTelemetryStatus::NotApplicable,
            output_format: None,
            cause: None,
            remediation: None,
        };
    };
    let program = std::path::Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command);
    if def.name != "grok" && program != "grok" {
        return ToolTelemetry {
            status: ToolTelemetryStatus::Unknown,
            output_format: None,
            cause: Some(
                "Orbit does not classify tool-call output for this executor's arguments; \
                 its runs are not claimed to record tool calls or first-edit samples."
                    .to_string(),
            ),
            remediation: None,
        };
    }
    classify_grok(output_format(&def.args))
}

fn classify_grok(format: Option<String>) -> ToolTelemetry {
    let (status, cause, remediation) = match format.as_deref() {
        Some("streaming-json") => (ToolTelemetryStatus::Supported, None, None),
        Some("json") => (
            ToolTelemetryStatus::Unavailable,
            Some(
                "The executor passes `--output-format json`, so grok prints one result \
                 document with no tool-call frames. Runs still succeed and their answer \
                 and usage are read from that document, but no tool calls are recorded: \
                 first-edit (turns-to-edit) analytics show every such run as an empty \
                 trace."
                    .to_string(),
            ),
            Some(
                "Change `--output-format json` to `--output-format streaming-json` in the \
                 installed grok executor definition (resources/executors/grok.yaml in the \
                 Orbit home), as the shipped definition does. Orbit does not edit it for \
                 you; only runs started afterwards record tool calls."
                    .to_string(),
            ),
        ),
        Some(_) => (
            ToolTelemetryStatus::Unknown,
            Some(
                "The executor selects an `--output-format` Orbit does not recognise as \
                 carrying grok tool-call frames; it is not claimed to record tool calls."
                    .to_string(),
            ),
            Some(
                "Use `--output-format streaming-json` to record tool calls and first-edit \
                 samples."
                    .to_string(),
            ),
        ),
        None => (
            ToolTelemetryStatus::Unknown,
            Some(
                "The executor sets no `--output-format`, so the transport is grok's own \
                 default and is not claimed to record tool calls."
                    .to_string(),
            ),
            Some(
                "Add `--output-format streaming-json` to record tool calls and first-edit \
                 samples."
                    .to_string(),
            ),
        ),
    };
    ToolTelemetry {
        status,
        output_format: format,
        cause,
        remediation,
    }
}

/// The last `--output-format <value>` or `--output-format=<value>` in `args`.
fn output_format(args: &[String]) -> Option<String> {
    let mut found = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--output-format" {
            found = iter.next().cloned();
        } else if let Some(value) = arg.strip_prefix("--output-format=") {
            found = Some(value.to_string());
        }
    }
    found
}
