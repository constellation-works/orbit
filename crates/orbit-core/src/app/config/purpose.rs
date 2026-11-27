//! Each crew purpose's resolved pool and where it came from, for `orbit config
//! show` and the dashboard's settings view.
//!
//! A projection of the effective values only: a purpose whose own key holds a
//! pool reports that pool and the layer that set it; an unset purpose reports
//! the route it keeps and, when that route is one pool key, that pool and its
//! layer.

use orbit_config::EffectiveConfigValue;
use orbit_types::workflow::CrewPurpose;
use serde_json::{Value as JsonValue, json};

/// One row per purpose, in [`CrewPurpose::ALL`] order.
pub fn crew_purposes_view(values: &[EffectiveConfigValue]) -> JsonValue {
    let entry = |key: &str| values.iter().find(|entry| entry.key == key);
    let pool = |entry: Option<&EffectiveConfigValue>| -> Vec<String> {
        entry
            .and_then(|entry| entry.value.as_array())
            .into_iter()
            .flatten()
            .filter_map(JsonValue::as_str)
            .map(str::to_string)
            .collect()
    };
    JsonValue::Array(
        CrewPurpose::ALL
            .into_iter()
            .map(|purpose| {
                let key = purpose.config_key();
                let own = entry(key);
                let own_pool = pool(own);
                // Final recovery and review have no fallback: their own key is
                // the only route, configured or not.
                let (resolved_from, resolved, configured) =
                    match (own_pool.is_empty(), purpose.fallback()) {
                        (true, Some(fallback)) => (fallback, entry(fallback), false),
                        _ => (key, own, true),
                    };
                json!({
                    "purpose": purpose.as_str(),
                    "key": key,
                    "configured": configured && !own_pool.is_empty(),
                    "pool": pool(resolved),
                    "resolved_from": resolved_from,
                    "source": resolved.map(|entry| entry.source.kind().label()),
                })
            })
            .collect(),
    )
}
