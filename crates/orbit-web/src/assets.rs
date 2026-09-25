use std::io::Write;
use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use flate2::{Compression, write::GzEncoder};
use orbit_core::OrbitError;

use crate::{DASHBOARD_CSS, state};

const HTML: &str = "text/html; charset=utf-8";
const CSS: &str = "text/css; charset=utf-8";
const JS: &str = "application/javascript; charset=utf-8";
const WOFF2: &str = "font/woff2";

/// Every embedded dashboard file as `(route, content type, body)`.
// L-0021: Keep embedded dashboard modules in sync with the files they import.
pub(super) const DASHBOARD_FILES: &[(&str, &str, &[u8])] = &[
    ("/", HTML, include_bytes!("../assets/dashboard/index.html")),
    ("/static/dashboard.css", CSS, DASHBOARD_CSS.as_bytes()),
    (
        "/static/fonts/geist-latin.woff2",
        WOFF2,
        include_bytes!("../assets/dashboard/fonts/geist-latin.woff2"),
    ),
    (
        "/static/fonts/geist-mono-latin.woff2",
        WOFF2,
        include_bytes!("../assets/dashboard/fonts/geist-mono-latin.woff2"),
    ),
    (
        "/static/vendor/marked.umd.js",
        JS,
        include_bytes!("../assets/dashboard/vendor/marked.umd.js"),
    ),
    (
        "/static/vendor/purify.min.js",
        JS,
        include_bytes!("../assets/dashboard/vendor/purify.min.js"),
    ),
    (
        "/static/app.js",
        JS,
        include_bytes!("../assets/dashboard/app.js"),
    ),
    (
        "/static/js/common.js",
        JS,
        include_bytes!("../assets/dashboard/js/common.js"),
    ),
    (
        "/static/js/config.js",
        JS,
        include_bytes!("../assets/dashboard/js/config.js"),
    ),
    (
        "/static/js/markdown.js",
        JS,
        include_bytes!("../assets/dashboard/js/markdown.js"),
    ),
    (
        "/static/js/tasks.js",
        JS,
        include_bytes!("../assets/dashboard/js/tasks.js"),
    ),
    (
        "/static/js/field-editor.js",
        JS,
        include_bytes!("../assets/dashboard/js/field-editor.js"),
    ),
    (
        "/static/js/audit.js",
        JS,
        include_bytes!("../assets/dashboard/js/audit.js"),
    ),
    (
        "/static/js/scoreboard.js",
        JS,
        include_bytes!("../assets/dashboard/js/scoreboard.js"),
    ),
    (
        "/static/js/reliability.js",
        JS,
        include_bytes!("../assets/dashboard/js/reliability.js"),
    ),
    (
        "/static/js/log-tail.js",
        JS,
        include_bytes!("../assets/dashboard/js/log-tail.js"),
    ),
    (
        "/static/js/diagnostics.js",
        JS,
        include_bytes!("../assets/dashboard/js/diagnostics.js"),
    ),
    (
        "/static/js/router.js",
        JS,
        include_bytes!("../assets/dashboard/js/router.js"),
    ),
    (
        "/static/js/runs.js",
        JS,
        include_bytes!("../assets/dashboard/js/runs.js"),
    ),
    (
        "/static/js/run-detail.js",
        JS,
        include_bytes!("../assets/dashboard/js/run-detail.js"),
    ),
    (
        "/static/js/distributed.js",
        JS,
        include_bytes!("../assets/dashboard/js/distributed.js"),
    ),
    (
        "/static/js/operations.js",
        JS,
        include_bytes!("../assets/dashboard/js/operations.js"),
    ),
    (
        "/static/js/automation.js",
        JS,
        include_bytes!("../assets/dashboard/js/automation.js"),
    ),
    (
        "/static/js/plugins.js",
        JS,
        include_bytes!("../assets/dashboard/js/plugins.js"),
    ),
];
pub(super) const DASHBOARD_CSP: &str = concat!(
    "default-src 'self'; ",
    "script-src 'self'; ",
    "style-src 'self' 'unsafe-inline'; ",
    "font-src 'self'; ",
    "img-src 'self' data:; ",
    "connect-src 'self'; ",
    "object-src 'none'; ",
    "base-uri 'none'; ",
    "frame-ancestors 'none'"
);
const DASHBOARD_CACHE_CONTROL: &str = "no-cache";

