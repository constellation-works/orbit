//! Short-lived read grants that let a session's own shell wait on attention.
//!
//! A shell caller holds no capability, so `orbit attention` is refused there.
//! A session that does hold one may mint a grant for the workspaces it just
//! read and hand the token to its shell. The grant stands for one thing:
//! reading attention's agent-readable kinds for those workspaces until it
//! expires. It never carries the minting session's capabilities, so an
//! operator session cannot lend operator reads, and it does not authorize any
//! write.
//!
//! The store keeps only the SHA-256 of the token, in one private file per
//! grant under the global root. A file is not authority on its own: nothing
//! reads a grant without presenting the token whose digest names it.
use crate::OrbitRuntime;
use chrono::{DateTime, Duration, Utc};
use orbit_common::{
    OrbitError,
    fs::io::{create_private_dir_all, remove_path_if_exists, write_new_private_text},
    governance::attention::ATTENTION_WAIT_TOKEN_ENV,
    security::token::random_hex,
};
use orbit_types::tool::AttentionWorkspace;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Prefix of a wait token, so a captured one is recognisable.
const WAIT_TOKEN_PREFIX: &str = "oatw_";
/// Random bytes in a token (256 bits).
const WAIT_TOKEN_BYTES: usize = 32;
/// How long a minted grant stays valid.
pub(crate) const WAIT_GRANT_TTL_MINUTES: i64 = 120;
const GRANT_DIR: &str = "attention-wait-grants";

/// What a presented token stands for.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredWaitGrant {
    workspace_ids: Vec<String>,
    workspace_names: Vec<String>,
    minted_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

fn grant_dir(global_root: &Path) -> PathBuf {
    global_root.join(GRANT_DIR)
}

fn grant_path(global_root: &Path, token: &str) -> PathBuf {
    let digest = Sha256::digest(token.as_bytes());
    grant_dir(global_root).join(format!(
        "{}.json",
        orbit_types::identity::lower_hex(digest.as_slice())
    ))
}

fn refusal(reason: &str) -> OrbitError {
    OrbitError::CapabilityDenied(format!(
        "attention wait needs a grant minted by a session ({reason}); call orbit.attention with \
         wait_grant true from an MCP session and export the returned token as \
         {ATTENTION_WAIT_TOKEN_ENV} in the shell"
    ))
}

/// Attach a freshly minted grant, for exactly the workspaces `response` read,
/// as its `wait_grant` field. An unreadable workspace scope mints nothing.
pub(crate) fn with_wait_grant(
    runtime: &OrbitRuntime,
    mut response: Value,
) -> Result<Value, OrbitError> {
    let workspaces = response["filters"]["workspace"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|value| serde_json::from_value::<AttentionWorkspace>(value.clone()).ok())
        .collect::<Vec<_>>();
    response["wait_grant"] = mint_wait_grant(&runtime.global_root(), &workspaces, Utc::now())?;
    Ok(response)
}

fn mint_wait_grant(
    global_root: &Path,
    workspaces: &[AttentionWorkspace],
    now: DateTime<Utc>,
) -> Result<Value, OrbitError> {
    if workspaces.is_empty() {
        return Err(OrbitError::InvalidInput(
            "wait_grant needs at least one resolved workspace".into(),
        ));
    }
    prune_expired_grants(global_root, now);
    let token = format!("{WAIT_TOKEN_PREFIX}{}", random_hex(WAIT_TOKEN_BYTES)?);
    let expires_at = now + Duration::minutes(WAIT_GRANT_TTL_MINUTES);
    let grant = StoredWaitGrant {
        workspace_ids: workspaces.iter().map(|w| w.id.clone()).collect(),
        workspace_names: workspaces.iter().map(|w| w.name.clone()).collect(),
        minted_at: now,
        expires_at,
    };
    let body = serde_json::to_string(&grant)
        .map_err(|error| OrbitError::Execution(format!("serialize wait grant: {error}")))?;
    let path = grant_path(global_root, &token);
    if let Some(parent) = path.parent() {
        create_private_dir_all(parent)
            .map_err(|error| OrbitError::Execution(format!("create wait grant store: {error}")))?;
    }
    write_new_private_text(&path, &body)
        .map_err(|error| OrbitError::Execution(format!("store wait grant: {error}")))?;
    Ok(json!({
        "token": token,
        "env": ATTENTION_WAIT_TOKEN_ENV,
        "expires_at": expires_at,
        "workspaces": workspaces.iter().map(|w| &w.name).collect::<Vec<_>>(),
        "scope": "attention read of agent-readable kinds for these workspaces; no writes, no operator kinds",
    }))
}

