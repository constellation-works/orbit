//! Listener-only configuration and response-lifetime permits.

use std::pin::Pin;
use std::task::{Context, Poll};

use axum::body::{Body, Bytes};
use axum::response::Response;
use hyper::body::Frame;
use orbit_config::LeaseListenerSettings;

/// Validated listener resource limits, independent of Orbit Web on loopback.
#[derive(Debug, Clone)]
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
            address_per_minute: 120,
            ipv6_prefix_len: 64,
            token_reads_per_minute: 60,
            token_writes_per_minute: 20,
            max_streams_per_token: 2,
            max_in_flight_per_token: 8,
            max_connections: 64,
            max_in_flight_requests: 16,
            max_body_bytes: 65536,
            header_timeout_seconds: 5,
            request_timeout_seconds: 10,
            max_tracked_keys: 4096,
        }
    }
}

impl LeaseListenerLimits {
    pub(super) fn from_settings(settings: &LeaseListenerSettings) -> Self {
        Self {
            address_per_minute: settings.address_per_minute,
            ipv6_prefix_len: settings.ipv6_prefix_len,
            token_reads_per_minute: settings.token_reads_per_minute,
            token_writes_per_minute: settings.token_writes_per_minute,
            max_streams_per_token: settings.max_streams_per_token,
            max_in_flight_per_token: settings.max_in_flight_per_token,
            max_connections: settings.max_connections,
            max_in_flight_requests: settings.max_in_flight_requests,
            max_body_bytes: settings.max_body_bytes,
            header_timeout_seconds: settings.header_timeout_seconds,
            request_timeout_seconds: settings.request_timeout_seconds,
            max_tracked_keys: settings.max_tracked_keys,
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
