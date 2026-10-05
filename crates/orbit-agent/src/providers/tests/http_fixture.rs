//! Local HTTP fixture shared by the HTTP transport tests.
//!
//! Serves endless chunked responses over a real socket so transports exercise their
//! actual `reqwest` read path, including chunked bodies that never end on
//! their own.

use crate::providers::http_body::MAX_RESPONSE_BODY_BYTES;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

const STREAM_CHUNK_BYTES: usize = 64 * 1024;

/// Upper bound for an endless stream. Reaching it means the client kept
/// reading far past the response cap instead of disconnecting.
pub(crate) const ENDLESS_STREAM_CEILING: usize = 4 * MAX_RESPONSE_BODY_BYTES;

pub(crate) struct FixtureResponse {
    pub status: u16,
}

impl FixtureResponse {
    pub(crate) fn endless(status: u16) -> Self {
        Self { status }
    }
}

pub(crate) struct ServedRequest {
    pub body_bytes_written: usize,
}

/// Serves one endless response per incoming connection, in order. Joining
/// the handle returns the body bytes written to each connection.
pub(crate) fn serve(
    responses: Vec<FixtureResponse>,
) -> (String, thread::JoinHandle<Vec<ServedRequest>>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind listener");
    let addr = listener.local_addr().expect("listener address");
    let handle = thread::spawn(move || {
        responses
            .into_iter()
            .map(|response| {
                let (mut stream, _) = listener.accept().expect("accept request");
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .expect("set read timeout");
                // Guards against a client that stops reading but keeps the
                // connection open; the stream then ends below the ceiling.
                stream
                    .set_write_timeout(Some(Duration::from_secs(10)))
                    .expect("set write timeout");
                read_request(&mut stream);
                let body_bytes_written = write_response(&mut stream, response);
                ServedRequest { body_bytes_written }
            })
            .collect()
    });
    (format!("http://{addr}"), handle)
}

fn read_request(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut buf = [0_u8; 4096];
    let header_end = loop {
        let n = stream.read(&mut buf).expect("read request");
        assert!(n > 0, "client closed before sending request headers");
        bytes.extend_from_slice(&buf[..n]);
        if let Some(pos) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let head = String::from_utf8_lossy(&bytes[..header_end]).into_owned();
    let content_length = head
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    // Drain the request body so closing the socket does not reset the
    // connection before the client reads the response.
    while bytes.len() < header_end + content_length {
        let n = stream.read(&mut buf).expect("read request body");
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&buf[..n]);
    }
    head.lines().next().unwrap_or_default().to_string()
}

fn write_response(stream: &mut TcpStream, response: FixtureResponse) -> usize {
    let status_line = format!("HTTP/1.1 {} Fixture\r\n", response.status);

    let head = format!(
        "{status_line}content-type: application/json\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n"
    );
    stream.write_all(head.as_bytes()).expect("write head");
    let mut frame = format!("{STREAM_CHUNK_BYTES:x}\r\n").into_bytes();
    frame.extend(std::iter::repeat_n(b'x', STREAM_CHUNK_BYTES));
    frame.extend_from_slice(b"\r\n");
    let mut written = 0;
    while written < ENDLESS_STREAM_CEILING {
        if stream.write_all(&frame).is_err() {
            return written;
        }
        written += STREAM_CHUNK_BYTES;
    }
    let _ = stream.write_all(b"0\r\n\r\n");
    written
}
