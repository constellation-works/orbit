//! Lexical search over the dashboard HTTP API.

use std::str::FromStr;

use axum::extract::RawQuery;
use axum::response::{IntoResponse, Json, Response};
use orbit_core::{GlobalSearchKind, SearchRequest, SearchRequestProfile};

use crate::state::Ws;

use super::{bad_request, blocking, non_empty_string};

pub(super) async fn search(Ws(runtime): Ws, RawQuery(raw): RawQuery) -> Response {
    let request = match parse_search_query(raw.as_deref()) {
        Ok(request) => request,
        Err(message) => return bad_request(message),
    };
    match blocking("search", move || {
        runtime.search_with_profile(SearchRequestProfile::Http, request)
    })
    .await
    {
        Ok(response) => Json(response).into_response(),
        Err(response) => *response,
    }
}

/// Decode the raw `/api/search` query string, accepting repeated and
/// comma-separated filters. Limit capping, defaults and the retired
/// `hybrid`/`semantic` refusal are the core Http profile's policy.
pub(super) fn parse_search_query(raw: Option<&str>) -> Result<SearchRequest, String> {
    let mut request = SearchRequest::default();

    for (key, value) in url::form_urlencoded::parse(raw.unwrap_or_default().as_bytes()) {
        if let Some(message) = SearchRequestProfile::Http.http_retired_parameter(key.as_ref()) {
            return Err(message);
        }
        match key.as_ref() {
            "query" | "q" => request.query = non_empty_string(&value),
            "kind" => {
                request.kind = GlobalSearchKind::from_str(&value)?;
            }
            "limit" => {
                request.limit = Some(value.parse::<usize>().map_err(|_| {
                    format!("invalid limit `{value}`; expected a non-negative integer")
                })?);
            }
            "tag" | "tags" => append_csv(&mut request.tags, &value),
            "all" => request.all = parse_bool("all", &value)?,
            "status" | "statuses" => append_csv(&mut request.status, &value),
            "path" => request.path = non_empty_string(&value),
            // `workspace` is consumed by the Ws extractor. Ignore unknown
            // query keys like the other dashboard GET endpoints do.
            _ => {}
        }
    }

    Ok(request)
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
