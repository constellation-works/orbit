//! Bounded HTTP/1 connections, including TLS handshakes and header reads.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::{Extension, Router, extract::ConnectInfo};
use hyper::server::conn::http1;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinSet;
use tokio_rustls::TlsAcceptor;

use super::admission::REQUEST_WINDOW;
use super::{LeaseListenerConfig, ListenerGate};

const OVERLOADED: &[u8] = b"HTTP/1.1 503 Service Unavailable\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: 27\r\n\r\n{\"error\":\"max_connections\"}";
/// Most request-head bytes a refused connection reads before its 503.
const REFUSED_HEAD_LIMIT: usize = 8 * 1024;

pub(super) async fn serve_bounded_listener(
    listener: TcpListener,
    config: LeaseListenerConfig,
    app: Router,
    gate: Arc<ListenerGate>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> io::Result<()> {
    let connections = Arc::new(Semaphore::new(config.limits.max_connections as usize));
    // A single, time-bounded refusal lane can negotiate TLS and return an
    // encrypted 503 without opening unbounded handshake tasks at saturation.
    let refusals = Arc::new(Semaphore::new(1));
    let mut tasks = JoinSet::new();
    let (closing, _) = watch::channel(false);
    let timeout = Duration::from_secs(u64::from(config.limits.header_timeout_seconds));
    let tls = config.tls.map(TlsAcceptor::from);
    let mut audit_tick = tokio::time::interval(Duration::from_secs(1));
    tokio::pin!(shutdown);
    let outcome = loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => break Ok(()),
            _ = audit_tick.tick() => gate.flush_audits(false).await,
            Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                if let Err(error) = result { tracing::warn!(%error, "lease connection task failed"); }
            }
            accepted = listener.accept() => {
                let (stream, peer) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => break Err(error),
                };
                let (permit, overloaded) = match Arc::clone(&connections).try_acquire_owned() {
                    Ok(permit) => (permit, false),
                    Err(_) => {
                        let key = match gate.limiter.lock() {
                            Ok(mut limiter) => { let key = limiter.address_key(peer.ip()); limiter.bounded_key(key) },
                            Err(_) => { continue; }
                        };
                        gate.audit(&key, "max_connections", REQUEST_WINDOW).await;
                        match Arc::clone(&refusals).try_acquire_owned() {
                            Ok(permit) => (permit, true),
                            Err(_) => {
                                // No work is queued. Plain HTTP can receive a best-effort
                                // 503 immediately; TLS closes if its refusal lane is busy.
                                if tls.is_none() { let _ = stream.try_write(OVERLOADED); }
                                continue;
                            }
                        }
                    }
                };
                let app = app.clone();
                let tls = tls.clone();
                let closing = closing.subscribe();
                tasks.spawn(async move {
                    let _permit = permit;
                    let result = serve_connection(stream, peer, tls, app, timeout, overloaded, closing).await;
                    if let Err(error) = result { tracing::debug!(%error, "lease connection closed"); }
                });
            }
        }
    };
    let _ = closing.send(true);
    gate.closing.send_replace(true);
    let grace = Duration::from_secs(u64::from(config.limits.request_timeout_seconds)) + timeout;
    let drained =
        tokio::time::timeout(grace, async { while tasks.join_next().await.is_some() {} }).await;
    if drained.is_err() {
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
    }
    gate.flush_audits(true).await;
    outcome
}

async fn serve_connection(
    stream: TcpStream,
    peer: SocketAddr,
    tls: Option<TlsAcceptor>,
    app: Router,
    timeout: Duration,
    overloaded: bool,
    mut closing: watch::Receiver<bool>,
) -> io::Result<()> {
    if *closing.borrow() {
        return Ok(());
    }
    if let Some(tls) = tls {
        let stream = tokio::select! {
            result = tokio::time::timeout(timeout, tls.accept(stream)) => result
                .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "TLS handshake timed out"))??,
            _ = closing.changed() => return Ok(()),
        };
        serve_http_connection(stream, peer, app, timeout, overloaded, closing).await
    } else {
        serve_http_connection(stream, peer, app, timeout, overloaded, closing).await
    }
}

/// Reads up to the blank line that ends a request head, EOF, or `REFUSED_HEAD_LIMIT` bytes.
async fn read_refused_head<S: AsyncRead + Unpin>(stream: &mut S) -> io::Result<()> {
    let mut head = Vec::new();
    let mut chunk = [0; 1024];
    while head.len() < REFUSED_HEAD_LIMIT && !head.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        head.extend_from_slice(&chunk[..read]);
    }
    Ok(())
}

async fn serve_http_connection<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    mut stream: S,
    peer: SocketAddr,
    app: Router,
    timeout: Duration,
    overloaded: bool,
    mut closing: watch::Receiver<bool>,
) -> io::Result<()> {
    if overloaded {
        // Read the request head before answering. hyper's client reports a response
        // that arrives before its request is in flight as an unexpected message, and
        // closing with the request unread can reset the 503 away. A client that never
        // sends a head still gets the 503 when the header timeout ends.
        let _ = tokio::time::timeout(timeout, read_refused_head(&mut stream)).await;
        return tokio::time::timeout(timeout, async {
            stream.write_all(OVERLOADED).await?;
            stream.shutdown().await
        })
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "overload response timed out"))?;
    }
    let mut builder = http1::Builder::new();
    // Close after the response: idle keep-alives cannot monopolize the
    // connection budget. SSE still owns its socket until its body closes.
    builder
        .keep_alive(false)
        .max_headers(64)
        .max_buf_size(16 * 1024)
        .timer(TokioTimer::new())
        .header_read_timeout(timeout);
    let app = app.layer(Extension(ConnectInfo(peer)));
    let connection = builder.serve_connection(TokioIo::new(stream), TowerToHyperService::new(app));
    tokio::pin!(connection);
    tokio::select! {
        result = &mut connection => result.map_err(io::Error::other),
        _ = closing.changed() => {
            connection.as_mut().graceful_shutdown();
            connection.await.map_err(io::Error::other)
        }
    }
}
