//! Listener-only configuration and response-lifetime permits.

use std::pin::Pin;
use std::task::{Context, Poll};

use axum::body::{Body, Bytes};
use axum::response::Response;
use hyper::body::Frame;

/// The former `lease.listener.limits.address_per_minute` default, fixed
/// when the key was removed [ORB-22808].
pub const LEASE_LISTENER_ADDRESS_PER_MINUTE: u32 = 120;
/// The former `lease.listener.limits.ipv6_prefix_len` default, fixed
/// when the key was removed [ORB-22808].
pub const LEASE_LISTENER_IPV6_PREFIX_LEN: u32 = 64;
/// The former `lease.listener.limits.token_reads_per_minute` default, fixed
/// when the key was removed [ORB-22808].
pub const LEASE_LISTENER_TOKEN_READS_PER_MINUTE: u32 = 60;
/// The former `lease.listener.limits.token_writes_per_minute` default, fixed
/// when the key was removed [ORB-22808].
pub const LEASE_LISTENER_TOKEN_WRITES_PER_MINUTE: u32 = 20;
/// The former `lease.listener.limits.max_streams_per_token` default, fixed
/// when the key was removed [ORB-22808].
pub const LEASE_LISTENER_MAX_STREAMS_PER_TOKEN: u32 = 2;
/// The former `lease.listener.limits.max_in_flight_per_token` default, fixed
/// when the key was removed [ORB-22808].
pub const LEASE_LISTENER_MAX_IN_FLIGHT_PER_TOKEN: u32 = 8;
/// The former `lease.listener.limits.max_connections` default, fixed
/// when the key was removed [ORB-22808].
pub const LEASE_LISTENER_MAX_CONNECTIONS: u32 = 64;
/// The former `lease.listener.limits.max_in_flight_requests` default, fixed
/// when the key was removed [ORB-22808].
pub const LEASE_LISTENER_MAX_IN_FLIGHT_REQUESTS: u32 = 16;
/// The former `lease.listener.limits.max_body_bytes` default, fixed
/// when the key was removed [ORB-22808].
pub const LEASE_LISTENER_MAX_BODY_BYTES: u32 = 65536;
/// The former `lease.listener.limits.header_timeout_seconds` default, fixed
/// when the key was removed [ORB-22808].
pub const LEASE_LISTENER_HEADER_TIMEOUT_SECONDS: u32 = 5;
/// The former `lease.listener.limits.request_timeout_seconds` default, fixed
/// when the key was removed [ORB-22808].
pub const LEASE_LISTENER_REQUEST_TIMEOUT_SECONDS: u32 = 10;
/// The former `lease.listener.limits.max_tracked_keys` default, fixed
/// when the key was removed [ORB-22808].
pub const LEASE_LISTENER_MAX_TRACKED_KEYS: u32 = 4096;

/// Validated listener resource limits, independent of Orbit Web on loopback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseListenerLimits {
    /// Requests per minute from one resolved address or IPv6 prefix.
    pub address_per_minute: u32,
    /// IPv6 prefix length for address budgets; mapped IPv4 remains per address.
    pub ipv6_prefix_len: u32,
    /// Read requests per minute per token, independent of writes and address.
    pub token_reads_per_minute: u32,
    /// Write requests per minute per token, independent of reads and address.
    pub token_writes_per_minute: u32,
    /// Open notice streams per token; streams also count as in-flight requests.
    pub max_streams_per_token: u32,
    /// In-flight requests per token, including open streams.
    pub max_in_flight_per_token: u32,
    /// Concurrent listener connections, including TLS handshakes.
    pub max_connections: u32,
    /// Concurrent listener requests; excess receives 503.
    pub max_in_flight_requests: u32,
    /// Maximum request body bytes; oversized requests receive 413.
    pub max_body_bytes: u32,
    /// Seconds allowed to receive HTTP headers or complete a TLS handshake.
    pub header_timeout_seconds: u32,
    /// Seconds allowed for a request handler and body receipt.
    pub request_timeout_seconds: u32,
    /// Maximum rate-limit identities, including two shared overflow buckets.
    pub max_tracked_keys: u32,
}

impl Default for LeaseListenerLimits {
    fn default() -> Self {
        Self {
            address_per_minute: LEASE_LISTENER_ADDRESS_PER_MINUTE,
            ipv6_prefix_len: LEASE_LISTENER_IPV6_PREFIX_LEN,
            token_reads_per_minute: LEASE_LISTENER_TOKEN_READS_PER_MINUTE,
            token_writes_per_minute: LEASE_LISTENER_TOKEN_WRITES_PER_MINUTE,
            max_streams_per_token: LEASE_LISTENER_MAX_STREAMS_PER_TOKEN,
            max_in_flight_per_token: LEASE_LISTENER_MAX_IN_FLIGHT_PER_TOKEN,
            max_connections: LEASE_LISTENER_MAX_CONNECTIONS,
            max_in_flight_requests: LEASE_LISTENER_MAX_IN_FLIGHT_REQUESTS,
            max_body_bytes: LEASE_LISTENER_MAX_BODY_BYTES,
            header_timeout_seconds: LEASE_LISTENER_HEADER_TIMEOUT_SECONDS,
            request_timeout_seconds: LEASE_LISTENER_REQUEST_TIMEOUT_SECONDS,
            max_tracked_keys: LEASE_LISTENER_MAX_TRACKED_KEYS,
        }
    }
}

pub(super) fn retain_response_allowance<G: Send + 'static>(
    response: Response,
    guard: G,
) -> Response {
    response.map(|body| {
        Body::new(AllowanceBody {
            body,
            guard: Some(guard),
        })
    })
}

struct AllowanceBody<G> {
    body: Body,
    guard: Option<G>,
}

// The guard has no pin-sensitive operations and is never projected as pinned.
impl<G> Unpin for AllowanceBody<G> {}

impl<G: Send + 'static> hyper::body::Body for AllowanceBody<G> {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let this = self.get_mut();
        let frame = Pin::new(&mut this.body).poll_frame(cx);
        if matches!(&frame, Poll::Ready(None | Some(Err(_)))) || this.body.is_end_stream() {
            this.guard.take();
        }
        frame
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }
    fn size_hint(&self) -> hyper::body::SizeHint {
        self.body.size_hint()
    }
}