struct DashboardAsset {
    content_type: &'static str,
    body: &'static [u8],
    gzip_body: Bytes,
    etag: HeaderValue,
}

impl DashboardAsset {
    fn new(content_type: &'static str, body: &'static [u8]) -> Result<Self, OrbitError> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(body)
            .map_err(|error| OrbitError::Execution(format!("gzip dashboard asset: {error}")))?;
        let gzip_body = encoder.finish().map_err(|error| {
            OrbitError::Execution(format!("finish gzip dashboard asset: {error}"))
        })?;
        let digest = blake3::hash(body);
        let etag = HeaderValue::from_str(&format!(r#""{}""#, digest.to_hex()))
            .map_err(|error| OrbitError::Execution(format!("dashboard asset ETag: {error}")))?;

        Ok(Self {
            content_type,
            body,
            gzip_body: Bytes::from(gzip_body),
            etag,
        })
    }
}

/// One route per embedded dashboard file, each serving its precompressed,
/// ETag-validated asset.
pub(super) fn dashboard_file_router() -> Result<Router<state::DashboardState>, OrbitError> {
    DASHBOARD_FILES
        .iter()
        .try_fold(Router::new(), |router, &(route, content_type, body)| {
            let asset = Arc::new(DashboardAsset::new(content_type, body)?);
            Ok(router.route(
                route,
                get(move |headers: HeaderMap| {
                    let asset = Arc::clone(&asset);
                    async move { dashboard_asset_response(&asset, &headers) }
                }),
            ))
        })
}

fn dashboard_asset_response(asset: &DashboardAsset, request_headers: &HeaderMap) -> Response {
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(asset.content_type),
    );
    response_headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(DASHBOARD_CACHE_CONTROL),
    );
    response_headers.insert(header::ETAG, asset.etag.clone());
    response_headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(DASHBOARD_CSP),
    );
    response_headers.insert(header::VARY, HeaderValue::from_static("Accept-Encoding"));

    if if_none_match_matches(request_headers.get(header::IF_NONE_MATCH), &asset.etag) {
        return (StatusCode::NOT_MODIFIED, response_headers).into_response();
    }

    if accepts_gzip(request_headers.get(header::ACCEPT_ENCODING)) {
        response_headers.insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        return (response_headers, asset.gzip_body.clone()).into_response();
    }

    (response_headers, asset.body).into_response()
}

fn if_none_match_matches(value: Option<&HeaderValue>, etag: &HeaderValue) -> bool {
    let Some(value) = value.and_then(|value| value.to_str().ok()) else {
        return false;
    };
    let Some(etag) = etag.to_str().ok() else {
        return false;
    };

    value.split(',').any(|candidate| {
        let candidate = candidate.trim();
        candidate == "*"
            || candidate == etag
            || candidate
                .strip_prefix("W/")
                .is_some_and(|weak_etag| weak_etag.trim() == etag)
    })
}

fn accepts_gzip(value: Option<&HeaderValue>) -> bool {
    let Some(value) = value.and_then(|value| value.to_str().ok()) else {
        return false;
    };

    let mut gzip_quality = None;
    let mut wildcard_quality = None;
    for encoding in value.split(',') {
        let mut parts = encoding.split(';');
        let coding = parts.next().unwrap_or("").trim();
        let quality = parts
            .filter_map(|parameter| parameter.trim().split_once('='))
            .find_map(|(name, value)| {
                name.trim()
                    .eq_ignore_ascii_case("q")
                    .then(|| value.trim().parse::<f32>().unwrap_or(0.0))
            })
            .unwrap_or(1.0);

        if coding.eq_ignore_ascii_case("gzip") {
            gzip_quality = Some(quality);
        } else if coding == "*" {
            wildcard_quality = Some(quality);
        }
    }

    gzip_quality.or(wildcard_quality).unwrap_or(0.0) > 0.0
}

/// Serve the embedded dashboard file at `route` as its route handler would.
#[cfg(test)]
pub(super) fn serve_dashboard_file(route: &str, headers: &HeaderMap) -> Response {
    let &(_, content_type, body) = DASHBOARD_FILES
        .iter()
        .find(|(candidate, _, _)| *candidate == route)
        .unwrap_or_else(|| panic!("no embedded dashboard file is routed at {route}"));
    let asset = DashboardAsset::new(content_type, body)
        .unwrap_or_else(|error| panic!("build dashboard asset {route}: {error}"));
    dashboard_asset_response(&asset, headers)
}
