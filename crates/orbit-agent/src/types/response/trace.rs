use orbit_types::telemetry::InvocationTrace;
use serde_json::Value;

use super::{tool_calls::extract_tool_calls, usage::sum_usage};

pub(super) fn extract_invocation_trace(documents: &[Value], duration_ms: u64) -> InvocationTrace {
    let usage = sum_usage(documents);
    let tool_calls = extract_tool_calls(documents);
    InvocationTrace {
        usage,
        tool_calls,
        // The caller knows whether the capture it parsed was cut short.
        tool_calls_incomplete: false,
        duration_ms,
        provider_model: extract_provider_model(documents),
        provider_cost_usd: extract_provider_cost_usd(documents),
        long_prompt_requests: None,
    }
}

fn extract_provider_model(documents: &[Value]) -> Option<String> {
    documents.iter().rev().find_map(|document| {
        let object = document.as_object()?;

        if let Some(model_usage) = object.get("modelUsage").and_then(Value::as_object) {
            return select_claude_model(model_usage);
        }

        object
            .get("stats")
            .and_then(Value::as_object)
            .and_then(|stats| stats.get("models"))
            .and_then(Value::as_object)
            .and_then(select_single_model)
    })
}

fn select_claude_model(model_usage: &serde_json::Map<String, Value>) -> Option<String> {
    if let Some(model) = select_single_model(model_usage) {
        return Some(model);
    }

    let ranked = model_usage
        .iter()
        .map(|(model, usage)| {
            let cost = usage.as_object()?.get("costUSD")?.as_f64()?;
            Some((model, cost))
        })
        .collect::<Option<Vec<_>>>()?;
    let mut ranked = ranked.into_iter();
    let (best_model, best_cost) = ranked.next()?;
    let mut tied = false;
    let (best_model, _) = ranked.fold((best_model, best_cost), |best, candidate| {
        if candidate.1 > best.1 {
            tied = false;
            candidate
        } else {
            if candidate.1 == best.1 {
                tied = true;
            }
            best
        }
    });

    (!tied).then(|| best_model.to_string())
}

fn select_single_model(models: &serde_json::Map<String, Value>) -> Option<String> {
    let mut keys = models.keys();
    let model = keys.next()?.trim();
    (keys.next().is_none() && !model.is_empty()).then(|| model.to_string())
}

fn extract_provider_cost_usd(documents: &[Value]) -> Option<f64> {
    documents.iter().rev().find_map(|document| {
        document
            .as_object()?
            .get("total_cost_usd")
            .and_then(Value::as_f64)
    })
}
