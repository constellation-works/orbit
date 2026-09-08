//! Unified lexical, hybrid, and neighbor search over the dashboard HTTP API.

use std::str::FromStr;

use axum::extract::RawQuery;
use axum::response::{IntoResponse, Json, Response};
use orbit_core::{GlobalSearchKind, GlobalSearchParams};

use crate::state::Ws;

use super::{bad_request, blocking, non_empty_string};

pub(super) async fn search(Ws(runtime): Ws, RawQuery(raw): RawQuery) -> Response {
    let params = match parse_search_query(raw.as_deref()) {
        Ok(params) => params,
        Err(message) => return bad_request(message),
    };
    match blocking("search", move || runtime.global_search(params)).await {
        Ok(response) => Json(response).into_response(),
        Err(response) => *response,
    }
}

fn parse_search_query(raw: Option<&str>) -> Result<GlobalSearchParams, String> {
    let mut params = GlobalSearchParams {
        limit: 10,
        ..GlobalSearchParams::default()
    };

    for (key, value) in url::form_urlencoded::parse(raw.unwrap_or_default().as_bytes()) {
        match key.as_ref() {
            "query" | "q" => params.query = non_empty_string(&value),
            "hybrid" => params.hybrid = parse_bool("hybrid", &value)?,
            "semantic" => params.semantic = non_empty_string(&value),
            "kind" => {
                params.kind = GlobalSearchKind::from_str(&value)?;
            }
            "limit" => {
                let requested = value.parse::<usize>().map_err(|_| {
                    format!("invalid limit `{value}`; expected a non-negative integer")
                })?;
                // Same ceiling as every other list endpoint.
                params.limit = super::bounded_limit(Some(requested), params.limit);
            }
            "tag" | "tags" => append_csv(&mut params.tags, &value),
            "all" => params.all = parse_bool("all", &value)?,
            "status" | "statuses" => append_csv(&mut params.status, &value),
            "path" => params.path = non_empty_string(&value),
            // `workspace` is consumed by the Ws extractor. Ignore unknown
            // query keys like the other dashboard GET endpoints do.
            _ => {}
        }
    }

    Ok(params)
}

fn append_csv(target: &mut Vec<String>, raw: &str) {
    target.extend(
        raw.split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
    );
}

fn parse_bool(field: &str, raw: &str) -> Result<bool, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        _ => Err(format!(
            "invalid {field} `{raw}`; expected true, false, 1, or 0"
        )),
    }
}

#[cfg(test)]
mod query_tests {
    use super::*;

    #[test]
    fn query_parser_accepts_repeated_and_csv_filters() {
        let params = parse_search_query(Some(
            "query=agent+loop&kind=task&hybrid=true&tag=rust,search&tag=api&status=task%3Aopen&path=src%2Flib.rs&limit=7",
        ))
        .expect("parse query");

        assert_eq!(params.query.as_deref(), Some("agent loop"));
        assert_eq!(params.kind, GlobalSearchKind::Task);
        assert!(params.hybrid);
        assert_eq!(params.tags, ["rust", "search", "api"]);
        assert_eq!(params.status, ["task:open"]);
        assert_eq!(params.path.as_deref(), Some("src/lib.rs"));
        assert_eq!(params.limit, 7);
    }

    #[test]
    fn query_parser_caps_limit_like_every_other_list_endpoint() {
        let params = parse_search_query(Some("q=a&limit=100000000")).expect("parse query");
        assert_eq!(params.limit, super::super::HISTORY_MAX_LIMIT);
    }
}
