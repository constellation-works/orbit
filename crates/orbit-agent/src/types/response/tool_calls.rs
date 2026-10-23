use orbit_types::telemetry::ToolCallTrace;
use serde_json::Value;

use super::{JsonMap, ToolCallCollector, usage::value_as_u64};

pub(super) fn extract_tool_calls(documents: &[Value]) -> Vec<ToolCallTrace> {
    let mut collector = ToolCallCollector::default();
    for document in documents {
        collector.walk(document);
    }
    collector.finish()
}

impl ToolCallCollector {
    // Only provider frames and their documented event containers are traversed.
    // Answer text, reasoning, histories and tool inputs/results are opaque even
    // when their JSON happens to describe a tool event.
    fn walk(&mut self, value: &Value) {
        match value {
            Value::Object(map) => self.collect_frame(map),
            Value::Array(items) => {
                for item in items {
                    self.walk(item);
                }
            }
            _ => {}
        }
    }

    fn collect_frame(&mut self, map: &JsonMap) {
        if map.contains_key("schemaVersion") {
            return;
        }
        let kind = map.get("type").and_then(Value::as_str);
        match kind {
            Some("item.started" | "item.completed") => {
                let Some(item) = map.get("item").and_then(Value::as_object) else {
                    return;
                };
                let item_kind = item.get("type").and_then(Value::as_str).unwrap_or_default();
                if kind == Some("item.started") && is_tool_use_kind(item_kind) {
                    self.record_tool_use(item);
                } else if kind == Some("item.completed")
                    && (is_tool_use_kind(item_kind) || is_tool_result_kind(item_kind))
                {
                    self.record_tool_result(item);
                }
            }
            Some("assistant" | "user") => {
                if let Some(message) = map.get("message").and_then(Value::as_object) {
                    self.collect_message(message, kind == Some("assistant"));
                }
            }
            Some(kind) if is_tool_use_kind(kind) => {
                if matches!(kind, "command_execution" | "mcp_tool_call") {
                    self.record_tool_result(map);
                } else {
                    self.record_tool_use(map);
                }
            }
            Some(kind) if is_tool_result_kind(kind) => self.record_tool_result(map),
            None => match map.get("role").and_then(Value::as_str) {
                Some("assistant") => self.collect_message(map, true),
                Some("user") => self.collect_message(map, false),
                Some("tool") => self.record_tool_result(map),
                None => self.collect_inline_tool_calls(map),
                _ => {}
            },
            _ => {}
        }
    }

    fn collect_message(&mut self, map: &JsonMap, assistant: bool) {
        if assistant {
            self.collect_inline_tool_calls(map);
        }
        if let Some(blocks) = map.get("content").and_then(Value::as_array) {
            for block in blocks.iter().filter_map(Value::as_object) {
                let kind = block
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if assistant && is_tool_use_kind(kind) {
                    self.record_tool_use(block);
                } else if !assistant && is_tool_result_kind(kind) {
                    self.record_tool_result(block);
                }
            }
        }
    }

    fn collect_inline_tool_calls(&mut self, map: &JsonMap) {
        if let Some(tool_calls) = map.get("tool_calls").and_then(Value::as_array) {
            for item in tool_calls {
                self.record_inline_tool_call(item);
            }
        }
    }

    fn record_inline_tool_call(&mut self, value: &Value) {
        let Some(map) = value.as_object() else {
            return;
        };
        let tool_name = tool_name_or_kind(map);
        if tool_name.is_empty() {
            return;
        }
        self.record_tool_use(map);
        if let Some(last) = self.calls.last_mut() {
            last.result_bytes = result_bytes_from_map(map);
        }
    }

    fn record_tool_use(&mut self, map: &JsonMap) {
        let tool_name = tool_name_or_kind(map);
        if tool_name.is_empty() {
            return;
        }
        let index = self.calls.len();
        self.calls.push(ToolCallTrace {
            seq: (index + 1) as u32,
            tool_name,
            result_bytes: 0,
            result_payload: None,
        });
        if let Some(id) = tool_call_id(map) {
            self.by_id.insert(id.to_string(), index);
        }
    }

    fn record_tool_result(&mut self, map: &JsonMap) {
        let result_bytes = result_bytes_from_map(map);

        if let Some(tool_use_id) = tool_call_id(map)
            && let Some(index) = self.by_id.get(tool_use_id).copied()
        {
            self.calls[index].result_bytes = result_bytes;
            self.calls[index].result_payload = None;
            return;
        }

        if tool_call_id(map).is_none()
            && let Some(last) = self
                .calls
                .iter_mut()
                .rev()
                .find(|call| call.result_bytes == 0)
        {
            last.result_bytes = result_bytes;
            last.result_payload = None;
            return;
        }

        let tool_name = tool_name_or_kind(map);
        if tool_name.is_empty() {
            return;
        }
        let index = self.calls.len();
        self.calls.push(ToolCallTrace {
            seq: (index + 1) as u32,
            tool_name: tool_name.clone(),
            result_bytes,
            result_payload: None,
        });
        if let Some(id) = tool_call_id(map) {
            self.by_id.insert(id.to_string(), index);
        }
    }

    fn finish(self) -> Vec<ToolCallTrace> {
        self.calls
    }
}

fn tool_name_from_map(map: &JsonMap) -> String {
    map.get("name")
        .and_then(Value::as_str)
        .or_else(|| map.get("tool_name").and_then(Value::as_str))
        .or_else(|| map.get("tool").and_then(Value::as_str))
        .unwrap_or_default()
        .to_string()
}

fn tool_name_or_kind(map: &JsonMap) -> String {
    let name = tool_name_from_map(map);
    if !name.is_empty() {
        return name;
    }

    map.get("type")
        .and_then(Value::as_str)
        .filter(|kind| is_tool_use_kind(kind))
        .unwrap_or_default()
        .to_string()
}

fn tool_call_id(map: &JsonMap) -> Option<&str> {
    map.get("tool_use_id")
        .and_then(Value::as_str)
        .or_else(|| map.get("call_id").and_then(Value::as_str))
        .or_else(|| map.get("id").and_then(Value::as_str))
}

fn result_bytes_from_map(map: &JsonMap) -> u64 {
    map.get("result_bytes")
        .and_then(value_as_u64)
        .unwrap_or_else(|| result_value_from_map(map).map(serialized_size).unwrap_or(0))
}

fn result_value_from_map(map: &JsonMap) -> Option<&Value> {
    map.get("result")
        .or_else(|| map.get("content"))
        .or_else(|| map.get("output"))
        .or_else(|| map.get("aggregated_output"))
}

fn is_tool_use_kind(kind: &str) -> bool {
    matches!(
        kind,
        "tool_use"
            | "tool_call"
            | "function_call"
            | "custom_tool_call"
            | "command_execution"
            | "mcp_tool_call"
    )
}

fn is_tool_result_kind(kind: &str) -> bool {
    matches!(
        kind,
        "tool_result" | "function_call_output" | "custom_tool_call_output"
    )
}

fn serialized_size(value: &Value) -> u64 {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len() as u64)
        .unwrap_or(0)
}
