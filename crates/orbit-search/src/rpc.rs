//! JSON-Lines RPC envelope shared between the orbit binary and the
//! `orbit-search-companion` subprocess. The protocol is deliberately small:
//! `info`, `embed`, `token_count`, `token_boundaries`, `exit`. Both sides
//! serialize via serde.

use orbit_common::OrbitError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum RpcRequest {
    Info { id: u64 },
    Embed { id: u64, texts: Vec<String> },
    TokenCount { id: u64, text: String },
    TokenBoundaries { id: u64, text: String },
    Exit { id: u64 },
}

impl RpcRequest {
    pub fn id(&self) -> u64 {
        match self {
            Self::Info { id }
            | Self::Embed { id, .. }
            | Self::TokenCount { id, .. }
            | Self::TokenBoundaries { id, .. }
            | Self::Exit { id } => *id,
        }
    }
}

/// Response id used when a malformed request line carries no trustworthy id.
///
/// Clients number their requests from 1, so `0` can never collide with an
/// in-flight request and such a response reads as uncorrelated.
pub const UNCORRELATED_REQUEST_ID: u64 = 0;

/// Best-effort correlation id for a line that failed to parse as an
/// [`RpcRequest`].
///
/// A line is trustworthy only when it is a JSON object whose `id` member is an
/// unsigned integer — enough to answer the right caller without treating
/// arbitrary malformed input as a valid request. Anything else (non-JSON, a
/// JSON array or scalar, a missing/negative/fractional/string `id`) yields
/// [`UNCORRELATED_REQUEST_ID`].
pub fn unparsed_request_id(line: &str) -> u64 {
    serde_json::from_str::<serde_json::Value>(line)
        .ok()
        .and_then(|value| value.get("id")?.as_u64())
        .unwrap_or(UNCORRELATED_REQUEST_ID)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum RpcResponse {
    Result { id: u64, result: RpcResult },
    Error { id: u64, error: RpcError },
}

impl RpcResponse {
    /// The request id this response claims to answer.
    pub fn id(&self) -> u64 {
        match self {
            Self::Result { id, .. } | Self::Error { id, .. } => *id,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum RpcResult {
    Info {
        model_id: String,
        dim: usize,
        max_input_tokens: usize,
        version: Option<String>,
    },
    Embed {
        vectors: Vec<Vec<f32>>,
    },
    TokenCount {
        tokens: usize,
    },
    TokenBoundaries {
        ends: Vec<usize>,
    },
    Exit {
        ok: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RpcError {
    pub code: String,
    pub message: String,
}

/// Translate a companion [`RpcError`] into the workspace-public [`OrbitError`]
/// surface at the subprocess boundary.
///
/// Every error the companion reports over the wire is an execution failure on
/// its side, so the whole `code` set collapses into [`OrbitError::Execution`];
/// callers translate with `.map_err(rpc_error_to_orbit)?` per
/// `docs/design-patterns/error_translation.md` [ORB-10013].
pub fn rpc_error_to_orbit(error: RpcError) -> OrbitError {
    OrbitError::Execution(format!(
        "search companion {}: {}",
        error.code, error.message
    ))
}
