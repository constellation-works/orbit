//! Bounded response-body reads shared by the HTTP transports.
//!
//! A configurable or compromised endpoint can stream an arbitrarily large
//! body within the request timeout. Every transport reads through these
//! helpers so success bodies stop at a fixed ceiling and error bodies keep
//! only a short diagnostic prefix. The ceiling is enforced while reading, so
//! chunked responses without `Content-Length` are bounded too.

use std::io::Read;

use reqwest::blocking::Response;

use crate::loop_engine::transport::TransportError;

/// Largest success body a transport will buffer (16 MiB).
pub(crate) const MAX_RESPONSE_BODY_BYTES: usize = 16 * 1024 * 1024;

/// Largest error-body prefix kept for diagnostics (64 KiB).
pub(crate) const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;

/// Reads a success body, refusing it once it exceeds
/// [`MAX_RESPONSE_BODY_BYTES`].
pub(crate) fn read_response_body(response: Response) -> Result<Vec<u8>, TransportError> {
    if response
        .content_length()
        .is_some_and(|len| len > MAX_RESPONSE_BODY_BYTES as u64)
    {
        return Err(oversized_body_error());
    }
    let (bytes, truncated) = read_capped(response, MAX_RESPONSE_BODY_BYTES)?;
    if truncated {
        return Err(oversized_body_error());
    }
    Ok(bytes)
}

/// Reads at most [`MAX_ERROR_BODY_BYTES`] of a non-2xx body and renders it
/// as a diagnostic string, marking it when the remainder was not read.
pub(crate) fn read_error_body(response: Response) -> Result<String, TransportError> {
    let (bytes, truncated) = read_capped(response, MAX_ERROR_BODY_BYTES)?;
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if truncated {
        text.push_str(&truncation_marker());
    }
    Ok(text)
}

/// Renders an already-buffered body for a diagnostic message, keeping at most
/// [`MAX_ERROR_BODY_BYTES`].
pub(crate) fn body_diagnostic(bytes: &[u8]) -> String {
    if bytes.len() <= MAX_ERROR_BODY_BYTES {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut text = String::from_utf8_lossy(&bytes[..MAX_ERROR_BODY_BYTES]).into_owned();
    text.push_str(&truncation_marker());
    text
}

/// Reads up to `cap` bytes; the flag reports whether more bytes followed.
/// Only `cap + 1` bytes are ever pulled from the connection.
fn read_capped(response: Response, cap: usize) -> Result<(Vec<u8>, bool), TransportError> {
    let mut bytes = Vec::new();
    response
        .take(cap as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| TransportError::Network(format!("read body: {}", io_error_message(e))))?;
    let truncated = bytes.len() > cap;
    bytes.truncate(cap);
    Ok((bytes, truncated))
}

fn oversized_body_error() -> TransportError {
    TransportError::Decode(format!(
        "response body exceeds the {MAX_RESPONSE_BODY_BYTES}-byte limit"
    ))
}

fn truncation_marker() -> String {
    format!("... [truncated after {MAX_ERROR_BODY_BYTES} bytes]")
}

/// Body-read failures wrap a `reqwest::Error`; strip its URL before
/// stringifying so endpoint query parameters never reach diagnostics.
fn io_error_message(error: std::io::Error) -> String {
    let kind = error.kind();
    match error.into_inner() {
        Some(inner) => match inner.downcast::<reqwest::Error>() {
            Ok(reqwest_error) => reqwest_error.without_url().to_string(),
            Err(other) => other.to_string(),
        },
        None => kind.to_string(),
    }
}
