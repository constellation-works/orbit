use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::uri::Authority;
use axum::http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};
use serde_json::json;
use url::Url;

use crate::state::{AlreadyRecorded, RefusalReason, RemoteAccess, TrustedHosts};

/// The identity header Tailscale Serve sets for a tailnet user. Serve strips
/// any client-supplied copy; Funnel and tagged-device requests carry none.
const TAILSCALE_USER_LOGIN: &str = "tailscale-user-login";

/// The Host a request was admitted on.
enum AdmittedHost {
    /// An approved loopback authority; Origin must be a matching loopback
    /// `http` origin.
    Loopback(Authority),
    /// A trusted host (lowercase name) whose request carried one listed
    /// `Tailscale-User-Login`; Origin must be exactly `https://<name>`.
    Trusted { name: String, login: String },
}

/// Browser CSRF and DNS-rebinding mitigation on the loopback path, and the
/// identity check on the trusted-host path.
///
/// Three gates on request headers:
///
/// 1. `Host` must be admitted ([`admitted_host`]). Missing or unparsable Host
///    is refused.
///    - A loopback authority (`localhost`, `127.0.0.1`, `[::1]`, with an
///      explicit port or the implicit HTTP default) is admitted on Host alone.
///      Curl can set Host arbitrarily, so this path is NOT authentication and
///      does not replace the loopback bind in `serve()` (ORB-00360). Browsers
///      omit Origin on same-origin GET, so without this gate a rebound
///      hostname could read every `/api` GET (ORB-12506),
///      `/healthz?detailed=true` (ORB-12531) and the dashboard shell and
///      assets (STD-05 §R16). The identity header is not consulted here.
///    - A name in `machine.web.trusted_hosts` (no port, or 443) is admitted
///      only with exactly one `Tailscale-User-Login` listed in
///      `machine.web.trusted_logins`. That header is the authentication for
///      this path: the server stays loopback-bound, so only a local process
///      or Tailscale Serve reaches it, and Serve strips client copies of the
///      header and sets it for tailnet users only (none for Funnel or tagged
///      devices). A local process can forge it, the same trust the loopback
///      path already extends to local processes.
/// 2. When Origin is present, or the method is unsafe, Origin must match the
///    admitted Host: a loopback `http` origin for a loopback Host
///    (ORB-11613 CSRF), exactly `https://<host>` for a trusted one.
/// 3. When the browser supplies `Sec-Fetch-Site`, it must say `same-origin`
///    or `none`. In particular, read-only GETs can open SSH tunnels or probe
///    hosts, so `same-site`, `cross-site` and unknown values are refused too.
///    The header is absent for direct CLI/curl requests and some older
///    browsers; those requests retain the Host and Origin checks above.
pub(crate) async fn require_localhost_origin(
    State(access): State<Arc<RemoteAccess>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let Some(gate) = Gate::open(&request, &access) else {
        return forbidden_cross_origin();
    };
    if request.headers().get_all(header::ORIGIN).iter().count() > 1 {
        return gate.refuse(RefusalReason::OriginMismatch);
    }

    if !fetch_site_allowed(request.headers()) {
        return gate.refuse(RefusalReason::SecFetchSite);
    }

    let unsafe_method = matches!(
        *request.method(),
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    );
    let origin = request.headers().get(header::ORIGIN);
    let allowed = origin
        .and_then(|origin| origin.to_str().ok())
        .and_then(|origin| Url::parse(origin).ok())
        .is_some_and(|origin| origin_matches_host(&origin, &gate.host));
    if !allowed && (unsafe_method || origin.is_some()) {
        return gate.refuse(RefusalReason::OriginMismatch);
    }
    gate.admit(request, next).await
}

/// Require an admitted `Host` (gate 1 of [`require_localhost_origin`],
/// including the trusted-host login check) and, when supplied, a single
/// `Origin` matching it. Origin-free safe navigation remains allowed. The
/// dashboard shell and embedded assets use this gate so a rebound Host or an
/// untrusted Origin cannot read them. Trusted hosts also enforce Fetch Metadata;
/// loopback asset requests retain their existing behavior.
pub(crate) async fn require_localhost_host(
    State(access): State<Arc<RemoteAccess>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let Some(gate) = Gate::open(&request, &access) else {
        return forbidden_cross_origin();
    };
    if matches!(gate.host, AdmittedHost::Trusted { .. }) && !fetch_site_allowed(request.headers()) {
        return gate.refuse(RefusalReason::SecFetchSite);
    }
    let origins = request.headers().get_all(header::ORIGIN);
    if origins.iter().count() > 1 {
        return gate.refuse(RefusalReason::OriginMismatch);
    }
    if let Some(origin) = request.headers().get(header::ORIGIN) {
        let allowed = origin
            .to_str()
            .ok()
            .and_then(|origin| Url::parse(origin).ok())
            .is_some_and(|origin| origin_matches_host(&origin, &gate.host));
        if !allowed {
            return gate.refuse(RefusalReason::OriginMismatch);
        }
    }
    gate.admit(request, next).await
}

fn fetch_site_allowed(headers: &HeaderMap) -> bool {
    headers.get_all("sec-fetch-site").iter().all(|site| {
        site.to_str()
            .is_ok_and(|site| matches!(site, "same-origin" | "none"))
    })
}

/// One request through either gate after Host admission: records what a
/// trusted-host request did on [`RemoteAccess`] (Fleet › Remote access).
/// Loopback requests, and a request the router dispatches to itself
/// ([`AlreadyRecorded`]), are never recorded.
struct Gate<'a> {
    host: AdmittedHost,
    access: &'a Arc<RemoteAccess>,
    record: bool,
}

