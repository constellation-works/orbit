//! Allowlisted pending-operation context, independent of refused line content.

use serde_json::{Value, json};

use super::super::config::Destination;

/// Only metadata already present locally is retained. Never keep arguments,
/// dynamic field names, payload prefixes, or stderr in a framing diagnostic.
pub(super) struct OversizedContext {
    pending: Value,
}

impl OversizedContext {
    pub(super) fn pending(method: &str, params: &Value, dispatched: bool) -> Self {
        let arguments = &params["arguments"];
        let worker_read = arguments["_worker_read"]
            .as_str()
            .filter(|value| {
                matches!(
                    *value,
                    "task"
                        | "tasks"
                        | "artifact"
                        | "artifacts"
                        | "steering"
                        | "tags"
                        | "search"
                        | "filtered"
                        | "status_index"
                        | "completion_by_complexity"
                        | "complexity_by_id"
                        | "terminal_transitions"
                        | "defect_filings"
                )
            })
            .unwrap_or("unknown");
        // Iterate the fixed allowlist, not untrusted argument keys or fields.
        let selected_fields = [
            "id",
            "title",
            "status",
            "description",
            "acceptance_criteria",
            "plan",
            "context_files",
            "comments",
            "artifacts",
            "execution_summary",
        ]
        .into_iter()
        .filter(|field| {
            arguments["fields"]
                .as_array()
                .is_some_and(|fields| fields.iter().any(|value| value.as_str() == Some(field)))
        })
        .collect::<Vec<_>>();
        Self {
            pending: json!({
                "phase": if dispatched { "dispatched_call" } else { "probe" },
                "method": method,
                "tool": params["name"].as_str().map(|name| name.chars().filter(|ch| !ch.is_control()).take(128).collect::<String>()).unwrap_or_else(|| "unknown".into()),
                "projection": {"_worker_read": worker_read, "selected_fields": selected_fields},
            }),
        }
    }

    pub(super) fn refusal(
        mut self,
        destination: &Destination,
        peer_version: Option<&str>,
        id: i64,
        cap: u64,
        observed_bytes_lower_bound: u64,
    ) -> String {
        self.pending["request_id"] = json!(id);
        let metadata = json!({
            "destination_machine_id": destination.machine_id,
            "local_version": env!("CARGO_PKG_VERSION"),
            "local_build": "unknown",
            "peer_version": peer_version.unwrap_or("unknown"),
            "peer_build": "unknown",
            "pending": self.pending,
            "line_cap_bytes": cap,
            "observed_bytes_lower_bound": observed_bytes_lower_bound,
            "correlation": "unknown",
        });
        format!("oversized transport line refused; diagnostic={metadata}")
    }
}

/// Version evidence comes from the existing handshake/discovery, never a new
/// probe. Refuse oversized or non-version text rather than quoting peer data.
pub(super) fn known_version(version: Option<&str>) -> Option<String> {
    version
        .filter(|version| {
            version.len() <= 64
                && version.split('.').take(3).count() == 3
                && version.as_bytes().first().is_some_and(u8::is_ascii_digit)
                && version
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b".+-".contains(&byte))
        })
        .map(ToOwned::to_owned)
}
