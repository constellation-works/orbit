use std::str::FromStr;

use axum::body::Body;
use axum::http::uri::Authority;
use axum::http::{HeaderValue, Method, Request, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};
use serde_json::json;
use url::Url;

/// Browser CSRF and DNS-rebinding mitigation — NOT an access-control boundary.
///
/// Two independent gates, both on client-supplied headers that curl can set
/// arbitrarily, so this is not authentication and does not replace the
/// loopback bind in `serve()` (ORB-00360):
///
/// 1. `Host` itself must be an approved loopback authority (`localhost`,
///    `127.0.0.1`, `[::1]`, with an explicit port or the implicit HTTP
///    default). Missing or unparsable Host is refused. Browsers omit Origin
///    on same-origin GET, so without this gate a rebound hostname can read
///    every `/api` GET (ORB-12506) and `/healthz?detailed=true` (ORB-12531).
/// 2. When Origin is present, or the method is unsafe, Origin must also
///    match that Host as a loopback `http` origin (ORB-11613 CSRF).
pub(crate) async fn require_localhost_origin(request: Request<Body>, next: Next) -> Response {
    let Some(host) = parse_loopback_authority(request.headers().get(header::HOST)) else {
        return forbidden_cross_origin();
    };

    let unsafe_method = matches!(
        *request.method(),
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    );
    let origin = request.headers().get(header::ORIGIN);
    let allowed = origin
        .and_then(|origin| origin.to_str().ok())
        .and_then(|origin| Url::parse(origin).ok())
        .is_some_and(|origin| localhost_origin_matches_authority(&origin, &host));
    if !allowed && (unsafe_method || origin.is_some()) {
        return forbidden_cross_origin();
    }
    next.run(request).await
}

fn forbidden_cross_origin() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({"error": "cross-origin requests not allowed"})),
    )
        .into_response()
}

fn parse_loopback_authority(host: Option<&HeaderValue>) -> Option<Authority> {
    let authority = host
        .and_then(|host| host.to_str().ok())
        .and_then(|host| Authority::from_str(host).ok())?;
    // HTTP Host is name[:port]; userinfo is not a valid Host authority.
    if authority.as_str().contains('@') {
        return None;
    }
    if !host_port_is_valid(&authority) {
        return None;
    }
    is_approved_loopback_host(authority.host()).then_some(authority)
}

/// Accept an omitted port (implicit HTTP default) or a numeric `:port`.
/// `Authority` still parses `localhost:not-a-port` as host `localhost` with
/// no `port_u16`, so a present but non-numeric suffix must be refused.
fn host_port_is_valid(authority: &Authority) -> bool {
    if authority.port_u16().is_some() {
        return true;
    }
    let raw = authority.as_str();
    if let Some(end) = raw.find(']') {
        return !raw[end + 1..].starts_with(':');
    }
    !raw.contains(':')
}

fn is_approved_loopback_host(host: &str) -> bool {
    let host = host.trim_matches(['[', ']']);
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
}

fn localhost_origin_matches_authority(origin: &Url, authority: &Authority) -> bool {
    let Some(origin_host) = origin.host_str().map(|host| host.trim_matches(['[', ']'])) else {
        return false;
    };

    let authority_host = authority.host().trim_matches(['[', ']']);
    let same_host = origin_host.eq_ignore_ascii_case(authority_host);
    let same_port = origin.port_or_known_default() == authority.port_u16().or(Some(80));
    let valid_origin = origin.scheme() == "http"
        && origin.path() == "/"
        && origin.username().is_empty()
        && origin.password().is_none()
        && origin.query().is_none()
        && origin.fragment().is_none();

    valid_origin && is_approved_loopback_host(origin_host) && same_host && same_port
}

/// Mark JSON responses as non-sniffable so an error body cannot be
/// interpreted as an active document. Applied to `/api` and `/healthz`.
pub(crate) async fn nosniff_json_responses(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}