impl<'a> Gate<'a> {
    /// Admit the Host, or record a trusted-path refusal and return `None`
    /// (the caller answers 403).
    fn open(request: &Request<Body>, access: &'a Arc<RemoteAccess>) -> Option<Self> {
        let record = request.extensions().get::<AlreadyRecorded>().is_none();
        match admitted_host(request.headers(), access.trusted_hosts()) {
            Ok(host) => Some(Self {
                record: record && matches!(host, AdmittedHost::Trusted { .. }),
                host,
                access,
            }),
            Err(refusal) => {
                if let Some((reason, login)) = refusal
                    && record
                {
                    access.record_refused(reason, login.as_deref());
                }
                None
            }
        }
    }

    fn refuse(&self, reason: RefusalReason) -> Response {
        if self.record {
            self.access.record_refused(reason, None);
        }
        forbidden_cross_origin()
    }

    /// Run the request; count it, and hold an open event stream's count
    /// until its body ends.
    async fn admit(self, request: Request<Body>, next: Next) -> Response {
        let AdmittedHost::Trusted { login, .. } = &self.host else {
            return next.run(request).await;
        };
        if !self.record {
            return next.run(request).await;
        }
        let device = forwarded_for(request.headers());
        self.access.record_admitted(device, login);
        let response = next.run(request).await;
        let stream = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"));
        if !stream {
            return response;
        }
        if let Some(guard) = self.access.open_stream(device, login) {
            crate::lease::listener::retain_response_allowance(response, guard)
        } else {
            response
        }
    }
}

/// The tailnet IP Serve forwarded for: the last `X-Forwarded-For` entry,
/// which Serve appends. Anything unparsable counts as an unknown device.
fn forwarded_for(headers: &HeaderMap) -> Option<IpAddr> {
    headers
        .get_all("x-forwarded-for")
        .iter()
        .next_back()?
        .to_str()
        .ok()?
        .rsplit(',')
        .next()?
        .trim()
        .parse()
        .ok()
}

fn forbidden_cross_origin() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({"error": "cross-origin requests not allowed"})),
    )
        .into_response()
}

/// A trusted-path refusal worth recording: its reason and, for an unlisted
/// login, the login attempted. `None` for a refusal off that path.
type Refusal = Option<(RefusalReason, Option<String>)>;

/// Gate 1 for the request's `Host`: exactly one header, naming an approved
/// loopback authority, or a trusted host on its default HTTPS port with
/// exactly one listed `Tailscale-User-Login`.
fn admitted_host(headers: &HeaderMap, trusted: &TrustedHosts) -> Result<AdmittedHost, Refusal> {
    if headers.get_all(header::HOST).iter().count() != 1 {
        return Err(None);
    }
    let authority = parse_host_authority(headers.get(header::HOST)).ok_or(None)?;
    if is_approved_loopback_host(authority.host()) {
        return Ok(AdmittedHost::Loopback(authority));
    }
    let name = authority.host().to_ascii_lowercase();
    if !trusted.admits_host(&name) || authority.port_u16().is_some_and(|port| port != 443) {
        return Err(None);
    }
    let mut logins = headers.get_all(TAILSCALE_USER_LOGIN).iter();
    let Some(login) = logins.next() else {
        return Err(Some((RefusalReason::NoIdentity, None)));
    };
    let Some(login) = login.to_str().ok().filter(|_| logins.next().is_none()) else {
        return Err(Some((RefusalReason::InvalidIdentity, None)));
    };
    if !trusted.admits_login(login) {
        return Err(Some((
            RefusalReason::UnlistedLogin,
            Some(login.to_string()),
        )));
    }
    Ok(AdmittedHost::Trusted {
        name,
        login: login.to_string(),
    })
}

fn parse_host_authority(host: Option<&HeaderValue>) -> Option<Authority> {
    let authority = host
        .and_then(|host| host.to_str().ok())
        .and_then(|host| Authority::from_str(host).ok())?;
    // HTTP Host is name[:port]; userinfo is not a valid Host authority.
    if authority.as_str().contains('@') {
        return None;
    }
    host_port_is_valid(&authority).then_some(authority)
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

/// Gate 2: an Origin a browser on the admitted Host would send — no path,
/// userinfo, query or fragment.
fn origin_matches_host(origin: &Url, host: &AdmittedHost) -> bool {
    let bare = origin.path() == "/"
        && origin.username().is_empty()
        && origin.password().is_none()
        && origin.query().is_none()
        && origin.fragment().is_none();
    bare && match host {
        AdmittedHost::Loopback(authority) => localhost_origin_matches_authority(origin, authority),
        AdmittedHost::Trusted { name, .. } => {
            origin.scheme() == "https"
                && origin.port().is_none()
                && origin
                    .host_str()
                    .is_some_and(|origin| origin.eq_ignore_ascii_case(name))
        }
    }
}

fn localhost_origin_matches_authority(origin: &Url, authority: &Authority) -> bool {
    let Some(origin_host) = origin.host_str().map(|host| host.trim_matches(['[', ']'])) else {
        return false;
    };

    let authority_host = authority.host().trim_matches(['[', ']']);
    let same_host = origin_host.eq_ignore_ascii_case(authority_host);
    let same_port = origin.port_or_known_default() == authority.port_u16().or(Some(80));
    origin.scheme() == "http" && is_approved_loopback_host(origin_host) && same_host && same_port
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
