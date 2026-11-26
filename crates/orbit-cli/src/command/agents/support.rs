use serde_json::Value;

pub(super) fn selection_line(value: &Value) -> String {
    // Only implementer counts read the execution scope.
    let execution = match value["execution"].as_str() {
        Some(scope) if value["role"] == "implementers" => format!(" · {scope} tasks"),
        _ => String::new(),
    };
    format!(
        "{} · window {} · minimum support {}{execution} · {} tasks scanned · computed {}",
        text(&value["role"]),
        text(&value["window"]),
        number(&value["min_support"]),
        number(&value["tasks_scanned"]),
        text(&value["computed_at"]),
    )
}

pub(super) fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}

/// A count, rate or score; `-` when the payload has none.
pub(super) fn number(value: &Value) -> String {
    match value {
        Value::Number(number) if number.is_f64() => {
            let rate = number.as_f64().unwrap_or(0.0);
            if rate.fract() == 0.0 {
                format!("{rate:.0}")
            } else {
                format!("{rate:.1}")
            }
        }
        Value::Number(number) => number.to_string(),
        _ => "-".into(),
    }
}