/// Best effort: a grant that cannot be removed is still refused once expired.
/// Only files that look like grant records are candidates: the atomic store
/// stages a mint in a sibling temp file in this same directory, so an
/// unreadable or unparseable entry is another mint's in-flight staging file
/// (or a newer binary's record), never an expired grant.
fn prune_expired_grants(global_root: &Path, now: DateTime<Utc>) {
    let Ok(entries) = std::fs::read_dir(grant_dir(global_root)) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !is_grant_file_name(&path) {
            continue;
        }
        let expired = match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<StoredWaitGrant>(&text) {
                Ok(grant) => grant.expires_at <= now,
                // Leave unreadable-shape records in place: a newer binary's
                // grant still stands for its token, and redeem refuses what
                // this binary cannot parse.
                Err(_) => false,
            },
            Err(_) => false,
        };
        if expired {
            let _ = remove_path_if_exists(&path);
        }
    }
}

/// Grant files are `<64 hex>.json`: the lowercase hex SHA-256 of the token.
/// Anything else — a staging temp file (`.<grant>.<nanos>.<counter>.tmp`),
/// a stray file — is left alone.
fn is_grant_file_name(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    const HEX_LEN: usize = 64;
    const SUFFIX: &str = ".json";
    if name.len() != HEX_LEN + SUFFIX.len() {
        return false;
    }
    let (hex, suffix) = name.split_at(HEX_LEN);
    if suffix != SUFFIX {
        return false;
    }
    hex.bytes()
        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn redeem_wait_grant(
    global_root: &Path,
    token: &str,
    now: DateTime<Utc>,
) -> Result<StoredWaitGrant, OrbitError> {
    let token = token.trim();
    if !token.starts_with(WAIT_TOKEN_PREFIX) {
        return Err(refusal("the token is malformed"));
    }
    let text = std::fs::read_to_string(grant_path(global_root, token))
        .map_err(|_| refusal("no such grant on this host"))?;
    let grant: StoredWaitGrant =
        serde_json::from_str(&text).map_err(|_| refusal("the grant record is unreadable"))?;
    if grant.expires_at <= now {
        return Err(refusal("the grant has expired"));
    }
    Ok(grant)
}

/// A redeemed grant: what one shell may read.
pub(crate) struct AttentionWaitScope(StoredWaitGrant);

impl AttentionWaitScope {
    /// Redeem `token`, or refuse it as `capability_denied`. A missing,
    /// unknown or expired token is never read as clean.
    pub(crate) fn redeem(global_root: &Path, token: &str) -> Result<Self, OrbitError> {
        redeem_wait_grant(global_root, token, Utc::now()).map(Self)
    }

    /// Refuse an input that reaches past the grant: the whole host, other
    /// hosts, grant minting, or a workspace the grant does not name.
    pub(crate) fn check_input(&self, input: &Value) -> Result<(), OrbitError> {
        for field in ["all", "host", "wait_grant", "candidate_read"] {
            if input.get(field).is_some_and(|value| !value.is_null()) {
                return Err(OrbitError::CapabilityDenied(format!(
                    "attention wait reads only the workspaces its grant names; `{field}` is not allowed"
                )));
            }
        }
        let selectors =
            orbit_common::governance::attention::filter::attention_strings(input, "workspace");
        if let Some(outside) = selectors.iter().find(|selector| {
            !self.0.workspace_ids.contains(selector) && !self.0.workspace_names.contains(selector)
        }) {
            return Err(OrbitError::CapabilityDenied(format!(
                "workspace `{outside}` is outside this attention wait grant"
            )));
        }
        Ok(())
    }

    /// Refuse a response whose echoed scope is not wholly inside the grant,
    /// whichever way the selector resolved.
    pub(crate) fn check_response(&self, response: &Value) -> Result<(), OrbitError> {
        let read = response["filters"]["workspace"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        let outside = read.is_empty()
            || read.iter().any(|workspace| {
                !workspace["id"]
                    .as_str()
                    .is_some_and(|id| self.0.workspace_ids.iter().any(|granted| granted == id))
            });
        if outside {
            return Err(OrbitError::CapabilityDenied(
                "this attention wait grant does not cover the workspace read; pass --workspace \
                 for one the minting session read"
                    .into(),
            ));
        }
        Ok(())
    }
}
