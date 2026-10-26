//! Process-local browser credentials. No request body or credential is audited.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::{FromRequestParts, Request};
use axum::http::{StatusCode, header, request::Parts};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use orbit_core::OrbitError;
use serde_json::json;

const TICKET_SECONDS: u64 = 300;
const MAX_SESSIONS: usize = 1024;
const MAX_USED_TICKETS: usize = 4096;
const COOKIE: &str = "orbit_browser_session";

/// Authentication state for one server lifetime, never serialized or debugged.
pub(crate) struct BrowserSessions {
    key: [u8; 32],
    ttl: Duration,
    credentials: Mutex<Credentials>,
}

#[derive(Default)]
struct Credentials {
    wall_high_water: u64,
    used: Vec<(blake3::Hash, u64)>,
    sessions: Vec<(blake3::Hash, Instant)>,
}

impl BrowserSessions {
    pub(crate) fn from_key(key: [u8; 32], ttl_seconds: u32) -> Self {
        Self {
            key,
            ttl: Duration::from_secs(ttl_seconds.into()),
            credentials: Mutex::new(Credentials::default()),
        }
    }

    /// A public boot identifier, domain-separated from ticket authentication.
    pub(crate) fn identity(&self) -> String {
        blake3::keyed_hash(&self.key, b"orbit browser session identity v1")
            .to_hex()
            .to_string()
    }

    fn exchange(&self, ticket: &str) -> Option<String> {
        let decoded = URL_SAFE_NO_PAD.decode(ticket).ok()?;
        if decoded.len() != 72 {
            return None;
        }
        let (payload, mac) = decoded.split_at(40);
        let expected = ticket_mac(&self.key, payload);
        // blake3::Hash equality uses constant_time_eq, including slice equality.
        if expected != *mac {
            return None;
        }
        let issued = u64::from_le_bytes(payload[..8].try_into().ok()?);
        let now = unix_seconds().ok()?;
        if issued > now || now.saturating_sub(issued) >= TICKET_SECONDS {
            return None;
        }
        let digest = blake3::hash(&decoded);
        let mut credentials = self.credentials.lock().ok()?;
        // A backwards wall-clock adjustment must not resurrect an expired
        // ticket whose replay record was already pruned. Sessions themselves
        // always use monotonic expiry.
        if now < credentials.wall_high_water {
            return None;
        }
        credentials.wall_high_water = now;
        credentials.used.retain(|(_, expiry)| *expiry > now);
        let monotonic_now = Instant::now();
        credentials
            .sessions
            .retain(|(_, expiry)| *expiry > monotonic_now);
        if credentials.used.len() >= MAX_USED_TICKETS
            || credentials.sessions.len() >= MAX_SESSIONS
            || credentials.used.iter().any(|(used, _)| *used == digest)
        {
            return None;
        }
        let cookie = URL_SAFE_NO_PAD.encode(random_key().ok()?);
        credentials.used.push((digest, issued + TICKET_SECONDS));
        credentials
            .sessions
            .push((blake3::hash(cookie.as_bytes()), monotonic_now + self.ttl));
        Some(cookie)
    }

    fn authenticated(&self, cookie: &str) -> bool {
        if cookie.len() != 43 {
            return false;
        }
        let digest = blake3::hash(cookie.as_bytes());
        let now = Instant::now();
        let Ok(mut credentials) = self.credentials.lock() else {
            return false;
        };
        credentials.sessions.retain(|(_, expiry)| *expiry > now);
        credentials
            .sessions
            .iter()
            .any(|(session, _)| *session == digest)
    }
}

pub(crate) fn random_key() -> Result<[u8; 32], OrbitError> {
    let mut key = [0; 32];
    getrandom::fill(&mut key)
        .map_err(|_| OrbitError::Execution("browser credential entropy unavailable".into()))?;
    Ok(key)
}

fn unix_seconds() -> Result<u64, OrbitError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| OrbitError::Execution("browser credential clock unavailable".into()))
}

fn ticket_mac(key: &[u8; 32], payload: &[u8]) -> blake3::Hash {
    let mut hasher = blake3::Hasher::new_keyed(key);
    hasher.update(b"orbit browser ticket v1");
    hasher.update(payload);
    hasher.finalize()
}

/// Mint locally from the private startup key; the key itself never crosses HTTP.
pub(crate) fn browser_ticket(key: &[u8; 32]) -> Result<String, OrbitError> {
    let mut bytes = Vec::with_capacity(72);
    bytes.extend_from_slice(&unix_seconds()?.to_le_bytes());
    bytes.extend_from_slice(&random_key()?);
    let mac = ticket_mac(key, &bytes);
    bytes.extend_from_slice(mac.as_bytes());
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// Local identity probe lets `web open` reject a stale credential file.
pub(crate) async fn session_identity(session: Option<Extension<Arc<BrowserSessions>>>) -> Response {
    match session {
        Some(Extension(session)) => (
            [(header::CACHE_CONTROL, "no-store")],
            Json(json!({"instance": session.identity()})),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Exchange the fragment's ticket once, after the router's origin guard.
pub(crate) async fn exchange_session(
    session: Option<Extension<Arc<BrowserSessions>>>,
    request: Request,
) -> Response {
    let Some(Extension(session)) = session else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(body) = axum::body::to_bytes(request.into_body(), 256).await else {
        return unauthorized();
    };
    let Some(cookie) = std::str::from_utf8(&body)
        .ok()
        .and_then(|ticket| session.exchange(ticket))
    else {
        return unauthorized();
    };
    (
        StatusCode::NO_CONTENT,
        [
            (
                header::SET_COOKIE,
                format!(
                    "{COOKIE}={cookie}; HttpOnly; SameSite=Strict; Path=/api; Max-Age={}",
                    session.ttl.as_secs()
                ),
            ),
            (header::CACHE_CONTROL, "no-store".into()),
        ],
    )
        .into_response()
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({"error": "unauthorized"})),
    )
        .into_response()
}

/// Require a live browser session on an explicitly opted-in route.
///
/// Existing dashboard routes do not use this extractor. Sessions expire in
/// memory as well as in the cookie and are invalidated by every server restart.
pub struct RequireBrowserSession;

impl<S: Send + Sync> FromRequestParts<S> for RequireBrowserSession {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let session = parts
            .extensions
            .get::<Arc<BrowserSessions>>()
            .ok_or_else(unauthorized)?;
        let mut cookies = parts
            .headers
            .get_all(header::COOKIE)
            .iter()
            .filter_map(|header| header.to_str().ok())
            .flat_map(|value| value.split(';'))
            .filter_map(|value| value.trim().split_once('='))
            .filter(|(name, _)| *name == COOKIE);
        let (_, cookie) = cookies.next().ok_or_else(unauthorized)?;
        if cookies.next().is_some() || !session.authenticated(cookie) {
            return Err(unauthorized());
        }
        Ok(Self)
    }
}
