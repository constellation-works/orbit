//! Shared validation and AND-across / OR-within item filters.
use orbit_common::protocol::tool_input::parse_duration_seconds;
use orbit_common::{
    OrbitError,
    governance::{attention::AttentionVerb, operation::OperationVerb},
};
use orbit_types::{
    task::TaskStatus,
    tool::{AttentionItem, AttentionKind, AttentionSeverity},
};
use serde_json::Value;
use std::str::FromStr;

pub(super) struct AttentionFilter {
    pub input: Value,
    pub kinds: Vec<AttentionKind>,
    pub severity: Option<AttentionSeverity>,
    pub older_than: Option<u64>,
    pub limit: usize,
}
pub(super) fn attention_strings(input: &Value, key: &str) -> Vec<String> {
    input
        .get(key)
        .map(|value| match value {
            Value::String(value) => vec![value.clone()],
            Value::Array(values) => values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            _ => Vec::new(),
        })
        .unwrap_or_default()
}
impl AttentionFilter {
    pub fn parse(input: Value) -> Result<Self, OrbitError> {
        let input = AttentionVerb::Query.spec().parse_input(input)?.into_value();
        let requested = attention_strings(&input, "kind");
        let excluded = attention_strings(&input, "exclude_kind");
        for kind in requested.iter().chain(&excluded) {
            if !AttentionKind::NAMES.contains(&kind.as_str()) {
                return Err(OrbitError::InvalidInput(format!(
                    "unknown_kind: {kind}; expected {}",
                    AttentionKind::NAMES.join(", ")
                )));
            }
        }
        let kinds = AttentionKind::NAMES
            .iter()
            .filter(|kind| {
                ((requested.is_empty() && AttentionKind::IMPLEMENTED.contains(kind))
                    || requested.iter().any(|v| v == **kind))
                    && !excluded.iter().any(|v| v == **kind)
            })
            .map(|kind| {
                serde_json::from_value(Value::String((*kind).into()))
                    .map_err(|error| OrbitError::InvalidInput(error.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let severity = input
            .get("severity")
            .and_then(Value::as_str)
            .map(|value| {
                serde_json::from_value::<AttentionSeverity>(Value::String(value.into())).map_err(
                    |_| {
                        OrbitError::InvalidInput(
                            "unknown severity; expected low, medium, high or critical".into(),
                        )
                    },
                )
            })
            .transpose()?;
        for status in attention_strings(&input, "status") {
            TaskStatus::from_str(&status).map_err(|_| OrbitError::InvalidInput(format!("unknown_status: {status}; expected proposed, backlog, in-progress, review, done, blocked, rejected, archived or someday")))?;
        }
        let older_than = input
            .get("older_than")
            .and_then(Value::as_str)
            .map(parse_duration_seconds)
            .transpose()?;
        let limit = input.get("limit").and_then(Value::as_u64).unwrap_or(100);
        if !(1..=500).contains(&limit) {
            return Err(OrbitError::InvalidInput(
                "limit must be between 1 and 500".into(),
            ));
        }
        if input.get("all") == Some(&Value::Bool(true))
            && !attention_strings(&input, "workspace").is_empty()
        {
            return Err(OrbitError::InvalidInput(
                "all conflicts with workspace".into(),
            ));
        }
        Ok(Self {
            input,
            kinds,
            severity,
            older_than,
            limit: limit as usize,
        })
    }
    pub fn selected(&self, kind: AttentionKind) -> bool {
        self.kinds.contains(&kind)
    }
    pub fn matches(&self, item: &AttentionItem) -> bool {
        if !self.selected(item.kind)
            || self.severity.is_some_and(|min| item.severity < min)
            || self
                .older_than
                .is_some_and(|min| item.age_seconds.is_none_or(|age| age < min))
        {
            return false;
        }
        for key in ["status", "tag", "task", "crew", "orchestrator"] {
            let values = attention_strings(&self.input, key);
            if values.is_empty() {
                continue;
            }
            let Some(task) = &item.task else {
                return false;
            };
            let matches = match key {
                "status" => values.iter().any(|value| value == &task.status.to_string()),
                "tag" => values.iter().any(|value| task.tags.contains(value)),
                "task" => values
                    .iter()
                    .any(|value| item.refs["task_id"].as_str() == Some(value)),
                "crew" => task
                    .crew
                    .as_ref()
                    .is_some_and(|value| values.contains(value)),
                "orchestrator" => task
                    .orchestrator
                    .as_ref()
                    .is_some_and(|value| values.contains(value)),
                _ => false,
            };
            if !matches {
                return false;
            }
        }
        true
    }
}
