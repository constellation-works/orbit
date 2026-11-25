//! Hydrating only some top-level keys of a run's stored input
//! ([`JobRunQuery::input_keys`](crate::contracts::JobRunQuery::input_keys)).

use std::fmt;

use serde::de::{DeserializeSeed, Deserializer, IgnoredAny, MapAccess, Visitor};
use serde_json::{Map, Value};

/// `raw` parsed as JSON, keeping only `keys` of a top-level object, in the
/// order and with the values a whole parse gives them. Other values are
/// skipped without being built. Any input that is not a JSON object is parsed
/// whole, so it reads exactly as it would without the projection.
pub(super) fn parse_input_keys(raw: &str, keys: &[&str]) -> serde_json::Result<Value> {
    let mut deserializer = serde_json::Deserializer::from_str(raw);
    match deserializer
        .deserialize_map(KeptKeys(keys))
        .and_then(|kept| deserializer.end().map(|()| kept))
    {
        Ok(kept) => Ok(Value::Object(kept)),
        Err(_) => serde_json::from_str(raw),
    }
}

/// Visits a top-level object, keeping the listed keys.
struct KeptKeys<'k>(&'k [&'k str]);

impl<'de> Visitor<'de> for KeptKeys<'_> {
    type Value = Map<String, Value>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut kept = Map::new();
        while let Some(key) = map.next_key_seed(KeyMatch(self.0))? {
            match key {
                // A repeated key replaces the earlier value, as a whole parse does.
                Some(key) => {
                    kept.insert(key.to_owned(), map.next_value::<Value>()?);
                }
                None => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(kept)
    }
}

/// An object key, matched against the listed keys without allocating.
struct KeyMatch<'k>(&'k [&'k str]);

impl<'de, 'k> DeserializeSeed<'de> for KeyMatch<'k> {
    type Value = Option<&'k str>;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_str(self)
    }
}

impl<'de, 'k> Visitor<'de> for KeyMatch<'k> {
    type Value = Option<&'k str>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an object key")
    }

    fn visit_str<E: serde::de::Error>(self, key: &str) -> Result<Self::Value, E> {
        Ok(self.0.iter().copied().find(|kept| *kept == key))
    }
}
