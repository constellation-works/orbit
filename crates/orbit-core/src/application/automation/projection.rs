//! Compact polling diagnostics shared by HTTP automation listings.

use serde_json::{Value, json};

const SAMPLE_LIMIT: usize = 20;

/// Project inspection state for polling without copying retained membership.
pub fn automation_summary(diagnostic: &Value) -> Value {
    let Some(fields) = diagnostic.as_object() else {
        return diagnostic.clone();
    };
    let mut result: serde_json::Map<String, Value> = fields
        .iter()
        .filter(|(name, _)| name.as_str() != "state")
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let Some(original) = diagnostic["state"].as_object() else {
        result.insert("state".into(), Value::Null);
        return Value::Object(result);
    };
    let inventories = [
        "pending",
        "pending_commits",
        "waived",
        "excluded",
        "unresolved",
        "associations",
        "members",
    ];
    let mut state: serde_json::Map<String, Value> = original
        .iter()
        .filter(|(name, _)| !inventories.contains(&name.as_str()))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let mut counts = serde_json::Map::new();
    for name in ["pending", "pending_commits", "waived", "excluded"] {
        if let Some(Value::Array(entries)) = original.get(name) {
            counts.insert(name.into(), json!(entries.len()));
            if name == "excluded" {
                state.insert(
                    name.into(),
                    json!(entries.iter().take(SAMPLE_LIMIT).collect::<Vec<_>>()),
                );
            }
        }
    }
    counts.insert(
        "unresolved".into(),
        json!(object_len(&diagnostic["state"]["unresolved"])),
    );
    state.insert(
        "unresolved".into(),
        sample_map(&diagnostic["state"]["unresolved"], Clone::clone),
    );
    state.insert("counts".into(), Value::Object(counts));
    if let Some(members) = original
        .get("members")
        .filter(|members| members.is_object())
    {
        let pending = &members["pending"];
        let fresh = members["assessed"]
            .as_object()
            .into_iter()
            .flat_map(|entries| entries.iter())
            .filter(|(key, assessment)| {
                pending.get(*key).is_none_or(|member| {
                    member["fingerprint"] == assessment["resulting_fingerprint"]
                })
            })
            .map(|(_, assessment)| assessment)
            .collect::<Vec<_>>();
        state.insert("members".into(), json!({
            "counts": {
                "pending": object_len(pending),
                "fresh": fresh.len(),
                "ready": fresh.iter().filter(|assessment| assessment["ready"] == true).count(),
                "withheld": object_len(&members["withheld"]),
                "failed": object_len(&members["failed"]),
            },
            "withheld": sample_map(&members["withheld"], Clone::clone),
            "failed": sample_map(&members["failed"], member_attempt),
            "active": members.get("active").filter(|active| !active.is_null()).map(member_attempt),
            "scan_after": members["scan_after"],
        }));
    }
    result.insert("state".into(), Value::Object(state));
    Value::Object(result)
}

fn object_len(value: &Value) -> usize {
    value.as_object().map_or(0, serde_json::Map::len)
}

fn sample_map(value: &Value, project: impl Fn(&Value) -> Value) -> Value {
    Value::Object(
        value
            .as_object()
            .into_iter()
            .flat_map(|entries| entries.iter())
            .take(SAMPLE_LIMIT)
            .map(|(key, value)| (key.clone(), project(value)))
            .collect(),
    )
}

fn member_attempt(attempt: &Value) -> Value {
    json!({
        "member": {"key": attempt["member"]["key"]},
        "members": attempt["members"].as_array().map(|members| members.iter().map(|member| json!({"key": member["key"]})).collect::<Vec<_>>()).unwrap_or_default(),
        "attempt": attempt["attempt"],
        "max_attempts": attempt["max_attempts"],
        "deadline": attempt["deadline"],
        "action_id": attempt["action_id"],
    })
}
